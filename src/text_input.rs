//! A minimal text field (search boxes): one line, or one that wraps and
//! grows with what is typed into it.

use std::ops::Range;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, AvailableSpace, Bounds,
    ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, SharedString, Style, TextAlign, TextRun, TextStyle, UTF16Selection,
    UnderlineStyle, Window, WrappedLine,
};

use crate::platform::keys;
use crate::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        DeleteToStart,
        Paste,
        Cut,
        Copy,
        Submit,
        Cancel,
        MoveDown,
        MoveUp,
        SelectDown,
        SelectUp,
        Newline,
        Indent,
    ]
);

/// How many rows a wrapping field shows before it scrolls instead of growing.
const MAX_ROWS: usize = 6;
/// What Tab types, and what a pasted tab becomes.
const INDENT: &str = "    ";
/// How far a selection reaches past the end of a line whose break it holds.
const LINE_BREAK_WIDTH: Pixels = px(4.);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new(keys("cmd-a", "ctrl-a"), SelectAll, ctx),
        KeyBinding::new(keys("cmd-v", "ctrl-v"), Paste, ctx),
        KeyBinding::new(keys("cmd-c", "ctrl-c"), Copy, ctx),
        KeyBinding::new(keys("cmd-x", "ctrl-x"), Cut, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new(keys("cmd-left", "ctrl-left"), Home, ctx),
        KeyBinding::new(keys("cmd-right", "ctrl-right"), End, ctx),
        KeyBinding::new("ctrl-e", End, ctx),
        KeyBinding::new(keys("cmd-backspace", "ctrl-backspace"), DeleteToStart, ctx),
        KeyBinding::new("ctrl-u", DeleteToStart, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("escape", Cancel, ctx),
        KeyBinding::new("down", MoveDown, ctx),
        KeyBinding::new("up", MoveUp, ctx),
    ]);
    if cfg!(target_os = "macos") {
        // The Mac's Emacs-style line start; on Windows Ctrl+A selects all.
        cx.bind_keys([KeyBinding::new("ctrl-a", Home, ctx)]);
    }
    // Only a field that wraps has lines to break, indent and select across.
    let ctx = Some("TextInput && multiline");
    cx.bind_keys([
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("alt-enter", Newline, ctx),
        KeyBinding::new("tab", Indent, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
    ]);
}

/// Clipboard text as a field takes it. One that wraps keeps the line breaks
/// and the indentation; a single line gets it all on that line.
fn pasted(text: &str, multiline: bool) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        // A copied line usually brings its line break along.
        text.trim_end_matches('\n').replace('\t', INDENT)
    } else {
        text.replace('\n', " ")
    }
}

/// A line break at `offset` that carries on the indentation of its line.
fn line_break_at(content: &str, offset: usize) -> String {
    let line = &content[line_start(content, offset)..offset];
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    format!("\n{}", &line[..indent])
}

/// Where the line holding `offset` begins.
fn line_start(content: &str, offset: usize) -> usize {
    content[..offset].rfind('\n').map_or(0, |at| at + 1)
}

/// Where the line holding `offset` ends, before its line break.
fn line_end(content: &str, offset: usize) -> usize {
    content[offset..]
        .find('\n')
        .map_or(content.len(), |at| offset + at)
}

/// One row of text on screen: a whole line, or a piece of a wrapped one.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    /// The bytes of the content it shows.
    range: Range<usize>,
    /// Which line of the content it is part of, and where that line begins.
    line: usize,
    line_start: usize,
    /// How far into the unwrapped line the row begins.
    start_x: Pixels,
    /// Whether the line carries on in the next row.
    wraps: bool,
}

/// Adds the rows of one line: `len` bytes long, starting at `line_start`,
/// wrapped at each of `breaks` (a byte within the line and how far along it
/// that is).
fn push_rows(
    rows: &mut Vec<Row>,
    line: usize,
    line_start: usize,
    len: usize,
    breaks: impl IntoIterator<Item = (usize, Pixels)>,
) {
    let (mut start, mut start_x) = (0, px(0.));
    for (index, x) in breaks {
        rows.push(Row {
            range: line_start + start..line_start + index,
            line,
            line_start,
            start_x,
            wraps: true,
        });
        (start, start_x) = (index, x);
    }
    rows.push(Row {
        range: line_start + start..line_start + len,
        line,
        line_start,
        start_x,
        wraps: false,
    });
}

/// The row the cursor is on at `offset`. Where a line wraps, that is the
/// start of the lower row rather than the end of the upper one.
fn row_for(rows: &[Row], offset: usize) -> usize {
    rows.iter()
        .rposition(|row| row.range.start <= offset)
        .unwrap_or(0)
}

/// The text as it was last laid out.
struct Layout {
    lines: Vec<WrappedLine>,
    /// Never empty: an empty field has one empty row.
    rows: Vec<Row>,
    line_height: Pixels,
}

impl Layout {
    fn height(&self) -> Pixels {
        self.line_height * self.rows.len() as f32
    }

    /// How far along row `row` the byte at `offset` is.
    fn x_in_row(&self, row: usize, offset: usize) -> Pixels {
        let row = &self.rows[row];
        let Some(line) = self.lines.get(row.line) else {
            return px(0.);
        };
        let offset = offset.clamp(row.range.start, row.range.end);
        line.unwrapped_layout.x_for_index(offset - row.line_start) - row.start_x
    }

    /// Where the cursor sits at `offset`, from the top left of the text.
    fn position(&self, offset: usize) -> Point<Pixels> {
        let row = row_for(&self.rows, offset);
        point(self.x_in_row(row, offset), self.line_height * row as f32)
    }

    /// The byte nearest to `x` along row `row`.
    fn offset_at(&self, row: usize, x: Pixels) -> usize {
        let row = &self.rows[row.min(self.rows.len() - 1)];
        let Some(line) = self.lines.get(row.line) else {
            return row.range.start;
        };
        let within = line
            .unwrapped_layout
            .closest_index_for_x(x.max(px(0.)) + row.start_x);
        let offset = (row.line_start + within).clamp(row.range.start, row.range.end);
        if row.wraps && offset == row.range.end {
            // The end of a wrapped row is the start of the next one; stay
            // on this row.
            let before = &line.text[..offset - row.line_start];
            let previous = before.char_indices().next_back().map_or(0, |(at, _)| at);
            (row.line_start + previous).max(row.range.start)
        } else {
            offset
        }
    }

    /// The row under a point `y` below the top of the text.
    fn row_at(&self, y: Pixels) -> usize {
        ((y / self.line_height).floor().max(0.) as usize).min(self.rows.len() - 1)
    }
}

pub enum TextInputEvent {
    Changed,
    Submit,
    Cancel,
    MoveDown,
    MoveUp,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<Layout>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Wraps long text onto further rows, growing up to [`MAX_ROWS`] of
    /// them, and takes line breaks.
    multiline: bool,
    /// How far a field with more rows than it shows is scrolled.
    scroll_top: Pixels,
    /// Scroll the cursor into view at the next paint.
    reveal_cursor: bool,
    /// How far along its row the cursor was when it set off up or down, so
    /// that passing through a short row does not pull it to the left.
    column: Option<Pixels>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            multiline: false,
            scroll_top: px(0.),
            reveal_cursor: false,
            column: None,
        }
    }

    /// A field that wraps long text onto further rows and grows with it,
    /// then scrolls. Shift+Return breaks a line, Tab indents, and pasted
    /// text keeps its line breaks and indentation.
    pub fn multiline(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            multiline: true,
            ..Self::new(placeholder, cx)
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Selects the whole text, so typing replaces it.
    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.selection_reversed = false;
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            return;
        }
        self.content.clear();
        self.selected_range = 0..0;
        self.marked_range = None;
        self.scroll_top = px(0.);
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(line_start(&self.content, self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(line_end(&self.content, self.cursor_offset()), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let mut start = line_start(&self.content, cursor);
        if start == cursor {
            // Nothing left on this line: take the line break before it.
            start = self.previous_boundary(cursor);
        }
        self.selected_range = start..cursor;
        self.selection_reversed = false;
        self.replace_text_in_range(None, "", window, cx)
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.selected_range.start;
        let text = line_break_at(&self.content, start);
        self.replace_text_in_range(None, &text, window, cx)
    }

    fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, INDENT, window, cx)
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submit);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Cancel);
    }

    /// The arrows move through the rows of the text first, and step out of
    /// the field from its first and last rows.
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        match self.row_away(1) {
            Some((offset, column)) => {
                self.move_to(offset, cx);
                self.column = Some(column);
            }
            None => cx.emit(TextInputEvent::MoveDown),
        }
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        match self.row_away(-1) {
            Some((offset, column)) => {
                self.move_to(offset, cx);
                self.column = Some(column);
            }
            None => cx.emit(TextInputEvent::MoveUp),
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let landing = self.row_away(1);
        self.select_to(landing.map_or(self.content.len(), |(offset, _)| offset), cx);
        self.column = landing.map(|(_, column)| column);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let landing = self.row_away(-1);
        self.select_to(landing.map_or(0, |(offset, _)| offset), cx);
        self.column = landing.map(|(_, column)| column);
    }

    /// Where the cursor lands `rows` rows down, or up when negative, and how
    /// far along the row it is aiming for. `None` when there is no such row.
    fn row_away(&self, rows: isize) -> Option<(usize, Pixels)> {
        if !self.multiline {
            return None;
        }
        let layout = self.last_layout.as_ref()?;
        let cursor = self.cursor_offset();
        let row = row_for(&layout.rows, cursor);
        let target = row
            .checked_add_signed(rows)
            .filter(|&target| target < layout.rows.len())?;
        let column = self.column.unwrap_or_else(|| layout.x_in_row(row, cursor));
        Some((self.snap(layout.offset_at(target, column)), column))
    }

    /// An offset worked out from the last layout, made safe for the text as
    /// it is now.
    fn snap(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return;
        };
        let hidden = layout.height() - bounds.size.height;
        if hidden <= px(0.) {
            return;
        }
        let moved = event.delta.pixel_delta(layout.line_height).y;
        self.scroll_top = (self.scroll_top - moved).clamp(px(0.), hidden);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &pasted(&text, self.multiline), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.reveal_cursor = true;
        self.column = None;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        let row = layout.row_at(position.y - bounds.top() + self.scroll_top);
        self.snap(layout.offset_at(row, position.x - bounds.left()))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.reveal_cursor = true;
        self.column = None;
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content[..offset.min(self.content.len())]
            .char_indices()
            .next_back()
            .map(|(ix, _)| ix)
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content[offset.min(self.content.len())..]
            .chars()
            .next()
            .map(|c| offset + c.len_utf8())
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.reveal_cursor = true;
        self.column = None;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.reveal_cursor = true;
        self.column = None;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let row = row_for(&layout.rows, range.start);
        let top = bounds.top() + layout.line_height * row as f32 - self.scroll_top;
        // A range that runs on past its row is cut off at the row's end.
        Some(Bounds::from_corners(
            point(bounds.left() + layout.x_in_row(row, range.start), top),
            point(
                bounds.left() + layout.x_in_row(row, range.end),
                top + layout.line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?.localize(&point)?;
        self.last_layout.as_ref()?;
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

/// The lines of a field and the rows they fill when wrapped at `wrap_width`.
/// The placeholder never wraps.
fn shape(
    input: &TextInput,
    style: &TextStyle,
    font_size: Pixels,
    wrap_width: Option<Pixels>,
    window: &Window,
) -> (Vec<WrappedLine>, Vec<Row>) {
    let empty = input.content.is_empty();
    let (text, color): (SharedString, _) = if empty {
        (input.placeholder.clone(), theme::hsla(theme::text_faint()))
    } else {
        (input.content.clone().into(), style.color)
    };

    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs: Vec<TextRun> = match input.marked_range.as_ref().filter(|_| !empty) {
        Some(marked_range) => vec![
            TextRun {
                len: marked_range.start,
                ..run.clone()
            },
            TextRun {
                len: marked_range.end - marked_range.start,
                underline: Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: text.len() - marked_range.end,
                ..run
            },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        None => vec![run],
    };

    let wrap_width = wrap_width
        .filter(|_| input.multiline && !empty)
        .map(|width| width.max(px(1.)));
    let lines = window
        .text_system()
        .shape_text(text, font_size, &runs, wrap_width, None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default();

    let mut rows = Vec::new();
    if !empty {
        let mut line_start = 0;
        for (index, line) in lines.iter().enumerate() {
            let breaks = line.wrap_boundaries().iter().map(|boundary| {
                let glyph = &line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix];
                (glyph.index, glyph.position.x)
            });
            push_rows(&mut rows, index, line_start, line.len(), breaks);
            line_start += line.len() + 1;
        }
    }
    if rows.is_empty() {
        push_rows(&mut rows, 0, 0, 0, []);
    }
    (lines, rows)
}

struct PrepaintState {
    layout: Option<Layout>,
    scroll_top: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let line_height = window.line_height();
        if !self.input.read(cx).multiline {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }

        // As tall as its rows, which depends on how wide it turns out to be.
        let input = self.input.clone();
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let layout = window.request_measured_layout(
            style,
            move |known, available, window, cx| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let (lines, rows) = shape(input.read(cx), &text_style, font_size, width, window);
                let width = width.unwrap_or_else(|| {
                    lines
                        .iter()
                        .map(|line| line.unwrapped_layout.width)
                        .max()
                        .unwrap_or_default()
                });
                size(width, line_height * rows.len().min(MAX_ROWS) as f32)
            },
        );
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let (lines, rows) = shape(input, &style, font_size, Some(bounds.size.width), window);
        let layout = Layout {
            lines,
            rows,
            line_height,
        };

        // Keep within the text, and bring the cursor into view if it moved.
        let hidden = (layout.height() - bounds.size.height).max(px(0.));
        let mut scroll_top = input.scroll_top;
        if input.reveal_cursor {
            let top = layout.position(cursor).y;
            scroll_top = scroll_top
                .min(top)
                .max(top + line_height - bounds.size.height);
        }
        let scroll_top = scroll_top.clamp(px(0.), hidden);
        let origin = point(bounds.left(), bounds.top() - scroll_top);

        let mut selection = Vec::new();
        for (index, row) in layout.rows.iter().enumerate() {
            let start = selected.start.max(row.range.start);
            let end = selected.end.min(row.range.end);
            // The line break after the row is part of the selection.
            let through = !row.wraps && selected.end > row.range.end;
            if start > end || (start == end && !through) {
                continue;
            }
            let top = origin.y + line_height * index as f32;
            let mut right = layout.x_in_row(index, end);
            if through {
                right += LINE_BREAK_WIDTH;
            }
            selection.push(fill(
                Bounds::from_corners(
                    point(origin.x + layout.x_in_row(index, start), top),
                    point(origin.x + right, top + line_height),
                ),
                theme::selection(),
            ));
        }
        let cursor = selected.is_empty().then(|| {
            let at = layout.position(cursor);
            fill(
                Bounds::new(
                    point(origin.x + at.x, origin.y + at.y + px(1.)),
                    size(px(1.5), line_height - px(2.)),
                ),
                theme::accent(),
            )
        });
        PrepaintState {
            layout: Some(layout),
            scroll_top,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let layout = prepaint.layout.take().unwrap();
        let scroll_top = prepaint.scroll_top;
        let line_height = layout.line_height;

        // Rows scrolled out of the field are not drawn over what is around it.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in prepaint.selection.drain(..) {
                window.paint_quad(selection);
            }
            let mut top = bounds.top() - scroll_top;
            for line in &layout.lines {
                line.paint(
                    point(bounds.left(), top),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
                top += line_height * (line.wrap_boundaries().len() + 1) as f32;
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(layout);
            input.last_bounds = Some(bounds);
            input.scroll_top = scroll_top;
            input.reveal_cursor = false;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .key_context(if self.multiline {
                "TextInput multiline"
            } else {
                "TextInput"
            })
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .line_height(px(20.))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_keeps_its_lines_only_where_they_fit() {
        let copied = "fn main() {\r\n\tlet x = 1;\r\n}\r\n";
        assert_eq!(
            pasted(copied, true),
            "fn main() {\n    let x = 1;\n}"
        );
        assert_eq!(pasted("  indented\n    more", true), "  indented\n    more");
        // A single line has nowhere to put a line break.
        assert_eq!(pasted("one\ntwo\r\nthree", false), "one two three");
    }

    #[test]
    fn a_new_line_carries_on_the_indentation() {
        let content = "first\n    second line\n\tthird";
        assert_eq!(line_break_at(content, 5), "\n");
        assert_eq!(line_break_at(content, content.find(" line").unwrap()), "\n    ");
        assert_eq!(line_break_at(content, content.len()), "\n\t");
        // Only the indentation before the cursor is there to carry on.
        assert_eq!(line_break_at(content, 8), "\n  ");
    }

    #[test]
    fn lines_start_and_end_at_their_breaks() {
        let content = "one\ntwo\n";
        assert_eq!((line_start(content, 0), line_end(content, 0)), (0, 3));
        assert_eq!((line_start(content, 3), line_end(content, 3)), (0, 3));
        assert_eq!((line_start(content, 5), line_end(content, 5)), (4, 7));
        assert_eq!((line_start(content, 8), line_end(content, 8)), (8, 8));
    }

    #[test]
    fn wrapped_lines_split_into_rows() {
        // "hello wide world" wrapped before "wide" and "world", then "ok".
        let mut rows = Vec::new();
        push_rows(&mut rows, 0, 0, 16, [(6, px(40.)), (11, px(75.))]);
        push_rows(&mut rows, 1, 17, 2, []);
        let ranges: Vec<_> = rows.iter().map(|row| row.range.clone()).collect();
        assert_eq!(ranges, vec![0..6, 6..11, 11..16, 17..19]);
        let wraps: Vec<_> = rows.iter().map(|row| row.wraps).collect();
        assert_eq!(wraps, vec![true, true, false, false]);
        assert_eq!(rows[1].start_x, px(40.));
        assert_eq!((rows[3].line, rows[3].line_start), (1, 17));

        assert_eq!(row_for(&rows, 0), 0);
        assert_eq!(row_for(&rows, 5), 0);
        // Where a line wraps, the cursor belongs to the lower row.
        assert_eq!(row_for(&rows, 6), 1);
        // The end of a line is on its last row, not the next line's first.
        assert_eq!(row_for(&rows, 16), 2);
        assert_eq!(row_for(&rows, 17), 3);
        assert_eq!(row_for(&rows, 19), 3);
    }
}
