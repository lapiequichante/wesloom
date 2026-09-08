//! Diagnostics: what went wrong, where, and how it renders.
//!
//! Owning the language means owning the error messages
//! ([ADR 0011](../../../docs/adr/0011-own-the-shading-language.md)). The
//! bar to clear is the one the WXSL compiler set: a message, the offending
//! line with a caret under it, and the module it came from — because a
//! shader author reading a failure is usually looking at a generated
//! material module that imported the thing that actually broke.

use core::fmt;

use crate::span::Span;

/// How bad it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Compilation cannot continue.
    Error,
    /// Compilation continues; the output may not be what was meant.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        })
    }
}

/// An extra span worth pointing at, with a reason.
///
/// The second half of a duplicate definition, the declaration a call did not
/// match, the template constraint an instantiation violated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// Where.
    pub span: Span,
    /// The module the span belongs to, if not the diagnostic's own.
    pub module: Option<String>,
    /// Why this place matters.
    pub message: String,
}

/// One problem found while compiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// How bad.
    pub severity: Severity,
    /// One line, lower case, no trailing period — the summary.
    pub message: String,
    /// The primary location.
    pub span: Span,
    /// Module path the primary span belongs to.
    pub module: Option<String>,
    /// Secondary locations.
    pub labels: Vec<Label>,
    /// Advice, or context that is not a location.
    pub notes: Vec<String>,
}

impl Diagnostic {
    /// An error at `span`.
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
            module: None,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// A warning at `span`.
    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            message: message.into(),
            span,
            module: None,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Say which module the primary span is in.
    pub fn in_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Fill in the module for this diagnostic and any label that has none.
    ///
    /// Called as a diagnostic crosses a module boundary on its way out, so a
    /// pass that only knows spans does not have to thread a module name
    /// through every call.
    pub fn or_module(mut self, module: &str) -> Self {
        if self.module.is_none() {
            self.module = Some(module.to_string());
        }
        for label in &mut self.labels {
            if label.module.is_none() {
                label.module = Some(module.to_string());
            }
        }
        self
    }

    /// Point at another place that explains this one.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            module: None,
            message: message.into(),
        });
        self
    }

    /// Point at a place in a different module.
    pub fn with_label_in(
        mut self,
        span: Span,
        module: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        self.labels.push(Label {
            span,
            module: Some(module.into()),
            message: message.into(),
        });
        self
    }

    /// Add a note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Render with source context, given a way to look a module's source up.
    ///
    /// `source_of` is passed the module path and returns its text. A span
    /// whose module has no source still renders — as the message and the
    /// position — because a diagnostic must never be swallowed just because
    /// its file went missing.
    pub fn render(&self, source_of: &dyn Fn(&str) -> Option<String>) -> String {
        let mut out = String::new();
        let module = self.module.as_deref().unwrap_or("<unknown>");
        // Look the source up even when no module is set: a diagnostic from
        // the lexer or parser has no module (only the driver knows the path),
        // and dropping its snippet would lose the caret for every syntax
        // error.
        let source = source_of(self.module.as_deref().unwrap_or(""));

        match &source {
            Some(text) => {
                let (line, column) = self.span.line_column(text);
                out.push_str(&format!(
                    "{}: {}\n  --> {module}:{line}:{column}\n",
                    self.severity, self.message
                ));
                push_snippet(&mut out, text, self.span);
            }
            None => {
                out.push_str(&format!(
                    "{}: {}\n  --> {module} (source unavailable, at {})\n",
                    self.severity, self.message, self.span
                ));
            }
        }

        for label in &self.labels {
            let label_module = label.module.as_deref().unwrap_or(module);
            match source_of(label_module) {
                Some(text) => {
                    let (line, column) = label.span.line_column(&text);
                    out.push_str(&format!(
                        "  note: {}\n  --> {label_module}:{line}:{column}\n",
                        label.message
                    ));
                    push_snippet(&mut out, &text, label.span);
                }
                None => {
                    out.push_str(&format!(
                        "  note: {} ({label_module}, source unavailable)\n",
                        label.message
                    ));
                }
            }
        }

        for note in &self.notes {
            out.push_str(&format!("  = {note}\n"));
        }
        out
    }
}

/// The offending line, and a caret run under the span.
fn push_snippet(out: &mut String, source: &str, span: Span) {
    let start = span.start.min(source.len() as u32) as usize;
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |index| start + index);
    let line = &source[line_start..line_end];
    let (number, _) = span.line_column(source);

    let gutter = number.to_string();
    out.push_str(&format!("{gutter} | {line}\n"));

    // Pad with the line's own leading characters so tabs line up, then a
    // caret per character of the span, clamped to the line.
    let pad: String = line[..start - line_start]
        .chars()
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let end = (span.end as usize).min(line_end);
    let width = source[start..end].chars().count().max(1);
    out.push_str(&format!(
        "{} | {pad}{}\n",
        " ".repeat(gutter.len()),
        "^".repeat(width)
    ));
}

/// Every problem found by one compilation.
///
/// A pass reports as many problems as it can rather than stopping at the
/// first, so a shader author fixes a batch per edit — the same reasoning as
/// `wxsl_core::error::GraphErrors`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// Record several.
    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(diagnostics);
    }

    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.severity == Severity::Error)
    }

    /// Every diagnostic, in the order found.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter()
    }

    /// How many.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing was reported.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Consume into the underlying list.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }

    /// Render all of them.
    pub fn render(&self, source_of: &dyn Fn(&str) -> Option<String>) -> String {
        self.items
            .iter()
            .map(|item| item.render(source_of))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl From<Diagnostic> for Diagnostics {
    fn from(diagnostic: Diagnostic) -> Self {
        Diagnostics {
            items: vec![diagnostic],
        }
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", item.severity, item.message)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_of(text: &'static str) -> impl Fn(&str) -> Option<String> {
        move |_| Some(text.to_string())
    }

    #[test]
    fn a_rendered_error_carries_the_line_and_a_caret() {
        let source = "fn a() -> f32 {\n    return nope;\n}\n";
        let start = source.find("nope").unwrap() as u32;
        let diagnostic = Diagnostic::error(
            "cannot find `nope`",
            Span::new(start, start + "nope".len() as u32),
        )
        .in_module("package::demo");

        let rendered = diagnostic.render(&source_of(source));
        assert!(rendered.contains("error: cannot find `nope`"), "{rendered}");
        assert!(rendered.contains("--> package::demo:2:12"), "{rendered}");
        assert!(rendered.contains("    return nope;"), "{rendered}");
        assert!(rendered.contains("^^^^"), "{rendered}");
    }

    #[test]
    fn a_missing_source_still_renders() {
        let diagnostic = Diagnostic::error("broken", Span::new(3, 7)).in_module("package::gone");
        let rendered = diagnostic.render(&|_| None);
        assert!(rendered.contains("error: broken"), "{rendered}");
        assert!(rendered.contains("package::gone"), "{rendered}");
        assert!(rendered.contains("3..7"), "{rendered}");
    }

    #[test]
    fn labels_can_point_into_another_module() {
        let diagnostic = Diagnostic::error("type mismatch", Span::new(0, 1))
            .in_module("package::a")
            .with_label_in(Span::new(0, 1), "package::b", "declared here")
            .with_note("templates are resolved from the call site");
        let rendered = diagnostic.render(&source_of("x"));
        assert!(rendered.contains("package::a"), "{rendered}");
        assert!(rendered.contains("package::b"), "{rendered}");
        assert!(rendered.contains("declared here"), "{rendered}");
        assert!(rendered.contains("= templates are resolved"), "{rendered}");
    }

    #[test]
    fn or_module_fills_in_only_what_is_missing() {
        let diagnostic = Diagnostic::error("x", Span::default())
            .with_label(Span::default(), "here")
            .with_label_in(Span::default(), "package::explicit", "there")
            .or_module("package::fallback");
        assert_eq!(diagnostic.module.as_deref(), Some("package::fallback"));
        assert_eq!(
            diagnostic.labels[0].module.as_deref(),
            Some("package::fallback")
        );
        assert_eq!(
            diagnostic.labels[1].module.as_deref(),
            Some("package::explicit")
        );
    }

    #[test]
    fn errors_and_warnings_are_distinguished() {
        let mut set = Diagnostics::new();
        assert!(!set.has_errors());
        set.push(Diagnostic::warning("unused", Span::default()));
        assert!(!set.has_errors());
        set.push(Diagnostic::error("broken", Span::default()));
        assert!(set.has_errors());
        assert_eq!(set.len(), 2);
    }
}
