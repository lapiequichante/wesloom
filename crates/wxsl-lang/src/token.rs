//! The token alphabet.
//!
//! One entry per terminal the grammar can see. Two decisions worth knowing:
//!
//! * Numeric literals keep their **source text** rather than a parsed value.
//!   A shader is full of constants whose exact spelling matters
//!   (`0.0031308`, `1e-8`, `0x7fu`), and re-emitting a parsed `f32` would
//!   quietly change them. Values are parsed later, only where a pass needs
//!   to compute with one.
//! * `<` and `>` arrive from the lexer already classified as either
//!   comparison operators or template delimiters
//!   ([`Tok::TemplateOpen`]/[`Tok::TemplateClose`]), because deciding that
//!   needs a token-level pre-pass rather than grammar lookahead. See
//!   [`crate::lexer`].

use core::fmt;

/// A lexical token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    // --- names and literals -------------------------------------------
    /// An identifier.
    Ident(String),
    /// An integer literal, as written, including any `i`/`u` suffix.
    IntLit(String),
    /// A floating-point literal, as written, including any `f`/`h` suffix.
    FloatLit(String),
    /// `true`.
    True,
    /// `false`.
    False,

    // --- attributes ---------------------------------------------------
    /// `@` followed by a name: `@fragment`, `@group`, `@if`, `@template`.
    Attr(String),

    // --- keywords -----------------------------------------------------
    /// `alias`
    Alias,
    /// `break`
    Break,
    /// `case`
    Case,
    /// `const`
    Const,
    /// `const_assert`
    ConstAssert,
    /// `continue`
    Continue,
    /// `continuing`
    Continuing,
    /// `default`
    Default,
    /// `discard`
    Discard,
    /// `else`
    Else,
    /// `enable`
    Enable,
    /// `fn`
    Fn,
    /// `for`
    For,
    /// `if`
    If,
    /// `import` — WXSL's, not WGSL's.
    Import,
    /// `let`
    Let,
    /// `loop`
    Loop,
    /// `override`
    Override,
    /// `requires`
    Requires,
    /// `return`
    Return,
    /// `struct`
    Struct,
    /// `switch`
    Switch,
    /// `var`
    Var,
    /// `while`
    While,
    /// `as` — import renaming.
    As,

    // --- delimiters ---------------------------------------------------
    /// `(`
    ParenOpen,
    /// `)`
    ParenClose,
    /// `{`
    BraceOpen,
    /// `}`
    BraceClose,
    /// `[`
    BracketOpen,
    /// `]`
    BracketClose,
    /// A `<` that opens a template list.
    TemplateOpen,
    /// A `>` that closes a template list.
    TemplateClose,

    // --- punctuation --------------------------------------------------
    /// `,`
    Comma,
    /// `;`
    Semi,
    /// `:`
    Colon,
    /// `::`
    ColonColon,
    /// `.`
    Dot,
    /// `->`
    Arrow,
    /// `_`
    Underscore,

    // --- operators ----------------------------------------------------
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    NotEq,
    /// `<` as less-than.
    Lt,
    /// `<=`
    Le,
    /// `>` as greater-than.
    Gt,
    /// `>=`
    Ge,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `!`
    Not,
    /// `~`
    Tilde,
    /// `&`
    And,
    /// `&&`
    AndAnd,
    /// `|`
    Or,
    /// `||`
    OrOr,
    /// `^`
    Caret,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,
    /// `&=`
    AndEq,
    /// `|=`
    OrEq,
    /// `^=`
    CaretEq,
    /// `<<=`
    ShlEq,
    /// `>>=`
    ShrEq,
    /// `++`
    PlusPlus,
    /// `--`
    MinusMinus,
}

impl Tok {
    /// The keyword for `text`, or `None` if it is an ordinary identifier.
    pub fn keyword(text: &str) -> Option<Tok> {
        Some(match text {
            "alias" => Tok::Alias,
            "as" => Tok::As,
            "break" => Tok::Break,
            "case" => Tok::Case,
            "const" => Tok::Const,
            "const_assert" => Tok::ConstAssert,
            "continue" => Tok::Continue,
            "continuing" => Tok::Continuing,
            "default" => Tok::Default,
            "discard" => Tok::Discard,
            "else" => Tok::Else,
            "enable" => Tok::Enable,
            "false" => Tok::False,
            "fn" => Tok::Fn,
            "for" => Tok::For,
            "if" => Tok::If,
            "import" => Tok::Import,
            "let" => Tok::Let,
            "loop" => Tok::Loop,
            "override" => Tok::Override,
            "requires" => Tok::Requires,
            "return" => Tok::Return,
            "struct" => Tok::Struct,
            "switch" => Tok::Switch,
            "true" => Tok::True,
            "var" => Tok::Var,
            "while" => Tok::While,
            _ => return None,
        })
    }

    /// Whether this token can be followed by a template list.
    ///
    /// Only an identifier can: `array<f32, 4>`, `vec3<f32>`, `var<uniform>`
    /// — `var` reaches here as a keyword, so it is listed too — and a call
    /// to a templated function. Used by the disambiguation pass, which runs
    /// before any name is resolved and so cannot ask what the identifier
    /// means.
    pub fn can_precede_template(&self) -> bool {
        matches!(self, Tok::Ident(_) | Tok::Var)
    }
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Tok::Ident(name) => return write!(f, "`{name}`"),
            Tok::IntLit(text) | Tok::FloatLit(text) => return write!(f, "`{text}`"),
            Tok::Attr(name) => return write!(f, "`@{name}`"),
            Tok::True => "true",
            Tok::False => "false",
            Tok::Alias => "alias",
            Tok::Break => "break",
            Tok::Case => "case",
            Tok::Const => "const",
            Tok::ConstAssert => "const_assert",
            Tok::Continue => "continue",
            Tok::Continuing => "continuing",
            Tok::Default => "default",
            Tok::Discard => "discard",
            Tok::Else => "else",
            Tok::Enable => "enable",
            Tok::Fn => "fn",
            Tok::For => "for",
            Tok::If => "if",
            Tok::Import => "import",
            Tok::Let => "let",
            Tok::Loop => "loop",
            Tok::Override => "override",
            Tok::Requires => "requires",
            Tok::Return => "return",
            Tok::Struct => "struct",
            Tok::Switch => "switch",
            Tok::Var => "var",
            Tok::While => "while",
            Tok::As => "as",
            Tok::ParenOpen => "(",
            Tok::ParenClose => ")",
            Tok::BraceOpen => "{",
            Tok::BraceClose => "}",
            Tok::BracketOpen => "[",
            Tok::BracketClose => "]",
            Tok::TemplateOpen => "< (template)",
            Tok::TemplateClose => "> (template)",
            Tok::Comma => ",",
            Tok::Semi => ";",
            Tok::Colon => ":",
            Tok::ColonColon => "::",
            Tok::Dot => ".",
            Tok::Arrow => "->",
            Tok::Underscore => "_",
            Tok::Eq => "=",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Not => "!",
            Tok::Tilde => "~",
            Tok::And => "&",
            Tok::AndAnd => "&&",
            Tok::Or => "|",
            Tok::OrOr => "||",
            Tok::Caret => "^",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::PlusEq => "+=",
            Tok::MinusEq => "-=",
            Tok::StarEq => "*=",
            Tok::SlashEq => "/=",
            Tok::PercentEq => "%=",
            Tok::AndEq => "&=",
            Tok::OrEq => "|=",
            Tok::CaretEq => "^=",
            Tok::ShlEq => "<<=",
            Tok::ShrEq => ">>=",
            Tok::PlusPlus => "++",
            Tok::MinusMinus => "--",
        };
        write!(f, "`{text}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_recognized_and_identifiers_are_not() {
        assert_eq!(Tok::keyword("fn"), Some(Tok::Fn));
        assert_eq!(Tok::keyword("import"), Some(Tok::Import));
        assert_eq!(Tok::keyword("true"), Some(Tok::True));
        assert_eq!(Tok::keyword("inverse_lerp"), None);
        // Not keywords in WGSL, and must stay usable as names.
        assert_eq!(Tok::keyword("f32"), None);
        assert_eq!(Tok::keyword("array"), None);
        assert_eq!(Tok::keyword("select"), None);
    }

    #[test]
    fn only_identifiers_and_var_can_precede_a_template() {
        assert!(Tok::Ident("vec3".into()).can_precede_template());
        assert!(Tok::Var.can_precede_template());
        assert!(!Tok::IntLit("4".into()).can_precede_template());
        assert!(!Tok::ParenClose.can_precede_template());
        assert!(!Tok::Fn.can_precede_template());
    }

    #[test]
    fn display_is_quoted_for_error_messages() {
        assert_eq!(Tok::Ident("x".into()).to_string(), "`x`");
        assert_eq!(Tok::Arrow.to_string(), "`->`");
        assert_eq!(Tok::Attr("fragment".into()).to_string(), "`@fragment`");
    }
}
