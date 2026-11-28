//! Selecting text with the mouse. The view draws its text through a
//! [`Selection`], which remembers where each piece landed and works out
//! what a press or a drag covers. A view whose text can be edited also has
//! it draw the cursor.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    fill, point, px, size, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseMoveEvent, Pixels, Point, SharedString, StyledText, TextLayout, WhiteSpace, Window,
};

use crate::theme;

/// A place in a view's text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    /// The row of the view: a block of a page, or a line of a file.
    pub row: usize,
    /// Which of the row's pieces of text, in reading order.
    pub piece: usize,
    /// A byte of that piece.
    pub offset: usize,
}

impl Place {
    /// Past the end of any text.
    pub const END: Place = Place {
        row: usize::MAX,
        piece: usize::MAX,
        offset: usize::MAX,
    };
}

/// The part of one piece of text that a selection covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub range: Range<usize>,
    /// The selection goes on past the end of the piece.
    pub continues: bool,
}

/// The links in a piece of text: where each is, and its address.
type Links = Rc<[(Range<usize>, Arc<str>)]>;

/// The nearest character boundary at or before `offset`.
fn floor(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The part of piece `piece` of `row`, holding `text`, that `selected`
/// covers. A piece the selection only touches the edge of has no part in it.
pub fn part(selected: &Range<Place>, row: usize, piece: usize, text: &str) -> Option<Part> {
    let key = (row, piece);
    let first = (selected.start.row, selected.start.piece);
    let last = (selected.end.row, selected.end.piece);
    if key < first || key > last {
        return None;
    }
    let start = if key == first {
        floor(text, selected.start.offset)
    } else {
        0
    };
    let end = if key == last {
        floor(text, selected.end.offset)
    } else {
        text.len()
    };
    if start >= end && (key == first || key == last) {
        return None;
    }
    Some(Part {
        range: start..end,
        continues: key < last,
    })
}

/// What a drag to `head` covers, when the press that began it covered `anchor`.
fn extended(anchor: &Range<Place>, head: Place) -> Range<Place> {
    if head < anchor.start {
        head..anchor.end
    } else if head > anchor.end {
        anchor.start..head
    } else {
        anchor.clone()
    }
}

/// The word that the gap at `offset` touches; failing that, one character.
fn word_at(text: &str, offset: usize) -> Range<usize> {
    let offset = floor(text, offset);
    let in_word = |c: &char| c.is_alphanumeric() || *c == '_';
    let before = text[..offset].chars().next_back();
    let after = text[offset..].chars().next();
    if before.as_ref().is_some_and(in_word) || after.as_ref().is_some_and(in_word) {
        let back: usize = text[..offset]
            .chars()
            .rev()
            .take_while(in_word)
            .map(char::len_utf8)
            .sum();
        let forward: usize = text[offset..]
            .chars()
            .take_while(in_word)
            .map(char::len_utf8)
            .sum();
        return offset - back..offset + forward;
    }
    match (before, after) {
        (_, Some(c)) => offset..offset + c.len_utf8(),
        (Some(c), None) => offset - c.len_utf8()..offset,
        (None, None) => 0..0,
    }
}

/// The line that `offset` is on.
fn line_at(text: &str, offset: usize) -> Range<usize> {
    let offset = floor(text, offset);
    let start = text[..offset].rfind('\n').map_or(0, |found| found + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |found| offset + found);
    start..end
}

/// A piece of text as it was last drawn.
struct Drawn {
    row: usize,
    piece: usize,
    text: SharedString,
    layout: TextLayout,
    links: Links,
}

impl Drawn {
    /// The link drawn at `position`.
    fn link_at(&self, position: Point<Pixels>) -> Option<&Arc<str>> {
        if self.links.is_empty() || !self.layout.bounds().contains(&position) {
            return None;
        }
        let index = self.layout.index_for_position(position).ok()?;
        self.links
            .iter()
            .find(|(range, _)| range.contains(&index))
            .map(|(_, address)| address)
    }

    /// The gap between characters nearest to `position`.
    fn offset_at(&self, position: Point<Pixels>) -> usize {
        let bounds = self.layout.bounds();
        if position.y < bounds.top() {
            return 0;
        }
        if position.y >= bounds.bottom() {
            return self.text.len();
        }
        // A point exactly on the line between two rows belongs to the upper.
        let position = point(position.x, (position.y - px(0.01)).max(bounds.top()));
        let index = match self.layout.index_for_position(position) {
            Ok(index) => index,
            Err(index) => return floor(&self.text, index),
        };
        // That is the character under the pointer; its far half is nearer
        // to the gap after it.
        let Some(next) = self
            .text
            .get(index..)
            .and_then(|rest| rest.chars().next())
            .map(|c| index + c.len_utf8())
        else {
            return floor(&self.text, index);
        };
        match (
            self.layout.position_for_index(index),
            self.layout.position_for_index(next),
        ) {
            (Some(near), Some(far)) if near.y == far.y && position.x > (near.x + far.x) / 2. => {
                next
            }
            _ => index,
        }
    }
}

/// What is selected in one view, and where the view last drew its text.
#[derive(Default)]
pub struct Selection {
    selected: Option<Range<Place>>,
    /// What the press that began a drag covered. The drag extends it.
    anchor: Option<Range<Place>>,
    /// The end that moves is the front one: the drag went backwards.
    reversed: bool,
    dragging: bool,
    /// Where to draw a text cursor, in a view that has one.
    cursor: Option<Place>,
    /// The link the press landed on, until the pointer selects something.
    pressed_link: Option<Arc<str>>,
    drawn: Rc<RefCell<Vec<Drawn>>>,
}

impl Selection {
    /// What is selected, if anything is.
    pub fn range(&self) -> Option<Range<Place>> {
        self.selected.clone().filter(|range| range.start < range.end)
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn clear(&mut self) {
        self.selected = None;
        self.anchor = None;
        self.reversed = false;
        self.dragging = false;
        self.pressed_link = None;
        self.cursor = None;
    }

    /// The two ends of the selection, even when they are the same place,
    /// and whether the one that moves is the first.
    pub fn ends(&self) -> Option<(Range<Place>, bool)> {
        Some((self.selected.clone()?, self.reversed))
    }

    /// Selects `selected`, as a view does whose keys move the selection.
    /// Shift-clicking then extends it from its still end.
    pub fn set(&mut self, selected: Range<Place>, reversed: bool) {
        let still = if reversed { selected.end } else { selected.start };
        self.anchor = Some(still..still);
        self.selected = Some(selected);
        self.reversed = reversed;
        self.dragging = false;
        self.pressed_link = None;
    }

    /// Draws a text cursor at `place`, until the selection is next cleared.
    pub fn set_cursor(&mut self, place: Option<Place>) {
        self.cursor = place;
    }

    pub fn select_all(&mut self) {
        self.clear();
        self.selected = Some(Place::default()..Place::END);
    }

    /// Forgets where text was drawn, before the view draws it again.
    pub fn begin_frame(&self) {
        self.drawn.borrow_mut().clear();
    }

    /// The selected part of a piece of text.
    pub fn part(&self, row: usize, piece: usize, text: &str) -> Option<Part> {
        part(&self.range()?, row, piece, text)
    }

    /// Makes `styled`, which shows `text`, selectable as piece `piece` of `row`.
    pub fn text(
        &self,
        row: usize,
        piece: usize,
        text: SharedString,
        styled: StyledText,
    ) -> Selectable {
        Selectable {
            part: self.part(row, piece, &text),
            cursor: self
                .cursor
                .filter(|cursor| (cursor.row, cursor.piece) == (row, piece))
                .map(|cursor| floor(&text, cursor.offset)),
            styled,
            text,
            row,
            piece,
            links: Rc::new([]),
            drawn: self.drawn.clone(),
        }
    }

    /// The place in the drawn text nearest to `position`, and the text of
    /// the piece it is in.
    fn place_at(&self, position: Point<Pixels>) -> Option<(Place, SharedString)> {
        let drawn = self.drawn.borrow();
        // How far outside a piece the point is: down first, then across.
        let distance = |piece: &Drawn| {
            let bounds = piece.layout.bounds();
            let outside = |at: Pixels, low: Pixels, high: Pixels| {
                (low - at).max(at - high).max(px(0.))
            };
            (
                outside(position.y, bounds.top(), bounds.bottom()),
                outside(position.x, bounds.left(), bounds.right()),
            )
        };
        let nearest = drawn.iter().min_by(|a, b| {
            distance(a)
                .partial_cmp(&distance(b))
                .unwrap_or(Ordering::Equal)
        })?;
        let place = Place {
            row: nearest.row,
            piece: nearest.piece,
            offset: nearest.offset_at(position),
        };
        Some((place, nearest.text.clone()))
    }

    fn link_at(&self, position: Point<Pixels>) -> Option<Arc<str>> {
        let drawn = self.drawn.borrow();
        drawn.iter().find_map(|piece| piece.link_at(position)).cloned()
    }

    /// Begins a selection where the mouse went down: at that point, or with
    /// the word or the line there for a second or third click. `extend`
    /// instead carries the selection there from where it last began.
    pub fn press(&mut self, position: Point<Pixels>, clicks: usize, extend: bool) {
        let Some((place, text)) = self.place_at(position) else {
            self.clear();
            return;
        };
        let within = |range: Range<usize>| {
            let at = |offset| Place { offset, ..place };
            at(range.start)..at(range.end)
        };
        let pressed = match clicks {
            0 | 1 => within(place.offset..place.offset),
            2 => within(word_at(&text, place.offset)),
            _ => within(line_at(&text, place.offset)),
        };
        self.pressed_link = self.link_at(position);
        self.dragging = true;
        match &self.anchor {
            Some(anchor) if extend => {
                self.reversed = place < anchor.start;
                self.selected = Some(extended(anchor, place));
            }
            _ => {
                self.reversed = false;
                self.selected = Some(pressed.clone());
                self.anchor = Some(pressed);
            }
        }
    }

    /// Carries the selection to where the mouse was dragged. Returns whether
    /// that changed it.
    pub fn drag(&mut self, position: Point<Pixels>) -> bool {
        if !self.dragging {
            return false;
        }
        let Some((head, _)) = self.place_at(position) else {
            return false;
        };
        let Some(anchor) = &self.anchor else {
            return false;
        };
        let selected = Some(extended(anchor, head));
        let changed = selected != self.selected;
        self.reversed = head < anchor.start;
        self.selected = selected;
        changed
    }

    /// Ends a drag. Returns the address of a link that was clicked: pressed
    /// and let go of, with nothing selected on the way.
    pub fn release(&mut self, position: Point<Pixels>) -> Option<Arc<str>> {
        if !std::mem::take(&mut self.dragging) {
            return None;
        }
        let link = self.pressed_link.take()?;
        (self.range().is_none() && self.link_at(position).as_ref() == Some(&link)).then_some(link)
    }
}

/// Text that can be selected: it reports where it is drawn, and paints the
/// part of it that is selected.
pub struct Selectable {
    styled: StyledText,
    text: SharedString,
    row: usize,
    piece: usize,
    part: Option<Part>,
    /// The byte a text cursor is drawn before.
    cursor: Option<usize>,
    links: Links,
    drawn: Rc<RefCell<Vec<Drawn>>>,
}

impl Selectable {
    /// Marks parts of the text as links, each with the address it leads to.
    pub fn links(mut self, links: Vec<(Range<usize>, Arc<str>)>) -> Self {
        self.links = links.into();
        self
    }
}

/// The rectangles that cover `part` of `text`, laid out as `layout`: one for
/// each row of text it reaches. `wraps` says whether long lines were wrapped;
/// `visible` is the area being painted.
fn selected_quads(
    layout: &TextLayout,
    text: &str,
    part: &Part,
    wraps: bool,
    visible: Bounds<Pixels>,
) -> Vec<Bounds<Pixels>> {
    let line_height = layout.line_height();
    let origin = layout.bounds().origin;
    // Stands for a selected line break, which has no width of its own.
    let line_break = line_height * 0.3;
    let (from, to) = (part.range.start, part.range.end);
    let mut quads = Vec::new();
    let mut top = origin.y;
    let mut line_start = 0;
    for line in text.split('\n') {
        let line_end = line_start + line.len();
        // Unwrapped text has one row for each line, so the lines out of
        // view need no measuring.
        if to < line_start || (!wraps && top > visible.bottom()) {
            break;
        }
        let in_view = wraps || top + line_height >= visible.top();
        let mut rows = 1;
        let wrapped = (wraps || (in_view && from <= line_end))
            .then(|| layout.line_layout_for_index(line_start))
            .flatten();
        if let Some(wrapped) = wrapped {
            let unwrapped = &wrapped.unwrapped_layout;
            // Where each row of the line begins, and where the last ends.
            let mut edges = vec![0];
            edges.extend(
                wrapped
                    .wrap_boundaries
                    .iter()
                    .map(|boundary| unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index),
            );
            edges.push(line.len());
            rows = edges.len() - 1;
            if from <= line_end {
                let start = from.max(line_start) - line_start;
                let end = to.min(line_end) - line_start;
                let breaks = to > line_end || (line_end == text.len() && part.continues);
                for (row, edge) in edges.windows(2).enumerate() {
                    let (start, end) = (start.max(edge[0]), end.min(edge[1]));
                    let extra = if breaks && row + 1 == rows {
                        line_break
                    } else {
                        px(0.)
                    };
                    if start > end || (start == end && extra == px(0.)) {
                        continue;
                    }
                    let row_left = unwrapped.x_for_index(edge[0]);
                    let left = unwrapped.x_for_index(start) - row_left;
                    let right = unwrapped.x_for_index(end) - row_left + extra;
                    quads.push(Bounds::new(
                        point(origin.x + left, top + line_height * row as f32),
                        size(right - left, line_height),
                    ));
                }
            }
        }
        top += line_height * rows as f32;
        line_start = line_end + 1;
    }
    quads
}

impl Element for Selectable {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.styled.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.styled
            .prepaint(None, inspector_id, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let drawn = Drawn {
            row: self.row,
            piece: self.piece,
            text: self.text.clone(),
            layout: self.styled.layout().clone(),
            links: self.links.clone(),
        };

        let on_link = drawn.link_at(window.mouse_position()).is_some();
        let cursor = if on_link {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, hitbox);
        if !drawn.links.is_empty() {
            // The pointer changes as it moves on and off a link.
            let hitbox = hitbox.clone();
            let layout = drawn.layout.clone();
            let links = drawn.links.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                let over = layout
                    .index_for_position(event.position)
                    .is_ok_and(|index| links.iter().any(|(range, _)| range.contains(&index)));
                if over != on_link {
                    window.refresh();
                }
            });
        }

        if let Some(part) = &self.part {
            let wraps = window.text_style().white_space == WhiteSpace::Normal;
            let visible = window.content_mask().bounds;
            for quad in selected_quads(&drawn.layout, &self.text, part, wraps, visible) {
                window.paint_quad(fill(quad, theme::selection()));
            }
        }
        self.styled
            .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);
        if let Some(offset) = self.cursor {
            let line_height = drawn.layout.line_height();
            // Text with nothing in it has no lines to say where it is.
            let at = drawn.layout.position_for_index(offset).unwrap_or_else(|| {
                point(bounds.left(), bounds.center().y - line_height / 2.)
            });
            window.paint_quad(fill(
                Bounds::new(
                    point(at.x, at.y + px(1.)),
                    size(px(1.5), line_height - px(2.)),
                ),
                theme::accent(),
            ));
        }
        self.drawn.borrow_mut().push(drawn);
    }
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(row: usize, piece: usize, offset: usize) -> Place {
        Place { row, piece, offset }
    }

    fn covered(range: Range<usize>, continues: bool) -> Option<Part> {
        Some(Part { range, continues })
    }

    #[test]
    fn a_selection_covers_the_pieces_between_its_ends() {
        let selected = place(1, 1, 2)..place(3, 0, 4);
        // Before it, and the piece it starts partway into.
        assert_eq!(part(&selected, 0, 5, "before"), None);
        assert_eq!(part(&selected, 1, 0, "before"), None);
        assert_eq!(part(&selected, 1, 1, "starts"), covered(2..6, true));
        // Whole pieces in between, even ones with nothing in them.
        assert_eq!(part(&selected, 1, 2, "whole"), covered(0..5, true));
        assert_eq!(part(&selected, 2, 0, ""), covered(0..0, true));
        // The piece it ends in, and those after.
        assert_eq!(part(&selected, 3, 0, "ending"), covered(0..4, false));
        assert_eq!(part(&selected, 3, 1, "after"), None);

        // Within one piece.
        let selected = place(0, 0, 1)..place(0, 0, 3);
        assert_eq!(part(&selected, 0, 0, "word"), covered(1..3, false));
        // Touching a piece's edge selects none of it.
        let selected = place(0, 0, 4)..place(1, 0, 0);
        assert_eq!(part(&selected, 0, 0, "word"), None);
        assert_eq!(part(&selected, 1, 0, "next"), None);
    }

    #[test]
    fn a_selection_fits_text_that_changed_under_it() {
        // Everything, however much there is.
        let all = Place::default()..Place::END;
        assert_eq!(part(&all, 7, 3, "any"), covered(0..3, true));
        // An end past the text, or inside a character.
        let selected = place(0, 0, 1)..place(0, 0, 90);
        assert_eq!(part(&selected, 0, 0, "short"), covered(1..5, false));
        let selected = place(0, 0, 0)..place(0, 0, 2);
        assert_eq!(part(&selected, 0, 0, "aé"), covered(0..1, false));
    }

    #[test]
    fn dragging_extends_what_the_press_selected() {
        let word = place(2, 0, 4)..place(2, 0, 9);
        assert_eq!(extended(&word, place(2, 0, 6)), word);
        assert_eq!(extended(&word, place(3, 1, 2)), place(2, 0, 4)..place(3, 1, 2));
        assert_eq!(extended(&word, place(0, 2, 7)), place(0, 2, 7)..place(2, 0, 9));
    }

    #[test]
    fn a_second_click_takes_the_word_and_a_third_the_line() {
        let text = "let total_count = 4;\nnext  line";
        assert_eq!(word_at(text, 6), 4..15);
        // The gap at either end of a word still belongs to it.
        assert_eq!(word_at(text, 4), 4..15);
        assert_eq!(word_at(text, 15), 4..15);
        // Away from any word, one character.
        assert_eq!(word_at(text, 16), 16..17);
        assert_eq!(word_at(text, 26), 26..27);
        assert_eq!(word_at("é!", 3), 2..3);
        assert_eq!(word_at("", 0), 0..0);

        assert_eq!(line_at(text, 6), 0..20);
        assert_eq!(line_at(text, 20), 0..20);
        assert_eq!(line_at(text, 21), 21..31);
        assert_eq!(line_at(text, 99), 21..31);
    }
}
