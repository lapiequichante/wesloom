//! Source positions.
//!
//! Every diagnostic this crate produces has to name a place in a `.wxsl`
//! file, and it has to survive two rewrites on the way to WGSL: import
//! mangling and template instantiation ([ADR 0011](../../../docs/adr/0011-own-the-shading-language.md)).
//! So spans are attached to syntax nodes from the lexer onwards and carried
//! through, rather than being recovered afterwards.

use core::fmt;
use core::ops::Range;

/// A half-open byte range in one source file.
///
/// Byte offsets, not line/column: the lexer and the parser both work in byte
/// offsets, and turning one into a line and column needs the source text,
/// which only the diagnostic renderer has. See [`Span::line_column`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Span {
    /// Byte offset of the first byte.
    pub start: u32,
    /// Byte offset one past the last byte.
    pub end: u32,
}

impl Span {
    /// A span covering `start..end`.
    pub const fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    /// The empty span at `offset`, for pointing at a place rather than a
    /// range — end of file, or an insertion point.
    pub const fn at(offset: u32) -> Self {
        Span {
            start: offset,
            end: offset,
        }
    }

    /// The smallest span covering both.
    pub fn join(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Length in bytes.
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// This span as a range, for slicing the source text.
    pub fn range(&self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    /// The text this span covers, or `None` if it is out of bounds or does
    /// not land on character boundaries.
    pub fn text<'a>(&self, source: &'a str) -> Option<&'a str> {
        source.get(self.range())
    }

    /// One-based line and column of the span's start, counting columns in
    /// characters rather than bytes so that a caret lands under the right
    /// glyph for non-ASCII source.
    pub fn line_column(&self, source: &str) -> (usize, usize) {
        let upto = source.get(..self.start as usize).unwrap_or(source);
        let line = upto.matches('\n').count() + 1;
        let line_start = upto.rfind('\n').map_or(0, |index| index + 1);
        let column = source[line_start..self.start as usize].chars().count() + 1;
        (line, column)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Range<usize>> for Span {
    fn from(range: Range<usize>) -> Self {
        Span::new(range.start as u32, range.end as u32)
    }
}

/// A value with the span it was parsed from.
///
/// `PartialEq` deliberately ignores the span: two identical declarations from
/// different files are equal as syntax, which is what template instantiation
/// and deduplication compare.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spanned<T> {
    /// The value.
    pub node: T,
    /// Where it came from.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Attach `span` to `node`.
    pub const fn new(node: T, span: Span) -> Self {
        Spanned { node, span }
    }

    /// Map the value, keeping the span.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            node: f(self.node),
            span: self.span,
        }
    }

    /// Borrow the value with its span.
    pub fn as_ref(&self) -> Spanned<&T> {
        Spanned {
            node: &self.node,
            span: self.span,
        }
    }
}

impl<T: PartialEq> PartialEq for Spanned<T> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T: Eq> Eq for Spanned<T> {}

impl<T> core::ops::Deref for Spanned<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.node
    }
}

impl<T: fmt::Display> fmt::Display for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.node.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column_are_one_based() {
        let source = "fn a() {}\nfn b() {}\n";
        assert_eq!(Span::at(0).line_column(source), (1, 1));
        assert_eq!(Span::at(3).line_column(source), (1, 4));
        // First byte of the second line.
        assert_eq!(Span::at(10).line_column(source), (2, 1));
        assert_eq!(Span::at(13).line_column(source), (2, 4));
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        // Two-byte characters in a comment before the token.
        let source = "// ééé\nfn a() {}";
        let offset = source.find("fn").unwrap() as u32;
        assert_eq!(Span::at(offset).line_column(source), (2, 1));
        let inside = source.find('é').unwrap() as u32;
        assert_eq!(Span::at(inside).line_column(source), (1, 4));
    }

    #[test]
    fn join_covers_both_and_text_slices() {
        let source = "alias T = f32;";
        let a = Span::new(0, 5);
        let b = Span::new(10, 13);
        assert_eq!(a.join(b), Span::new(0, 13));
        assert_eq!(a.text(source), Some("alias"));
        assert_eq!(b.text(source), Some("f32"));
        assert_eq!(Span::new(0, 99).text(source), None);
    }

    #[test]
    fn spans_do_not_affect_equality() {
        let a = Spanned::new("x", Span::new(0, 1));
        let b = Spanned::new("x", Span::new(50, 51));
        assert_eq!(a, b);
        assert!(a.span != b.span);
    }
}
