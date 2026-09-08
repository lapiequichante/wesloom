//! Driving the generated parser, and turning its failures into diagnostics.
//!
//! The grammar in `grammar.lalrpop` builds [`crate::ast`] nodes directly.
//! This module supplies the three things it needs from Rust: the token
//! iterator, the tree-building helpers whose logic does not belong in a
//! grammar action, and the translation from LALRPOP's `ParseError` into the
//! [`Diagnostic`] form the rest of the compiler speaks.

use lalrpop_util::ParseError;

use crate::ast::{BinaryExpr, BinaryOp, Expr, Import, ImportItem, ModulePath, UnaryExpr, UnaryOp};
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::lexer::tokenize;
use crate::span::{Span, Spanned};
use crate::token::Tok;

/// The error type the grammar declares. The lexer runs to completion before
/// the parser starts, so no lexical error can reach the parser — this exists
/// to satisfy LALRPOP's `extern` block and is deliberately uninhabitable in
/// practice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexFailure {
    /// What went wrong.
    pub message: String,
    /// Where.
    pub span: Span,
}

impl core::fmt::Display for LexFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

/// What follows a module path in an `import`.
pub enum ImportTail {
    /// `import package::a::b;` — the last path segment is the item.
    Whole,
    /// `import package::a::b as c;`
    Alias(Spanned<String>),
    /// `import package::a::{b, c as d};`
    Items(Vec<(Spanned<String>, Option<Spanned<String>>)>),
}

/// Assemble an [`Import`] from the path segments and whatever followed them.
///
/// `import package::math::remap;` names one item in `package::math`, so the
/// last segment moves out of the path and becomes the item — matching how
/// the module map is keyed.
pub fn build_import(segments: Vec<Spanned<String>>, tail: ImportTail, span: Span) -> Import {
    match tail {
        ImportTail::Items(items) => Import {
            path: ModulePath::new(segments.iter().map(|s| s.node.clone())),
            items: items
                .into_iter()
                .map(|(name, alias)| ImportItem {
                    local: alias.as_ref().map_or(name.node.clone(), |a| a.node.clone()),
                    name,
                    alias,
                })
                .collect(),
            span,
        },
        ImportTail::Whole | ImportTail::Alias(_) => {
            let alias = match tail {
                ImportTail::Alias(alias) => Some(alias),
                _ => None,
            };
            let mut segments = segments;
            // An empty path cannot be produced by the grammar (it requires at
            // least one identifier), but do not panic if it ever is.
            let item = segments.pop();
            let path = ModulePath::new(segments.iter().map(|s| s.node.clone()));
            let items = match item {
                Some(name) => vec![ImportItem {
                    local: alias.as_ref().map_or(name.node.clone(), |a| a.node.clone()),
                    name,
                    alias,
                }],
                None => Vec::new(),
            };
            Import { path, items, span }
        }
    }
}

/// Build a binary expression node.
pub fn binary(op: BinaryOp, left: Expr, right: Expr, start: usize, end: usize) -> Expr {
    Expr::Binary(Box::new(BinaryExpr {
        op,
        left,
        right,
        span: Span::new(start as u32, end as u32),
    }))
}

/// Build a unary expression node.
pub fn unary(op: UnaryOp, operand: Expr, start: usize, end: usize) -> Expr {
    Expr::Unary(Box::new(UnaryExpr {
        op,
        operand,
        span: Span::new(start as u32, end as u32),
    }))
}

/// Parse one source file.
///
/// Lexes first, so a lexical problem is reported as a lexical problem rather
/// than as a mystifying parse failure several tokens later.
pub fn parse(source: &str) -> Result<crate::ast::Module, Diagnostics> {
    let tokens = tokenize(source)?;
    let stream = tokens.iter().map(|token| {
        Ok((
            token.span.start as usize,
            token.node.clone(),
            token.span.end as usize,
        ))
    });

    crate::grammar::ModuleParser::new()
        .parse(stream)
        .map_err(|error| Diagnostics::from(describe(error, source)))
}

/// Parse a single expression, for attribute values and tests.
pub fn parse_expr(source: &str) -> Result<Expr, Diagnostics> {
    let tokens = tokenize(source)?;
    let stream = tokens.iter().map(|token| {
        Ok((
            token.span.start as usize,
            token.node.clone(),
            token.span.end as usize,
        ))
    });
    crate::grammar::ExprEntryParser::new()
        .parse(stream)
        .map_err(|error| Diagnostics::from(describe(error, source)))
}

/// Turn a LALRPOP failure into a diagnostic that names what was found and
/// what could have appeared instead.
fn describe(error: ParseError<usize, Tok, LexFailure>, source: &str) -> Diagnostic {
    match error {
        ParseError::InvalidToken { location } => {
            Diagnostic::error("invalid token", Span::at(location as u32))
        }
        ParseError::UnrecognizedEof { location, expected } => Diagnostic::error(
            "unexpected end of file",
            Span::at(location.min(source.len()) as u32),
        )
        .with_note(expectation(&expected)),
        ParseError::UnrecognizedToken {
            token: (start, found, end),
            expected,
        } => Diagnostic::error(
            format!("unexpected {found}"),
            Span::new(start as u32, end as u32),
        )
        .with_note(expectation(&expected)),
        ParseError::ExtraToken {
            token: (start, found, end),
        } => Diagnostic::error(
            format!("unexpected {found} after the end of the module"),
            Span::new(start as u32, end as u32),
        ),
        ParseError::User { error } => Diagnostic::error(error.message, error.span),
    }
}

/// Render LALRPOP's expected-token set readably.
///
/// The raw set can run to dozens of terminals, which is noise rather than
/// help, so a long list is trimmed.
fn expectation(expected: &[String]) -> String {
    if expected.is_empty() {
        return "no continuation is valid here".to_string();
    }
    let names: Vec<String> = expected
        .iter()
        .map(|name| name.trim_matches('"').to_string())
        .collect();
    if names.len() > 6 {
        format!(
            "expected one of `{}` and {} more",
            names[..6].join("`, `"),
            names.len() - 6
        )
    } else if names.len() == 1 {
        format!("expected `{}`", names[0])
    } else {
        format!("expected one of `{}`", names.join("`, `"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;

    fn module(source: &str) -> Module {
        match parse(source) {
            Ok(module) => module,
            Err(diagnostics) => panic!(
                "failed to parse:\n{}",
                diagnostics.render(&|_| Some(source.to_string()))
            ),
        }
    }

    #[test]
    fn a_function_with_attributes_and_a_body() {
        let parsed = module(
            "@fragment\nfn fs(@location(0) uv: vec2f) -> @location(0) vec4f {\n\
             \x20   let x = uv.x * 2.0;\n    return vec4f(x, 0.0, 0.0, 1.0);\n}\n",
        );
        assert_eq!(parsed.declarations.len(), 1);
        let Declaration::Function(function) = &parsed.declarations[0] else {
            panic!("expected a function");
        };
        assert_eq!(function.name.node, "fs");
        assert_eq!(function.stage(), Some(Stage::Fragment));
        assert_eq!(function.params.len(), 1);
        assert_eq!(function.params[0].attributes[0].name.node, "location");
        assert_eq!(function.return_attributes.len(), 1);
        assert!(function.return_type.as_ref().unwrap().is_named("vec4f"));
        assert_eq!(function.body.statements.len(), 2);
    }

    #[test]
    fn imports_split_the_item_off_the_path() {
        let parsed = module("import package::math::remap;\n");
        let import = &parsed.imports[0];
        assert_eq!(import.path.to_string(), "package::math");
        assert_eq!(import.items[0].name.node, "remap");
        assert_eq!(import.items[0].local, "remap");
    }

    #[test]
    fn imports_support_braces_and_renaming() {
        let parsed = module("import package::wxsl::bindings::{camera, scene as world};\n");
        let import = &parsed.imports[0];
        assert_eq!(import.path.to_string(), "package::wxsl::bindings");
        assert_eq!(import.items.len(), 2);
        assert_eq!(import.items[0].local, "camera");
        assert_eq!(import.items[1].name.node, "scene");
        assert_eq!(import.items[1].local, "world");

        let renamed = module("import package::a::b as c;\n");
        assert_eq!(renamed.imports[0].path.to_string(), "package::a");
        assert_eq!(renamed.imports[0].items[0].local, "c");
    }

    #[test]
    fn templates_parse_on_declarations_and_types() {
        let parsed = module(
            "fn inverse_lerp<T: f32 | vec2f | vec3f>(a: T, b: T) -> T { return a; }\n\
             var<uniform> lights: array<Light, 4>;\n",
        );
        let Declaration::Function(function) = &parsed.declarations[0] else {
            panic!("expected a function");
        };
        assert_eq!(function.generics.len(), 1);
        assert_eq!(function.generics[0].name.node, "T");
        assert_eq!(function.generics[0].constraints.len(), 3);
        assert!(function.generics[0].accepts(&TypeExpr::named("vec2f", Span::default())));
        assert!(!function.generics[0].accepts(&TypeExpr::named("vec4f", Span::default())));

        let Declaration::Var(var) = &parsed.declarations[1] else {
            panic!("expected a var");
        };
        assert_eq!(var.address_space.len(), 1);
        assert_eq!(var.ty.as_ref().unwrap().to_string(), "array<Light, 4>");
    }

    #[test]
    fn macro_constants_are_recognized() {
        let parsed = module(
            "@macro const OCTAVES: i32 = 5;\n@macro const RIDGED: bool = false;\nconst PI = 3.14;\n",
        );
        let names: Vec<&str> = parsed.macros().map(|m| m.name.node.as_str()).collect();
        assert_eq!(names, ["OCTAVES", "RIDGED"]);
    }

    #[test]
    fn conditional_attributes_attach_to_declarations_and_statements() {
        let parsed = module(
            "@if(RIDGED)\nfn ridge(x: f32) -> f32 {\n\
             \x20   @if(EXTRA) let y = x * 2.0;\n    return x;\n}\n",
        );
        let Declaration::Function(function) = &parsed.declarations[0] else {
            panic!("expected a function");
        };
        assert_eq!(
            function.attributes[0].condition().and_then(Expr::as_name),
            Some("RIDGED")
        );
        assert_eq!(
            function.body.statements[0]
                .attributes()
                .first()
                .and_then(Attribute::condition)
                .and_then(Expr::as_name),
            Some("EXTRA")
        );
    }

    #[test]
    fn precedence_nests_the_way_wgsl_says() {
        let expr = parse_expr("a + b * c").expect("parses");
        // Multiplication binds tighter, so the root is the addition.
        let Expr::Binary(add) = &expr else {
            panic!("expected a binary expression");
        };
        assert_eq!(add.op, BinaryOp::Add);
        let Expr::Binary(mul) = &add.right else {
            panic!("expected the product on the right");
        };
        assert_eq!(mul.op, BinaryOp::Multiply);

        // Explicit parentheses are preserved rather than reconstructed.
        let parens = parse_expr("(a + b) * c").expect("parses");
        assert_eq!(parens.to_string(), "(a + b) * c");
    }

    #[test]
    fn every_statement_form_parses() {
        let parsed = module(
            "fn all_of_them(n: i32) -> f32 {\n\
             \x20   var total = 0.0;\n\
             \x20   for (var i = 0; i < n; i++) { total += f32(i); }\n\
             \x20   while total > 100.0 { total -= 1.0; }\n\
             \x20   loop { total = total * 2.0; continuing { break if total > 8.0; } }\n\
             \x20   switch n { case 0, 1: { total = 0.0; } default: { total = 1.0; } }\n\
             \x20   if n == 0 { discard; } else if n == 1 { return 0.0; } else { total++; }\n\
             \x20   _ = total;\n\
             \x20   f32(total);\n\
             \x20   return total;\n\
             }\n",
        );
        let Declaration::Function(function) = &parsed.declarations[0] else {
            panic!("expected a function");
        };
        let kinds: Vec<&str> = function
            .body
            .statements
            .iter()
            .map(|statement| match statement {
                Statement::Local(_) => "local",
                Statement::For(_) => "for",
                Statement::While(_) => "while",
                Statement::Loop(_) => "loop",
                Statement::Switch(_) => "switch",
                Statement::If(_) => "if",
                Statement::Assign(_) => "assign",
                Statement::Call(_) => "call",
                Statement::Return(_) => "return",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["local", "for", "while", "loop", "switch", "if", "assign", "call", "return"]
        );
    }

    #[test]
    fn structs_keep_member_order_and_attributes() {
        let parsed = module(
            "struct VertexOut {\n\
             \x20   @builtin(position) clip: vec4f,\n\
             \x20   @location(0) world: vec3f,\n\
             }\n",
        );
        let Declaration::Struct(item) = &parsed.declarations[0] else {
            panic!("expected a struct");
        };
        assert_eq!(item.members.len(), 2);
        assert_eq!(item.members[0].name.node, "clip");
        assert_eq!(item.members[0].attributes[0].name.node, "builtin");
        assert_eq!(item.members[1].name.node, "world");
    }

    #[test]
    fn a_parse_error_names_the_token_and_points_at_it() {
        let source = "fn broken( { }\n";
        let diagnostics = parse(source).expect_err("should not parse");
        let rendered = diagnostics.render(&|_| Some(source.to_string()));
        assert!(rendered.contains("unexpected `{`"), "{rendered}");
        assert!(rendered.contains("^"), "{rendered}");
    }

    #[test]
    fn a_lexical_error_is_reported_as_one() {
        let diagnostics = parse("fn a() { let x = 1.0zz; }").expect_err("bad literal");
        assert!(
            diagnostics.to_string().contains("invalid suffix"),
            "{diagnostics}"
        );
    }

    #[test]
    fn directives_come_first() {
        let parsed = module("enable f16;\nimport package::a::b;\nfn f() { }\n");
        assert_eq!(parsed.directives.len(), 1);
        assert_eq!(parsed.directives[0].kind, DirectiveKind::Enable);
        assert_eq!(parsed.directives[0].names[0].node, "f16");
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.declarations.len(), 1);
    }
}
