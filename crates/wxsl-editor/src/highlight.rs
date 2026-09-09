//! Syntax colouring for the WXSL and WGSL code panels.
//!
//! One highlighter for both, by construction rather than by convention: WXSL
//! is WGSL plus templates, imports, conditional translation and macros (see
//! `wxsl-lang`'s crate docs), so `wxsl-lang`'s own lexer already tokenizes
//! either text correctly — there is no separate "WGSL grammar" to
//! reimplement, and no risk of a hand-rolled tokenizer here drifting from
//! what the compiler actually accepts. [`highlight`] runs it and classifies
//! the result into [`Run`]s; [`Kind::color`] is the one place a run's colour
//! comes from, so both panels always agree.

use std::ops::Range;

use wxsl_lang::lexer::tokenize_with_comments;
use wxsl_lang::token::Tok;
use wxsl_render::ui::Color;

use crate::theme::Palette;

/// What one run of source text is, for colouring.
///
/// Anything not covered by a run — punctuation, plain identifiers, macro
/// names — is left in the panel's ordinary text colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A reserved word: `fn`, `let`, `return`, `if`, `import`, …
    Keyword,
    /// A built-in type name: `f32`, `vec3f`, `array`, `texture_2d`, …
    Type,
    /// An integer or floating-point literal.
    Number,
    /// `@fragment`, `@group(0)`, `@if(FEATURE)`, …
    Attribute,
    /// A line or block comment.
    Comment,
}

impl Kind {
    /// This kind's colour, from the shared palette both panels draw with.
    pub fn color(self, palette: &Palette) -> Color {
        match self {
            Kind::Keyword => palette.syntax_keyword,
            Kind::Type => palette.syntax_type,
            Kind::Number => palette.syntax_number,
            Kind::Attribute => palette.syntax_attribute,
            Kind::Comment => palette.syntax_comment,
        }
    }
}

/// One coloured run: source bytes `range` are `kind`.
///
/// Sorted by `range.start` and non-overlapping, so a renderer can walk a
/// line's runs left to right and paint whatever falls between them (or
/// before the first, or after the last) in the ordinary text colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// The byte range this run covers, in the source [`highlight`] ran on.
    pub range: Range<usize>,
    /// What it is.
    pub kind: Kind,
}

/// Classify `source` into coloured runs.
///
/// Never fails: both panels only ever show text `wxsl-lang` itself produced
/// (the WXSL codegen emitted) or accepted (the WGSL it compiled that to), so
/// a source the lexer rejects should not happen — but if it somehow does,
/// this comes back empty rather than losing the panel over a highlighter
/// bug. An empty result just leaves the text in its ordinary colour.
pub fn highlight(source: &str) -> Vec<Run> {
    let Ok((tokens, comments)) = tokenize_with_comments(source) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = tokens
        .iter()
        .filter_map(|token| {
            let kind = classify(&token.node)?;
            Some(Run {
                range: token.span.range(),
                kind,
            })
        })
        .chain(comments.iter().map(|span| Run {
            range: span.range(),
            kind: Kind::Comment,
        }))
        .collect();
    runs.sort_by_key(|run| run.range.start);
    runs
}

/// The token kind a run gets, or `None` for punctuation and plain
/// identifiers (which stay in the panel's ordinary text colour).
fn classify(tok: &Tok) -> Option<Kind> {
    use Tok::*;
    Some(match tok {
        Ident(name) if is_builtin_type(name) => Kind::Type,
        IntLit(_) | FloatLit(_) => Kind::Number,
        Attr(_) => Kind::Attribute,
        True | False | Alias | Break | Case | Const | ConstAssert | Continue | Continuing
        | Default | Discard | Else | Enable | Fn | For | If | Import | Let | Loop | Override
        | Requires | Return | Struct | Switch | Var | While | As => Kind::Keyword,
        _ => return None,
    })
}

/// Whether `name` is one of WGSL's predeclared type names.
///
/// These are ordinary identifiers to the lexer (`f32`, `vec3f` and the rest
/// are not keywords — see [`Tok::keyword`]), so recognising them here is a
/// fixed list rather than something the grammar can tell us.
fn is_builtin_type(name: &str) -> bool {
    const TYPES: &[&str] = &[
        "bool",
        "i32",
        "u32",
        "f32",
        "f16", //
        "vec2",
        "vec3",
        "vec4", //
        "vec2i",
        "vec2u",
        "vec2f",
        "vec2h", //
        "vec3i",
        "vec3u",
        "vec3f",
        "vec3h", //
        "vec4i",
        "vec4u",
        "vec4f",
        "vec4h", //
        "mat2x2",
        "mat2x3",
        "mat2x4", //
        "mat3x2",
        "mat3x3",
        "mat3x4", //
        "mat4x2",
        "mat4x3",
        "mat4x4", //
        "mat2x2f",
        "mat2x3f",
        "mat2x4f", //
        "mat3x2f",
        "mat3x3f",
        "mat3x4f", //
        "mat4x2f",
        "mat4x3f",
        "mat4x4f", //
        "mat2x2h",
        "mat2x3h",
        "mat2x4h", //
        "mat3x2h",
        "mat3x3h",
        "mat3x4h", //
        "mat4x2h",
        "mat4x3h",
        "mat4x4h", //
        "array",
        "ptr",
        "atomic", //
        "sampler",
        "sampler_comparison", //
        "texture_1d",
        "texture_2d",
        "texture_2d_array",
        "texture_3d",
        "texture_cube",
        "texture_cube_array",
        "texture_multisampled_2d",
        "texture_external", //
        "texture_depth_2d",
        "texture_depth_2d_array",
        "texture_depth_cube",
        "texture_depth_cube_array",
        "texture_depth_multisampled_2d", //
        "texture_storage_1d",
        "texture_storage_2d",
        "texture_storage_2d_array",
        "texture_storage_3d",
    ];
    TYPES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_at(source: &str, text: &str) -> Vec<Kind> {
        highlight(source)
            .into_iter()
            .filter(|run| &source[run.range.clone()] == text)
            .map(|run| run.kind)
            .collect()
    }

    #[test]
    fn keywords_types_numbers_and_attributes_are_classified() {
        let source = "@fragment\nfn f(x: f32) -> vec3f { let n = 4; return vec3f(n); }";
        assert_eq!(kinds_at(source, "@fragment"), vec![Kind::Attribute]);
        assert_eq!(kinds_at(source, "fn"), vec![Kind::Keyword]);
        assert_eq!(kinds_at(source, "let"), vec![Kind::Keyword]);
        assert_eq!(kinds_at(source, "return"), vec![Kind::Keyword]);
        assert_eq!(kinds_at(source, "f32"), vec![Kind::Type]);
        assert_eq!(kinds_at(source, "vec3f"), vec![Kind::Type, Kind::Type]);
        assert_eq!(kinds_at(source, "4"), vec![Kind::Number]);
    }

    #[test]
    fn plain_identifiers_and_punctuation_are_not_classified() {
        // `f`, `x` and `n` are not built-in types or keywords, and the
        // parens/braces/colon/arrow are punctuation — none of that should
        // produce a run: it stays in the panel's ordinary text colour.
        let source = "fn f(x: f32) -> f32 { return x; }";
        assert!(kinds_at(source, "f").is_empty());
        assert!(kinds_at(source, "x").is_empty());
    }

    #[test]
    fn line_and_block_comments_are_classified_and_stay_out_of_the_token_runs() {
        let source = "// a comment\nfn f() {} /* and\nanother */";
        let runs = highlight(source);
        let comment_runs: Vec<&Run> = runs.iter().filter(|r| r.kind == Kind::Comment).collect();
        assert_eq!(comment_runs.len(), 2);
        assert_eq!(&source[comment_runs[0].range.clone()], "// a comment",);
        assert_eq!(&source[comment_runs[1].range.clone()], "/* and\nanother */",);
    }

    #[test]
    fn runs_are_sorted_and_never_overlap() {
        let source = "@group(0) @binding(0) var<uniform> t: f32; // trailing\nfn f() -> vec2f {}";
        let runs = highlight(source);
        for window in runs.windows(2) {
            assert!(window[0].range.start <= window[1].range.start);
            assert!(window[0].range.end <= window[1].range.start);
        }
    }

    #[test]
    fn a_lexer_error_yields_no_runs_instead_of_panicking() {
        // A backtick is not valid anywhere in WGSL/WXSL, so the lexer
        // rejects the whole source; the panel should just show it
        // uncoloured rather than the highlighter failing loudly.
        assert!(highlight("let x = `y`;").is_empty());
    }

    #[test]
    fn wxsl_only_syntax_highlights_the_same_way_as_wgsl() {
        // `import` and a generic parameter list are WXSL, not WGSL, but
        // they run through the same lexer and the same classification.
        let source = "import package::math::remap;\nfn f<T: f32 | vec2f>(a: T) -> T { a }";
        assert_eq!(kinds_at(source, "import"), vec![Kind::Keyword]);
        assert_eq!(kinds_at(source, "f32"), vec![Kind::Type]);
        assert_eq!(kinds_at(source, "vec2f"), vec![Kind::Type]);
    }
}
