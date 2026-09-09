//! Turning source text into tokens, and deciding what `<` and `>` mean.
//!
//! # Why this is not a regex
//!
//! WGSL's grammar is not context-free at the token level: `a < b` is a
//! comparison and `vec3<f32>` is a template list, and the difference cannot
//! be settled by lookahead of any fixed depth — `array<f32, N>` and
//! `a < b, c > (d)` differ only in what the names mean. WGSL specifies a
//! token-level pre-pass for this, and [`disambiguate`] implements it: scan
//! left to right, remember every `<` that *could* open a template, and let
//! later tokens confirm or kill the candidate.
//!
//! WXSL adds one case the WGSL algorithm gets wrong, because WGSL has no
//! such syntax: a declaration's generic list, `fn f<T: f32 | vec2f>(…)`.
//! There the `:` would clear the candidate stack. It does not need the
//! general algorithm at all, though — after `fn NAME` a `<` is unambiguously
//! a generic list — so the lexer tracks those regions explicitly and
//! suspends the clearing rules inside them.

use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::span::{Span, Spanned};
use crate::token::Tok;

/// Tokenize `source`, with `<`/`>` already classified.
///
/// Reports as many lexical errors as it can rather than stopping at the
/// first, so one edit can fix a batch.
pub fn tokenize(source: &str) -> Result<Vec<Spanned<Tok>>, Diagnostics> {
    let mut scanner = Scanner::new(source);
    let raw = scanner.scan();
    if scanner.diagnostics.has_errors() {
        return Err(scanner.diagnostics);
    }
    Ok(disambiguate(raw))
}

/// Tokenize without the `<`/`>` pass, for testing that pass in isolation.
#[cfg(test)]
fn tokenize_raw(source: &str) -> Result<Vec<Spanned<Tok>>, Diagnostics> {
    let mut scanner = Scanner::new(source);
    let raw = scanner.scan();
    if scanner.diagnostics.has_errors() {
        return Err(scanner.diagnostics);
    }
    Ok(raw)
}

/// Tokenize `source`, plus the span of every comment it skipped as trivia.
///
/// [`tokenize`] is what every compiler pass uses, and none of them need
/// comments — they are not part of the grammar. The editor's code-panel
/// highlighter is the one caller that wants them (to colour them rather
/// than lose them), so this exists alongside `tokenize` instead of changing
/// what every other caller gets.
pub fn tokenize_with_comments(source: &str) -> Result<(Vec<Spanned<Tok>>, Vec<Span>), Diagnostics> {
    let mut scanner = Scanner::new(source);
    let raw = scanner.scan();
    if scanner.diagnostics.has_errors() {
        return Err(scanner.diagnostics);
    }
    Ok((disambiguate(raw), scanner.comments))
}

struct Scanner<'a> {
    source: &'a str,
    bytes: &'a [u8],
    at: usize,
    diagnostics: Diagnostics,
    comments: Vec<Span>,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Scanner {
            source,
            bytes: source.as_bytes(),
            at: 0,
            diagnostics: Diagnostics::new(),
            comments: Vec::new(),
        }
    }

    fn scan(&mut self) -> Vec<Spanned<Tok>> {
        let mut out = Vec::new();
        while let Some(token) = self.next_token() {
            out.push(token);
        }
        out
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.at + offset).copied()
    }

    /// Consume whitespace and comments. Block comments nest, per WGSL.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(byte) if byte.is_ascii_whitespace() => self.at += 1,
                // Non-ASCII whitespace (a stray no-break space, say).
                Some(byte) if byte >= 0x80 => {
                    let rest = &self.source[self.at..];
                    match rest.chars().next() {
                        Some(c) if c.is_whitespace() => self.at += c.len_utf8(),
                        _ => return,
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    let start = self.at as u32;
                    while let Some(byte) = self.peek() {
                        if byte == b'\n' {
                            break;
                        }
                        self.at += 1;
                    }
                    self.comments.push(Span::new(start, self.at as u32));
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => self.skip_block_comment(),
                _ => return,
            }
        }
    }

    fn skip_block_comment(&mut self) {
        let start = self.at as u32;
        self.at += 2;
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                None => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "unterminated block comment",
                            Span::new(start, self.at as u32),
                        )
                        .with_note("block comments nest, so every `/*` needs its own `*/`"),
                    );
                    return;
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    depth += 1;
                    self.at += 2;
                }
                Some(b'*') if self.peek_at(1) == Some(b'/') => {
                    depth -= 1;
                    self.at += 2;
                }
                Some(_) => {
                    // Step by characters, not bytes, to stay on boundaries.
                    let rest = &self.source[self.at..];
                    self.at += rest.chars().next().map_or(1, char::len_utf8);
                }
            }
        }
        self.comments.push(Span::new(start, self.at as u32));
    }

    fn next_token(&mut self) -> Option<Spanned<Tok>> {
        self.skip_trivia();
        let start = self.at;
        let byte = self.peek()?;

        let token = match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => return Some(self.word(start)),
            b'0'..=b'9' => return Some(self.number(start)),
            b'.' if self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) => {
                return Some(self.number(start))
            }
            b'@' => return Some(self.attribute(start)),
            _ => match self.punctuation() {
                Some(token) => token,
                None => {
                    let rest = &self.source[self.at..];
                    let character = rest.chars().next().unwrap_or('\u{fffd}');
                    self.at += character.len_utf8();
                    self.diagnostics.push(Diagnostic::error(
                        format!("unexpected character `{character}`"),
                        Span::new(start as u32, self.at as u32),
                    ));
                    return self.next_token();
                }
            },
        };
        Some(Spanned::new(token, Span::new(start as u32, self.at as u32)))
    }

    /// An identifier or a keyword. `_` alone is the phony assignment target.
    fn word(&mut self, start: usize) -> Spanned<Tok> {
        while let Some(byte) = self.peek() {
            match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => self.at += 1,
                _ => break,
            }
        }
        let text = &self.source[start..self.at];
        let token = if text == "_" {
            Tok::Underscore
        } else {
            Tok::keyword(text).unwrap_or_else(|| Tok::Ident(text.to_string()))
        };
        Spanned::new(token, Span::new(start as u32, self.at as u32))
    }

    /// `@name`. The name is required.
    fn attribute(&mut self, start: usize) -> Spanned<Tok> {
        self.at += 1; // '@'
        let name_start = self.at;
        while let Some(byte) = self.peek() {
            match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => self.at += 1,
                _ => break,
            }
        }
        let span = Span::new(start as u32, self.at as u32);
        if name_start == self.at {
            self.diagnostics.push(
                Diagnostic::error("`@` must be followed by an attribute name", span)
                    .with_note("for example `@fragment`, `@group(0)` or `@if(FEATURE)`"),
            );
        }
        let name = self.source[name_start..self.at].to_string();
        Spanned::new(Tok::Attr(name), span)
    }

    /// A numeric literal, kept as source text.
    ///
    /// The token is a float if it has a fraction, an exponent, or an `f`/`h`
    /// suffix; otherwise an integer. Malformed literals are reported but
    /// still produce a token, so one typo does not cascade.
    fn number(&mut self, start: usize) -> Spanned<Tok> {
        let hex = self.peek() == Some(b'0')
            && matches!(self.peek_at(1), Some(b'x') | Some(b'X'))
            && self
                .peek_at(2)
                .is_some_and(|b| b.is_ascii_hexdigit() || b == b'.');
        let mut is_float = false;

        if hex {
            self.at += 2;
            while self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                self.at += 1;
            }
            if self.peek() == Some(b'.') {
                is_float = true;
                self.at += 1;
                while self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                    self.at += 1;
                }
            }
            if matches!(self.peek(), Some(b'p') | Some(b'P')) {
                is_float = true;
                self.at += 1;
                if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                    self.at += 1;
                }
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.at += 1;
                }
            }
        } else {
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.at += 1;
            }
            // A `.` is part of the number only when it is not member access:
            // `1.0`, `1.` and `.5` are numbers, the `.` in `v.x` is not, and
            // we only get here having started on a digit or on `.digit`.
            if self.peek() == Some(b'.') {
                is_float = true;
                self.at += 1;
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.at += 1;
                }
            }
            if matches!(self.peek(), Some(b'e') | Some(b'E')) {
                let sign = matches!(self.peek_at(1), Some(b'+') | Some(b'-'));
                let digit_at = if sign { 2 } else { 1 };
                if self.peek_at(digit_at).is_some_and(|b| b.is_ascii_digit()) {
                    is_float = true;
                    self.at += digit_at;
                    while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                        self.at += 1;
                    }
                }
            }
        }

        // Suffix. `f`/`h` force a float; `i`/`u` force an integer.
        match self.peek() {
            Some(b'f') | Some(b'h') => {
                is_float = true;
                self.at += 1;
            }
            Some(b'i') | Some(b'u') => {
                if is_float {
                    self.diagnostics.push(Diagnostic::error(
                        "integer suffix on a floating-point literal",
                        Span::new(start as u32, self.at as u32 + 1),
                    ));
                }
                self.at += 1;
            }
            _ => {}
        }

        // Anything word-like still attached is a typo, not a new token.
        if self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            let bad_start = self.at;
            while self
                .peek()
                .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                self.at += 1;
            }
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "invalid suffix `{}` on a numeric literal",
                    &self.source[bad_start..self.at]
                ),
                Span::new(start as u32, self.at as u32),
            ));
        }

        let text = self.source[start..self.at].to_string();
        let token = if is_float {
            Tok::FloatLit(text)
        } else {
            Tok::IntLit(text)
        };
        Spanned::new(token, Span::new(start as u32, self.at as u32))
    }

    /// Longest-match punctuation. `<`/`>` come out as comparison operators
    /// here; [`disambiguate`] reclassifies the ones that are templates.
    fn punctuation(&mut self) -> Option<Tok> {
        let rest = &self.source[self.at..];
        const OPERATORS: &[(&str, Tok)] = &[
            (">>=", Tok::ShrEq),
            ("<<=", Tok::ShlEq),
            ("->", Tok::Arrow),
            ("==", Tok::EqEq),
            ("!=", Tok::NotEq),
            ("<=", Tok::Le),
            (">=", Tok::Ge),
            ("&&", Tok::AndAnd),
            ("||", Tok::OrOr),
            ("<<", Tok::Shl),
            (">>", Tok::Shr),
            ("+=", Tok::PlusEq),
            ("-=", Tok::MinusEq),
            ("*=", Tok::StarEq),
            ("/=", Tok::SlashEq),
            ("%=", Tok::PercentEq),
            ("&=", Tok::AndEq),
            ("|=", Tok::OrEq),
            ("^=", Tok::CaretEq),
            ("++", Tok::PlusPlus),
            ("--", Tok::MinusMinus),
            ("::", Tok::ColonColon),
            ("(", Tok::ParenOpen),
            (")", Tok::ParenClose),
            ("{", Tok::BraceOpen),
            ("}", Tok::BraceClose),
            ("[", Tok::BracketOpen),
            ("]", Tok::BracketClose),
            (",", Tok::Comma),
            (";", Tok::Semi),
            (":", Tok::Colon),
            (".", Tok::Dot),
            ("=", Tok::Eq),
            ("<", Tok::Lt),
            (">", Tok::Gt),
            ("+", Tok::Plus),
            ("-", Tok::Minus),
            ("*", Tok::Star),
            ("/", Tok::Slash),
            ("%", Tok::Percent),
            ("!", Tok::Not),
            ("~", Tok::Tilde),
            ("&", Tok::And),
            ("|", Tok::Or),
            ("^", Tok::Caret),
        ];
        for (text, token) in OPERATORS {
            if rest.starts_with(text) {
                self.at += text.len();
                return Some(token.clone());
            }
        }
        None
    }
}

/// Reclassify `<` and `>` as template delimiters where they are ones.
///
/// The general algorithm, per WGSL: an identifier followed by `<` starts a
/// *candidate* template list, recorded with the parenthesis depth it began
/// at. A `>` at the same depth confirms the nearest candidate. Anything that
/// cannot appear inside a template list kills the candidates it encloses —
/// a closing paren or bracket, a short-circuit operator — and anything that
/// ends an expression clears them all.
///
/// `>>` and `>>=` may each close *two* nested lists (`ptr<function,
/// array<f32, 2>>`), so they are split rather than matched whole.
pub fn disambiguate(tokens: Vec<Spanned<Tok>>) -> Vec<Spanned<Tok>> {
    let mut out: Vec<Spanned<Tok>> = Vec::with_capacity(tokens.len());
    // (paren depth, index in `out` of the `<` this candidate refers to)
    let mut pending: Vec<(i32, usize)> = Vec::new();
    let mut depth: i32 = 0;
    // Depth of nested generic lists in a declaration (`fn f<T: …>`), where
    // the general rules are suspended: after `fn NAME`, `<` cannot be a
    // comparison, and a `:` inside must not clear anything.
    let mut declaration: u32 = 0;

    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        index += 1;

        if declaration > 0 {
            match &token.node {
                Tok::Lt => {
                    declaration += 1;
                    out.push(Spanned::new(Tok::TemplateOpen, token.span));
                }
                Tok::Gt => {
                    declaration -= 1;
                    out.push(Spanned::new(Tok::TemplateClose, token.span));
                }
                Tok::Shr => {
                    // Two closes, or one close and a stray `>`.
                    let (first, second) = split_span(token.span);
                    out.push(Spanned::new(Tok::TemplateClose, first));
                    declaration -= 1;
                    if declaration > 0 {
                        declaration -= 1;
                        out.push(Spanned::new(Tok::TemplateClose, second));
                    } else {
                        out.push(Spanned::new(Tok::Gt, second));
                    }
                }
                Tok::Ge => {
                    let (first, second) = split_span(token.span);
                    declaration -= 1;
                    out.push(Spanned::new(Tok::TemplateClose, first));
                    out.push(Spanned::new(Tok::Eq, second));
                }
                // A generic list cannot span a statement, so an unbalanced
                // one stops being treated as one rather than eating the file.
                Tok::Semi | Tok::BraceOpen => {
                    declaration = 0;
                    out.push(token);
                }
                _ => out.push(token),
            }
            continue;
        }

        match &token.node {
            // `fn NAME <` and `struct NAME <` open a declaration's generics.
            Tok::Lt if starts_declaration_generics(&out) => {
                declaration = 1;
                out.push(Spanned::new(Tok::TemplateOpen, token.span));
            }
            Tok::Lt if out.last().is_some_and(|t| t.node.can_precede_template()) => {
                pending.push((depth, out.len()));
                out.push(token);
            }
            Tok::Gt => match pending.last() {
                Some(&(candidate_depth, at)) if candidate_depth == depth => {
                    pending.pop();
                    out[at].node = Tok::TemplateOpen;
                    out.push(Spanned::new(Tok::TemplateClose, token.span));
                }
                _ => out.push(token),
            },
            Tok::Ge => match pending.last() {
                Some(&(candidate_depth, at)) if candidate_depth == depth => {
                    pending.pop();
                    out[at].node = Tok::TemplateOpen;
                    let (first, second) = split_span(token.span);
                    out.push(Spanned::new(Tok::TemplateClose, first));
                    out.push(Spanned::new(Tok::Eq, second));
                }
                _ => out.push(token),
            },
            Tok::Shr => {
                let closes = pending
                    .iter()
                    .rev()
                    .take(2)
                    .take_while(|(candidate_depth, _)| *candidate_depth == depth)
                    .count();
                let (first, second) = split_span(token.span);
                match closes {
                    2 => {
                        for span in [first, second] {
                            let (_, at) = pending.pop().expect("counted above");
                            out[at].node = Tok::TemplateOpen;
                            out.push(Spanned::new(Tok::TemplateClose, span));
                        }
                    }
                    1 => {
                        let (_, at) = pending.pop().expect("counted above");
                        out[at].node = Tok::TemplateOpen;
                        out.push(Spanned::new(Tok::TemplateClose, first));
                        out.push(Spanned::new(Tok::Gt, second));
                    }
                    _ => out.push(token),
                }
            }
            Tok::ParenOpen | Tok::BracketOpen => {
                depth += 1;
                out.push(token);
            }
            Tok::ParenClose | Tok::BracketClose => {
                // Candidates opened inside the group being closed can never
                // be confirmed.
                while pending.last().is_some_and(|(d, _)| *d >= depth) {
                    pending.pop();
                }
                depth = depth.saturating_sub(1);
                out.push(token);
            }
            // Lower precedence than comparison, so they cannot occur inside
            // a template list: any candidate they enclose is dead.
            Tok::AndAnd | Tok::OrOr => {
                while pending.last().is_some_and(|(d, _)| *d >= depth) {
                    pending.pop();
                }
                out.push(token);
            }
            // End of an expression: nothing outstanding can still be a
            // template.
            Tok::Semi | Tok::BraceOpen | Tok::BraceClose => {
                pending.clear();
                depth = 0;
                out.push(token);
            }
            _ => out.push(token),
        }
    }
    out
}

/// Whether the tokens emitted so far end in `fn NAME` or `struct NAME`.
fn starts_declaration_generics(out: &[Spanned<Tok>]) -> bool {
    let mut tail = out.iter().rev();
    let name = tail.next();
    let keyword = tail.next();
    matches!(name.map(|t| &t.node), Some(Tok::Ident(_)))
        && matches!(keyword.map(|t| &t.node), Some(Tok::Fn) | Some(Tok::Struct))
}

/// Split a two-byte operator's span into its halves.
fn split_span(span: Span) -> (Span, Span) {
    let middle = span.start + 1;
    (Span::new(span.start, middle), Span::new(middle, span.end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<Tok> {
        tokenize(source)
            .expect("lexes")
            .into_iter()
            .map(|t| t.node)
            .collect()
    }

    fn count(source: &str, wanted: &Tok) -> usize {
        kinds(source).iter().filter(|t| *t == wanted).count()
    }

    #[test]
    fn words_numbers_and_attributes() {
        assert_eq!(
            kinds("fn a(@location(0) x: f32) -> f32 { return x; }"),
            vec![
                Tok::Fn,
                Tok::Ident("a".into()),
                Tok::ParenOpen,
                Tok::Attr("location".into()),
                Tok::ParenOpen,
                Tok::IntLit("0".into()),
                Tok::ParenClose,
                Tok::Ident("x".into()),
                Tok::Colon,
                Tok::Ident("f32".into()),
                Tok::ParenClose,
                Tok::Arrow,
                Tok::Ident("f32".into()),
                Tok::BraceOpen,
                Tok::Return,
                Tok::Ident("x".into()),
                Tok::Semi,
                Tok::BraceClose,
            ]
        );
    }

    #[test]
    fn literals_keep_their_exact_spelling() {
        // The whole reason literals are strings: re-emitting a parsed f32
        // would change these.
        let tokens = kinds("0.0031308 1e-8 1.0 5. .5 0x7fu 4i 2.5e1 0.5h 3u");
        let texts: Vec<String> = tokens
            .iter()
            .map(|t| match t {
                Tok::FloatLit(text) | Tok::IntLit(text) => text.clone(),
                other => panic!("not a literal: {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "0.0031308",
                "1e-8",
                "1.0",
                "5.",
                ".5",
                "0x7fu",
                "4i",
                "2.5e1",
                "0.5h",
                "3u"
            ]
        );
        assert!(matches!(tokens[0], Tok::FloatLit(_)));
        assert!(matches!(tokens[1], Tok::FloatLit(_)));
        assert!(matches!(tokens[5], Tok::IntLit(_)));
        assert!(matches!(tokens[6], Tok::IntLit(_)));
        assert!(matches!(tokens[8], Tok::FloatLit(_)));
    }

    #[test]
    fn a_dot_after_a_number_is_a_fraction_but_member_access_is_not() {
        assert_eq!(
            kinds("v.x"),
            vec![Tok::Ident("v".into()), Tok::Dot, Tok::Ident("x".into())]
        );
        assert_eq!(kinds("1.0"), vec![Tok::FloatLit("1.0".into())]);
    }

    #[test]
    fn block_comments_nest() {
        assert_eq!(
            kinds("/* outer /* inner */ still */ fn"),
            vec![Tok::Fn],
            "a naive scanner stops at the first `*/`"
        );
        let error = tokenize("/* /* */ fn a() {}").expect_err("unterminated");
        assert!(error.to_string().contains("unterminated block comment"));
    }

    #[test]
    fn line_comments_and_unicode_are_skipped() {
        assert_eq!(kinds("// héllo\nfn"), vec![Tok::Fn]);
        assert_eq!(kinds("fn\u{a0}a"), vec![Tok::Fn, Tok::Ident("a".into())]);
    }

    // --- template disambiguation ---------------------------------------

    #[test]
    fn a_type_template_is_a_template() {
        assert_eq!(
            kinds("array<Light, 4>"),
            vec![
                Tok::Ident("array".into()),
                Tok::TemplateOpen,
                Tok::Ident("Light".into()),
                Tok::Comma,
                Tok::IntLit("4".into()),
                Tok::TemplateClose,
            ]
        );
        assert_eq!(count("var<uniform> x: vec3<f32>;", &Tok::TemplateOpen), 2);
    }

    #[test]
    fn a_comparison_is_a_comparison() {
        assert_eq!(
            kinds("a < b"),
            vec![Tok::Ident("a".into()), Tok::Lt, Tok::Ident("b".into())]
        );
        // The paren kills the candidate opened inside it.
        assert_eq!(count("if (a < b) { }", &Tok::Lt), 1);
        assert_eq!(count("if (a < b) { }", &Tok::TemplateOpen), 0);
        // `>=` after a call is a comparison, not a template close.
        assert_eq!(count("abs(span) >= T(1e-8)", &Tok::Ge), 1);
        assert_eq!(count("abs(span) >= T(1e-8)", &Tok::TemplateClose), 0);
    }

    #[test]
    fn a_short_circuit_operator_kills_a_candidate() {
        // Without the `&&` rule this reads as one template list.
        let tokens = kinds("a < b && c > d");
        assert_eq!(tokens.iter().filter(|t| **t == Tok::Lt).count(), 1);
        assert_eq!(tokens.iter().filter(|t| **t == Tok::Gt).count(), 1);
        assert!(!tokens.contains(&Tok::TemplateOpen));
    }

    #[test]
    fn a_statement_boundary_clears_candidates() {
        // The `<` in the for-header's condition is a comparison.
        let tokens = kinds("for (var i = 0; i < n; i = i + 1) { }");
        assert!(tokens.contains(&Tok::Lt));
        assert!(!tokens.contains(&Tok::TemplateOpen));
    }

    #[test]
    fn shift_right_splits_into_two_template_closes() {
        let tokens = kinds("ptr<function, array<f32, 2>>");
        assert_eq!(
            tokens.iter().filter(|t| **t == Tok::TemplateOpen).count(),
            2
        );
        assert_eq!(
            tokens.iter().filter(|t| **t == Tok::TemplateClose).count(),
            2
        );
        assert!(!tokens.contains(&Tok::Shr));
        // But a genuine shift stays a shift.
        assert_eq!(count("x = a >> 2;", &Tok::Shr), 1);
    }

    #[test]
    fn split_closes_get_one_byte_spans_each() {
        let tokens = tokenize("array<array<f32, 2>, 3>").expect("lexes");
        let closes: Vec<Span> = tokens
            .iter()
            .filter(|t| t.node == Tok::TemplateClose)
            .map(|t| t.span)
            .collect();
        assert_eq!(closes.len(), 2);
        // Inner close is the standalone `>` before the comma; the outer one
        // is the final `>`. Both are one byte, and distinct.
        assert!(closes.iter().all(|span| span.len() == 1));
        assert_ne!(closes[0], closes[1]);
    }

    #[test]
    fn declaration_generics_survive_a_colon() {
        // The general algorithm clears candidates at `:`, which would make
        // this `>` a comparison. Declaration lists are tracked separately.
        assert_eq!(
            kinds("fn f<T: f32 | vec2f>(a: T) -> T { }"),
            vec![
                Tok::Fn,
                Tok::Ident("f".into()),
                Tok::TemplateOpen,
                Tok::Ident("T".into()),
                Tok::Colon,
                Tok::Ident("f32".into()),
                Tok::Or,
                Tok::Ident("vec2f".into()),
                Tok::TemplateClose,
                Tok::ParenOpen,
                Tok::Ident("a".into()),
                Tok::Colon,
                Tok::Ident("T".into()),
                Tok::ParenClose,
                Tok::Arrow,
                Tok::Ident("T".into()),
                Tok::BraceOpen,
                Tok::BraceClose,
            ]
        );
        assert_eq!(count("struct S<T: f32> { x: T }", &Tok::TemplateOpen), 1);
    }

    #[test]
    fn an_unbalanced_declaration_list_does_not_eat_the_file() {
        // Missing `>`: the `{` ends the region rather than swallowing every
        // later token as generics.
        let tokens = kinds("fn f<T: f32 { }\nfn g() { }");
        assert_eq!(tokens.iter().filter(|t| **t == Tok::Fn).count(), 2);
    }

    #[test]
    fn the_raw_pass_leaves_angles_alone() {
        let raw: Vec<Tok> = tokenize_raw("array<f32, 4>")
            .expect("lexes")
            .into_iter()
            .map(|t| t.node)
            .collect();
        assert!(raw.contains(&Tok::Lt));
        assert!(raw.contains(&Tok::Gt));
        assert!(!raw.contains(&Tok::TemplateOpen));
    }

    #[test]
    fn bad_characters_and_suffixes_are_reported_with_spans() {
        let error = tokenize("fn a() { let x = 1.0zz; }").expect_err("bad suffix");
        assert!(error.to_string().contains("invalid suffix `zz`"), "{error}");

        let error = tokenize("let x = `y`;").expect_err("bad character");
        assert!(
            error.to_string().contains("unexpected character"),
            "{error}"
        );

        let error = tokenize("fn a(@ x: f32) {}").expect_err("bare @");
        assert!(error.to_string().contains("must be followed by"), "{error}");
    }

    #[test]
    fn spans_point_at_the_token() {
        let source = "fn hello() {}";
        let tokens = tokenize(source).expect("lexes");
        assert_eq!(tokens[1].span.text(source), Some("hello"));
        assert_eq!(tokens[1].span.line_column(source), (1, 4));
    }

    #[test]
    fn tokenize_with_comments_reports_line_and_block_comments_as_trivia_spans() {
        let source = "// a line comment\nfn a() { /* nested /* block */ comment */ }";
        let (tokens, comments) = tokenize_with_comments(source).expect("lexes");
        assert_eq!(tokens.iter().filter(|t| t.node == Tok::Fn).count(), 1);
        assert_eq!(comments.len(), 2);
        assert_eq!(
            comments[0].text(source),
            Some("// a line comment"),
            "the line comment stops before the newline"
        );
        assert_eq!(
            comments[1].text(source),
            Some("/* nested /* block */ comment */"),
            "nesting is tracked the same way `tokenize` skips it"
        );
    }

    #[test]
    fn tokenize_with_comments_fails_the_same_way_tokenize_does() {
        let error = tokenize_with_comments("fn a() { let x = `y`; }").expect_err("bad character");
        assert!(error.to_string().contains("unexpected character"));
    }
}
