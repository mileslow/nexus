//! The text of a file being edited: where its lines are, what is selected,
//! what each key changes and how to take it back. Nothing here draws; the
//! file view shows the lines and passes on the keys.

use std::ops::Range;

/// What Tab types, and how far it moves a line.
pub const INDENT: &str = "    ";
/// Tabs in a file are drawn out to columns this far apart.
const TAB_STOP: usize = 4;

/// One character of a line, and where it is drawn.
struct Cell {
    /// Its first byte in the line as it is in the file.
    raw: usize,
    raw_len: usize,
    /// Its first byte in the line as it is drawn, with tabs spread out.
    shown: usize,
    shown_len: usize,
    column: usize,
    columns: usize,
}

/// The characters of `line` as the file view draws them: a tab as the spaces
/// up to the next stop, any other control character as one space.
fn cells(line: &str) -> impl Iterator<Item = Cell> + '_ {
    let (mut shown, mut column) = (0, 0);
    line.char_indices().map(move |(raw, c)| {
        let (shown_len, columns) = if c == '\t' {
            let width = TAB_STOP - column % TAB_STOP;
            (width, width)
        } else if c.is_control() {
            (1, 1)
        } else {
            (c.len_utf8(), 1)
        };
        let cell = Cell {
            raw,
            raw_len: c.len_utf8(),
            shown,
            shown_len,
            column,
            columns,
        };
        shown += shown_len;
        column += columns;
        cell
    })
}

/// Where byte `offset` of `line` is in the line as drawn.
pub fn to_shown(line: &str, offset: usize) -> usize {
    let mut end = 0;
    for cell in cells(line) {
        if cell.raw >= offset {
            return cell.shown;
        }
        end = cell.shown + cell.shown_len;
    }
    end
}

/// The byte of `line` drawn at byte `shown` of the line as drawn. A place
/// inside a tab goes to whichever side of it is nearer.
pub fn from_shown(line: &str, shown: usize) -> usize {
    for cell in cells(line) {
        if shown < cell.shown + cell.shown_len {
            let past_middle = (shown - cell.shown) * 2 > cell.shown_len;
            return if past_middle {
                cell.raw + cell.raw_len
            } else {
                cell.raw
            };
        }
    }
    line.len()
}

/// How many columns across byte `offset` of `line` is drawn.
pub fn column_of(line: &str, offset: usize) -> usize {
    let mut end = 0;
    for cell in cells(line) {
        if cell.raw >= offset {
            return cell.column;
        }
        end = cell.column + cell.columns;
    }
    end
}

/// The byte of `line` nearest to `column` columns across.
fn offset_at_column(line: &str, column: usize) -> usize {
    for cell in cells(line) {
        if column < cell.column + cell.columns {
            let past_middle = (column - cell.column) * 2 > cell.columns;
            return if past_middle {
                cell.raw + cell.raw_len
            } else {
                cell.raw
            };
        }
    }
    line.len()
}

/// The lines of the text an edit replaced, and how many took their place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edited {
    pub first: usize,
    pub old: usize,
    pub new: usize,
}

/// Where the cursor goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    /// Down this many lines; up when negative.
    Lines(isize),
    Start,
    End,
}

/// How much Backspace or Delete takes when nothing is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Character,
    Word,
    Line,
}

/// What kind of edit the last one was, so that a run of typing or of
/// erasing is taken back in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Typing,
    Erasing,
}

/// One edit: `removed` gave way to `inserted` at `at`.
struct Change {
    at: usize,
    removed: String,
    inserted: String,
    /// A line break was added at the end of the text, to give the cursor a
    /// last line to sit on.
    appended: bool,
    /// The selection (anchor, head) before and after.
    before: (usize, usize),
    after: (usize, usize),
}

pub struct Buffer {
    text: String,
    /// Where each line begins. A line break that ends the text ends its
    /// last line rather than beginning another.
    starts: Vec<usize>,
    /// The selection runs from `anchor` to `head`, where the cursor is.
    anchor: usize,
    head: usize,
    /// The column the cursor set off from, up or down, so that passing
    /// through a short line does not pull it to the left.
    goal: Option<usize>,
    /// Text an input method is still composing.
    marked: Option<Range<usize>>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    run: Option<Run>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// What Return adds after `before`, the text of a line up to the cursor: a
/// line break, the line's indentation and, in a list or a quote, the mark
/// of its next item. `None` when `before` is nothing but such a mark: the
/// list ends there.
fn continuation(before: &str) -> Option<String> {
    let indent_len = before.len() - before.trim_start_matches([' ', '\t']).len();
    let (indent, rest) = before.split_at(indent_len);
    let mut mark = String::new();
    let mut content = rest;
    // A quote, and whatever it holds.
    while let Some(after) = content.strip_prefix('>') {
        mark.push('>');
        content = after;
        if let Some(after) = content.strip_prefix(' ') {
            mark.push(' ');
            content = after;
        }
    }
    let bullet = ['-', '*', '+']
        .into_iter()
        .find(|bullet| content.starts_with(*bullet) && content[1..].starts_with(' '));
    if let Some(bullet) = bullet {
        mark.push(bullet);
        mark.push(' ');
        content = &content[2..];
        let boxes = ["[ ] ", "[x] ", "[X] "];
        if let Some(found) = boxes.iter().find(|found| content.starts_with(**found)) {
            mark.push_str("[ ] ");
            content = &content[found.len()..];
        }
    } else {
        let digits = content.chars().take_while(char::is_ascii_digit).count();
        let after = &content[digits..];
        let number = content[..digits].parse::<u64>().ok();
        if let (Some(number), true) = (number, after.starts_with(". ") || after.starts_with(") ")) {
            mark.push_str(&format!("{}{}", number + 1, &after[..2]));
            content = &after[2..];
        }
    }
    if !mark.is_empty() && content.trim().is_empty() {
        return None;
    }
    Some(format!("\n{indent}{mark}"))
}

/// How much of the start of `line` moving it out one step takes off.
fn outdent(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    let spaces = line.len() - line.trim_start_matches(' ').len();
    spaces.min(INDENT.len())
}

/// What differs between two texts: the part of `old` that changed and the
/// part of `new` that took its place.
pub fn changed_span(old: &str, new: &str) -> (Range<usize>, Range<usize>) {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let most = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(most)
        .take_while(|(x, y)| x == y)
        .count();
    while !old.is_char_boundary(old.len() - suffix) {
        suffix -= 1;
    }
    (prefix..old.len() - suffix, prefix..new.len() - suffix)
}

/// Both sets of changes made to `base`: the ones that led to `ours` and the
/// ones that led to `theirs`. `None` when they changed the same part.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (ours_from, ours_to) = changed_span(base, ours);
    let (theirs_from, theirs_to) = changed_span(base, theirs);
    let ours_text = &ours[ours_to];
    let at = if ours_from.end <= theirs_from.start {
        // Ours comes first, where their text is still as it was.
        ours_from
    } else if theirs_from.end <= ours_from.start {
        let grown = theirs_to.len();
        let was = theirs_from.len();
        ours_from.start + grown - was..ours_from.end + grown - was
    } else {
        return None;
    };
    Some(format!(
        "{}{}{}",
        &theirs[..at.start],
        ours_text,
        &theirs[at.end..]
    ))
}

impl Buffer {
    pub fn new(text: String) -> Self {
        let mut buffer = Self {
            text,
            starts: Vec::new(),
            anchor: 0,
            head: 0,
            goal: None,
            marked: None,
            undo: Vec::new(),
            redo: Vec::new(),
            run: None,
        };
        buffer.index();
        buffer
    }

    /// Finds where the lines begin.
    fn index(&mut self) {
        self.starts.clear();
        self.starts.push(0);
        self.starts.extend(
            self.text
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        if self.starts.len() > 1 && self.starts.last() == Some(&self.text.len()) {
            self.starts.pop();
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.starts[line.min(self.starts.len() - 1)]
    }

    /// The bytes of a line, without its line break.
    fn line_range(&self, line: usize) -> Range<usize> {
        let line = line.min(self.starts.len() - 1);
        let start = self.starts[line];
        let mut end = match self.starts.get(line + 1) {
            Some(next) => next - 1,
            None if self.text.ends_with('\n') => self.text.len() - 1,
            None => self.text.len(),
        };
        if end > start && self.text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        start..end
    }

    /// The text of a line, without its line break.
    pub fn line(&self, line: usize) -> &str {
        &self.text[self.line_range(line)]
    }

    /// The line that byte `offset` is on.
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset) - 1
    }

    /// The last place the cursor can be.
    pub fn end(&self) -> usize {
        self.line_range(self.starts.len() - 1).end
    }

    /// The nearest place at or before `offset` that the cursor can be: in
    /// the text, between characters, and not inside a line break.
    fn snap(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.end());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset.min(self.line_range(self.line_of(offset)).end)
    }

    pub fn head(&self) -> usize {
        self.head
    }

    /// What is selected, from its first byte to past its last.
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Whether the cursor is at the front of the selection.
    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Selects from `anchor` to `head`, leaving the cursor at `head`.
    pub fn select(&mut self, anchor: usize, head: usize) {
        self.anchor = self.snap(anchor);
        self.head = self.snap(head);
        self.goal = None;
        self.run = None;
        self.marked = None;
    }

    pub fn select_all(&mut self) {
        self.select(0, self.end());
    }

    /// The line break this file uses.
    fn line_break(&self) -> &'static str {
        match self.text.find('\n') {
            Some(at) if at > 0 && self.text.as_bytes()[at - 1] == b'\r' => "\r\n",
            _ => "\n",
        }
    }

    /// Text from outside, with this file's line breaks.
    fn foreign(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.line_break() {
            "\n" => text,
            line_break => text.replace('\n', line_break),
        }
    }

    // ---- moving ---------------------------------------------------------------

    fn before(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        if offset == self.starts[line] {
            return match line {
                0 => 0,
                _ => self.line_range(line - 1).end,
            };
        }
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn after(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        if offset >= self.line_range(line).end {
            return match self.starts.get(line + 1) {
                Some(next) => *next,
                None => offset,
            };
        }
        let c = self.text[offset..].chars().next();
        offset + c.map_or(0, char::len_utf8)
    }

    fn word_before(&self, offset: usize) -> usize {
        let mut at = offset;
        // Back over what separates words, then over the word.
        let mut in_word = false;
        loop {
            let previous = self.before(at);
            if previous == at {
                return at;
            }
            let c = self.text[previous..].chars().next().unwrap_or(' ');
            // A line break stops the cursor unless that is where it began.
            let crossed = self.line_of(previous) != self.line_of(at);
            if crossed && at != offset {
                return at;
            }
            match (is_word(c) && !crossed, in_word) {
                (true, _) => in_word = true,
                (false, true) => return at,
                (false, false) => {}
            }
            at = previous;
            if crossed {
                return at;
            }
        }
    }

    fn word_after(&self, offset: usize) -> usize {
        let mut at = offset;
        let mut in_word = false;
        loop {
            let next = self.after(at);
            if next == at {
                return at;
            }
            let crossed = self.line_of(next) != self.line_of(at);
            if crossed {
                return if at == offset { next } else { at };
            }
            let c = self.text[at..].chars().next().unwrap_or(' ');
            match (is_word(c), in_word) {
                (true, _) => in_word = true,
                (false, true) => return at,
                (false, false) => {}
            }
            at = next;
        }
    }

    /// Where `motion` takes the cursor from `from`, and the column it is
    /// then aiming for.
    fn target(&self, from: usize, motion: Motion) -> (usize, Option<usize>) {
        let line = self.line_of(from);
        let range = self.line_range(line);
        let at = match motion {
            Motion::Left => self.before(from),
            Motion::Right => self.after(from),
            Motion::WordLeft => self.word_before(from),
            Motion::WordRight => self.word_after(from),
            Motion::LineStart => {
                // First to where the line's text begins, then to its edge.
                let text = self.line(line);
                let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
                if from == range.start + indent {
                    range.start
                } else {
                    range.start + indent
                }
            }
            Motion::LineEnd => range.end,
            Motion::Start => 0,
            Motion::End => self.end(),
            Motion::Lines(lines) => {
                let column = self
                    .goal
                    .unwrap_or_else(|| column_of(self.line(line), from - range.start));
                let last = self.starts.len() - 1;
                let target = line.saturating_add_signed(lines).min(last);
                let at = if target == line && lines < 0 {
                    0
                } else if target == line && lines > 0 {
                    self.end()
                } else {
                    self.starts[target] + offset_at_column(self.line(target), column)
                };
                return (at, Some(column));
            }
        };
        (at, None)
    }

    /// Moves the cursor; `select` carries the selection along with it.
    pub fn go(&mut self, motion: Motion, select: bool) {
        let selected = self.selection();
        let (head, goal) = match motion {
            // Stepping off a selection lands on its near end.
            Motion::Left if !select && !selected.is_empty() => (selected.start, None),
            Motion::Right if !select && !selected.is_empty() => (selected.end, None),
            _ => self.target(self.head, motion),
        };
        let anchor = if select { self.anchor } else { head };
        self.select(anchor, head);
        self.goal = goal;
    }

    // ---- changing -------------------------------------------------------------

    /// Puts `text` in place of `at`, and says which lines that touched.
    /// Returns whether a line break had to be added at the end as well.
    fn splice(&mut self, at: Range<usize>, text: &str, cursor: usize) -> (Edited, bool) {
        let count = self.starts.len();
        let first = self.line_of(at.start);
        let last = self.line_of(at.end);
        self.text.replace_range(at, text);
        self.index();
        // The cursor needs a line to be on after the text's last line break.
        let appended = cursor > self.end();
        if appended {
            self.text.push('\n');
            self.index();
        }
        let new = (last + 1 + self.starts.len()).saturating_sub(count + first);
        (
            Edited {
                first,
                old: last - first + 1,
                new,
            },
            appended,
        )
    }

    /// Makes one edit that can be taken back, leaving the selection as
    /// `after` (anchor, head).
    fn change(
        &mut self,
        at: Range<usize>,
        text: &str,
        after: Option<(usize, usize)>,
        run: Option<Run>,
    ) -> Edited {
        let before = (self.anchor, self.head);
        let removed = self.text[at.clone()].to_string();
        let cursor = at.start + text.len();
        let after = after.unwrap_or((cursor, cursor));
        let composing = self.marked.take();
        let (edited, appended) = self.splice(at.clone(), text, after.0.max(after.1));
        self.anchor = self.snap(after.0);
        self.head = self.snap(after.1);
        self.goal = None;
        self.redo.clear();

        let continues = run.is_some() && run == self.run && !appended;
        let merged = match self.undo.last_mut() {
            // An input method trying out what to write: only the last try counts.
            Some(last)
                if composing.as_ref() == Some(&at)
                    && last.at == at.start
                    && last.inserted.len() == at.len()
                    && !last.appended =>
            {
                last.inserted = text.to_string();
                last.appended = appended;
                true
            }
            Some(last) if continues && !last.appended => match run {
                Some(Run::Typing)
                    if removed.is_empty()
                        && last.removed.is_empty()
                        && at.start == last.at + last.inserted.len()
                        // A new word is a new step.
                        && !(text.starts_with(char::is_whitespace)
                            && !last.inserted.ends_with(char::is_whitespace)) =>
                {
                    last.inserted.push_str(text);
                    true
                }
                Some(Run::Erasing) if text.is_empty() && last.inserted.is_empty() => {
                    if at.end == last.at {
                        last.removed.insert_str(0, &removed);
                        last.at = at.start;
                        true
                    } else if at.start == last.at {
                        last.removed.push_str(&removed);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
            _ => false,
        };
        if merged {
            if let Some(last) = self.undo.last_mut() {
                last.after = (self.anchor, self.head);
            }
        } else {
            self.undo.push(Change {
                at: at.start,
                removed,
                inserted: text.to_string(),
                appended,
                before,
                after: (self.anchor, self.head),
            });
        }
        self.run = run;
        edited
    }

    /// Types `text` over `range`; failing that, over what is being composed
    /// or what is selected.
    pub fn insert(&mut self, range: Option<Range<usize>>, text: &str) -> Edited {
        let at = range
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        if text.contains(['\n', '\r']) {
            let text = self.foreign(text);
            return self.change(at, &text, None, None);
        }
        self.change(at, text, None, Some(Run::Typing))
    }

    /// As [`Buffer::insert`], but the text is still being composed: it stays
    /// marked, with `selected` of it (bytes within `text`) selected.
    pub fn compose(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) -> Edited {
        let at = range
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let start = at.start;
        let after = selected.map(|selected| (start + selected.start, start + selected.end));
        let edited = self.change(at, text, after, Some(Run::Typing));
        self.marked = (!text.is_empty()).then(|| start..start + text.len());
        edited
    }

    /// Puts in text from the clipboard.
    pub fn paste(&mut self, text: &str) -> Edited {
        let text = self.foreign(text);
        self.change(self.selection(), &text, None, None)
    }

    /// Removes the selection or, with nothing selected, what is before the
    /// cursor (`forward`: after it).
    pub fn erase(&mut self, reach: Reach, forward: bool) -> Option<Edited> {
        let mut at = self.selection();
        if at.is_empty() {
            let line = self.line_range(self.line_of(self.head));
            let to = match (reach, forward) {
                (Reach::Character, false) => self.before(self.head),
                (Reach::Character, true) => self.after(self.head),
                (Reach::Word, false) => self.word_before(self.head),
                (Reach::Word, true) => self.word_after(self.head),
                // With nothing left on the line, its line break goes.
                (Reach::Line, false) if self.head == line.start => self.before(self.head),
                (Reach::Line, false) => line.start,
                (Reach::Line, true) if self.head == line.end => self.after(self.head),
                (Reach::Line, true) => line.end,
            };
            at = to.min(self.head)..to.max(self.head);
        }
        if at.is_empty() {
            return None;
        }
        Some(self.change(at, "", None, Some(Run::Erasing)))
    }

    /// What Return does: a new line that carries on the indentation and
    /// any list of the one before it.
    pub fn newline(&mut self) -> Edited {
        let at = self.selection();
        let line = self.line_range(self.line_of(at.start));
        match continuation(&self.text[line.start..at.start]) {
            Some(text) => {
                let text = text.replace('\n', self.line_break());
                self.change(at, &text, None, None)
            }
            // An empty list item: Return ends the list instead.
            None => self.change(line.start..at.end.max(line.start), "", None, None),
        }
    }

    /// Moves the selected lines in by one step, or back out. With nothing
    /// selected, moving in types the step at the cursor.
    pub fn indent(&mut self, out: bool) -> Option<Edited> {
        let selected = self.selection();
        let first = self.line_of(selected.start);
        let mut last = self.line_of(selected.end);
        if !out && first == last {
            return Some(self.change(selected, INDENT, None, None));
        }
        // A selection that stops at the start of a line leaves that line be.
        if last > first && selected.end == self.starts[last] {
            last -= 1;
        }
        let block = self.starts[first]..self.line_range(last).end;
        let mut moved = String::new();
        for line in first..=last {
            let range = self.line_range(line);
            let text = &self.text[range.clone()];
            if out {
                moved.push_str(&text[outdent(text)..]);
            } else {
                if !text.is_empty() {
                    moved.push_str(INDENT);
                }
                moved.push_str(text);
            }
            if line < last {
                // Its own line break, whichever kind that is.
                moved.push_str(&self.text[range.end..self.starts[line + 1]]);
            }
        }
        if moved == self.text[block.clone()] {
            return None;
        }
        let after = (
            self.shifted(self.anchor, first, last, out),
            self.shifted(self.head, first, last, out),
        );
        Some(self.change(block, &moved, Some(after), None))
    }

    /// Where `end`, an end of the selection, is once lines `first..=last`
    /// have moved in, or `out`.
    fn shifted(&self, end: usize, first: usize, last: usize, out: bool) -> usize {
        let mut by: isize = 0;
        for line in first..=last {
            let range = self.line_range(line);
            if range.start > end {
                break;
            }
            let text = &self.text[range.clone()];
            if out {
                // An end inside what is cut lands where the cut began.
                by -= outdent(text).min(end - range.start) as isize;
            } else if !text.is_empty() {
                by += INDENT.len() as isize;
            }
        }
        (end as isize + by).max(0) as usize
    }

    /// Undoes or redoes one change.
    fn revert(&mut self, change: &Change, undo: bool) -> Edited {
        let count = self.starts.len();
        let (was, becomes, selection) = if undo {
            (&change.inserted, &change.removed, change.before)
        } else {
            (&change.removed, &change.inserted, change.after)
        };
        let at = change.at..change.at + was.len();
        let first = self.line_of(at.start);
        let last = self.line_of(at.end);
        if undo && change.appended {
            self.text.pop();
        }
        self.text.replace_range(at, becomes);
        if !undo && change.appended {
            self.text.push('\n');
        }
        self.index();
        self.anchor = self.snap(selection.0);
        self.head = self.snap(selection.1);
        self.goal = None;
        self.run = None;
        self.marked = None;
        // Taking back an added last line takes its line along too.
        let last = last.max(if change.appended { count - 1 } else { 0 });
        Edited {
            first,
            old: last - first + 1,
            new: (last + 1 + self.starts.len()).saturating_sub(count + first),
        }
    }

    pub fn undo(&mut self) -> Option<Edited> {
        let change = self.undo.pop()?;
        let edited = self.revert(&change, true);
        self.redo.push(change);
        Some(edited)
    }

    pub fn redo(&mut self) -> Option<Edited> {
        let change = self.redo.pop()?;
        let edited = self.revert(&change, false);
        self.undo.push(change);
        Some(edited)
    }

    /// Takes text that changed somewhere else, keeping the selection on the
    /// same words where they are still there. What was done before cannot
    /// be undone afterwards.
    pub fn set_text(&mut self, text: String) {
        let (old, new) = changed_span(&self.text, &text);
        let moved = |at: usize| {
            if at <= old.start {
                at
            } else if at >= old.end {
                at + new.len() - old.len()
            } else {
                (new.start + (at - old.start)).min(new.end)
            }
        };
        let (anchor, head) = (moved(self.anchor), moved(self.head));
        self.text = text;
        self.index();
        self.anchor = self.snap(anchor);
        self.head = self.snap(head);
        self.goal = None;
        self.marked = None;
        self.run = None;
        self.undo.clear();
        self.redo.clear();
    }

    // ---- for the input method, which counts in UTF-16 ---------------------------

    pub fn to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .encode_utf16()
            .count()
    }

    pub fn from_utf16(&self, offset: usize) -> usize {
        let mut seen = 0;
        for (at, c) in self.text.char_indices() {
            if seen >= offset {
                return at;
            }
            seen += c.len_utf16();
        }
        self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.line_count())
            .map(|line| buffer.line(line).to_string())
            .collect()
    }

    /// Keeps a list of lines up to date the way the file view does, and
    /// checks it against the buffer.
    fn follow(shown: &mut Vec<String>, buffer: &Buffer, edited: Edited) {
        let new = (edited.first..edited.first + edited.new).map(|line| buffer.line(line).to_string());
        shown.splice(edited.first..edited.first + edited.old, new);
        assert_eq!(*shown, lines(buffer), "after {edited:?}");
    }

    #[test]
    fn a_tab_is_drawn_out_to_the_next_stop() {
        let line = "\ta\tbé\u{1}c";
        // Drawn as "    a   bé c".
        assert_eq!(to_shown(line, 0), 0);
        assert_eq!(to_shown(line, 1), 4);
        assert_eq!(to_shown(line, 2), 5);
        assert_eq!(to_shown(line, 3), 8);
        assert_eq!(to_shown(line, line.len()), 13);
        assert_eq!(column_of(line, line.len()), 12);
        for offset in [0, 1, 2, 3, 4, 6, 7, 8] {
            assert_eq!(from_shown(line, to_shown(line, offset)), offset);
        }
        // Inside a tab: whichever side is nearer.
        assert_eq!(from_shown(line, 1), 0);
        assert_eq!(from_shown(line, 3), 1);
        assert_eq!(from_shown(line, 99), line.len());
        assert_eq!(offset_at_column(line, 5), 2);
        assert_eq!(offset_at_column(line, 99), line.len());
    }

    #[test]
    fn a_last_line_break_ends_the_last_line() {
        let buffer = Buffer::new("one\r\ntwo\n".into());
        assert_eq!(lines(&buffer), ["one", "two"]);
        assert_eq!(buffer.end(), 8);
        assert_eq!(buffer.line_of(5), 1);
        assert_eq!(lines(&Buffer::new("".into())), [""]);
        assert_eq!(lines(&Buffer::new("\n".into())), [""]);
        assert_eq!(lines(&Buffer::new("a\n\n".into())), ["a", ""]);
        assert_eq!(lines(&Buffer::new("a\nb".into())), ["a", "b"]);
    }

    #[test]
    fn typing_and_erasing_change_the_lines_they_touch() {
        let mut buffer = Buffer::new("one\ntwo\nthree\n".into());
        let mut shown = lines(&buffer);
        buffer.select(3, 3);
        let edited = buffer.insert(None, "!");
        assert_eq!(edited, Edited { first: 0, old: 1, new: 1 });
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "one!\ntwo\nthree\n");

        // Over a selection that spans lines.
        buffer.select(2, 7);
        let edited = buffer.insert(None, "X\nY\nZ");
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "onX\nY\nZo\nthree\n");
        assert_eq!(buffer.head(), 7);

        // Backspace at the start of a line joins it to the one before.
        buffer.select(4, 4);
        let edited = buffer.erase(Reach::Character, false).unwrap();
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "onXY\nZo\nthree\n");
        // At the very start there is nothing to take.
        buffer.select(0, 0);
        assert_eq!(buffer.erase(Reach::Character, false), None);
        buffer.select(buffer.end(), buffer.end());
        assert_eq!(buffer.erase(Reach::Character, true), None);

        buffer.select(7, 7);
        let edited = buffer.erase(Reach::Line, false).unwrap();
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "onXY\n\nthree\n");
        buffer.select(9, 9);
        let edited = buffer.erase(Reach::Word, true).unwrap();
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "onXY\n\nthr\n");
    }

    #[test]
    fn the_cursor_always_has_a_line_to_be_on() {
        // Return at the end of a file with no last line break.
        let mut buffer = Buffer::new("a".into());
        let mut shown = lines(&buffer);
        buffer.select(1, 1);
        let edited = buffer.newline();
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "a\n\n");
        assert_eq!((buffer.head(), buffer.line_of(buffer.head())), (2, 1));
        let edited = buffer.insert(None, "b");
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "a\nb\n");
        // And taking it all back.
        while let Some(edited) = buffer.undo() {
            follow(&mut shown, &buffer, edited);
        }
        assert_eq!(buffer.text(), "a");
        while let Some(edited) = buffer.redo() {
            follow(&mut shown, &buffer, edited);
        }
        assert_eq!(buffer.text(), "a\nb\n");

        // Emptying the last line of such a file.
        let mut buffer = Buffer::new("a\nb".into());
        let mut shown = lines(&buffer);
        buffer.select(3, 3);
        let edited = buffer.erase(Reach::Character, false).unwrap();
        follow(&mut shown, &buffer, edited);
        assert_eq!(lines(&buffer), ["a", ""]);
        let edited = buffer.undo().unwrap();
        follow(&mut shown, &buffer, edited);
        assert_eq!(buffer.text(), "a\nb");
    }

    #[test]
    fn a_run_of_typing_is_taken_back_in_one_step() {
        let mut buffer = Buffer::new("".into());
        for c in ["h", "i", " ", "y", "o", "u"] {
            buffer.insert(None, c);
        }
        assert_eq!(buffer.text(), "hi you");
        buffer.undo();
        assert_eq!(buffer.text(), "hi");
        buffer.undo();
        assert_eq!(buffer.text(), "");
        buffer.redo();
        buffer.redo();
        assert_eq!((buffer.text(), buffer.head()), ("hi you", 6));

        // Moving the cursor ends the run; so does switching to erasing.
        buffer.go(Motion::Left, false);
        buffer.go(Motion::Right, false);
        buffer.insert(None, "!");
        buffer.erase(Reach::Character, false);
        buffer.erase(Reach::Character, false);
        assert_eq!(buffer.text(), "hi yo");
        buffer.undo();
        assert_eq!(buffer.text(), "hi you!");
        buffer.undo();
        assert_eq!(buffer.text(), "hi you");
        // A new edit forgets what was undone.
        buffer.insert(None, "?");
        assert_eq!(buffer.redo(), None);

        // An input method's tries are one step, however many there were.
        let mut buffer = Buffer::new("a".into());
        buffer.select(1, 1);
        buffer.compose(None, "k", None);
        buffer.compose(None, "ka", None);
        assert_eq!(buffer.marked(), Some(1..3));
        buffer.insert(None, "か");
        assert_eq!((buffer.text(), buffer.marked()), ("aか", None));
        buffer.undo();
        assert_eq!(buffer.text(), "a");
    }

    #[test]
    fn the_cursor_moves_by_characters_words_and_lines() {
        let mut buffer = Buffer::new("  first word\r\n\tsecond\nlast".into());
        let at = |buffer: &mut Buffer, motion, select| {
            buffer.go(motion, select);
            buffer.head()
        };
        buffer.select(12, 12);
        // Across a line break in one step, both ways.
        assert_eq!(at(&mut buffer, Motion::Right, false), 14);
        assert_eq!(at(&mut buffer, Motion::Left, false), 12);
        assert_eq!(at(&mut buffer, Motion::WordLeft, false), 8);
        assert_eq!(at(&mut buffer, Motion::WordLeft, false), 2);
        assert_eq!(at(&mut buffer, Motion::WordRight, false), 7);
        // To where the text begins, then to the edge, then back.
        assert_eq!(at(&mut buffer, Motion::LineStart, false), 2);
        assert_eq!(at(&mut buffer, Motion::LineStart, false), 0);
        assert_eq!(at(&mut buffer, Motion::LineStart, false), 2);
        assert_eq!(at(&mut buffer, Motion::LineEnd, false), 12);
        // Down a line keeps the column, through tabs and short lines.
        buffer.select(6, 6);
        assert_eq!(at(&mut buffer, Motion::Lines(1), false), 17);
        assert_eq!(at(&mut buffer, Motion::Lines(1), false), 26);
        assert_eq!(at(&mut buffer, Motion::Lines(-1), false), 17);
        // Past the last line goes to the end; past the first, to the start.
        assert_eq!(at(&mut buffer, Motion::Lines(9), false), 26);
        assert_eq!(at(&mut buffer, Motion::Lines(1), false), 26);
        assert_eq!(at(&mut buffer, Motion::Lines(-9), false), 6);
        assert_eq!(at(&mut buffer, Motion::Lines(-1), false), 0);

        // Selecting, and stepping off a selection.
        buffer.select(4, 4);
        buffer.go(Motion::WordRight, true);
        assert_eq!((buffer.selection(), buffer.reversed()), (4..7, false));
        buffer.go(Motion::Start, true);
        assert_eq!((buffer.selection(), buffer.reversed()), (0..4, true));
        assert_eq!(at(&mut buffer, Motion::Right, false), 4);
        buffer.select_all();
        assert_eq!(buffer.selection(), 0..26);
    }

    #[test]
    fn return_carries_on_a_list() {
        assert_eq!(continuation("plain").as_deref(), Some("\n"));
        assert_eq!(continuation("  indented").as_deref(), Some("\n  "));
        assert_eq!(continuation("- item").as_deref(), Some("\n- "));
        assert_eq!(continuation("  * [x] done").as_deref(), Some("\n  * [ ] "));
        assert_eq!(continuation("9. nine").as_deref(), Some("\n10. "));
        assert_eq!(continuation("> quoted").as_deref(), Some("\n> "));
        assert_eq!(continuation("> - in a quote").as_deref(), Some("\n> - "));
        // Not lists.
        assert_eq!(continuation("-not").as_deref(), Some("\n"));
        assert_eq!(continuation("1.5 is a number").as_deref(), Some("\n"));
        // A mark with nothing after it ends the list.
        assert_eq!(continuation("- "), None);
        assert_eq!(continuation("  2. "), None);
        assert_eq!(continuation("- [ ] "), None);

        let mut buffer = Buffer::new("- one\r\n- two".into());
        buffer.select(5, 5);
        buffer.newline();
        assert_eq!(buffer.text(), "- one\r\n- \r\n- two");
        buffer.newline();
        assert_eq!((buffer.text(), buffer.head()), ("- one\r\n\r\n- two", 7));
    }

    #[test]
    fn tab_moves_the_selected_lines() {
        let mut buffer = Buffer::new("a\n  b\n\n\tc\nd".into());
        buffer.select(1, 1);
        buffer.indent(false);
        assert_eq!((buffer.text(), buffer.head()), ("a    \n  b\n\n\tc\nd", 5));
        buffer.undo();

        buffer.select(1, 9);
        buffer.indent(false);
        assert_eq!(buffer.text(), "    a\n      b\n\n    \tc\nd");
        assert_eq!(buffer.selection(), 5..21);
        buffer.indent(true);
        assert_eq!(buffer.text(), "a\n  b\n\n\tc\nd");
        assert_eq!(buffer.selection(), 1..9);
        buffer.indent(true);
        assert_eq!(buffer.text(), "a\nb\n\nc\nd");
        assert_eq!(buffer.selection(), 1..6);
        assert_eq!(buffer.indent(true), None);
    }

    #[test]
    fn changes_from_two_sides_are_both_kept() {
        assert_eq!(changed_span("one two", "one 2 two"), (4..4, 4..6));
        assert_eq!(changed_span("aéb", "aèb"), (1..3, 1..3));
        assert_eq!(changed_span("same", "same"), (4..4, 4..4));
        assert_eq!(changed_span("aaa", "aa"), (2..3, 2..2));

        let base = "# Plan\n\none\ntwo\n";
        let ours = "# Plan\n\none!\ntwo\n";
        let theirs = "# Plan\n\none\ntwo\nthree\n";
        assert_eq!(
            merge(base, ours, theirs).as_deref(),
            Some("# Plan\n\none!\ntwo\nthree\n")
        );
        let theirs = "# The plan\n\none\ntwo\n";
        assert_eq!(
            merge(base, ours, theirs).as_deref(),
            Some("# The plan\n\none!\ntwo\n")
        );
        assert_eq!(merge(base, ours, base).as_deref(), Some(ours));
        // Side by side is still two different parts.
        assert_eq!(
            merge(base, ours, "# Plan\n\nONE\ntwo\n").as_deref(),
            Some("# Plan\n\nONE!\ntwo\n")
        );
        // The same part changed both ways.
        assert_eq!(merge(base, "# Plan\n\no!ne\ntwo\n", "# Plan\n\nONE\ntwo\n"), None);
        assert_eq!(merge(base, ours, "# Plan\n"), None);

        // The selection stays on its words when text changes around it.
        let mut buffer = Buffer::new(base.into());
        buffer.select(8, 11);
        buffer.set_text("# The plan\n\none\ntwo\n".into());
        assert_eq!(buffer.selection(), 12..15);
        buffer.set_text("# The plan\n".into());
        assert_eq!(buffer.selection(), 10..10);
        assert_eq!(buffer.undo(), None);
    }

    #[test]
    fn the_input_method_counts_in_utf16() {
        let buffer = Buffer::new("a😀é".into());
        assert_eq!(buffer.to_utf16(5), 3);
        assert_eq!(buffer.from_utf16(3), 5);
        assert_eq!(buffer.from_utf16(99), 7);
        assert_eq!(buffer.to_utf16(99), 4);
    }
}
