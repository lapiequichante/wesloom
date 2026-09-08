//! The WGSL backend: turning a syntax tree back into source text.
//!
//! Two entry points, for two different jobs.
//!
//! [`emit`] renders *whatever is in the tree*, WXSL-only constructs
//! included. It is what `--dump-wxsl`-style tooling wants, and what the
//! round-trip tests exercise: parsing then emitting then parsing again must
//! reach the same text, which is a strong check that the parser is not
//! silently dropping anything.
//!
//! [`emit_wgsl`] is the actual backend. It refuses to emit if any WXSL-only
//! construct is still present — an unresolved import, an uninstantiated
//! template, a surviving `@if` — because those mean a transformation pass
//! was skipped. Catching that here produces a diagnostic naming the
//! construct, instead of handing `wgpu` text it will reject with a message
//! about a syntax it has never heard of.
//!
//! Expressions render through [`core::fmt::Display`] on the AST nodes, which
//! is WGSL syntax already. `Expr::Paren` survives in the tree precisely so
//! this step never has to reconstruct grouping from precedence.

use core::fmt::Write;

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Diagnostics};

/// One indentation level.
const INDENT: &str = "    ";

/// Render `module` as source text, faithfully, including any WXSL-only
/// constructs it still contains.
pub fn emit(module: &Module) -> String {
    let mut out = String::new();
    let mut first = true;

    for directive in &module.directives {
        let keyword = match directive.kind {
            DirectiveKind::Enable => "enable",
            DirectiveKind::Requires => "requires",
        };
        let names: Vec<&str> = directive.names.iter().map(|n| n.node.as_str()).collect();
        let _ = writeln!(out, "{keyword} {};", names.join(", "));
        first = false;
    }
    if !module.directives.is_empty() && !module.imports.is_empty() {
        out.push('\n');
    }

    for import in &module.imports {
        emit_import(&mut out, import);
        first = false;
    }

    for declaration in &module.declarations {
        if !first {
            out.push('\n');
        }
        emit_declaration(&mut out, declaration);
        first = false;
    }
    out
}

/// Render `module` as WGSL, refusing anything WGSL cannot express.
///
/// Reports every offending construct rather than the first, so one pass over
/// the output tells you which transformations still need to run.
pub fn emit_wgsl(module: &Module) -> Result<String, Diagnostics> {
    let mut diagnostics = Diagnostics::new();
    check_wgsl(module, &mut diagnostics);
    if diagnostics.has_errors() {
        return Err(diagnostics);
    }
    Ok(emit(module))
}

/// Collect every reason `module` is not yet WGSL.
fn check_wgsl(module: &Module, diagnostics: &mut Diagnostics) {
    for import in &module.imports {
        diagnostics.push(
            Diagnostic::error("unresolved import reached the WGSL backend", import.span)
                .with_note("imports are removed by resolution; this means that pass did not run"),
        );
    }
    for declaration in &module.declarations {
        for generic in declaration.generics() {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "uninstantiated template parameter `{}` reached the WGSL backend",
                        generic.name.node
                    ),
                    generic.span,
                )
                .with_note("WGSL has no generics; monomorphization must run first"),
            );
        }
        check_attributes(declaration.attributes(), diagnostics);
        if let Declaration::Function(function) = declaration {
            check_block(&function.body, diagnostics);
            for param in &function.params {
                check_attributes(&param.attributes, diagnostics);
            }
        }
        if let Declaration::Struct(item) = declaration {
            for member in &item.members {
                check_attributes(&member.attributes, diagnostics);
            }
        }
    }
}

/// `@if` and `@macro` have no WGSL meaning.
fn check_attributes(attributes: &[Attribute], diagnostics: &mut Diagnostics) {
    for attribute in attributes {
        let name = attribute.name.node.as_str();
        if name == "if" || name == "macro" {
            diagnostics.push(
                Diagnostic::error(
                    format!("`@{name}` reached the WGSL backend"),
                    attribute.span,
                )
                .with_note(if name == "if" {
                    "conditional translation must be applied before emitting"
                } else {
                    "a macro constant must be bound to a value before emitting"
                }),
            );
        }
    }
}

fn check_block(block: &Block, diagnostics: &mut Diagnostics) {
    check_attributes(&block.attributes, diagnostics);
    for statement in &block.statements {
        check_attributes(statement.attributes(), diagnostics);
        match statement {
            Statement::Block(inner) => check_block(inner, diagnostics),
            Statement::If(item) => {
                for (_, body) in &item.arms {
                    check_block(body, diagnostics);
                }
                if let Some(body) = &item.otherwise {
                    check_block(body, diagnostics);
                }
            }
            Statement::Switch(item) => {
                for clause in &item.clauses {
                    check_attributes(&clause.attributes, diagnostics);
                    check_block(&clause.body, diagnostics);
                }
            }
            Statement::Loop(item) => {
                check_block(&item.body, diagnostics);
                if let Some(continuing) = &item.continuing {
                    check_block(&continuing.body, diagnostics);
                }
            }
            Statement::For(item) => check_block(&item.body, diagnostics),
            Statement::While(item) => check_block(&item.body, diagnostics),
            _ => {}
        }
    }
}

fn emit_import(out: &mut String, import: &Import) {
    let path = import.path.to_string();
    match import.items.len() {
        0 => {
            let _ = writeln!(out, "import {path};");
        }
        1 => {
            let item = &import.items[0];
            let _ = write!(out, "import {path}::{}", item.name.node);
            if let Some(alias) = &item.alias {
                let _ = write!(out, " as {}", alias.node);
            }
            out.push_str(";\n");
        }
        _ => {
            let _ = write!(out, "import {path}::{{");
            for (index, item) in import.items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{}", item.name.node);
                if let Some(alias) = &item.alias {
                    let _ = write!(out, " as {}", alias.node);
                }
            }
            out.push_str("};\n");
        }
    }
}

fn emit_declaration(out: &mut String, declaration: &Declaration) {
    match declaration {
        Declaration::Const(value) => emit_global_value(out, value, "const"),
        Declaration::Override(value) => emit_global_value(out, value, "override"),
        Declaration::Var(value) => emit_global_value(out, value, "var"),
        Declaration::Alias(alias) => {
            emit_attributes_inline(out, &alias.attributes);
            let _ = writeln!(out, "alias {} = {};", alias.name.node, alias.ty);
        }
        Declaration::Struct(item) => emit_struct(out, item),
        Declaration::Function(function) => emit_function(out, function),
        Declaration::ConstAssert(assert) => {
            emit_attributes_inline(out, &assert.attributes);
            let _ = writeln!(out, "const_assert {};", assert.expr);
        }
    }
}

fn emit_global_value(out: &mut String, value: &GlobalValue, keyword: &str) {
    emit_attributes_inline(out, &value.attributes);
    let _ = write!(out, "{keyword}");
    if !value.address_space.is_empty() {
        let args: Vec<String> = value
            .address_space
            .iter()
            .map(ToString::to_string)
            .collect();
        let _ = write!(out, "<{}>", args.join(", "));
    }
    let _ = write!(out, " {}", value.name.node);
    if let Some(ty) = &value.ty {
        let _ = write!(out, ": {ty}");
    }
    if let Some(init) = &value.init {
        let _ = write!(out, " = {init}");
    }
    out.push_str(";\n");
}

fn emit_struct(out: &mut String, item: &StructDecl) {
    emit_attribute_lines(out, &item.attributes);
    let _ = write!(out, "struct {}", item.name.node);
    emit_generics(out, &item.generics);
    out.push_str(" {\n");
    for member in &item.members {
        out.push_str(INDENT);
        emit_attributes_inline(out, &member.attributes);
        let _ = writeln!(out, "{}: {},", member.name.node, member.ty);
    }
    out.push_str("}\n");
}

fn emit_generics(out: &mut String, generics: &[GenericParam]) {
    if generics.is_empty() {
        return;
    }
    out.push('<');
    for (index, generic) in generics.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&generic.name.node);
        if !generic.constraints.is_empty() {
            let names: Vec<String> = generic
                .constraints
                .iter()
                .map(ToString::to_string)
                .collect();
            let _ = write!(out, ": {}", names.join(" | "));
        }
    }
    out.push('>');
}

fn emit_function(out: &mut String, function: &Function) {
    emit_attribute_lines(out, &function.attributes);
    let _ = write!(out, "fn {}", function.name.node);
    emit_generics(out, &function.generics);
    out.push('(');
    for (index, param) in function.params.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        emit_attributes_inline(out, &param.attributes);
        let _ = write!(out, "{}: {}", param.name.node, param.ty);
    }
    out.push(')');
    if let Some(ty) = &function.return_type {
        out.push_str(" -> ");
        emit_attributes_inline(out, &function.return_attributes);
        let _ = write!(out, "{ty}");
    }
    out.push_str(" {\n");
    for statement in &function.body.statements {
        emit_statement(out, statement, 1);
    }
    out.push_str("}\n");
}

/// Attributes on their own line, all of them on one line.
///
/// Used for functions and structs, whose attributes (`@fragment`,
/// `@workgroup_size`, `@if`) read better above the declaration than beside
/// it. Value declarations use `emit_attributes_inline` instead.
fn emit_attribute_lines(out: &mut String, attributes: &[Attribute]) {
    if attributes.is_empty() {
        return;
    }
    for (index, attribute) in attributes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        emit_attribute(out, attribute);
    }
    out.push('\n');
}

/// Attributes followed by a space, for parameters and struct members.
fn emit_attributes_inline(out: &mut String, attributes: &[Attribute]) {
    for attribute in attributes {
        emit_attribute(out, attribute);
        out.push(' ');
    }
}

fn emit_attribute(out: &mut String, attribute: &Attribute) {
    let _ = write!(out, "@{}", attribute.name.node);
    if !attribute.args.is_empty() {
        let args: Vec<String> = attribute.args.iter().map(ToString::to_string).collect();
        let _ = write!(out, "({})", args.join(", "));
    }
}

fn emit_statement(out: &mut String, statement: &Statement, depth: usize) {
    let pad = INDENT.repeat(depth);

    // Attributes precede the statement on the same line, which keeps a
    // one-line `@if(X) total += 1;` readable.
    let mut prefix = String::new();
    for attribute in statement.attributes() {
        emit_attribute(&mut prefix, attribute);
        prefix.push(' ');
    }

    match statement {
        Statement::Local(local) => {
            let keyword = match local.kind {
                LocalKind::Let => "let",
                LocalKind::Var => "var",
                LocalKind::Const => "const",
            };
            let _ = write!(out, "{pad}{prefix}{keyword}");
            if !local.address_space.is_empty() {
                let args: Vec<String> = local
                    .address_space
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let _ = write!(out, "<{}>", args.join(", "));
            }
            let _ = write!(out, " {}", local.name.node);
            if let Some(ty) = &local.ty {
                let _ = write!(out, ": {ty}");
            }
            if let Some(init) = &local.init {
                let _ = write!(out, " = {init}");
            }
            out.push_str(";\n");
        }
        Statement::Assign(assign) => {
            let target = match &assign.target {
                Some(target) => target.to_string(),
                None => "_".to_string(),
            };
            let _ = writeln!(
                out,
                "{pad}{prefix}{target} {} {};",
                assign.op.text(),
                assign.value
            );
        }
        Statement::Step(step) => {
            let op = if step.increment { "++" } else { "--" };
            let _ = writeln!(out, "{pad}{prefix}{}{op};", step.target);
        }
        Statement::Block(block) => {
            let _ = writeln!(out, "{pad}{prefix}{{");
            for inner in &block.statements {
                emit_statement(out, inner, depth + 1);
            }
            let _ = writeln!(out, "{pad}}}");
        }
        Statement::If(item) => {
            for (index, (condition, body)) in item.arms.iter().enumerate() {
                if index == 0 {
                    let _ = writeln!(out, "{pad}{prefix}if {condition} {{");
                } else {
                    let _ = writeln!(out, "{pad}}} else if {condition} {{");
                }
                for inner in &body.statements {
                    emit_statement(out, inner, depth + 1);
                }
            }
            match &item.otherwise {
                Some(body) => {
                    let _ = writeln!(out, "{pad}}} else {{");
                    for inner in &body.statements {
                        emit_statement(out, inner, depth + 1);
                    }
                    let _ = writeln!(out, "{pad}}}");
                }
                None => {
                    let _ = writeln!(out, "{pad}}}");
                }
            }
        }
        Statement::Switch(item) => {
            let _ = writeln!(out, "{pad}{prefix}switch {} {{", item.selector);
            for clause in &item.clauses {
                let mut clause_prefix = String::new();
                for attribute in &clause.attributes {
                    emit_attribute(&mut clause_prefix, attribute);
                    clause_prefix.push(' ');
                }
                let inner_pad = INDENT.repeat(depth + 1);
                if clause.selectors.is_empty() {
                    let _ = writeln!(out, "{inner_pad}{clause_prefix}default: {{");
                } else {
                    let selectors: Vec<String> = clause
                        .selectors
                        .iter()
                        .map(|selector| match selector {
                            CaseSelector::Value(expr) => expr.to_string(),
                            CaseSelector::Default(_) => "default".to_string(),
                        })
                        .collect();
                    let _ = writeln!(
                        out,
                        "{inner_pad}{clause_prefix}case {}: {{",
                        selectors.join(", ")
                    );
                }
                for inner in &clause.body.statements {
                    emit_statement(out, inner, depth + 2);
                }
                let _ = writeln!(out, "{inner_pad}}}");
            }
            let _ = writeln!(out, "{pad}}}");
        }
        Statement::Loop(item) => {
            let _ = writeln!(out, "{pad}{prefix}loop {{");
            for inner in &item.body.statements {
                emit_statement(out, inner, depth + 1);
            }
            if let Some(continuing) = &item.continuing {
                let inner_pad = INDENT.repeat(depth + 1);
                let _ = writeln!(out, "{inner_pad}continuing {{");
                for inner in &continuing.body.statements {
                    emit_statement(out, inner, depth + 2);
                }
                if let Some(condition) = &continuing.break_if {
                    let _ = writeln!(out, "{}break if {condition};", INDENT.repeat(depth + 2));
                }
                let _ = writeln!(out, "{inner_pad}}}");
            }
            let _ = writeln!(out, "{pad}}}");
        }
        Statement::For(item) => {
            let init = item
                .init
                .as_ref()
                .map(|statement| inline_statement(statement))
                .unwrap_or_default();
            let condition = item
                .condition
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let update = item
                .update
                .as_ref()
                .map(|statement| inline_statement(statement))
                .unwrap_or_default();
            let _ = writeln!(out, "{pad}{prefix}for ({init}; {condition}; {update}) {{");
            for inner in &item.body.statements {
                emit_statement(out, inner, depth + 1);
            }
            let _ = writeln!(out, "{pad}}}");
        }
        Statement::While(item) => {
            let _ = writeln!(out, "{pad}{prefix}while {} {{", item.condition);
            for inner in &item.body.statements {
                emit_statement(out, inner, depth + 1);
            }
            let _ = writeln!(out, "{pad}}}");
        }
        Statement::Return(item) => match &item.value {
            Some(value) => {
                let _ = writeln!(out, "{pad}{prefix}return {value};");
            }
            None => {
                let _ = writeln!(out, "{pad}{prefix}return;");
            }
        },
        Statement::Break(_) => {
            let _ = writeln!(out, "{pad}{prefix}break;");
        }
        Statement::Continue(_) => {
            let _ = writeln!(out, "{pad}{prefix}continue;");
        }
        Statement::Discard(_) => {
            let _ = writeln!(out, "{pad}{prefix}discard;");
        }
        Statement::Call(item) => {
            let _ = writeln!(out, "{pad}{prefix}{};", item.call);
        }
        Statement::ConstAssert(item) => {
            let _ = writeln!(out, "{pad}{prefix}const_assert {};", item.expr);
        }
    }
}

/// A statement rendered without indentation or trailing `;`, for a `for`
/// header.
fn inline_statement(statement: &Statement) -> String {
    let mut out = String::new();
    emit_statement(&mut out, statement, 0);
    out.trim_end().trim_end_matches(';').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn round_trip(source: &str) -> String {
        let module = parse(source).expect("parses");
        emit(&module)
    }

    #[test]
    fn a_function_emits_readable_wgsl() {
        let emitted = round_trip(
            "@fragment fn fs(@location(0) uv: vec2f) -> @location(0) vec4f {\n\
             let x = uv.x * 2.0; return vec4f(x, 0.0, 0.0, 1.0); }",
        );
        assert_eq!(
            emitted,
            "@fragment\n\
             fn fs(@location(0) uv: vec2f) -> @location(0) vec4f {\n\
             \x20   let x = uv.x * 2.0;\n\
             \x20   return vec4f(x, 0.0, 0.0, 1.0);\n\
             }\n"
        );
    }

    #[test]
    fn emission_is_idempotent() {
        // The property that matters: the first emit normalizes formatting,
        // and every emit after that must be a fixed point. If the parser
        // drops something or the emitter renders something it cannot parse
        // back, this diverges.
        let source = "\
enable f16;

import package::math::remap;
import package::wxsl::bindings::{camera, scene as world};

@macro const OCTAVES: i32 = 5;

alias Weight = f32;

struct VertexOut {
    @builtin(position) clip: vec4f,
    @location(0) world: vec3f,
}

var<uniform> lights: array<Light, 4>;

fn inverse_lerp<T: f32 | vec2f | vec3f>(a: T, b: T, value: T) -> T {
    let span = b - a;
    return select(T(0.0), (value - a) / span, abs(span) >= T(1e-8));
}

@if(RIDGED)
fn ridge(x: f32) -> f32 {
    var total = 0.0;
    for (var i = 0; i < OCTAVES; i++) {
        total += f32(i) * 0.5;
    }
    while total > 100.0 {
        total -= 1.0;
    }
    loop {
        total = total * 2.0;
        continuing {
            break if total > 8.0;
        }
    }
    switch i32(total) {
        case 0, 1: {
            total = 0.0;
        }
        default: {
            total = 1.0;
        }
    }
    if total == 0.0 {
        discard;
    } else if total == 1.0 {
        return 0.0;
    } else {
        total++;
    }
    _ = total;
    return total;
}
";
        let once = round_trip(source);
        let twice = round_trip(&once);
        assert_eq!(once, twice, "emission is not a fixed point");
        // And the normalized form should already equal the input, which is
        // written in the emitter's own style.
        assert_eq!(once, source);
    }

    #[test]
    fn literals_survive_the_round_trip_exactly() {
        let emitted = round_trip(
            "const a = 0.0031308;\nconst b = 1e-8;\nconst c = 0x7fu;\nconst d = 5.;\nconst e = 4i;\n",
        );
        for text in ["0.0031308", "1e-8", "0x7fu", "5.", "4i"] {
            assert!(emitted.contains(text), "lost `{text}` in:\n{emitted}");
        }
    }

    #[test]
    fn parentheses_are_preserved_not_reconstructed() {
        // `(a + b) * c` and `a + b * c` mean different things; the tree keeps
        // the grouping so the emitter never has to infer it.
        assert!(round_trip("const x = (a + b) * c;\n").contains("(a + b) * c"));
        assert!(round_trip("const x = a + b * c;\n").contains("a + b * c"));
    }

    #[test]
    fn the_wgsl_backend_rejects_an_unresolved_import() {
        let module = parse("import package::math::remap;\nfn f() { }\n").expect("parses");
        let error = emit_wgsl(&module).expect_err("imports are not WGSL");
        assert!(error.to_string().contains("unresolved import"), "{error}");
    }

    #[test]
    fn the_wgsl_backend_rejects_an_uninstantiated_template() {
        let module = parse("fn f<T: f32>(a: T) -> T { return a; }\n").expect("parses");
        let error = emit_wgsl(&module).expect_err("generics are not WGSL");
        assert!(
            error.to_string().contains("uninstantiated template"),
            "{error}"
        );
    }

    #[test]
    fn the_wgsl_backend_rejects_surviving_conditionals_and_macros() {
        let module = parse("@if(X)\nfn f() { }\n").expect("parses");
        let error = emit_wgsl(&module).expect_err("@if is not WGSL");
        assert!(error.to_string().contains("`@if` reached"), "{error}");

        let module = parse("@macro const N: i32 = 4;\n").expect("parses");
        let error = emit_wgsl(&module).expect_err("@macro is not WGSL");
        assert!(error.to_string().contains("`@macro` reached"), "{error}");

        // And an `@if` buried in a statement is caught too, not just on a
        // declaration.
        let module = parse("fn f() {\n    @if(X) let y = 1.0;\n}\n").expect("parses");
        let error = emit_wgsl(&module).expect_err("@if in a body is not WGSL");
        assert!(error.to_string().contains("`@if` reached"), "{error}");
    }

    #[test]
    fn plain_wgsl_passes_the_backend_unchanged() {
        let source = "@fragment\nfn fs() -> @location(0) vec4f {\n    return vec4f(1.0);\n}\n";
        let module = parse(source).expect("parses");
        assert_eq!(emit_wgsl(&module).expect("is WGSL"), source);
    }

    #[test]
    fn every_reason_is_reported_not_just_the_first() {
        let module = parse(
            "import package::a::b;\nimport package::c::d;\n@if(X)\nfn f<T: f32>(a: T) -> T { return a; }\n",
        )
        .expect("parses");
        let error = emit_wgsl(&module).expect_err("several problems");
        // Two imports, one template parameter, one `@if`.
        assert_eq!(error.into_vec().len(), 4);
    }
}
