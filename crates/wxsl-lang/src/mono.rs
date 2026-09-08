//! Template monomorphization: one generic declaration in, one concrete
//! declaration per instantiation out.
//!
//! WGSL has no generics, so `fn f<T: f32 | vec2f>(a: T) -> T` cannot reach
//! the backend. This pass replaces each template with one copy per distinct
//! set of type arguments, rewrites every reference to name the copy, and
//! deletes the template.
//!
//! ```wxsl
//! fn inverse_lerp<T: f32 | vec3f>(low: T, high: T, value: T) -> T {
//!     return (value - low) / (high - low);
//! }
//!
//! fn use_it(a: vec3f) -> vec3f {
//!     return inverse_lerp(vec3f(0.0), vec3f(1.0), a);
//! }
//! ```
//!
//! becomes `fn inverse_lerp_vec3f(low: vec3f, …)` plus a call to it, and no
//! `f32` copy at all — an instantiation exists only if something asks for it.
//!
//! # Where the type arguments come from
//!
//! Explicitly, `inverse_lerp<vec3f>(a, b, t)`, which always works and is
//! what the node graph emits: a graph knows every socket's type exactly.
//!
//! Otherwise by inference from the arguments, which is deliberately shallow.
//! A type parameter is bound from an argument when the parameter's declared
//! type is *exactly* that type parameter and the argument's own type is
//! evident from syntax alone — a name with a declared type in scope, a
//! literal with a type suffix, a constructor call, or an operator applied to
//! two operands of the same evident type. There is no type checker here, and
//! deliberately no guessing: an argument whose type is not evident
//! contributes nothing, and if that leaves a parameter unbound the call is
//! an error naming the parameter and asking for the types to be written. A
//! wrong instantiation is far worse than a diagnostic, because it fails
//! inside a body the author did not write.
//!
//! # `components(T)`
//!
//! The one builtin this pass provides: the number of scalar components in a
//! type — 1 for a scalar, `N` for `vecN`, `C * R` for `matCxR`. It folds to
//! an integer literal, so it works as an array size or a loop bound. It
//! folds in concrete code too, and not at all if the module declares
//! something of its own called `components`.
//!
//! It is *not* a size in bytes, which is why it is not called `sizeof`: the
//! useful question inside a template over `f32 | vec2f | vec3f` is how many
//! lanes there are, not how much memory they occupy.
//!
//! `components(T)` cannot be used in an `@if`, because conditional
//! translation runs per module before this pass and has no `T` to look at.
//! [`crate::cond`] says so when it happens.
//!
//! # Ordering
//!
//! This runs on the *flattened* module, after import resolution. A template
//! and its call sites can be in different files, so nothing before
//! flattening sees them all; and by running after resolution every name is
//! already final, so an instance name is a plain string with no mangling
//! left to do. Dead-code elimination runs after, which is what stops an
//! unused instantiation from reaching the output.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::span::{Span, Spanned};

/// The builtin this pass folds.
const COMPONENTS: &str = "components";

/// A ceiling on instantiations, so a template that instantiates itself at a
/// fresh type each round fails with a diagnostic instead of running forever.
const INSTANTIATION_LIMIT: usize = 512;

/// How many `alias` hops to follow before giving up.
const ALIAS_DEPTH_LIMIT: usize = 16;

/// Which module each declaration in the flattened module came from.
///
/// The flat module holds declarations from many files, so a span in it only
/// means something together with the file it was measured in. Resolution
/// knows that mapping and passes it here so a diagnostic can point at the
/// right source. An empty map is fine — diagnostics then carry no module and
/// render against whatever source the caller offers for `""`.
pub type Origins = BTreeMap<String, String>;

/// Instantiate every template in `module` and delete the templates.
///
/// Reports every problem it finds rather than the first, so one compile
/// tells the author about all of their call sites.
pub fn apply(module: &mut Module, origins: &Origins) -> Result<(), Diagnostics> {
    let mut pass = Mono::new(module, origins);
    pass.run(module);
    if pass.diagnostics.has_errors() {
        Err(pass.diagnostics)
    } else {
        Ok(())
    }
}

/// One instantiation waiting to be produced.
struct Pending {
    /// The template's name.
    template: String,
    /// The instance's name.
    name: String,
    /// The type arguments, normalized through aliases.
    args: Vec<TypeExpr>,
}

struct Mono<'a> {
    /// Generic declarations, by name.
    templates: BTreeMap<String, Declaration>,
    /// `alias` targets, for normalizing a type argument to its underlying
    /// type — so `f<MyFloat>` and `f<f32>` are one instantiation, and a
    /// constraint written `f32` accepts an alias for it.
    aliases: HashMap<String, TypeExpr>,
    /// Struct names, which are also constructor names.
    structs: HashSet<String>,
    /// Every name the module declares, so an instance name cannot collide.
    declared: HashSet<String>,
    /// Whether `components(T)` is this pass's builtin here, or the module's
    /// own declaration.
    fold_components: bool,
    /// Return types of concrete functions and of instances, for inference.
    returns: HashMap<String, TypeExpr>,
    /// Module-scope value types, for inference.
    globals: HashMap<String, TypeExpr>,
    /// Finished instances, by instance name.
    instances: BTreeMap<String, Declaration>,
    /// Instance names per template, in the order they were requested, so the
    /// output is deterministic.
    produced: BTreeMap<String, Vec<String>>,
    /// Instance name to the template that claimed it.
    claimed: HashMap<String, String>,
    queue: VecDeque<Pending>,
    /// The type parameter bindings of the instance being walked.
    subst: HashMap<String, TypeExpr>,
    /// Types of parameters and locals in scope, innermost last.
    scopes: Vec<HashMap<String, TypeExpr>>,
    origins: &'a Origins,
    /// The module the declaration being walked came from.
    current: Option<String>,
    /// Names already reported, so one bad name is one diagnostic.
    reported: HashSet<String>,
    diagnostics: Diagnostics,
}

impl<'a> Mono<'a> {
    fn new(module: &Module, origins: &'a Origins) -> Self {
        let mut templates = BTreeMap::new();
        let mut aliases = HashMap::new();
        let mut structs = HashSet::new();
        let mut declared = HashSet::new();
        let mut returns = HashMap::new();
        let mut globals = HashMap::new();

        for declaration in &module.declarations {
            if let Some(name) = declaration.name() {
                declared.insert(name.to_string());
                if !declaration.generics().is_empty() {
                    let mut template = declaration.clone();
                    canonicalize_constraints(&mut template);
                    templates.insert(name.to_string(), template);
                }
            }
            match declaration {
                Declaration::Alias(alias) => {
                    aliases.insert(alias.name.node.clone(), alias.ty.clone());
                }
                Declaration::Struct(item) => {
                    structs.insert(item.name.node.clone());
                }
                Declaration::Const(value)
                | Declaration::Override(value)
                | Declaration::Var(value) => {
                    if let Some(ty) = &value.ty {
                        globals.insert(value.name.node.clone(), ty.clone());
                    }
                }
                Declaration::Function(function) => {
                    if !function.is_generic() {
                        if let Some(ty) = &function.return_type {
                            returns.insert(function.name.node.clone(), ty.clone());
                        }
                    }
                }
                Declaration::ConstAssert(_) => {}
            }
        }

        let fold_components = !declared.contains(COMPONENTS);
        Mono {
            templates,
            aliases,
            structs,
            declared,
            fold_components,
            returns,
            globals,
            instances: BTreeMap::new(),
            produced: BTreeMap::new(),
            claimed: HashMap::new(),
            queue: VecDeque::new(),
            subst: HashMap::new(),
            scopes: Vec::new(),
            origins,
            current: None,
            reported: HashSet::new(),
            diagnostics: Diagnostics::new(),
        }
    }

    fn run(&mut self, module: &mut Module) {
        // Walk the concrete declarations, collecting instantiation requests
        // and rewriting the references that produced them. Templates stay in
        // place for now, as markers of where their instances go.
        let mut declarations = std::mem::take(&mut module.declarations);
        let mut is_template = Vec::with_capacity(declarations.len());
        for declaration in &mut declarations {
            let template = !declaration.generics().is_empty();
            is_template.push(template);
            if !template {
                self.current = declaration
                    .name()
                    .and_then(|name| self.origins.get(name))
                    .cloned();
                self.declaration(declaration);
            }
        }

        // Drain the worklist: an instantiated body may instantiate another
        // template, or the same one at a different type.
        while let Some(pending) = self.queue.pop_front() {
            self.instantiate(pending);
        }
        self.current = None;

        // Each template is replaced, in place, by its instantiations.
        // Module-scope declarations in WGSL may appear in any order, so
        // keeping the template's position is a readability choice rather
        // than a correctness one: it keeps an instance beside the concrete
        // code it was written beside.
        let mut out = Vec::with_capacity(declarations.len());
        for (declaration, template) in declarations.into_iter().zip(is_template) {
            if !template {
                out.push(declaration);
                continue;
            }
            let Some(name) = declaration.name() else {
                continue;
            };
            for instance in self.produced.get(name).cloned().unwrap_or_default() {
                if let Some(instance) = self.instances.remove(&instance) {
                    out.push(instance);
                }
            }
        }
        module.declarations = out;
    }

    fn instantiate(&mut self, pending: Pending) {
        let Some(template) = self.templates.get(&pending.template) else {
            return;
        };
        let mut instance = template.clone();
        self.subst = instance
            .generics()
            .iter()
            .map(|parameter| parameter.name.node.clone())
            .zip(pending.args.iter().cloned())
            .collect();
        set_name(&mut instance, &pending.name);
        clear_generics(&mut instance);

        // Spans inside the instance are the template's, so diagnostics from
        // its body belong to the file the template was written in.
        self.current = self.origins.get(&pending.template).cloned();
        self.declaration(&mut instance);
        self.subst.clear();

        self.instances.insert(pending.name.clone(), instance);
        self.produced
            .entry(pending.template)
            .or_default()
            .push(pending.name);
    }

    // --- requests ---------------------------------------------------------

    /// Register an instantiation of `base` and return the instance's name.
    fn request(&mut self, base: &str, args: Vec<TypeExpr>, span: Span) -> Option<String> {
        let template = self.templates.get(base)?.clone();
        let parameters = template.generics();

        if args.len() != parameters.len() {
            let expected = parameters.len();
            let found = args.len();
            self.report(Diagnostic::error(
                format!(
                    "`{base}` takes {expected} type argument{}, but {found} {} given",
                    if expected == 1 { "" } else { "s" },
                    if found == 1 { "was" } else { "were" }
                ),
                span,
            ));
            return None;
        }

        for (parameter, argument) in parameters.iter().zip(&args) {
            if parameter.accepts(argument) {
                continue;
            }
            let allowed = parameter
                .constraints
                .iter()
                .map(TypeExpr::to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            self.report(
                Diagnostic::error(
                    format!(
                        "`{base}` cannot be instantiated with `{argument}` for `{}`",
                        parameter.name.node
                    ),
                    span,
                )
                .with_note(format!(
                    "`{}` must be one of: {allowed}",
                    parameter.name.node
                )),
            );
            return None;
        }

        let name = instance_name(base, &args);
        if let Some(owner) = self.claimed.get(&name) {
            if owner == base {
                return Some(name);
            }
            let owner = owner.clone();
            if self.reported.insert(name.clone()) {
                self.report(
                    Diagnostic::error(
                        format!("instantiating `{base}` and `{owner}` both need the name `{name}`"),
                        span,
                    )
                    .with_note("rename one of the templates"),
                );
            }
            return None;
        }
        if self.declared.contains(&name) {
            if self.reported.insert(name.clone()) {
                self.report(
                    Diagnostic::error(
                        format!(
                            "instantiating `{base}` needs the name `{name}`, \
                             which is already declared"
                        ),
                        span,
                    )
                    .with_note("rename the existing declaration, or the template"),
                );
            }
            return None;
        }
        if self.claimed.len() >= INSTANTIATION_LIMIT {
            if self.reported.insert(String::new()) {
                self.report(
                    Diagnostic::error(
                        format!("more than {INSTANTIATION_LIMIT} template instantiations"),
                        span,
                    )
                    .with_note("a template that instantiates itself at a new type never finishes"),
                );
            }
            return None;
        }

        // An instance's return type makes the next call inferable, so record
        // it now rather than when the body is produced: a call site that
        // uses it may be walked first.
        if let Declaration::Function(function) = &template {
            if let Some(ty) = &function.return_type {
                let bound: HashMap<String, TypeExpr> = parameters
                    .iter()
                    .map(|parameter| parameter.name.node.clone())
                    .zip(args.iter().cloned())
                    .collect();
                self.returns
                    .insert(name.clone(), substitute_type(ty, &bound));
            }
        }

        self.claimed.insert(name.clone(), base.to_string());
        self.queue.push_back(Pending {
            template: base.to_string(),
            name: name.clone(),
            args,
        });
        Some(name)
    }

    /// The types in a template argument list, or `None` after reporting why
    /// there are none to be had.
    fn type_arguments(
        &mut self,
        base: &str,
        args: &[TemplateArg],
        span: Span,
    ) -> Option<Vec<TypeExpr>> {
        if args.is_empty() {
            self.report(
                Diagnostic::error(
                    format!("`{base}` is a template and needs type arguments"),
                    span,
                )
                .with_note(format!(
                    "write `{base}<f32>`, naming a type for each parameter"
                )),
            );
            return None;
        }
        let mut out = Vec::with_capacity(args.len());
        for arg in args {
            match type_from_expr(&arg.expr) {
                Some(ty) => out.push(self.concrete(&ty)),
                None => {
                    self.report(Diagnostic::error(
                        format!("`{}` is not a type, so `{base}` cannot use it", arg.expr),
                        span,
                    ));
                    return None;
                }
            }
        }
        Some(out)
    }

    /// Resolve a call to a template, inferring its types if none are written.
    fn resolve_call(&mut self, call: &mut CallExpr) {
        let base = call.callee.name.node.clone();
        let args = if call.callee.template_args.is_empty() {
            self.infer(&base, call)
        } else {
            self.type_arguments(&base, &call.callee.template_args, call.span)
        };
        let Some(args) = args else {
            return;
        };
        if let Some(instance) = self.request(&base, args, call.span) {
            call.callee.name.node = instance;
            call.callee.template_args.clear();
        }
    }

    /// Bind each type parameter from an argument whose type is evident.
    fn infer(&mut self, base: &str, call: &CallExpr) -> Option<Vec<TypeExpr>> {
        let Some(Declaration::Function(function)) = self.templates.get(base).cloned() else {
            self.report(
                Diagnostic::error(
                    format!("`{base}` is a template and needs type arguments"),
                    call.span,
                )
                .with_note("only a templated function infers its types from its arguments"),
            );
            return None;
        };

        let mut bound: HashMap<String, TypeExpr> = HashMap::new();
        for (parameter, argument) in function.params.iter().zip(&call.args) {
            if !parameter.ty.template_args.is_empty() {
                continue;
            }
            let variable = parameter.ty.name.node.clone();
            if !function
                .generics
                .iter()
                .any(|generic| generic.name.node == variable)
            {
                continue;
            }
            let Some(found) = self.evident_type(argument) else {
                continue;
            };
            let found = self.concrete(&found);
            match bound.get(&variable) {
                Some(existing) if !existing.same_type(&found) => {
                    let existing = existing.to_string();
                    self.report(
                        Diagnostic::error(
                            format!(
                                "`{variable}` would have to be both `{existing}` and \
                                 `{found}` in this call to `{base}`"
                            ),
                            argument.span(),
                        )
                        .with_note(format!(
                            "every argument declared `{variable}` must have the same type"
                        )),
                    );
                    return None;
                }
                Some(_) => {}
                None => {
                    bound.insert(variable, found);
                }
            }
        }

        let missing: Vec<&str> = function
            .generics
            .iter()
            .filter(|generic| !bound.contains_key(&generic.name.node))
            .map(|generic| generic.name.node.as_str())
            .collect();
        if !missing.is_empty() {
            let names = missing.join("`, `");
            let example = function
                .generics
                .iter()
                .map(|generic| {
                    generic
                        .constraints
                        .first()
                        .map_or_else(|| "f32".to_string(), TypeExpr::to_string)
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.report(
                Diagnostic::error(
                    format!("cannot tell what `{names}` is in this call to `{base}`"),
                    call.span,
                )
                .with_note(format!("write the types: `{base}<{example}>(…)`"))
                .with_note(
                    "a type is read off an argument only when it is a name with a declared \
                     type, a literal with a suffix, or a constructor call",
                ),
            );
            return None;
        }

        function
            .generics
            .iter()
            .map(|generic| bound.get(&generic.name.node).cloned())
            .collect()
    }

    // --- types ------------------------------------------------------------

    /// A type argument with the current type parameters substituted and any
    /// aliases followed, which is the form instantiations are keyed by.
    fn concrete(&self, ty: &TypeExpr) -> TypeExpr {
        let mut out = ty.clone();
        if out.template_args.is_empty() {
            if let Some(bound) = self.subst.get(&out.name.node) {
                let span = out.span;
                out = bound.clone();
                out.span = span;
            }
        }
        for _ in 0..ALIAS_DEPTH_LIMIT {
            if !out.template_args.is_empty() {
                break;
            }
            let Some(target) = self.aliases.get(&out.name.node) else {
                break;
            };
            let span = out.span;
            out = target.clone();
            out.span = span;
        }
        canonical_type(&out)
    }

    /// The type of `expr`, when syntax alone settles it.
    fn evident_type(&self, expr: &Expr) -> Option<TypeExpr> {
        match expr {
            Expr::Paren(paren) => self.evident_type(&paren.inner),
            Expr::Literal(literal) => literal_type(literal),
            Expr::Name(name) if name.template_args.is_empty() => self.lookup(&name.name.node),
            Expr::Unary(unary) => match unary.op {
                // Negation and complement keep the operand's type; a pointer
                // changes it, and there is no type for the pointee here.
                UnaryOp::Negate | UnaryOp::Not | UnaryOp::BitNot => {
                    self.evident_type(&unary.operand)
                }
                UnaryOp::Deref | UnaryOp::Ref => None,
            },
            Expr::Binary(binary) => {
                if !keeps_operand_type(binary.op) {
                    return None;
                }
                let left = self.evident_type(&binary.left)?;
                let right = self.evident_type(&binary.right)?;
                // Only when both sides agree: `mat3x3f * vec3f` and
                // `f32 * vec3f` are both legal, and neither result is the
                // left operand's type.
                left.same_type(&right).then_some(left)
            }
            Expr::Call(call) if call.callee.template_args.is_empty() => {
                let name = &call.callee.name.node;
                if self.structs.contains(name) || component_count(name).is_some() {
                    Some(TypeExpr::named(name.clone(), call.span))
                } else {
                    self.returns.get(name).cloned()
                }
            }
            _ => None,
        }
    }

    fn lookup(&self, name: &str) -> Option<TypeExpr> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .or_else(|| self.globals.get(name))
            .cloned()
    }

    fn bind(&mut self, name: String, ty: TypeExpr) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    // --- the builtin ------------------------------------------------------

    fn is_components(&self, call: &CallExpr) -> bool {
        self.fold_components
            && call.callee.name.node == COMPONENTS
            && call.callee.template_args.is_empty()
    }

    /// `components(T)` as an integer literal.
    fn components_of(&mut self, call: &CallExpr) -> Option<Expr> {
        if call.args.len() != 1 {
            let found = call.args.len();
            self.report(Diagnostic::error(
                format!("`{COMPONENTS}` takes one type, but {found} arguments were given"),
                call.span,
            ));
            return None;
        }
        let Some(ty) = type_from_expr(&call.args[0]) else {
            self.report(Diagnostic::error(
                format!(
                    "`{COMPONENTS}` takes a type, and `{}` is not one",
                    call.args[0]
                ),
                call.span,
            ));
            return None;
        };
        let ty = self.concrete(&ty);
        let Some(count) = component_count(&ty.name.node) else {
            self.report(
                Diagnostic::error(
                    format!("`{COMPONENTS}` does not know how many components `{ty}` has"),
                    call.span,
                )
                .with_note("it is defined for scalars, `vecN` and `matCxR`"),
            );
            return None;
        };
        Some(Expr::Literal(Literal {
            kind: LiteralKind::Int,
            text: count.to_string(),
            span: call.span,
        }))
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        let diagnostic = match &self.current {
            Some(module) => diagnostic.or_module(module),
            None => diagnostic,
        };
        self.diagnostics.push(diagnostic);
    }

    // --- the walk ---------------------------------------------------------

    fn declaration(&mut self, declaration: &mut Declaration) {
        match declaration {
            Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
                if let Some(ty) = &mut value.ty {
                    self.type_expr(ty);
                }
                for arg in &mut value.address_space {
                    self.expr(&mut arg.expr);
                }
                if let Some(init) = &mut value.init {
                    self.expr(init);
                }
            }
            Declaration::Alias(alias) => self.type_expr(&mut alias.ty),
            Declaration::Struct(item) => {
                for member in &mut item.members {
                    self.type_expr(&mut member.ty);
                }
            }
            Declaration::Function(function) => {
                self.scopes.push(HashMap::new());
                for param in &mut function.params {
                    self.type_expr(&mut param.ty);
                }
                // After the types are substituted, so a parameter declared
                // `T` is in scope as the type `T` was bound to.
                let params: Vec<(String, TypeExpr)> = function
                    .params
                    .iter()
                    .map(|param| (param.name.node.clone(), param.ty.clone()))
                    .collect();
                for (name, ty) in params {
                    self.bind(name, ty);
                }
                if let Some(ty) = &mut function.return_type {
                    self.type_expr(ty);
                }
                self.block(&mut function.body);
                self.scopes.pop();
            }
            Declaration::ConstAssert(assert) => self.expr(&mut assert.expr),
        }
    }

    fn block(&mut self, block: &mut Block) {
        self.scopes.push(HashMap::new());
        for statement in &mut block.statements {
            self.statement(statement);
        }
        self.scopes.pop();
    }

    fn statement(&mut self, statement: &mut Statement) {
        match statement {
            Statement::Local(local) => {
                if let Some(ty) = &mut local.ty {
                    self.type_expr(ty);
                }
                for arg in &mut local.address_space {
                    self.expr(&mut arg.expr);
                }
                if let Some(init) = &mut local.init {
                    self.expr(init);
                }
                // A declared type wins; otherwise take whatever the
                // initializer makes evident, which is what makes
                // `let x = vec3f(…); f(x)` inferable.
                let ty = match (&local.ty, &local.init) {
                    (Some(ty), _) => Some(ty.clone()),
                    (None, Some(init)) => self.evident_type(init),
                    (None, None) => None,
                };
                if let Some(ty) = ty {
                    let name = local.name.node.clone();
                    self.bind(name, ty);
                }
            }
            Statement::Assign(assign) => {
                if let Some(target) = &mut assign.target {
                    self.expr(target);
                }
                self.expr(&mut assign.value);
            }
            Statement::Step(step) => self.expr(&mut step.target),
            Statement::Block(block) => self.block(block),
            Statement::If(item) => {
                for (condition, body) in &mut item.arms {
                    self.expr(condition);
                    self.block(body);
                }
                if let Some(body) = &mut item.otherwise {
                    self.block(body);
                }
            }
            Statement::Switch(item) => {
                self.expr(&mut item.selector);
                for clause in &mut item.clauses {
                    for selector in &mut clause.selectors {
                        if let CaseSelector::Value(expr) = selector {
                            self.expr(expr);
                        }
                    }
                    self.block(&mut clause.body);
                }
            }
            Statement::Loop(item) => {
                self.block(&mut item.body);
                if let Some(continuing) = &mut item.continuing {
                    self.block(&mut continuing.body);
                    if let Some(condition) = &mut continuing.break_if {
                        self.expr(condition);
                    }
                }
            }
            Statement::For(item) => {
                // The init's binding is visible in the condition, the update
                // and the body, so they share one scope.
                self.scopes.push(HashMap::new());
                if let Some(init) = &mut item.init {
                    self.statement(init);
                }
                if let Some(condition) = &mut item.condition {
                    self.expr(condition);
                }
                if let Some(update) = &mut item.update {
                    self.statement(update);
                }
                self.block(&mut item.body);
                self.scopes.pop();
            }
            Statement::While(item) => {
                self.expr(&mut item.condition);
                self.block(&mut item.body);
            }
            Statement::Return(item) => {
                if let Some(value) = &mut item.value {
                    self.expr(value);
                }
            }
            Statement::Call(item) => self.expr(&mut item.call),
            Statement::ConstAssert(item) => self.expr(&mut item.expr),
            Statement::Break(_) | Statement::Continue(_) | Statement::Discard(_) => {}
        }
    }

    fn type_expr(&mut self, ty: &mut TypeExpr) {
        if ty.template_args.is_empty() {
            if let Some(bound) = self.subst.get(&ty.name.node).cloned() {
                ty.name.node = bound.name.node;
                ty.template_args = bound.template_args;
            }
        }
        for arg in &mut ty.template_args {
            self.expr(&mut arg.expr);
        }
        if !self.templates.contains_key(&ty.name.node) {
            return;
        }
        let base = ty.name.node.clone();
        let Some(args) = self.type_arguments(&base, &ty.template_args, ty.span) else {
            return;
        };
        if let Some(instance) = self.request(&base, args, ty.span) {
            ty.name.node = instance;
            ty.template_args.clear();
        }
    }

    fn expr(&mut self, expr: &mut Expr) {
        // `components(T)` is folded before anything descends into it: its
        // argument is a type, not a value, and `T` is not in scope as one.
        let folded = match &*expr {
            Expr::Call(call) if self.is_components(call) => Some(self.components_of(call)),
            _ => None,
        };
        if let Some(result) = folded {
            if let Some(literal) = result {
                *expr = literal;
            }
            return;
        }

        match expr {
            Expr::Literal(_) => {}
            Expr::Name(name) => {
                self.substitute_name(name);
                for arg in &mut name.template_args {
                    self.expr(&mut arg.expr);
                }
                if self.templates.contains_key(&name.name.node) {
                    let base = name.name.node.clone();
                    if let Some(args) = self.type_arguments(&base, &name.template_args, name.span) {
                        if let Some(instance) = self.request(&base, args, name.span) {
                            name.name.node = instance;
                            name.template_args.clear();
                        }
                    }
                }
            }
            Expr::Unary(unary) => self.expr(&mut unary.operand),
            Expr::Binary(binary) => {
                self.expr(&mut binary.left);
                self.expr(&mut binary.right);
            }
            Expr::Call(call) => {
                // Arguments first: inference reads their types, and a nested
                // call to a template has to be resolved before its return
                // type is known.
                for arg in &mut call.args {
                    self.expr(arg);
                }
                self.substitute_name(&mut call.callee);
                for arg in &mut call.callee.template_args {
                    self.expr(&mut arg.expr);
                }
                if self.templates.contains_key(&call.callee.name.node) {
                    self.resolve_call(call);
                }
            }
            Expr::Index(index) => {
                self.expr(&mut index.base);
                self.expr(&mut index.index);
            }
            // The member name is a field, not a type or a declaration.
            Expr::Member(member) => self.expr(&mut member.base),
            Expr::Paren(paren) => self.expr(&mut paren.inner),
        }
    }

    /// Substitute a type parameter used in expression position, which is how
    /// `T(0.0)` becomes `vec3f(0.0)`.
    fn substitute_name(&self, name: &mut NameExpr) {
        if !name.template_args.is_empty() {
            return;
        }
        if let Some(bound) = self.subst.get(&name.name.node).cloned() {
            name.name.node = bound.name.node;
            name.template_args = bound.template_args;
        }
    }
}

/// The name an instantiation takes: the template's name with each type
/// argument appended.
///
/// `inverse_lerp` at `vec3f` becomes `inverse_lerp_vec3f`, and at
/// `array<f32, 4>` becomes `inverse_lerp_array_f32_4`. Readable on purpose:
/// this name is what appears in a shader diagnostic and in a captured GPU
/// frame.
fn instance_name(base: &str, args: &[TypeExpr]) -> String {
    let mut out = base.to_string();
    for arg in args {
        out.push('_');
        out.push_str(&identifier(&arg.to_string()));
    }
    out
}

/// A type's rendering reduced to something WGSL accepts as an identifier.
fn identifier(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending && !out.is_empty() {
                out.push('_');
            }
            pending = false;
            out.push(ch);
        } else {
            pending = true;
        }
    }
    out
}

/// The short spelling of a predeclared type alias: `vec3<f32>` becomes
/// `vec3f`, `mat3x3<f16>` becomes `mat3x3h`.
///
/// WGSL treats the two as one type, but [`TypeExpr::same_type`] is
/// structural, so without this a template constrained to `vec3f` would
/// refuse an argument written `vec3<f32>` — and the two spellings would
/// otherwise produce two identical instantiations under different names.
/// The short form wins because it is what the node graph and the shader
/// library both write.
fn canonical_type(ty: &TypeExpr) -> TypeExpr {
    if ty.template_args.len() != 1 {
        return ty.clone();
    }
    let Some(scalar) = ty.template_args[0].expr.as_name() else {
        return ty.clone();
    };
    let suffix = match scalar {
        "f32" => "f",
        "f16" => "h",
        "i32" => "i",
        "u32" => "u",
        _ => return ty.clone(),
    };
    let name = ty.name.node.as_str();
    let vector = name
        .strip_prefix("vec")
        .is_some_and(|size| matches!(size, "2" | "3" | "4"));
    // Matrices only have short forms for the float component types: there
    // is no `mat3x3i`.
    let matrix = matches!(suffix, "f" | "h")
        && matches!(
            name,
            "mat2x2"
                | "mat2x3"
                | "mat2x4"
                | "mat3x2"
                | "mat3x3"
                | "mat3x4"
                | "mat4x2"
                | "mat4x3"
                | "mat4x4"
        );
    if !vector && !matrix {
        return ty.clone();
    }
    TypeExpr {
        name: Spanned::new(format!("{name}{suffix}"), ty.name.span),
        template_args: Vec::new(),
        span: ty.span,
    }
}

/// Rewrite a template constraint list into canonical spellings, so the
/// comparison against a canonicalized argument is like for like.
fn canonicalize_constraints(declaration: &mut Declaration) {
    let generics = match declaration {
        Declaration::Function(function) => &mut function.generics,
        Declaration::Struct(item) => &mut item.generics,
        _ => return,
    };
    for parameter in generics {
        for constraint in &mut parameter.constraints {
            *constraint = canonical_type(constraint);
        }
    }
}

/// A template argument read as a type, or `None` if it is a value.
fn type_from_expr(expr: &Expr) -> Option<TypeExpr> {
    match expr {
        Expr::Name(name) => Some(TypeExpr {
            name: name.name.clone(),
            template_args: name.template_args.clone(),
            span: name.span,
        }),
        Expr::Paren(paren) => type_from_expr(&paren.inner),
        _ => None,
    }
}

/// Replace type parameters in `ty` with what they are bound to.
fn substitute_type(ty: &TypeExpr, bound: &HashMap<String, TypeExpr>) -> TypeExpr {
    if ty.template_args.is_empty() {
        return bound
            .get(&ty.name.node)
            .cloned()
            .unwrap_or_else(|| ty.clone());
    }
    let mut out = ty.clone();
    for arg in &mut out.template_args {
        if let Expr::Name(name) = &mut arg.expr {
            if name.template_args.is_empty() {
                if let Some(target) = bound.get(&name.name.node) {
                    name.name.node = target.name.node.clone();
                    name.template_args = target.template_args.clone();
                }
            }
        }
    }
    out
}

/// Whether an operator's result has the same type as its operands.
fn keeps_operand_type(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Modulo
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::ShiftLeft
            | BinaryOp::ShiftRight
    )
}

/// The type a literal's suffix names, or `None` for an abstract literal.
///
/// `1.0f` is an `f32`; `1.0` could be either, and guessing is how an
/// instantiation ends up at the wrong type, so it does not guess.
fn literal_type(literal: &Literal) -> Option<TypeExpr> {
    let name = match literal.kind {
        LiteralKind::Bool => "bool",
        LiteralKind::Int => match literal.text.chars().last()? {
            'i' => "i32",
            'u' => "u32",
            _ => return None,
        },
        LiteralKind::Float => match literal.text.chars().last()? {
            'f' => "f32",
            'h' => "f16",
            _ => return None,
        },
    };
    Some(TypeExpr::named(name, literal.span))
}

/// How many scalar components a type has.
///
/// Reads the name, because that is all a type is here: `vec3<f32>` and
/// `vec3f` both name three components, and `mat3x3f` names nine.
fn component_count(name: &str) -> Option<u32> {
    if matches!(name, "f32" | "f16" | "i32" | "u32" | "bool") {
        return Some(1);
    }
    if let Some(rest) = name.strip_prefix("vec") {
        let mut chars = rest.chars();
        let size = chars.next()?.to_digit(10)?;
        if !(2..=4).contains(&size) {
            return None;
        }
        return scalar_suffix(chars.as_str()).then_some(size);
    }
    if let Some(rest) = name.strip_prefix("mat") {
        let mut chars = rest.chars();
        let columns = chars.next()?.to_digit(10)?;
        if chars.next()? != 'x' {
            return None;
        }
        let rows = chars.next()?.to_digit(10)?;
        if !(2..=4).contains(&columns) || !(2..=4).contains(&rows) {
            return None;
        }
        return scalar_suffix(chars.as_str()).then_some(columns * rows);
    }
    None
}

/// Whether what follows a `vecN`/`matCxR` prefix is a component-type suffix.
fn scalar_suffix(text: &str) -> bool {
    matches!(text, "" | "f" | "h" | "i" | "u")
}

fn set_name(declaration: &mut Declaration, name: &str) {
    match declaration {
        Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
            value.name.node = name.to_string();
        }
        Declaration::Alias(alias) => alias.name.node = name.to_string(),
        Declaration::Struct(item) => item.name.node = name.to_string(),
        Declaration::Function(function) => function.name.node = name.to_string(),
        Declaration::ConstAssert(_) => {}
    }
}

fn clear_generics(declaration: &mut Declaration) {
    match declaration {
        Declaration::Struct(item) => item.generics.clear(),
        Declaration::Function(function) => function.generics.clear(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::emit;
    use crate::parse::parse;

    /// Monomorphize a single module and render the result.
    fn mono(source: &str) -> String {
        let mut module = parse(source).expect("parses");
        if let Err(diagnostics) = apply(&mut module, &Origins::new()) {
            panic!("{}", diagnostics.render(&|_| Some(source.to_string())));
        }
        emit(&module)
    }

    /// The rendered diagnostics of a module that must not monomorphize.
    fn errors(source: &str) -> String {
        let mut module = parse(source).expect("parses");
        let diagnostics = apply(&mut module, &Origins::new()).expect_err("should not compile");
        diagnostics.render(&|_| Some(source.to_string()))
    }

    #[test]
    fn an_explicit_instantiation_replaces_the_template() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec3f) -> vec3f { return half<vec3f>(a); }\n",
        );
        assert!(wgsl.contains("fn half_vec3f(v: vec3f) -> vec3f"), "{wgsl}");
        // The type parameter is gone from the body, including the
        // constructor call it was the callee of.
        assert!(wgsl.contains("v * vec3f(0.5)"), "{wgsl}");
        assert!(!wgsl.contains("<T"), "{wgsl}");
        assert!(!wgsl.contains(": T"), "{wgsl}");
        // The template itself does not survive.
        assert!(!wgsl.contains("fn half("), "{wgsl}");
        assert!(wgsl.contains("return half_vec3f(a);"), "{wgsl}");
    }

    #[test]
    fn a_type_is_inferred_from_a_parameter() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec3f) -> vec3f { return half(a); }\n",
        );
        assert!(wgsl.contains("fn half_vec3f(v: vec3f)"), "{wgsl}");
        assert!(wgsl.contains("return half_vec3f(a);"), "{wgsl}");
    }

    #[test]
    fn a_type_is_inferred_from_a_local_and_from_a_constructor() {
        let wgsl = mono(
            "fn half<T: f32 | vec2f | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it() -> vec3f {\n\
             \x20   let a: vec2f = vec2f(1.0);\n\
             \x20   let b = vec3f(2.0);\n\
             \x20   return vec3f(half(a), 0.0) + half(b);\n}\n",
        );
        assert!(wgsl.contains("fn half_vec2f(v: vec2f)"), "{wgsl}");
        assert!(wgsl.contains("fn half_vec3f(v: vec3f)"), "{wgsl}");
        assert!(
            !wgsl.contains("half_f32"),
            "no f32 copy was asked for:\n{wgsl}"
        );
    }

    #[test]
    fn each_type_gets_one_copy_and_a_repeat_gets_none() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec3f, b: f32) -> vec3f {\n\
             \x20   return half(a) + half(a) * half(b);\n}\n",
        );
        assert_eq!(wgsl.matches("fn half_vec3f(").count(), 1, "{wgsl}");
        assert_eq!(wgsl.matches("fn half_f32(").count(), 1, "{wgsl}");
        assert_eq!(wgsl.matches("half_vec3f(a)").count(), 2, "{wgsl}");
    }

    #[test]
    fn an_uninstantiated_template_leaves_nothing_behind() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec3f) -> vec3f { return a; }\n",
        );
        assert!(!wgsl.contains("half"), "{wgsl}");
        assert!(wgsl.contains("fn use_it"), "{wgsl}");
    }

    #[test]
    fn a_template_that_calls_a_template_instantiates_both() {
        let wgsl = mono(
            "fn twice<T: f32 | vec3f>(v: T) -> T { return v + v; }\n\
             fn quad<T: f32 | vec3f>(v: T) -> T { return twice(twice(v)); }\n\
             fn use_it(a: vec3f) -> vec3f { return quad(a); }\n",
        );
        assert!(wgsl.contains("fn twice_vec3f(v: vec3f)"), "{wgsl}");
        assert!(wgsl.contains("fn quad_vec3f(v: vec3f)"), "{wgsl}");
        assert!(
            wgsl.contains("return twice_vec3f(twice_vec3f(v));"),
            "{wgsl}"
        );
        assert!(!wgsl.contains("twice_f32"), "{wgsl}");
    }

    #[test]
    fn a_generic_return_type_feeds_the_next_inference() {
        // `twice(a)` has no declared type anywhere; its type comes from the
        // instantiation that was just registered for it.
        let wgsl = mono(
            "fn twice<T: f32 | vec3f>(v: T) -> T { return v + v; }\n\
             fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec3f) -> vec3f { return half(twice(a)); }\n",
        );
        assert!(
            wgsl.contains("return half_vec3f(twice_vec3f(a));"),
            "{wgsl}"
        );
    }

    #[test]
    fn a_generic_struct_instantiates_from_a_type_position() {
        let wgsl = mono(
            "struct Pair<T> { a: T, b: T }\n\
             fn use_it(p: Pair<f32>) -> f32 { return p.a + p.b; }\n",
        );
        assert!(wgsl.contains("struct Pair_f32"), "{wgsl}");
        assert!(wgsl.contains("a: f32"), "{wgsl}");
        assert!(wgsl.contains("p: Pair_f32"), "{wgsl}");
        assert!(!wgsl.contains("Pair<"), "{wgsl}");
    }

    #[test]
    fn a_generic_struct_instantiates_from_its_constructor() {
        let wgsl = mono(
            "struct Pair<T> { a: T, b: T }\n\
             fn use_it() -> f32 {\n\
             \x20   let p = Pair<vec2f>(vec2f(1.0), vec2f(2.0));\n\
             \x20   return p.a.x;\n}\n",
        );
        assert!(wgsl.contains("struct Pair_vec2f"), "{wgsl}");
        assert!(
            wgsl.contains("Pair_vec2f(vec2f(1.0), vec2f(2.0))"),
            "{wgsl}"
        );
    }

    #[test]
    fn the_two_spellings_of_a_predeclared_type_are_one_instantiation() {
        // `vec3<f32>` and `vec3f` are the same WGSL type. The constraint
        // is written one way and the arguments the other, and it still
        // has to resolve — to a single copy.
        let wgsl = mono(
            "fn first<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn a(v: vec3<f32>) -> vec3<f32> { return first<vec3<f32>>(v); }\n\
             fn b(v: vec3f) -> vec3f { return first(v); }\n",
        );
        assert_eq!(wgsl.matches("fn first_vec3f(").count(), 1, "{wgsl}");
        assert!(wgsl.contains("fn first_vec3f(v: vec3f) -> vec3f"), "{wgsl}");
        assert!(!wgsl.contains("first_vec3_f32"), "{wgsl}");
    }

    #[test]
    fn a_template_argument_may_itself_be_a_type_expression() {
        let wgsl = mono(
            "fn count<T>(v: T) -> i32 { return components(T); }\n\
             fn use_it(a: array<f32, 4>) -> i32 { return count<f32>(a[0]); }\n",
        );
        assert!(wgsl.contains("fn count_f32(v: f32) -> i32"), "{wgsl}");
        assert!(wgsl.contains("return 1;"), "{wgsl}");
    }

    #[test]
    fn an_alias_argument_shares_the_instance_it_stands_for() {
        let wgsl = mono(
            "alias Color = vec3f;\n\
             fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: Color, b: vec3f) -> Color { return half(a) + half<Color>(b); }\n",
        );
        // One instance, named for the underlying type, so the constraint
        // written `vec3f` accepts the alias too.
        assert_eq!(wgsl.matches("fn half_vec3f(").count(), 1, "{wgsl}");
        assert!(!wgsl.contains("half_Color"), "{wgsl}");
    }

    // --- components -------------------------------------------------------

    #[test]
    fn components_folds_per_instantiation() {
        let wgsl = mono(
            "fn total<T: f32 | vec3f>(v: T) -> i32 { return components(T); }\n\
             fn use_it(a: vec3f, b: f32) -> i32 { return total(a) + total(b); }\n",
        );
        assert!(
            wgsl.contains("fn total_vec3f(v: vec3f) -> i32 {\n    return 3;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("fn total_f32(v: f32) -> i32 {\n    return 1;"),
            "{wgsl}"
        );
    }

    #[test]
    fn components_folds_in_concrete_code_too() {
        let wgsl = mono("fn f() -> i32 { return components(mat3x3f) + components(vec2<f32>); }\n");
        assert!(wgsl.contains("return 9 + 2;"), "{wgsl}");
    }

    #[test]
    fn components_counts_every_shape_it_claims_to() {
        for (ty, count) in [
            ("f32", 1),
            ("f16", 1),
            ("i32", 1),
            ("u32", 1),
            ("bool", 1),
            ("vec2f", 2),
            ("vec3", 3),
            ("vec4u", 4),
            ("mat2x2f", 4),
            ("mat4x3", 12),
            ("mat4x4f", 16),
        ] {
            assert_eq!(component_count(ty), Some(count), "{ty}");
        }
        for ty in ["vec1f", "vec5f", "mat1x1", "mat3", "Surface", "array"] {
            assert_eq!(component_count(ty), None, "{ty}");
        }
    }

    #[test]
    fn a_module_that_declares_components_keeps_its_own() {
        let wgsl = mono(
            "fn components(v: vec3f) -> f32 { return v.x; }\n\
             fn use_it(a: vec3f) -> f32 { return components(a); }\n",
        );
        assert!(wgsl.contains("fn components(v: vec3f)"), "{wgsl}");
        assert!(wgsl.contains("return components(a);"), "{wgsl}");
    }

    #[test]
    fn components_of_a_type_it_cannot_measure_is_an_error() {
        let rendered = errors(
            "struct Surface { albedo: vec3f }\n\
             fn f() -> i32 { return components(Surface); }\n",
        );
        assert!(
            rendered.contains("does not know how many components `Surface` has"),
            "{rendered}"
        );
        assert!(rendered.contains("`vecN`"), "{rendered}");
    }

    // --- diagnostics ------------------------------------------------------

    #[test]
    fn a_type_outside_the_constraints_names_what_is_allowed() {
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec2f) -> vec2f { return half(a); }\n",
        );
        assert!(
            rendered.contains("cannot be instantiated with `vec2f` for `T`"),
            "{rendered}"
        );
        assert!(
            rendered.contains("must be one of: f32 | vec3f"),
            "{rendered}"
        );
    }

    #[test]
    fn a_type_that_cannot_be_inferred_asks_for_it_to_be_written() {
        // A swizzle type is not evident here, on purpose: inference that
        // guesses is worse than a diagnostic.
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v * T(0.5); }\n\
             fn use_it(a: vec4f) -> f32 { return half(a.x); }\n",
        );
        assert!(
            rendered.contains("cannot tell what `T` is in this call to `half`"),
            "{rendered}"
        );
        assert!(
            rendered.contains("write the types: `half<f32>("),
            "{rendered}"
        );
    }

    #[test]
    fn one_parameter_bound_to_two_types_is_an_error() {
        let rendered = errors(
            "fn pick<T: f32 | vec3f>(a: T, b: T) -> T { return a + b; }\n\
             fn use_it(x: f32, y: vec3f) -> f32 { return pick(x, y).x; }\n",
        );
        assert!(
            rendered.contains("would have to be both `f32` and `vec3f`"),
            "{rendered}"
        );
    }

    #[test]
    fn the_wrong_number_of_type_arguments_is_an_error() {
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it(a: vec3f) -> vec3f { return half<vec3f, f32>(a); }\n",
        );
        assert!(
            rendered.contains("`half` takes 1 type argument, but 2 were given"),
            "{rendered}"
        );
    }

    #[test]
    fn a_template_type_used_without_arguments_is_an_error() {
        let rendered = errors(
            "struct Pair<T> { a: T, b: T }\n\
             fn use_it(p: Pair) -> f32 { return p.a; }\n",
        );
        assert!(
            rendered.contains("`Pair` is a template and needs type arguments"),
            "{rendered}"
        );
        assert!(rendered.contains("write `Pair<f32>`"), "{rendered}");
    }

    #[test]
    fn a_value_where_a_type_belongs_is_an_error() {
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it(a: vec3f) -> vec3f { return half<4>(a); }\n",
        );
        assert!(rendered.contains("is not a type"), "{rendered}");
    }

    #[test]
    fn an_instance_name_that_is_already_taken_is_reported() {
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn half_vec3f(v: vec3f) -> vec3f { return v; }\n\
             fn use_it(a: vec3f) -> vec3f { return half(a); }\n",
        );
        assert!(
            rendered.contains("needs the name `half_vec3f`, which is already declared"),
            "{rendered}"
        );
    }

    #[test]
    fn a_template_that_grows_its_own_type_stops_at_the_limit() {
        // `f` at `T` asks for `g` at `array<T, 2>`, which asks for `f`
        // again, forever. Without the ceiling this pass would not
        // terminate.
        let rendered = errors(
            "fn f<T>(v: T) -> i32 { return g<array<T, 2>>(); }\n\
             fn g<U>() -> i32 { return f<U>(U()); }\n\
             fn use_it(a: f32) -> i32 { return f(a); }\n",
        );
        assert!(rendered.contains("template instantiations"), "{rendered}");
    }

    #[test]
    fn every_bad_call_is_reported_not_just_the_first() {
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn a(x: vec2f) -> vec2f { return half(x); }\n\
             fn b(y: vec4f) -> vec4f { return half(y); }\n",
        );
        assert_eq!(
            rendered.matches("cannot be instantiated with").count(),
            2,
            "{rendered}"
        );
    }

    // --- inference boundaries --------------------------------------------

    #[test]
    fn a_comparison_is_not_read_as_its_operand_type() {
        // `x > y` is a bool, not an `f32`. Reading the operand type here
        // would instantiate at `f32` and produce a body that does not
        // compile, so inference declines instead.
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it(x: f32, y: f32) -> f32 { return half(x > y); }\n",
        );
        assert!(rendered.contains("cannot tell what `T` is"), "{rendered}");
    }

    #[test]
    fn an_operator_on_two_matching_operands_is_read() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it(x: vec3f, y: vec3f) -> vec3f { return half(x + y); }\n",
        );
        assert!(wgsl.contains("half_vec3f(x + y)"), "{wgsl}");
    }

    #[test]
    fn an_operator_on_mixed_operands_is_not_read() {
        // `m * v` is a `vec3f` and `v * v` is too, but the left operand type
        // is only the answer in the second case. Rather than encode WGSL
        // whole overload table, inference declines.
        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it(m: mat3x3f, v: vec3f) -> vec3f { return half(m * v); }\n",
        );
        assert!(rendered.contains("cannot tell what `T` is"), "{rendered}");
    }

    #[test]
    fn a_suffixed_literal_is_read_and_a_bare_one_is_not() {
        let wgsl = mono(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it() -> f32 { return half(1.0f); }\n",
        );
        assert!(wgsl.contains("fn half_f32(v: f32)"), "{wgsl}");

        let rendered = errors(
            "fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn use_it() -> f32 { return half(1.0); }\n",
        );
        assert!(rendered.contains("cannot tell what `T` is"), "{rendered}");
    }

    #[test]
    fn a_global_is_in_scope_for_inference_and_a_parameter_shadows_it() {
        let wgsl = mono(
            "const level: vec3f = vec3f(1.0);\n\
             fn half<T: f32 | vec3f>(v: T) -> T { return v; }\n\
             fn from_global() -> vec3f { return half(level); }\n\
             fn from_parameter(level: f32) -> f32 { return half(level); }\n",
        );
        assert!(wgsl.contains("fn half_vec3f(v: vec3f)"), "{wgsl}");
        assert!(wgsl.contains("fn half_f32(v: f32)"), "{wgsl}");
    }

    #[test]
    fn a_parameter_declared_with_a_shape_rather_than_the_variable_is_skipped() {
        // `vec2<T>` is not `T`, so it binds nothing and the type has to be
        // written. A documented boundary, not an accident.
        let rendered = errors(
            "fn flatten<T: f32>(v: vec2<T>) -> T { return v.x; }\n\
             fn use_it(a: vec2f) -> f32 { return flatten(a); }\n",
        );
        assert!(rendered.contains("cannot tell what `T` is"), "{rendered}");

        let wgsl = mono(
            "fn flatten<T: f32>(v: vec2<T>) -> T { return v.x; }\n\
             fn use_it(a: vec2f) -> f32 { return flatten<f32>(a); }\n",
        );
        assert!(
            wgsl.contains("fn flatten_f32(v: vec2<f32>) -> f32"),
            "{wgsl}"
        );
    }

    #[test]
    fn an_unconstrained_parameter_accepts_anything() {
        let wgsl = mono(
            "fn identity<T>(v: T) -> T { return v; }\n\
             fn use_it(a: mat3x3f) -> mat3x3f { return identity(a); }\n",
        );
        assert!(wgsl.contains("fn identity_mat3x3f(v: mat3x3f)"), "{wgsl}");
    }

    #[test]
    fn two_parameters_instantiate_independently() {
        let wgsl = mono(
            "fn pick<A, B>(v: A, w: B) -> B { return w; }\n\
             fn use_it(a: f32, b: vec3f) -> vec3f { return pick(a, b); }\n",
        );
        assert!(
            wgsl.contains("fn pick_f32_vec3f(v: f32, w: vec3f) -> vec3f"),
            "{wgsl}"
        );
    }

    #[test]
    fn a_type_parameter_reaches_a_template_argument_position() {
        let wgsl = mono(
            "fn sized<T: f32 | vec3f>() -> i32 {\n\
             \x20   var values: array<T, 2>;\n\
             \x20   return components(T) * 2;\n}\n\
             fn use_it() -> i32 { return sized<vec3f>(); }\n",
        );
        assert!(wgsl.contains("array<vec3f, 2>"), "{wgsl}");
        assert!(wgsl.contains("return 3 * 2;"), "{wgsl}");
    }

    #[test]
    fn a_diagnostic_from_a_template_body_names_the_module_it_was_written_in() {
        let mut origins = Origins::new();
        origins.insert("half".to_string(), "package::math::half".to_string());
        origins.insert("use_it".to_string(), "package::main".to_string());
        let source = "fn half<T: f32>(v: T) -> i32 { return components(T) + components(Odd); }\n\
                      fn use_it(a: f32) -> i32 { return half(a); }\n";
        let mut module = parse(source).expect("parses");
        let diagnostics = apply(&mut module, &origins).expect_err("Odd has no component count");
        let rendered =
            diagnostics.render(&|path| (path == "package::math::half").then(|| source.to_string()));
        // The span is in the template file, so the caret only lands if the
        // diagnostic was attributed to that module rather than to the
        // caller.
        assert!(rendered.contains("package::math::half"), "{rendered}");
        assert!(rendered.contains("components(Odd)"), "{rendered}");
    }
}
