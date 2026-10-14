//! An editable multi-line text buffer: the model behind the code editor
//! (plan3 M11).
//!
//! The same text model as [`crate::ui::Ui::text_field`] — a `String` and
//! byte offsets — grown the three things a multi-line editor needs: lines
//! (a caret has a row and a column, and up/down keep the column it was
//! aiming for), a selection that spans them, and per-buffer undo. A
//! function file is a few hundred bytes, so an undo step is a snapshot of
//! the whole text rather than an inverse operation: simpler to get right,
//! and nothing a profile would ever see.
//!
//! Pure data, no device: everything here is tested without a GPU, and the
//! widget that draws it ([`crate::ui::Ui::code_editor`]) only maps the
//! pointer to offsets and keys to calls.

use std::ops::Range;

use wxsl_render::ui::input::{Key, KeyPress};

/// How deep a buffer's undo history goes.
const UNDO_DEPTH: usize = 256;

/// Spaces a tab stop is.
pub const TAB_WIDTH: usize = 4;

/// A text, a caret, a selection, and the history to undo them.
#[derive(Clone, Debug, Default)]
pub struct TextBuffer {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    /// The column a vertical move aims for, kept across shorter lines.
    goal: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// What the last edit was, so a run of typing is one undo step.
    last: Option<Run>,
    revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// A coalescable edit: typing or deleting, ending at `at`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    Typing { at: usize },
    Deleting { at: usize },
}

/// What a key press did to the buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// The text changed.
    pub edited: bool,
    /// The caret or selection moved, so the view should follow it.
    pub moved: bool,
    /// The key was the buffer's to handle.
    pub handled: bool,
}

impl TextBuffer {
    /// A buffer holding `text`, the caret at its start.
    pub fn new(text: impl Into<String>) -> Self {
        TextBuffer {
            text: text.into(),
            ..TextBuffer::default()
        }
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Bumped by every change to the text — what a cache of anything
    /// derived from it (highlighting) keys on.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The caret, as a byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected byte range, low to high, if anything is selected.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// The selected text, empty when nothing is.
    pub fn selected_text(&self) -> &str {
        self.selection().map_or("", |range| &self.text[range])
    }

    /// How many lines the text has; an empty text is one empty line.
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// The byte range of line `line` (0-based), without its newline.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let mut start = 0;
        for (index, text) in self.text.split('\n').enumerate() {
            if index == line {
                return start..start + text.len();
            }
            start += text.len() + 1;
        }
        self.text.len()..self.text.len()
    }

    /// The 0-based line and character column of byte `offset`.
    pub fn line_column(&self, offset: usize) -> (usize, usize) {
        let offset = floor_boundary(&self.text, offset);
        let before = &self.text[..offset];
        let line = before.matches('\n').count();
        let start = before.rfind('\n').map_or(0, |index| index + 1);
        (line, self.text[start..offset].chars().count())
    }

    /// The byte offset of `column` characters into `line`, clamped to the
    /// line's end and to the last line.
    pub fn offset(&self, line: usize, column: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        let range = self.line_range(line);
        let text = &self.text[range.clone()];
        range.start
            + text
                .char_indices()
                .nth(column)
                .map_or(text.len(), |(index, _)| index)
    }

    /// Put the caret at `offset`; `extend` keeps (or starts) a selection
    /// from where it was.
    pub fn set_cursor(&mut self, offset: usize, extend: bool) {
        let offset = floor_boundary(&self.text, offset);
        self.anchor = if extend {
            Some(self.anchor.unwrap_or(self.cursor))
        } else {
            None
        };
        self.cursor = offset;
        self.goal = None;
        self.last = None;
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
        self.goal = None;
        self.last = None;
    }

    /// Select the word (or run of punctuation, or of spaces) at `offset`.
    pub fn select_word_at(&mut self, offset: usize) {
        let offset = floor_boundary(&self.text, offset);
        let class = self.text[offset..]
            .chars()
            .next()
            .or_else(|| self.text[..offset].chars().next_back())
            .map(class_of);
        let Some(class) = class else { return };
        let start = self.text[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, c)| class_of(*c) == class)
            .last()
            .map_or(offset, |(index, _)| index);
        let end = self.text[offset..]
            .char_indices()
            .find(|(_, c)| class_of(*c) != class)
            .map_or(self.text.len(), |(index, _)| offset + index);
        self.anchor = Some(start);
        self.cursor = end;
        self.goal = None;
        self.last = None;
    }

    /// Type `text` over the selection.
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let selection = self.selection();
        // A run of typing coalesces; a newline, a paste or an overwrite
        // starts its own step.
        let run = (selection.is_none() && !text.contains('\n') && text.chars().count() == 1)
            .then_some(Run::Typing {
                at: self.cursor + text.len(),
            });
        self.checkpoint(run);
        let mut at = match selection {
            Some(range) => {
                self.text.replace_range(range.clone(), "");
                range.start
            }
            None => self.cursor,
        };
        // A closing bracket typed first on a line closes the block the
        // newline after its opener indented: one stop back.
        if matches!(text, "}" | ")" | "]") {
            let (line, _) = self.line_column(at);
            let start = self.line_range(line).start;
            let before = &self.text[start..at];
            if before.len() >= TAB_WIDTH && before.bytes().all(|b| b == b' ') {
                self.text.replace_range(at - TAB_WIDTH..at, "");
                at -= TAB_WIDTH;
            }
        }
        self.text.insert_str(at, text);
        self.cursor = at + text.len();
        self.anchor = None;
        self.goal = None;
        self.changed();
    }

    /// A newline, indented like the line it breaks, one stop deeper after
    /// an opening bracket.
    pub fn newline(&mut self) {
        let (line, _) = self.line_column(self.cursor);
        let range = self.line_range(line);
        let before = &self.text[range.start..self.cursor.max(range.start)];
        let mut indent: String = before.chars().take_while(|c| *c == ' ').collect();
        if before.trim_end().ends_with(['{', '(', '[']) {
            indent.push_str(&" ".repeat(TAB_WIDTH));
        }
        self.insert(&format!("\n{indent}"));
    }

    /// Indent: spaces to the next tab stop, or every selected line one
    /// stop deeper when the selection spans lines.
    pub fn indent(&mut self) {
        match self.selected_lines() {
            Some(lines) => self.reindent(lines, true),
            None => {
                let (_, column) = self.line_column(self.cursor);
                self.insert(&" ".repeat(TAB_WIDTH - column % TAB_WIDTH));
            }
        }
    }

    /// Every line the selection touches (or the caret's) one stop
    /// shallower.
    pub fn outdent(&mut self) {
        let lines = self.selected_lines().unwrap_or_else(|| {
            let (line, _) = self.line_column(self.cursor);
            line..line + 1
        });
        self.reindent(lines, false);
    }

    /// Delete the selection, or the character before the caret.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let from = previous_boundary(&self.text, self.cursor);
        self.checkpoint(Some(Run::Deleting { at: from }));
        self.text.replace_range(from..self.cursor, "");
        self.cursor = from;
        self.changed();
    }

    /// Delete the selection, or the character after the caret.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let to = next_boundary(&self.text, self.cursor);
        self.checkpoint(Some(Run::Deleting { at: self.cursor }));
        self.text.replace_range(self.cursor..to, "");
        self.changed();
    }

    /// Take the last edit back. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.redo.push(current);
        self.restore(snapshot);
        true
    }

    /// Put an undone edit back. Returns whether there was one.
    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.undo.push(current);
        self.restore(snapshot);
        true
    }

    /// Apply one key press. `page` is how many lines a page key moves;
    /// `clipboard` is where copy and cut put text and paste takes it from.
    pub fn key(&mut self, press: &KeyPress, page: usize, clipboard: &mut String) -> KeyOutcome {
        let modifiers = press.modifiers;
        let shift = modifiers.shift;
        // Word moves are Alt on a Mac and Control elsewhere; Command on its
        // own is line and document ends.
        let word = modifiers.alt || modifiers.ctrl;
        let revision = self.revision;
        let (cursor, anchor) = (self.cursor, self.anchor);
        let mut handled = true;
        match press.key {
            Key::Left if modifiers.logo => self.home(shift),
            Key::Right if modifiers.logo => self.end(shift),
            Key::Up if modifiers.logo => self.move_to(0, shift),
            Key::Down if modifiers.logo => self.move_to(self.text.len(), shift),
            Key::Left => self.horizontal(false, word, shift),
            Key::Right => self.horizontal(true, word, shift),
            Key::Up => self.vertical(-1, shift),
            Key::Down => self.vertical(1, shift),
            Key::PageUp => self.vertical(-(page.max(1) as isize), shift),
            Key::PageDown => self.vertical(page.max(1) as isize, shift),
            Key::Home if modifiers.command() => self.move_to(0, shift),
            Key::End if modifiers.command() => self.move_to(self.text.len(), shift),
            Key::Home => self.home(shift),
            Key::End => self.end(shift),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Enter => self.newline(),
            Key::Tab if shift => self.outdent(),
            Key::Tab => self.indent(),
            Key::Char('a') if modifiers.command() => self.select_all(),
            Key::Char('z') if modifiers.command() && shift => {
                self.redo();
            }
            Key::Char('z') if modifiers.command() => {
                self.undo();
            }
            Key::Char('y') if modifiers.command() => {
                self.redo();
            }
            Key::Char('c') if modifiers.command() => {
                if self.selection().is_some() {
                    *clipboard = self.selected_text().to_string();
                }
            }
            Key::Char('x') if modifiers.command() => {
                if self.selection().is_some() {
                    *clipboard = self.selected_text().to_string();
                    self.delete_selection();
                }
            }
            Key::Char('v') if modifiers.command() => {
                let text = clipboard.clone();
                self.insert(&text);
            }
            _ => handled = false,
        }
        KeyOutcome {
            edited: self.revision != revision,
            moved: self.cursor != cursor || self.anchor != anchor,
            handled,
        }
    }

    fn move_to(&mut self, offset: usize, extend: bool) {
        self.set_cursor(offset, extend);
    }

    fn horizontal(&mut self, forward: bool, word: bool, extend: bool) {
        // Collapsing a selection without shift lands on its edge.
        if let (Some(range), false) = (self.selection(), extend) {
            let at = if forward { range.end } else { range.start };
            self.set_cursor(at, false);
            return;
        }
        let target = match (forward, word) {
            (true, false) => next_boundary(&self.text, self.cursor),
            (false, false) => previous_boundary(&self.text, self.cursor),
            (true, true) => self.word_end(self.cursor),
            (false, true) => self.word_start(self.cursor),
        };
        self.set_cursor(target, extend);
    }

    fn vertical(&mut self, lines: isize, extend: bool) {
        let (line, column) = self.line_column(self.cursor);
        let goal = self.goal.unwrap_or(column);
        let target = line as isize + lines;
        let offset = if target < 0 {
            0
        } else if target as usize >= self.line_count() {
            self.text.len()
        } else {
            self.offset(target as usize, goal)
        };
        self.set_cursor(offset, extend);
        self.goal = Some(goal);
    }

    /// Smart home: the first non-space of the line, then column zero.
    fn home(&mut self, extend: bool) {
        let (line, _) = self.line_column(self.cursor);
        let range = self.line_range(line);
        let indent = self.text[range.clone()]
            .chars()
            .take_while(|c| *c == ' ')
            .count();
        let first = range.start + indent;
        let target = if self.cursor == first {
            range.start
        } else {
            first
        };
        self.set_cursor(target, extend);
    }

    fn end(&mut self, extend: bool) {
        let (line, _) = self.line_column(self.cursor);
        self.set_cursor(self.line_range(line).end, extend);
    }

    fn word_end(&self, from: usize) -> usize {
        let mut chars = self.text[from..].char_indices().peekable();
        // Skip spaces, then one run of a single class.
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let Some(&(_, first)) = chars.peek() else {
            return self.text.len();
        };
        let class = class_of(first);
        while chars.next_if(|(_, c)| class_of(*c) == class).is_some() {}
        chars
            .peek()
            .map_or(self.text.len(), |(index, _)| from + index)
    }

    fn word_start(&self, from: usize) -> usize {
        let mut chars = self.text[..from].char_indices().rev().peekable();
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let Some(&(_, first)) = chars.peek() else {
            return 0;
        };
        let class = class_of(first);
        let mut start = from;
        while let Some((index, _)) = chars.next_if(|(_, c)| class_of(*c) == class) {
            start = index;
        }
        start
    }

    /// The lines a selection spans, when it spans more than one.
    fn selected_lines(&self) -> Option<Range<usize>> {
        let range = self.selection()?;
        let (first, _) = self.line_column(range.start);
        let (last, column) = self.line_column(range.end);
        // A selection ending at a line's start does not take that line.
        let last = if column == 0 && last > first {
            last - 1
        } else {
            last
        };
        (last > first).then_some(first..last + 1)
    }

    fn reindent(&mut self, lines: Range<usize>, deeper: bool) {
        let mut text = String::with_capacity(self.text.len() + lines.len() * TAB_WIDTH);
        for (index, line) in self.text.split('\n').enumerate() {
            if index > 0 {
                text.push('\n');
            }
            if !lines.contains(&index) {
                text.push_str(line);
            } else if deeper {
                text.push_str(&" ".repeat(TAB_WIDTH));
                text.push_str(line);
            } else {
                let spaces = line.chars().take_while(|c| *c == ' ').count();
                text.push_str(&line[spaces.min(TAB_WIDTH)..]);
            }
        }
        if text == self.text {
            return;
        }
        self.checkpoint(None);
        self.text = text;
        // The selection becomes the whole of the lines it touched, which
        // is what a repeated tab wants to act on again.
        let start = self.line_range(lines.start).start;
        let end = self.line_range(lines.end - 1).end;
        self.anchor = Some(start);
        self.cursor = end;
        self.goal = None;
        self.changed();
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.checkpoint(None);
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = None;
        self.changed();
        true
    }

    /// Record the state before an edit, unless `run` continues the last
    /// edit's run — the coalescing that makes a typed word one undo step.
    fn checkpoint(&mut self, run: Option<Run>) {
        let continues = match (self.last, run) {
            (Some(Run::Typing { at }), Some(Run::Typing { .. })) => {
                at == self.cursor
                    && !self.text[..self.cursor]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace)
            }
            (Some(Run::Deleting { at }), Some(Run::Deleting { .. })) => at == self.cursor,
            _ => false,
        };
        if !continues {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = run;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.anchor = snapshot.anchor;
        self.goal = None;
        self.last = None;
        self.revision += 1;
    }

    fn changed(&mut self) {
        self.revision += 1;
        // A run continues from where this edit left the caret.
        self.last = match self.last {
            Some(Run::Typing { .. }) => Some(Run::Typing { at: self.cursor }),
            Some(Run::Deleting { .. }) => Some(Run::Deleting { at: self.cursor }),
            None => None,
        };
    }
}

/// What a double click or a word move treats as one word.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class_of(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn previous_boundary(text: &str, index: usize) -> usize {
    let index = floor_boundary(text, index);
    text[..index]
        .char_indices()
        .next_back()
        .map_or(0, |(offset, _)| offset)
}

fn next_boundary(text: &str, index: usize) -> usize {
    let index = floor_boundary(text, index);
    text[index..]
        .chars()
        .next()
        .map_or(index, |c| index + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_render::ui::input::Modifiers;

    fn press(key: Key, modifiers: Modifiers) -> KeyPress {
        KeyPress {
            key,
            modifiers,
            repeat: false,
        }
    }

    fn keys(buffer: &mut TextBuffer, presses: &[(Key, Modifiers)]) {
        let mut clipboard = String::new();
        for (key, modifiers) in presses {
            buffer.key(&press(*key, *modifiers), 10, &mut clipboard);
        }
    }

    const PLAIN: Modifiers = Modifiers::NONE;
    const COMMAND: Modifiers = Modifiers {
        logo: true,
        ..Modifiers::NONE
    };

    #[test]
    fn lines_and_columns_round_trip_through_offsets() {
        let buffer = TextBuffer::new("fn f() {\n    return 1é;\n}");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line_range(1), 9..24);
        for offset in [0, 3, 9, 13, 21, 23, 25, buffer.text().len()] {
            let (line, column) = buffer.line_column(offset);
            assert_eq!(buffer.offset(line, column), offset, "offset {offset}");
        }
        // A column past the end of a line lands on its end.
        assert_eq!(buffer.offset(0, 80), 8);
        assert_eq!(buffer.offset(9, 0), buffer.line_range(2).start);
    }

    #[test]
    fn up_and_down_keep_the_column_they_aim_for() {
        let mut buffer = TextBuffer::new("a long line\nab\nanother long line");
        buffer.set_cursor(8, false);
        keys(&mut buffer, &[(Key::Down, PLAIN)]);
        assert_eq!(buffer.line_column(buffer.cursor()), (1, 2), "clamped");
        keys(&mut buffer, &[(Key::Down, PLAIN)]);
        assert_eq!(buffer.line_column(buffer.cursor()), (2, 8), "restored");
        keys(
            &mut buffer,
            &[(Key::Up, Modifiers::SHIFT), (Key::Up, Modifiers::SHIFT)],
        );
        assert_eq!(buffer.selected_text(), "ine\nab\nanother ");
        keys(&mut buffer, &[(Key::PageDown, PLAIN)]);
        assert_eq!(buffer.cursor(), buffer.text().len(), "past the end");
    }

    #[test]
    fn typing_a_word_is_one_undo_step_and_redo_puts_it_back() {
        let mut buffer = TextBuffer::new("");
        for c in "let x".chars() {
            buffer.insert(&c.to_string());
        }
        buffer.newline();
        for c in "y".chars() {
            buffer.insert(&c.to_string());
        }
        assert_eq!(buffer.text(), "let x\ny");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "let x\n");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "let x");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "let ", "a space ends the word's step");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
        assert!(!buffer.undo());
        keys(
            &mut buffer,
            &[(
                Key::Char('z'),
                Modifiers {
                    shift: true,
                    ..COMMAND
                },
            )],
        );
        assert_eq!(buffer.text(), "let ");
        assert!(buffer.redo() && buffer.redo() && buffer.redo());
        assert_eq!(buffer.text(), "let x\ny");
        // A new edit forgets what was undone.
        buffer.undo();
        buffer.insert("z");
        assert!(!buffer.redo());
    }

    #[test]
    fn a_moved_caret_starts_a_new_undo_step() {
        let mut buffer = TextBuffer::new("ab");
        buffer.set_cursor(2, false);
        buffer.insert("c");
        buffer.set_cursor(0, false);
        buffer.insert("d");
        assert_eq!(buffer.text(), "dabc");
        buffer.undo();
        assert_eq!(buffer.text(), "abc");
        buffer.backspace();
        buffer.backspace();
        assert_eq!(buffer.text(), "abc", "nothing before the caret");
        buffer.set_cursor(3, false);
        buffer.backspace();
        buffer.backspace();
        assert_eq!(buffer.text(), "a");
        buffer.undo();
        assert_eq!(buffer.text(), "abc", "a run of deletes is one step");
    }

    #[test]
    fn enter_keeps_the_indent_and_deepens_it_after_a_bracket() {
        let mut buffer = TextBuffer::new("fn f() {");
        buffer.set_cursor(8, false);
        buffer.newline();
        buffer.insert("let a = (");
        buffer.newline();
        assert_eq!(buffer.text(), "fn f() {\n    let a = (\n        ");
        buffer.insert(")");
        buffer.insert(";");
        buffer.newline();
        buffer.insert("}");
        assert_eq!(buffer.text(), "fn f() {\n    let a = (\n    );\n}");
    }

    #[test]
    fn tab_indents_selected_lines_and_shift_tab_takes_it_back() {
        let mut buffer = TextBuffer::new("a\n  b\nc");
        buffer.insert("x");
        buffer.set_cursor(1, false);
        keys(&mut buffer, &[(Key::Tab, PLAIN)]);
        assert_eq!(buffer.text(), "x   a\n  b\nc", "to the next stop");
        buffer.set_cursor(2, false);
        buffer.set_cursor(10, true);
        keys(&mut buffer, &[(Key::Tab, PLAIN)]);
        assert_eq!(buffer.text(), "    x   a\n      b\nc");
        keys(
            &mut buffer,
            &[(Key::Tab, Modifiers::SHIFT), (Key::Tab, Modifiers::SHIFT)],
        );
        assert_eq!(buffer.text(), "x   a\nb\nc");
    }

    #[test]
    fn home_is_smart_and_word_moves_skip_runs() {
        let mut buffer = TextBuffer::new("    return foo_bar(1.0);");
        buffer.set_cursor(14, false);
        keys(&mut buffer, &[(Key::Home, PLAIN)]);
        assert_eq!(buffer.cursor(), 4);
        keys(&mut buffer, &[(Key::Home, PLAIN)]);
        assert_eq!(buffer.cursor(), 0);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        keys(&mut buffer, &[(Key::Right, alt), (Key::Right, alt)]);
        assert_eq!(buffer.cursor(), 18, "past `return`, then `foo_bar`");
        keys(&mut buffer, &[(Key::Left, alt)]);
        assert_eq!(buffer.cursor(), 11);
        buffer.select_word_at(13);
        assert_eq!(buffer.selected_text(), "foo_bar");
        keys(&mut buffer, &[(Key::Right, PLAIN)]);
        assert_eq!(buffer.cursor(), 18, "a plain arrow collapses to the edge");
    }

    #[test]
    fn copy_cut_and_paste_go_through_the_clipboard() {
        let mut buffer = TextBuffer::new("one two");
        let mut clipboard = String::new();
        buffer.select_word_at(1);
        buffer.key(&press(Key::Char('x'), COMMAND), 10, &mut clipboard);
        assert_eq!((buffer.text(), clipboard.as_str()), (" two", "one"));
        buffer.set_cursor(buffer.text().len(), false);
        let outcome = buffer.key(&press(Key::Char('v'), COMMAND), 10, &mut clipboard);
        assert!(outcome.edited && outcome.handled);
        assert_eq!(buffer.text(), " twoone");
        let outcome = buffer.key(&press(Key::Char('q'), PLAIN), 10, &mut clipboard);
        assert!(!outcome.handled, "plain characters arrive as text");
    }
}
