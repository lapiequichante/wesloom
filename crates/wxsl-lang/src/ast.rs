//! The syntax tree.
//!
//! Close to WGSL's own grammar, with four additions
//! ([ADR 0011](../../../docs/adr/0011-own-the-shading-language.md)):
//! [`Import`], [`GenericParam`] on a function or struct, `@if` attributes
//! read from [`Attribute`], and macro constants — a `const` carrying the
//! `@macro` attribute, which makes it overridable per compilation and per
//! node instance.
//!
//! # Shape decisions
//!
//! *Spans everywhere.* Every node that can be the subject of a diagnostic
//! carries one, because an error has to point back at a `.wxsl` line
//! through import mangling and template instantiation.
//!
//! *Literals stay text.* [`Literal`] holds the source spelling. Passes that
//! need a value parse it themselves; passes that only rewrite and re-emit —
//! which is most of them — cannot corrupt a constant by round-tripping it.
//!
//! *No name resolution.* An identifier is a string here. Resolution happens
//! in a later pass that has the module map, so the tree stays a faithful
//! record of what was written.

use crate::span::{Span, Spanned};

/// One parsed source file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    /// `enable` / `requires` directives, which must precede everything else.
    pub directives: Vec<Directive>,
    /// `import` declarations.
    pub imports: Vec<Import>,
    /// Everything else, in source order — order matters for emission.
    pub declarations: Vec<Declaration>,
}

impl Module {
    /// The declaration named `name`, if any.
    pub fn declaration(&self, name: &str) -> Option<&Declaration> {
        self.declarations
            .iter()
            .find(|declaration| declaration.name().is_some_and(|found| found == name))
    }

    /// Every macro constant this module declares, in source order.
    ///
    /// These are the knobs a graph can set globally or a node can override.
    pub fn macros(&self) -> impl Iterator<Item = &GlobalValue> {
        self.declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Const(value) if value.is_macro() => Some(value),
                _ => None,
            })
    }
}

/// `enable f16;` or `requires readonly_and_readwrite_storage_textures;`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directive {
    /// `enable` or `requires`.
    pub kind: DirectiveKind,
    /// The names listed.
    pub names: Vec<Spanned<String>>,
    /// The whole directive.
    pub span: Span,
}

/// Which directive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectiveKind {
    /// `enable`
    Enable,
    /// `requires`
    Requires,
}

/// `import package::math::remap;` or `import package::a::{b, c as d};`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The module path, without the imported item names.
    pub path: ModulePath,
    /// What is being taken from it. Empty means the whole module.
    pub items: Vec<ImportItem>,
    /// The whole declaration.
    pub span: Span,
}

/// One name taken from a module, optionally renamed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportItem {
    /// The name as declared in the source module.
    pub name: Spanned<String>,
    /// The name it is bound to here, if renamed with `as`.
    pub alias: Option<Spanned<String>>,
    /// The name this item is referred to by in this module.
    pub local: String,
}

/// A `::`-separated module path, such as `package::math::remap`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePath {
    /// The segments, without separators.
    pub segments: Vec<String>,
}

impl ModulePath {
    /// Build a path from its segments.
    pub fn new(segments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        ModulePath {
            segments: segments.into_iter().map(Into::into).collect(),
        }
    }

    /// Parse `package::a::b`. Returns `None` if any segment is empty.
    pub fn parse(text: &str) -> Option<Self> {
        let segments: Vec<String> = text.split("::").map(str::to_string).collect();
        if segments.is_empty() || segments.iter().any(String::is_empty) {
            return None;
        }
        Some(ModulePath { segments })
    }

    /// The last segment.
    pub fn last(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }

    /// This path with `segment` appended.
    pub fn join(&self, segment: impl Into<String>) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment.into());
        ModulePath { segments }
    }

    /// This path without its last segment.
    pub fn parent(&self) -> Option<Self> {
        (self.segments.len() > 1).then(|| ModulePath {
            segments: self.segments[..self.segments.len() - 1].to_vec(),
        })
    }
}

impl core::fmt::Display for ModulePath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.pad(&self.segments.join("::"))
    }
}

/// A module-level declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum Declaration {
    /// `const x: T = e;` — or, with `@macro`, an overridable macro constant.
    Const(GlobalValue),
    /// `override x: T = e;` — a pipeline-overridable constant.
    Override(GlobalValue),
    /// `var<uniform> x: T;`
    Var(GlobalValue),
    /// `alias T = U;`
    Alias(AliasDecl),
    /// `struct S { … }`
    Struct(StructDecl),
    /// `fn f(…) -> T { … }`
    Function(Function),
    /// `const_assert e;`
    ConstAssert(ConstAssert),
}

impl Declaration {
    /// The name this declaration binds, if it binds one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                Some(&value.name.node)
            }
            Declaration::Alias(alias) => Some(&alias.name.node),
            Declaration::Struct(item) => Some(&item.name.node),
            Declaration::Function(function) => Some(&function.name.node),
            Declaration::ConstAssert(_) => None,
        }
    }

    /// The attributes on this declaration.
    pub fn attributes(&self) -> &[Attribute] {
        match self {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                &value.attributes
            }
            Declaration::Alias(alias) => &alias.attributes,
            Declaration::Struct(item) => &item.attributes,
            Declaration::Function(function) => &function.attributes,
            Declaration::ConstAssert(assert) => &assert.attributes,
        }
    }

    /// The whole declaration's span.
    pub fn span(&self) -> Span {
        match self {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                value.span
            }
            Declaration::Alias(alias) => alias.span,
            Declaration::Struct(item) => item.span,
            Declaration::Function(function) => function.span,
            Declaration::ConstAssert(assert) => assert.span,
        }
    }

    /// The generic parameters this declaration introduces.
    pub fn generics(&self) -> &[GenericParam] {
        match self {
            Declaration::Function(function) => &function.generics,
            Declaration::Struct(item) => &item.generics,
            _ => &[],
        }
    }
}

/// A `const`, `override` or `var` declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalValue {
    /// Attributes, including `@macro` and `@group`/`@binding`.
    pub attributes: Vec<Attribute>,
    /// The template on a `var`: `var<uniform>`, `var<storage, read>`.
    pub address_space: Vec<TemplateArg>,
    /// The name.
    pub name: Spanned<String>,
    /// The declared type, if written.
    pub ty: Option<TypeExpr>,
    /// The initializer, if written.
    pub init: Option<Expr>,
    /// The whole declaration.
    pub span: Span,
}

impl GlobalValue {
    /// Whether this is a macro constant — a `const` marked `@macro`, whose
    /// value a graph or a node instance may override.
    pub fn is_macro(&self) -> bool {
        self.attributes
            .iter()
            .any(|attribute| attribute.name.node == "macro")
    }
}

/// `alias Name = Type;`
#[derive(Clone, Debug, PartialEq)]
pub struct AliasDecl {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The alias name.
    pub name: Spanned<String>,
    /// What it names.
    pub ty: TypeExpr,
    /// The whole declaration.
    pub span: Span,
}

/// `struct Name<T: …> { members }`
#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The struct name.
    pub name: Spanned<String>,
    /// Generic parameters, empty for a concrete struct.
    pub generics: Vec<GenericParam>,
    /// The members, in declaration order — which is the memory layout.
    pub members: Vec<StructMember>,
    /// The whole declaration.
    pub span: Span,
}

/// One `name: type` member, with its attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct StructMember {
    /// Attributes such as `@location`, `@builtin`, `@align`, `@if`.
    pub attributes: Vec<Attribute>,
    /// The member name.
    pub name: Spanned<String>,
    /// The member type.
    pub ty: TypeExpr,
    /// The whole member.
    pub span: Span,
}

/// `fn name<generics>(params) -> ret { body }`
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Attributes such as `@vertex`, `@fragment`, `@if`.
    pub attributes: Vec<Attribute>,
    /// The function name.
    pub name: Spanned<String>,
    /// Generic parameters, empty for a concrete function.
    pub generics: Vec<GenericParam>,
    /// The parameters.
    pub params: Vec<Param>,
    /// Attributes on the return type, such as `@location(0)`.
    pub return_attributes: Vec<Attribute>,
    /// The return type, absent for a function returning nothing.
    pub return_type: Option<TypeExpr>,
    /// The body.
    pub body: Block,
    /// The whole declaration.
    pub span: Span,
}

impl Function {
    /// Whether this function is templated.
    pub fn is_generic(&self) -> bool {
        !self.generics.is_empty()
    }

    /// The entry-point stage this function declares, if any.
    pub fn stage(&self) -> Option<Stage> {
        self.attributes
            .iter()
            .find_map(|attribute| Stage::from_attribute(&attribute.name.node))
    }
}

/// A shader stage, named by an entry point's attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// `@vertex`
    Vertex,
    /// `@fragment`
    Fragment,
    /// `@compute`
    Compute,
}

impl Stage {
    /// The stage an attribute name denotes.
    pub fn from_attribute(name: &str) -> Option<Stage> {
        match name {
            "vertex" => Some(Stage::Vertex),
            "fragment" => Some(Stage::Fragment),
            "compute" => Some(Stage::Compute),
            _ => None,
        }
    }

    /// The attribute name for this stage.
    pub fn attribute(&self) -> &'static str {
        match self {
            Stage::Vertex => "vertex",
            Stage::Fragment => "fragment",
            Stage::Compute => "compute",
        }
    }
}

/// One function parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Attributes such as `@location`, `@builtin`.
    pub attributes: Vec<Attribute>,
    /// The parameter name.
    pub name: Spanned<String>,
    /// The parameter type.
    pub ty: TypeExpr,
    /// The whole parameter.
    pub span: Span,
}

/// A generic parameter: `T: f32 | vec2f | vec3f`.
///
/// The constraint list is what makes instantiation checkable up front: a
/// call resolving `T` to a type outside the list is an error at the call
/// site, naming the declaration, rather than a puzzle inside an
/// instantiated body.
#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    /// The parameter name, as used in the signature and body.
    pub name: Spanned<String>,
    /// The types it may be instantiated at. Empty means unconstrained.
    pub constraints: Vec<TypeExpr>,
    /// The whole parameter.
    pub span: Span,
}

impl GenericParam {
    /// Whether `ty` satisfies this parameter's constraints.
    ///
    /// An unconstrained parameter accepts anything.
    pub fn accepts(&self, ty: &TypeExpr) -> bool {
        self.constraints.is_empty() || self.constraints.iter().any(|allowed| allowed.same_type(ty))
    }
}

/// `const_assert e;`
#[derive(Clone, Debug, PartialEq)]
pub struct ConstAssert {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The asserted expression.
    pub expr: Expr,
    /// The whole declaration.
    pub span: Span,
}

/// `@name` or `@name(args)`.
///
/// Kept general rather than parsed into an enum: WGSL keeps adding
/// attributes, and passes that do not care about a given one should
/// re-emit it untouched rather than fail on it. The ones with meaning
/// here — `@if`, `@macro`, `@template` — are recognized by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    /// The name, without the `@`.
    pub name: Spanned<String>,
    /// The arguments, empty for a bare attribute.
    pub args: Vec<Expr>,
    /// The whole attribute.
    pub span: Span,
}

impl Attribute {
    /// The condition of an `@if`, or `None` for any other attribute.
    pub fn condition(&self) -> Option<&Expr> {
        (self.name.node == "if")
            .then(|| self.args.first())
            .flatten()
    }
}

/// A type, possibly with a template list: `f32`, `vec3<f32>`,
/// `array<Light, 4>`, `ptr<function, f32>`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    /// The type's name.
    pub name: Spanned<String>,
    /// Template arguments, empty for a plain name.
    pub template_args: Vec<TemplateArg>,
    /// The whole type expression.
    pub span: Span,
}

impl TypeExpr {
    /// A bare named type.
    pub fn named(name: impl Into<String>, span: Span) -> Self {
        TypeExpr {
            name: Spanned::new(name.into(), span),
            template_args: Vec::new(),
            span,
        }
    }

    /// Whether this is the bare name `name` with no template list.
    pub fn is_named(&self, name: &str) -> bool {
        self.template_args.is_empty() && self.name.node == name
    }

    /// Whether this and `other` denote the same type.
    ///
    /// *Not* `==`: the derived `PartialEq` compares spans, so a type parsed
    /// from source never equals an identical one built in Rust. Type
    /// identity is structural, and [`Display`](core::fmt::Display) renders
    /// exactly the structure with no span in it, so comparing the rendered
    /// form is both correct and cheap at the sizes types actually reach.
    pub fn same_type(&self, other: &TypeExpr) -> bool {
        self.name.node == other.name.node
            && self.template_args.len() == other.template_args.len()
            && self.to_string() == other.to_string()
    }
}

impl core::fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.name.node)?;
        if !self.template_args.is_empty() {
            write!(f, "<")?;
            for (index, arg) in self.template_args.iter().enumerate() {
                if index > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{arg}")?;
            }
            write!(f, ">")?;
        }
        Ok(())
    }
}

/// One argument inside a template list.
///
/// Syntactically a template argument is just an expression — `array<f32, 4>`
/// has a type and a count, and only name resolution can tell which is which
/// — so it is stored as one and interpreted later.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateArg {
    /// The argument.
    pub expr: Expr,
}

impl core::fmt::Display for TemplateArg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.expr)
    }
}

/// A brace-delimited list of statements.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    /// Attributes on the block itself, including `@if`.
    pub attributes: Vec<Attribute>,
    /// The statements.
    pub statements: Vec<Statement>,
    /// The whole block, braces included.
    pub span: Span,
}

/// A statement.
#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    /// `let x = e;`, `var x: T = e;`, `const x = e;`
    Local(LocalValue),
    /// `x = e;`, `x += e;`, `_ = e;`
    Assign(Assign),
    /// `x++;` / `x--;`
    Step(Step),
    /// A nested `{ … }`.
    Block(Block),
    /// `if c { … } else if c { … } else { … }`
    If(IfStatement),
    /// `switch e { case … }`
    Switch(SwitchStatement),
    /// `loop { … }`
    Loop(LoopStatement),
    /// `for (init; cond; update) { … }`
    For(ForStatement),
    /// `while c { … }`
    While(WhileStatement),
    /// `return e;`
    Return(ReturnStatement),
    /// `break;`
    Break(Simple),
    /// `continue;`
    Continue(Simple),
    /// `discard;`
    Discard(Simple),
    /// A call used as a statement.
    Call(CallStatement),
    /// `const_assert e;`
    ConstAssert(ConstAssert),
}

impl Statement {
    /// The attributes on this statement, including any `@if`.
    pub fn attributes(&self) -> &[Attribute] {
        match self {
            Statement::Local(local) => &local.attributes,
            Statement::Assign(assign) => &assign.attributes,
            Statement::Step(step) => &step.attributes,
            Statement::Block(block) => &block.attributes,
            Statement::If(item) => &item.attributes,
            Statement::Switch(item) => &item.attributes,
            Statement::Loop(item) => &item.attributes,
            Statement::For(item) => &item.attributes,
            Statement::While(item) => &item.attributes,
            Statement::Return(item) => &item.attributes,
            Statement::Break(item) | Statement::Continue(item) | Statement::Discard(item) => {
                &item.attributes
            }
            Statement::Call(item) => &item.attributes,
            Statement::ConstAssert(item) => &item.attributes,
        }
    }

    /// This statement's span.
    pub fn span(&self) -> Span {
        match self {
            Statement::Local(local) => local.span,
            Statement::Assign(assign) => assign.span,
            Statement::Step(step) => step.span,
            Statement::Block(block) => block.span,
            Statement::If(item) => item.span,
            Statement::Switch(item) => item.span,
            Statement::Loop(item) => item.span,
            Statement::For(item) => item.span,
            Statement::While(item) => item.span,
            Statement::Return(item) => item.span,
            Statement::Break(item) | Statement::Continue(item) | Statement::Discard(item) => {
                item.span
            }
            Statement::Call(item) => item.span,
            Statement::ConstAssert(item) => item.span,
        }
    }
}

/// Which binding form a local declaration uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// `let` — immutable, function scope.
    Let,
    /// `var` — mutable.
    Var,
    /// `const` — compile-time.
    Const,
}

/// `let`/`var`/`const` inside a function.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalValue {
    /// Attributes, including `@if`.
    pub attributes: Vec<Attribute>,
    /// Which form.
    pub kind: LocalKind,
    /// The address space template on a `var`, if written.
    pub address_space: Vec<TemplateArg>,
    /// The name.
    pub name: Spanned<String>,
    /// The declared type, if written.
    pub ty: Option<TypeExpr>,
    /// The initializer, if written.
    pub init: Option<Expr>,
    /// The whole statement.
    pub span: Span,
}

/// An assignment, compound or plain.
#[derive(Clone, Debug, PartialEq)]
pub struct Assign {
    /// Attributes, including `@if`.
    pub attributes: Vec<Attribute>,
    /// The assignment target, or `None` for the phony `_`.
    pub target: Option<Expr>,
    /// The operator.
    pub op: AssignOp,
    /// The right-hand side.
    pub value: Expr,
    /// The whole statement.
    pub span: Span,
}

/// An assignment operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Subtract,
    /// `*=`
    Multiply,
    /// `/=`
    Divide,
    /// `%=`
    Modulo,
    /// `&=`
    And,
    /// `|=`
    Or,
    /// `^=`
    Xor,
    /// `<<=`
    ShiftLeft,
    /// `>>=`
    ShiftRight,
}

impl AssignOp {
    /// The operator as written.
    pub fn text(&self) -> &'static str {
        match self {
            AssignOp::Assign => "=",
            AssignOp::Add => "+=",
            AssignOp::Subtract => "-=",
            AssignOp::Multiply => "*=",
            AssignOp::Divide => "/=",
            AssignOp::Modulo => "%=",
            AssignOp::And => "&=",
            AssignOp::Or => "|=",
            AssignOp::Xor => "^=",
            AssignOp::ShiftLeft => "<<=",
            AssignOp::ShiftRight => ">>=",
        }
    }
}

/// `x++` or `x--`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The target.
    pub target: Expr,
    /// Up or down.
    pub increment: bool,
    /// The whole statement.
    pub span: Span,
}

/// `if`, with any number of `else if` arms and an optional `else`.
#[derive(Clone, Debug, PartialEq)]
pub struct IfStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The `if` and `else if` arms, in order.
    pub arms: Vec<(Expr, Block)>,
    /// The `else` block.
    pub otherwise: Option<Block>,
    /// The whole statement.
    pub span: Span,
}

/// `switch e { … }`
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The selector.
    pub selector: Expr,
    /// The clauses.
    pub clauses: Vec<SwitchClause>,
    /// The whole statement.
    pub span: Span,
}

/// One `case`/`default` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchClause {
    /// Attributes, including `@if`.
    pub attributes: Vec<Attribute>,
    /// The case selectors; an empty entry list is `default`.
    pub selectors: Vec<CaseSelector>,
    /// The clause body.
    pub body: Block,
    /// The whole clause.
    pub span: Span,
}

/// One selector in a `case` list.
#[derive(Clone, Debug, PartialEq)]
pub enum CaseSelector {
    /// A constant expression.
    Value(Expr),
    /// The `default` selector.
    Default(Span),
}

/// `loop { … continuing { … } }`
#[derive(Clone, Debug, PartialEq)]
pub struct LoopStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The body.
    pub body: Block,
    /// The `continuing` block, if written.
    pub continuing: Option<Continuing>,
    /// The whole statement.
    pub span: Span,
}

/// A `continuing` block, with its optional `break if`.
#[derive(Clone, Debug, PartialEq)]
pub struct Continuing {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The body.
    pub body: Block,
    /// The `break if e;` condition, if written.
    pub break_if: Option<Expr>,
    /// The whole block.
    pub span: Span,
}

/// `for (init; condition; update) { … }`
#[derive(Clone, Debug, PartialEq)]
pub struct ForStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The initializer, if written.
    pub init: Option<Box<Statement>>,
    /// The condition, if written.
    pub condition: Option<Expr>,
    /// The update, if written.
    pub update: Option<Box<Statement>>,
    /// The body.
    pub body: Block,
    /// The whole statement.
    pub span: Span,
}

/// `while c { … }`
#[derive(Clone, Debug, PartialEq)]
pub struct WhileStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The condition.
    pub condition: Expr,
    /// The body.
    pub body: Block,
    /// The whole statement.
    pub span: Span,
}

/// `return e;`
#[derive(Clone, Debug, PartialEq)]
pub struct ReturnStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The returned value, if any.
    pub value: Option<Expr>,
    /// The whole statement.
    pub span: Span,
}

/// A statement that is only a keyword: `break`, `continue`, `discard`.
#[derive(Clone, Debug, PartialEq)]
pub struct Simple {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The whole statement.
    pub span: Span,
}

/// A function call used as a statement.
#[derive(Clone, Debug, PartialEq)]
pub struct CallStatement {
    /// Attributes.
    pub attributes: Vec<Attribute>,
    /// The call.
    pub call: Expr,
    /// The whole statement.
    pub span: Span,
}

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A literal, as written.
    Literal(Literal),
    /// A name, possibly with a template list: `x`, `vec3<f32>`.
    Name(NameExpr),
    /// A prefix operator.
    Unary(Box<UnaryExpr>),
    /// An infix operator.
    Binary(Box<BinaryExpr>),
    /// `f(args)` — a call, or a value constructor.
    Call(Box<CallExpr>),
    /// `base[index]`
    Index(Box<IndexExpr>),
    /// `base.member`, including swizzles.
    Member(Box<MemberExpr>),
    /// `(inner)` — kept so re-emission does not have to reinvent
    /// parenthesization from precedence.
    Paren(Box<ParenExpr>),
}

impl Expr {
    /// This expression's span.
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal(literal) => literal.span,
            Expr::Name(name) => name.span,
            Expr::Unary(unary) => unary.span,
            Expr::Binary(binary) => binary.span,
            Expr::Call(call) => call.span,
            Expr::Index(index) => index.span,
            Expr::Member(member) => member.span,
            Expr::Paren(paren) => paren.span,
        }
    }

    /// The name, if this expression is a bare identifier with no template.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Expr::Name(name) if name.template_args.is_empty() => Some(&name.name.node),
            _ => None,
        }
    }
}

impl core::fmt::Display for Expr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Expr::Literal(literal) => write!(f, "{}", literal.text),
            Expr::Name(name) => write!(f, "{name}"),
            Expr::Unary(unary) => write!(f, "{}{}", unary.op.text(), unary.operand),
            Expr::Binary(binary) => {
                write!(f, "{} {} {}", binary.left, binary.op.text(), binary.right)
            }
            Expr::Call(call) => {
                write!(f, "{}(", call.callee)?;
                for (index, arg) in call.args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Expr::Index(index) => write!(f, "{}[{}]", index.base, index.index),
            Expr::Member(member) => write!(f, "{}.{}", member.base, member.member.node),
            Expr::Paren(paren) => write!(f, "({})", paren.inner),
        }
    }
}

/// A literal, kept as source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Literal {
    /// Which sort.
    pub kind: LiteralKind,
    /// The spelling, exactly as written.
    pub text: String,
    /// Where.
    pub span: Span,
}

/// What sort of literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralKind {
    /// An integer, with or without an `i`/`u` suffix.
    Int,
    /// A float, with or without an `f`/`h` suffix.
    Float,
    /// `true` or `false`.
    Bool,
}

/// A name reference, possibly templated.
#[derive(Clone, Debug, PartialEq)]
pub struct NameExpr {
    /// The name.
    pub name: Spanned<String>,
    /// Template arguments, empty for a plain name.
    pub template_args: Vec<TemplateArg>,
    /// Where.
    pub span: Span,
}

impl core::fmt::Display for NameExpr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.name.node)?;
        if !self.template_args.is_empty() {
            write!(f, "<")?;
            for (index, arg) in self.template_args.iter().enumerate() {
                if index > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{arg}")?;
            }
            write!(f, ">")?;
        }
        Ok(())
    }
}

/// A prefix operator applied to an operand.
#[derive(Clone, Debug, PartialEq)]
pub struct UnaryExpr {
    /// The operator.
    pub op: UnaryOp,
    /// The operand.
    pub operand: Expr,
    /// Where.
    pub span: Span,
}

/// A prefix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`
    Negate,
    /// `!`
    Not,
    /// `~`
    BitNot,
    /// `*` — pointer dereference.
    Deref,
    /// `&` — address-of.
    Ref,
}

impl UnaryOp {
    /// The operator as written.
    pub fn text(&self) -> &'static str {
        match self {
            UnaryOp::Negate => "-",
            UnaryOp::Not => "!",
            UnaryOp::BitNot => "~",
            UnaryOp::Deref => "*",
            UnaryOp::Ref => "&",
        }
    }
}

/// An infix operator applied to two operands.
#[derive(Clone, Debug, PartialEq)]
pub struct BinaryExpr {
    /// The operator.
    pub op: BinaryOp,
    /// Left operand.
    pub left: Expr,
    /// Right operand.
    pub right: Expr,
    /// Where.
    pub span: Span,
}

/// An infix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Subtract,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Modulo,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `&&`
    LogicalAnd,
    /// `||`
    LogicalOr,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
}

impl BinaryOp {
    /// The operator as written.
    pub fn text(&self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Subtract => "-",
            BinaryOp::Multiply => "*",
            BinaryOp::Divide => "/",
            BinaryOp::Modulo => "%",
            BinaryOp::Equal => "==",
            BinaryOp::NotEqual => "!=",
            BinaryOp::Less => "<",
            BinaryOp::LessEqual => "<=",
            BinaryOp::Greater => ">",
            BinaryOp::GreaterEqual => ">=",
            BinaryOp::LogicalAnd => "&&",
            BinaryOp::LogicalOr => "||",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::ShiftLeft => "<<",
            BinaryOp::ShiftRight => ">>",
        }
    }
}

/// `callee(args)`.
#[derive(Clone, Debug, PartialEq)]
pub struct CallExpr {
    /// What is being called: a name, possibly templated.
    pub callee: NameExpr,
    /// The arguments.
    pub args: Vec<Expr>,
    /// Where.
    pub span: Span,
}

/// `base[index]`.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexExpr {
    /// The indexed value.
    pub base: Expr,
    /// The index.
    pub index: Expr,
    /// Where.
    pub span: Span,
}

/// `base.member`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberExpr {
    /// The value whose member is taken.
    pub base: Expr,
    /// The member or swizzle name.
    pub member: Spanned<String>,
    /// Where.
    pub span: Span,
}

/// `(inner)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParenExpr {
    /// The parenthesized expression.
    pub inner: Expr,
    /// Where.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn module_paths_parse_and_round_trip() {
        let path = ModulePath::parse("package::math::remap").expect("valid");
        assert_eq!(path.segments, ["package", "math", "remap"]);
        assert_eq!(path.to_string(), "package::math::remap");
        assert_eq!(path.last(), Some("remap"));
        assert_eq!(
            path.parent().expect("has a parent").to_string(),
            "package::math"
        );
        assert_eq!(
            path.parent().unwrap().join("wrap").to_string(),
            "package::math::wrap"
        );
        assert_eq!(ModulePath::parse("package::"), None);
        assert_eq!(ModulePath::parse("::a"), None);
    }

    #[test]
    fn a_const_is_a_macro_only_when_marked() {
        let plain = GlobalValue {
            attributes: Vec::new(),
            address_space: Vec::new(),
            name: Spanned::new("OCTAVES".into(), span()),
            ty: None,
            init: None,
            span: span(),
        };
        assert!(!plain.is_macro());

        let marked = GlobalValue {
            attributes: vec![Attribute {
                name: Spanned::new("macro".into(), span()),
                args: Vec::new(),
                span: span(),
            }],
            ..plain
        };
        assert!(marked.is_macro());
    }

    #[test]
    fn module_lists_only_its_macro_constants() {
        let value = |name: &str, is_macro: bool| GlobalValue {
            attributes: if is_macro {
                vec![Attribute {
                    name: Spanned::new("macro".into(), span()),
                    args: Vec::new(),
                    span: span(),
                }]
            } else {
                Vec::new()
            },
            address_space: Vec::new(),
            name: Spanned::new(name.into(), span()),
            ty: None,
            init: None,
            span: span(),
        };
        let module = Module {
            declarations: vec![
                Declaration::Const(value("OCTAVES", true)),
                Declaration::Const(value("PI", false)),
                // An `override` is a pipeline constant, not a macro.
                Declaration::Override(value("EXPOSURE", true)),
            ],
            ..Module::default()
        };
        let names: Vec<&str> = module.macros().map(|m| m.name.node.as_str()).collect();
        assert_eq!(names, ["OCTAVES"]);
    }

    #[test]
    fn generic_constraints_gate_instantiation() {
        let param = GenericParam {
            name: Spanned::new("T".into(), span()),
            constraints: vec![
                TypeExpr::named("f32", span()),
                TypeExpr::named("vec2f", span()),
            ],
            span: span(),
        };
        assert!(param.accepts(&TypeExpr::named("f32", span())));
        assert!(!param.accepts(&TypeExpr::named("vec4f", span())));

        let open = GenericParam {
            constraints: Vec::new(),
            ..param
        };
        assert!(open.accepts(&TypeExpr::named("mat3x3f", span())));
    }

    #[test]
    fn types_display_with_their_template_lists() {
        let ty = TypeExpr {
            name: Spanned::new("array".into(), span()),
            template_args: vec![
                TemplateArg {
                    expr: Expr::Name(NameExpr {
                        name: Spanned::new("Light".into(), span()),
                        template_args: Vec::new(),
                        span: span(),
                    }),
                },
                TemplateArg {
                    expr: Expr::Literal(Literal {
                        kind: LiteralKind::Int,
                        text: "4".into(),
                        span: span(),
                    }),
                },
            ],
            span: span(),
        };
        assert_eq!(ty.to_string(), "array<Light, 4>");
        assert!(!ty.is_named("array"), "a templated type is not a bare name");
        assert!(TypeExpr::named("f32", span()).is_named("f32"));
    }

    #[test]
    fn an_if_attribute_exposes_its_condition() {
        let condition = Expr::Name(NameExpr {
            name: Spanned::new("RIDGED".into(), span()),
            template_args: Vec::new(),
            span: span(),
        });
        let conditional = Attribute {
            name: Spanned::new("if".into(), span()),
            args: vec![condition],
            span: span(),
        };
        assert_eq!(
            conditional.condition().and_then(Expr::as_name),
            Some("RIDGED")
        );

        let other = Attribute {
            name: Spanned::new("fragment".into(), span()),
            args: Vec::new(),
            span: span(),
        };
        assert!(other.condition().is_none());
    }

    #[test]
    fn stage_attributes_round_trip() {
        for stage in [Stage::Vertex, Stage::Fragment, Stage::Compute] {
            assert_eq!(Stage::from_attribute(stage.attribute()), Some(stage));
        }
        assert_eq!(Stage::from_attribute("group"), None);
    }

    #[test]
    fn expressions_display_close_to_their_source() {
        let name = |text: &str| {
            Expr::Name(NameExpr {
                name: Spanned::new(text.into(), span()),
                template_args: Vec::new(),
                span: span(),
            })
        };
        let expr = Expr::Binary(Box::new(BinaryExpr {
            op: BinaryOp::Subtract,
            left: name("b"),
            right: name("a"),
            span: span(),
        }));
        assert_eq!(expr.to_string(), "b - a");

        let call = Expr::Call(Box::new(CallExpr {
            callee: NameExpr {
                name: Spanned::new("select".into(), span()),
                template_args: Vec::new(),
                span: span(),
            },
            args: vec![name("x"), name("y"), expr],
            span: span(),
        }));
        assert_eq!(call.to_string(), "select(x, y, b - a)");
    }
}
