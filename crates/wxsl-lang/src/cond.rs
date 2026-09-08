//! Conditional translation and macro binding.
//!
//! A WXSL file declares its own knobs, with defaults:
//!
//! ```wxsl
//! @macro const OCTAVES: i32 = 5;
//! @macro const RIDGED: bool = false;
//! ```
//!
//! and uses them two ways. As an `@if` condition, where a false value
//! *deletes* the declaration or statement it guards — which is what lets a
//! body contain code that would not even type-check under other settings.
//! And as an ordinary constant, usable in an array size or a loop bound
//! because it really is a module-scope `const`.
//!
//! [`apply`] does both: it evaluates every `@if`, drops what is false, then
//! rewrites each `@macro const` initializer to its bound value and strips
//! the marker attribute. What comes out is plain WGSL-shaped: a `const` with
//! a literal, and no conditionals left.
//!
//! Precedence is file default, then whatever the caller binds. The caller's
//! bindings are how a graph sets a macro globally, and how a single node
//! overrides one for its own instantiation — two instantiations of the same
//! module with different bindings are simply run through this pass twice
//! ([ADR 0011](../../../docs/adr/0011-own-the-shading-language.md)).

use std::collections::BTreeMap;

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::span::Span;

/// A macro's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// A `bool`, usable as an `@if` condition.
    Bool(bool),
    /// An `i32`.
    Int(i64),
    /// An `f32`.
    Float(f64),
}

impl Value {
    /// The WGSL type this value is written as.
    pub fn wgsl_type(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) => "i32",
            Value::Float(_) => "f32",
        }
    }

    /// This value as a WGSL literal, or `None` if it has none (a non-finite
    /// float).
    pub fn literal(&self) -> Option<String> {
        match self {
            Value::Bool(v) => Some(v.to_string()),
            Value::Int(v) => Some(v.to_string()),
            Value::Float(v) if v.is_finite() => {
                let text = format!("{v:?}");
                // WGSL needs a decimal point or exponent for a float literal.
                Some(if text.contains('.') || text.contains('e') {
                    text
                } else {
                    format!("{text}.0")
                })
            }
            Value::Float(_) => None,
        }
    }

    /// Parse a value from the `name=value` syntax used on command lines.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text {
            "true" => return Some(Value::Bool(true)),
            "false" => return Some(Value::Bool(false)),
            _ => {}
        }
        if text.contains('.') || text.contains('e') || text.contains('E') {
            text.parse::<f64>().ok().map(Value::Float)
        } else {
            text.parse::<i64>().ok().map(Value::Int)
        }
    }

    /// Read a value out of a literal in the syntax tree.
    fn from_literal(literal: &Literal) -> Option<Self> {
        match literal.kind {
            LiteralKind::Bool => Some(Value::Bool(literal.text == "true")),
            LiteralKind::Int => {
                let text = literal.text.trim_end_matches(['i', 'u']);
                if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                    i64::from_str_radix(hex, 16).ok().map(Value::Int)
                } else {
                    text.parse::<i64>().ok().map(Value::Int)
                }
            }
            LiteralKind::Float => literal
                .text
                .trim_end_matches(['f', 'h'])
                .parse::<f64>()
                .ok()
                .map(Value::Float),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Bool(v) => *v,
            Value::Int(v) => *v != 0,
            Value::Float(v) => *v != 0.0,
        }
    }
}

impl core::fmt::Display for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
        }
    }
}

/// Macro values supplied by the caller, overriding file defaults.
pub type Bindings = BTreeMap<String, Value>;

/// Evaluate `@if` conditions and bind macro constants, in place.
///
/// Reports every problem it finds rather than the first.
pub fn apply(module: &mut Module, bindings: &Bindings) -> Result<(), Diagnostics> {
    let mut diagnostics = Diagnostics::new();

    // File defaults first, then the caller's bindings on top.
    let mut values: Bindings = BTreeMap::new();
    for declaration in &module.declarations {
        if let Declaration::Const(value) = declaration {
            if !value.is_macro() {
                continue;
            }
            match value.init.as_ref().and_then(literal_value) {
                Some(parsed) => {
                    values.insert(value.name.node.clone(), parsed);
                }
                None => diagnostics.push(
                    Diagnostic::error(
                        format!("`@macro const {}` needs a literal default", value.name.node),
                        value.span,
                    )
                    .with_note(
                        "a macro's default is read at compile time, so it cannot be computed",
                    ),
                ),
            }
        }
    }
    for (name, value) in bindings {
        // Only override a macro the module actually declares. Binding a name
        // nothing declares is silently ignored: one set of bindings is
        // applied to every module in a compilation, and most of them will
        // not declare most of the macros.
        if values.contains_key(name) {
            values.insert(name.clone(), *value);
        }
    }

    // Conditionals first, so a macro used only inside a dropped branch does
    // not have to be bindable.
    module
        .declarations
        .retain(|declaration| keep(declaration.attributes(), &values, &mut diagnostics));

    for declaration in &mut module.declarations {
        strip_conditionals(declaration_attributes_mut(declaration));
        match declaration {
            Declaration::Function(function) => {
                prune_block(&mut function.body, &values, &mut diagnostics);
            }
            Declaration::Struct(item) => {
                item.members
                    .retain(|member| keep(&member.attributes, &values, &mut diagnostics));
                for member in &mut item.members {
                    strip_conditionals(&mut member.attributes);
                }
            }
            _ => {}
        }
    }

    // Then bind: a `@macro const` becomes an ordinary const with a literal.
    for declaration in &mut module.declarations {
        let Declaration::Const(value) = declaration else {
            continue;
        };
        if !value.is_macro() {
            continue;
        }
        let bound = values.get(&value.name.node).copied();
        if let Some(bound) = bound {
            match bound.literal() {
                Some(text) => {
                    let kind = match bound {
                        Value::Bool(_) => LiteralKind::Bool,
                        Value::Int(_) => LiteralKind::Int,
                        Value::Float(_) => LiteralKind::Float,
                    };
                    value.init = Some(Expr::Literal(Literal {
                        kind,
                        text,
                        span: value.span,
                    }));
                }
                None => diagnostics.push(Diagnostic::error(
                    format!(
                        "macro `{}` was bound to `{bound}`, which has no WGSL literal",
                        value.name.node
                    ),
                    value.span,
                )),
            }
        }
        value
            .attributes
            .retain(|attribute| attribute.name.node != "macro");
    }

    if diagnostics.has_errors() {
        return Err(diagnostics);
    }
    Ok(())
}

/// The literal value of an expression, looking through parentheses.
fn literal_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal(literal) => Value::from_literal(literal),
        Expr::Paren(paren) => literal_value(&paren.inner),
        Expr::Unary(unary) if unary.op == UnaryOp::Negate => match literal_value(&unary.operand)? {
            Value::Int(v) => Some(Value::Int(-v)),
            Value::Float(v) => Some(Value::Float(-v)),
            Value::Bool(_) => None,
        },
        _ => None,
    }
}

fn declaration_attributes_mut(declaration: &mut Declaration) -> &mut Vec<Attribute> {
    match declaration {
        Declaration::Const(value) | Declaration::Override(value) | Declaration::Var(value) => {
            &mut value.attributes
        }
        Declaration::Alias(alias) => &mut alias.attributes,
        Declaration::Struct(item) => &mut item.attributes,
        Declaration::Function(function) => &mut function.attributes,
        Declaration::ConstAssert(assert) => &mut assert.attributes,
    }
}

fn strip_conditionals(attributes: &mut Vec<Attribute>) {
    attributes.retain(|attribute| attribute.name.node != "if");
}

/// Whether an item guarded by `attributes` survives.
fn keep(attributes: &[Attribute], values: &Bindings, diagnostics: &mut Diagnostics) -> bool {
    for attribute in attributes {
        if let Some(condition) = attribute.condition() {
            match evaluate(condition, values) {
                Ok(value) => {
                    if !value.truthy() {
                        return false;
                    }
                }
                Err(diagnostic) => {
                    diagnostics.push(diagnostic);
                    // Keep it, so one bad condition does not also produce a
                    // cascade of "undefined name" errors downstream.
                    return true;
                }
            }
        }
    }
    true
}

fn prune_block(block: &mut Block, values: &Bindings, diagnostics: &mut Diagnostics) {
    block
        .statements
        .retain(|statement| keep(statement.attributes(), values, diagnostics));

    for statement in &mut block.statements {
        strip_conditionals(statement_attributes_mut(statement));
        match statement {
            Statement::Block(inner) => prune_block(inner, values, diagnostics),
            Statement::If(item) => {
                for (_, body) in &mut item.arms {
                    prune_block(body, values, diagnostics);
                }
                if let Some(body) = &mut item.otherwise {
                    prune_block(body, values, diagnostics);
                }
            }
            Statement::Switch(item) => {
                item.clauses
                    .retain(|clause| keep(&clause.attributes, values, diagnostics));
                for clause in &mut item.clauses {
                    strip_conditionals(&mut clause.attributes);
                    prune_block(&mut clause.body, values, diagnostics);
                }
            }
            Statement::Loop(item) => {
                prune_block(&mut item.body, values, diagnostics);
                if let Some(continuing) = &mut item.continuing {
                    prune_block(&mut continuing.body, values, diagnostics);
                }
            }
            Statement::For(item) => prune_block(&mut item.body, values, diagnostics),
            Statement::While(item) => prune_block(&mut item.body, values, diagnostics),
            _ => {}
        }
    }
}

fn statement_attributes_mut(statement: &mut Statement) -> &mut Vec<Attribute> {
    match statement {
        Statement::Local(local) => &mut local.attributes,
        Statement::Assign(assign) => &mut assign.attributes,
        Statement::Step(step) => &mut step.attributes,
        Statement::Block(block) => &mut block.attributes,
        Statement::If(item) => &mut item.attributes,
        Statement::Switch(item) => &mut item.attributes,
        Statement::Loop(item) => &mut item.attributes,
        Statement::For(item) => &mut item.attributes,
        Statement::While(item) => &mut item.attributes,
        Statement::Return(item) => &mut item.attributes,
        Statement::Break(item) | Statement::Continue(item) | Statement::Discard(item) => {
            &mut item.attributes
        }
        Statement::Call(item) => &mut item.attributes,
        Statement::ConstAssert(item) => &mut item.attributes,
    }
}

/// Evaluate a compile-time expression over the macro values.
///
/// Deliberately small: names, literals, the logical and comparison
/// operators, and arithmetic. An `@if` condition that needs more than this
/// is a sign the logic belongs in the shader rather than in the condition.
fn evaluate(expr: &Expr, values: &Bindings) -> Result<Value, Diagnostic> {
    match expr {
        Expr::Literal(literal) => Value::from_literal(literal).ok_or_else(|| {
            Diagnostic::error(
                format!("`{}` is not a value a condition can use", literal.text),
                literal.span,
            )
        }),
        Expr::Paren(paren) => evaluate(&paren.inner, values),
        Expr::Name(name) if name.template_args.is_empty() => {
            values.get(&name.name.node).copied().ok_or_else(|| {
                Diagnostic::error(
                    format!(
                        "`{}` is not a macro declared in this module",
                        name.name.node
                    ),
                    name.span,
                )
                .with_note("declare it with `@macro const NAME: bool = false;`")
            })
        }
        Expr::Unary(unary) => {
            let operand = evaluate(&unary.operand, values)?;
            match unary.op {
                UnaryOp::Not => Ok(Value::Bool(!operand.truthy())),
                UnaryOp::Negate => match operand {
                    Value::Int(v) => Ok(Value::Int(-v)),
                    Value::Float(v) => Ok(Value::Float(-v)),
                    Value::Bool(_) => Err(unsupported(unary.span, "cannot negate a bool")),
                },
                _ => Err(unsupported(
                    unary.span,
                    "only `!` and `-` work in a condition",
                )),
            }
        }
        Expr::Binary(binary) => {
            let left = evaluate(&binary.left, values)?;
            // Short-circuit, so `A && B` does not require B to be declared
            // when A is false.
            match binary.op {
                BinaryOp::LogicalAnd if !left.truthy() => return Ok(Value::Bool(false)),
                BinaryOp::LogicalOr if left.truthy() => return Ok(Value::Bool(true)),
                _ => {}
            }
            let right = evaluate(&binary.right, values)?;
            binary_value(binary.op, left, right, binary.span)
        }
        // `components(T)` folds during template instantiation, which runs
        // after this pass and per instantiation. Saying so is worth more
        // than the generic message, because the code reads as if it should
        // work.
        Expr::Call(call) if call.callee.name.node == "components" => Err(unsupported(
            call.span,
            "`components` is folded when a template is instantiated, which is after              conditional translation, so an `@if` cannot use it",
        )),
        other => Err(unsupported(
            other.span(),
            "a condition may only use macros, literals and operators",
        )),
    }
}

fn binary_value(op: BinaryOp, left: Value, right: Value, span: Span) -> Result<Value, Diagnostic> {
    use BinaryOp::*;
    // Compare and compute in f64 when either side is a float, in i64
    // otherwise, so `OCTAVES > 2` and `SCALE > 0.5` both work.
    let numeric = |left: Value, right: Value| -> (f64, f64) {
        let to_f64 = |value: Value| match value {
            Value::Bool(v) => u8::from(v) as f64,
            Value::Int(v) => v as f64,
            Value::Float(v) => v,
        };
        (to_f64(left), to_f64(right))
    };
    let both_int = matches!(
        (left, right),
        (
            Value::Int(_) | Value::Bool(_),
            Value::Int(_) | Value::Bool(_)
        )
    );

    Ok(match op {
        LogicalAnd => Value::Bool(left.truthy() && right.truthy()),
        LogicalOr => Value::Bool(left.truthy() || right.truthy()),
        Equal | NotEqual | Less | LessEqual | Greater | GreaterEqual => {
            let (a, b) = numeric(left, right);
            #[allow(clippy::float_cmp)]
            let result = match op {
                Equal => a == b,
                NotEqual => a != b,
                Less => a < b,
                LessEqual => a <= b,
                Greater => a > b,
                _ => a >= b,
            };
            Value::Bool(result)
        }
        Add | Subtract | Multiply | Divide | Modulo => {
            let (a, b) = numeric(left, right);
            if matches!(op, Divide | Modulo) && b == 0.0 {
                return Err(unsupported(span, "division by zero in a condition"));
            }
            let result = match op {
                Add => a + b,
                Subtract => a - b,
                Multiply => a * b,
                Divide => a / b,
                _ => a % b,
            };
            if both_int {
                Value::Int(result as i64)
            } else {
                Value::Float(result)
            }
        }
        _ => {
            return Err(unsupported(
                span,
                "that operator does not work in a condition",
            ))
        }
    })
}

fn unsupported(span: Span, message: &str) -> Diagnostic {
    Diagnostic::error(message.to_string(), span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::emit;
    use crate::parse::parse;

    fn run(source: &str, bindings: &[(&str, Value)]) -> String {
        let mut module = parse(source).expect("parses");
        let bindings: Bindings = bindings
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect();
        match apply(&mut module, &bindings) {
            Ok(()) => emit(&module),
            Err(diagnostics) => panic!("{}", diagnostics.render(&|_| Some(source.to_string()))),
        }
    }

    #[test]
    fn a_false_condition_deletes_the_declaration() {
        let source =
            "@macro const RIDGED: bool = false;\n\n@if(RIDGED)\nfn ridge() { }\n\nfn keep() { }\n";
        let out = run(source, &[]);
        assert!(!out.contains("fn ridge"), "{out}");
        assert!(out.contains("fn keep"), "{out}");
    }

    #[test]
    fn a_binding_overrides_the_file_default() {
        let source = "@macro const RIDGED: bool = false;\n\n@if(RIDGED)\nfn ridge() { }\n";
        let out = run(source, &[("RIDGED", Value::Bool(true))]);
        assert!(out.contains("fn ridge"), "{out}");
        // The marker attributes are gone either way.
        assert!(!out.contains("@if"), "{out}");
        assert!(!out.contains("@macro"), "{out}");
    }

    #[test]
    fn a_numeric_macro_becomes_an_ordinary_const() {
        let source = "@macro const OCTAVES: i32 = 5;\n\nfn f() {\n    for (var i = 0; i < OCTAVES; i++) { }\n}\n";
        let out = run(source, &[("OCTAVES", Value::Int(2))]);
        assert!(out.contains("const OCTAVES: i32 = 2;"), "{out}");
        assert!(!out.contains("@macro"), "{out}");
        // The use site is untouched; it refers to a real const now.
        assert!(out.contains("i < OCTAVES"), "{out}");
    }

    #[test]
    fn statements_and_struct_members_are_pruned_too() {
        let source = "\
@macro const EXTRA: bool = false;

struct S {
    a: f32,
    @if(EXTRA) b: f32,
}

fn f() {
    let x = 1.0;
    @if(EXTRA) let y = 2.0;
}
";
        let out = run(source, &[]);
        assert!(out.contains("a: f32"), "{out}");
        assert!(!out.contains("b: f32"), "{out}");
        assert!(out.contains("let x"), "{out}");
        assert!(!out.contains("let y"), "{out}");
    }

    #[test]
    fn conditions_can_compare_and_combine() {
        let source = "\
@macro const OCTAVES: i32 = 5;
@macro const RIDGED: bool = false;

@if(OCTAVES > 3)
fn many() { }

@if(OCTAVES > 3 && !RIDGED)
fn many_smooth() { }

@if(OCTAVES > 3 && RIDGED)
fn many_ridged() { }

@if(OCTAVES == 1 || RIDGED)
fn neither() { }
";
        let out = run(source, &[]);
        assert!(out.contains("fn many()"), "{out}");
        assert!(out.contains("fn many_smooth"), "{out}");
        assert!(!out.contains("fn many_ridged"), "{out}");
        assert!(!out.contains("fn neither"), "{out}");
    }

    #[test]
    fn an_undeclared_macro_in_a_condition_is_an_error() {
        let mut module = parse("@if(NOPE)\nfn f() { }\n").expect("parses");
        let error = apply(&mut module, &Bindings::new()).expect_err("undeclared");
        assert!(
            error.to_string().contains("is not a macro declared"),
            "{error}"
        );
    }

    #[test]
    fn short_circuit_means_the_right_side_need_not_be_declared() {
        // `false && UNDECLARED` must not error: this is what lets a shared
        // header guard a block on a macro only some modules declare.
        let source = "@macro const A: bool = false;\n\n@if(A && B)\nfn f() { }\n";
        let out = run(source, &[]);
        assert!(!out.contains("fn f"), "{out}");
    }

    #[test]
    fn a_binding_for_a_macro_this_module_does_not_declare_is_ignored() {
        // One binding set is applied to every module in a compilation, so
        // most modules see names they never declared.
        let source = "@macro const A: bool = true;\n\n@if(A)\nfn f() { }\n";
        let out = run(source, &[("SOMETHING_ELSE", Value::Bool(false))]);
        assert!(out.contains("fn f"), "{out}");
    }

    #[test]
    fn values_round_trip_through_their_literal_form() {
        assert_eq!(Value::parse("true"), Some(Value::Bool(true)));
        assert_eq!(Value::parse("-3"), Some(Value::Int(-3)));
        assert_eq!(Value::parse("0.5"), Some(Value::Float(0.5)));
        assert_eq!(Value::parse("nope"), None);

        // A float always keeps a decimal point, or WGSL reads it as an int.
        assert_eq!(Value::Float(2.0).literal().as_deref(), Some("2.0"));
        assert_eq!(Value::Float(0.5).literal().as_deref(), Some("0.5"));
        assert_eq!(Value::Float(f64::NAN).literal(), None);
        assert_eq!(Value::Int(4).literal().as_deref(), Some("4"));
    }

    #[test]
    fn a_macro_default_must_be_a_literal() {
        let mut module = parse("@macro const N: i32 = 2 + 2;\n").expect("parses");
        let error = apply(&mut module, &Bindings::new()).expect_err("computed default");
        assert!(
            error.to_string().contains("needs a literal default"),
            "{error}"
        );
    }

    #[test]
    fn hex_and_suffixed_defaults_are_read_correctly() {
        let source = "@macro const A: i32 = 0x10;\n@macro const B: i32 = 4i;\n\n@if(A == 16 && B == 4)\nfn f() { }\n";
        let out = run(source, &[]);
        assert!(out.contains("fn f"), "{out}");
    }
}
