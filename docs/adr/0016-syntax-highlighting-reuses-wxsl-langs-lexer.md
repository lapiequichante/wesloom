# 0016. The editor's code-panel syntax highlighting reuses `wxsl-lang`'s lexer

Date: 2026-09-09

Status: Accepted

## Context

The editor's inspector shows two read-only code panels: the WXSL a graph
compiled to, and the WGSL that WXSL compiled to (`preview.rs`, `app.rs`'s
`code_panel`). Both were drawn as plain monospaced text, one colour for
everything.

Colouring them means classifying source text — keywords, built-in type
names, numbers, attributes, comments — which means tokenizing it. WXSL is
WGSL plus templates, imports, conditional translation and macros (see ADR
0011), so a WGSL-only tokenizer would mis-highlight every WXSL-only
construct (`import`, a generic parameter list), and getting `<`/`>`
right at all needs the same disambiguation `wxsl-lang`'s lexer already does
(a type's template list vs. a comparison — see `wxsl-lang::lexer`'s module
doc). `wxsl-lang` is exactly this tokenizer already, built and tested for
the same language the two panels show.

## Decision

`wxsl-editor` depends on `wxsl-lang` and reuses its lexer rather than
writing a second, editor-local one. `wxsl-lang::lexer` gains
`tokenize_with_comments`, alongside the existing `tokenize`: the same
scan, plus the span of every comment it skips as trivia (comments are not
part of the grammar, so no other caller wants them — `tokenize` is
untouched, and every existing call site is unaffected).

A new `wxsl-editor::highlight` module runs `tokenize_with_comments` and
classifies the result into `Run`s (`Kind::{Keyword, Type, Number,
Attribute, Comment}`), one function for both panels — a WXSL keyword and a
WGSL one are the same keyword, coloured the same way, because the same code
tokenizes and classifies either text. Built-in type names (`f32`, `vec3f`,
`array`, `texture_2d`, …) are ordinary identifiers to the lexer (not
keywords — see `Tok::keyword`), so recognising them for the `Type` colour is
a fixed name list `highlight.rs` owns; everything else the classifier does
not recognise (punctuation, plain identifiers) stays in the panel's
ordinary text colour rather than getting a colour of its own.

`Kind::color` reads from five new `Palette` fields
(`syntax_keyword`/`syntax_type`/`syntax_number`/`syntax_attribute`/
`syntax_comment`), so the highlight colours live in the same place every
other editor colour does and repaint with the rest of the theme.

Highlighting is computed once, in `Preview::rebuild`/`refresh_wgsl`,
alongside the `wxsl`/`wgsl` strings themselves — not per frame in
`code_panel`. A full lex is cheap next to shaping, but the code panel
already goes out of its way to lay out only the visible lines specifically
because doing anything to the whole file every frame does not scale to the
WGSL a graph can produce (`Ui::code_view`'s doc comment), and recomputing a
lex on every one of those frames for text that changes only when the graph
does would be paying that cost for nothing.

`Ui::code_view` keeps its existing signature and behaviour (used unchanged
by the "problems" tab, which is diagnostic text, not code); a new
`Ui::highlighted_code_view(id, rect, text, runs)` takes the coloured runs
and is what the WXSL/WGSL tabs call. A visible line is split into
plain/coloured segments at each run boundary and drawn with one
`mono_label` call per segment — still only the visible lines, exactly as
before.

## Alternatives considered

- **A hand-rolled regex/keyword-list highlighter in `wxsl-editor`,
  depending on nothing new.** Rejected: it would have needed its own
  answer to the `<`/`>` disambiguation `wxsl-lang::lexer` already solves
  (and tests — see `lexer.rs`'s template-disambiguation tests), plus its
  own list of WXSL-only keywords kept in sync with `wxsl-lang::token::Tok`
  by hand. Two tokenizers for one language, mismatched schedules apart,
  is exactly the kind of duplication ADR 0015 spent that session's whole
  effort removing from the node registry.
- **Extend `tokenize` itself to return comments, instead of adding
  `tokenize_with_comments`.** Rejected: every existing caller (the parser,
  `resolve`, `mono`, every test in the compiler pipeline) would need
  updating for a return value none of them use — comments are trivia to
  every pass except this one.

## Consequences

- `wxsl-editor` now depends on `wxsl-lang` (`Cargo.toml`), in addition to
  `wxsl-core` and `wxsl-render` (ADR 0013) — update `AGENTS.md`'s crate
  table and `docs/architecture.md`'s crate graph alongside this ADR. The
  dependency still only ever points into `wxsl-core` territory (ADR 0002):
  `wxsl-lang` depends on nothing in this workspace, so no cycle and no new
  edge into `wxsl-core`/`wxsl-render` is created.
- A source either panel would show that the lexer rejects cannot happen in
  practice (both are text `wxsl-lang` itself just produced or accepted),
  but `highlight` returns no runs rather than propagating the error, so a
  highlighter bug would show the text uncoloured rather than losing the
  panel.
