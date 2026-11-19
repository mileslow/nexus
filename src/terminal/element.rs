//! Paints the terminal grid.
//!
//! Each frame the visible cells are snapshotted into per-row background spans
//! and same-style text batches while the terminal lock is held, then shaped
//! and painted after it is released so the PTY reader is never blocked on
//! text layout. GPUI caches shaped lines across frames, so unchanged rows
//! cost a hash lookup.

use std::ops::Range;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point as AlacPoint;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use gpui::{
    fill, font, outline, point, px, relative, rgb, size, App, BorderStyle, Bounds, ContentMask,
    CursorStyle, Element, ElementId, Entity, FontStyle, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InputHandler, IntoElement, LayoutId, Pixels, Point, Rgba, ShapedLine,
    SharedString, StrikethroughStyle, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
};

use super::{TermSize, TerminalView};
use crate::theme;

const PADDING_X: Pixels = px(10.);
const PADDING_TOP: Pixels = px(6.);

pub struct TerminalElement {
    view: Entity<TerminalView>,
    focused: bool,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>, focused: bool) -> Self {
        Self { view, focused }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// Consecutive cells on one row that share a style.
struct Batch {
    row: usize,
    column: usize,
    cells: usize,
    text: String,
    style: CellStyle,
}

struct BackgroundSpan {
    row: usize,
    columns: Range<usize>,
    color: Hsla,
}

struct CursorSnapshot {
    row: usize,
    column: usize,
    shape: CursorShape,
    text: char,
    wide: bool,
    bold: bool,
}

struct Snapshot {
    batches: Vec<Batch>,
    backgrounds: Vec<BackgroundSpan>,
    cursor: Option<CursorSnapshot>,
    display_offset: usize,
    history_size: usize,
}

pub struct PrepaintState {
    hitbox: Hitbox,
    grid_origin: Point<Pixels>,
    cell: gpui::Size<Pixels>,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    underlines: Vec<Bounds<Pixels>>,
    cursor: Vec<gpui::PaintQuad>,
    cursor_text: Option<(Point<Pixels>, ShapedLine)>,
    marked_text: Option<(Bounds<Pixels>, ShapedLine)>,
    cursor_bounds: Option<Bounds<Pixels>>,
    scrollbar: Option<Bounds<Pixels>>,
    link_hovered: bool,
}

impl Element for TerminalElement {
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
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
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
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let base_font = font(theme::MONO_FONT);
        let font_size = theme::MONO_TEXT_SIZE;
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&base_font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(px(7.5));
        let line_height = (font_size * 1.38).round();
        let cell = size(cell_width, line_height);

        let grid_origin = bounds.origin + point(PADDING_X, PADDING_TOP);
        let usable_width = (bounds.size.width - PADDING_X * 2.).max(cell_width);
        let usable_height = (bounds.size.height - PADDING_TOP).max(line_height);
        let term_size = TermSize {
            cols: ((usable_width / cell_width).floor() as usize).clamp(2, 1000),
            rows: ((usable_height / line_height).floor() as usize).clamp(1, 500),
        };
        self.view.update(cx, |view, _| {
            view.set_layout(bounds, grid_origin, cell, term_size)
        });

        let view = self.view.read(cx);
        let snapshot = snapshot(view, term_size);
        let hovered_link = view.hovered_link().cloned();
        let marked = view.marked_text().map(str::to_owned);

        let cell_origin = |row: usize, column: usize| {
            point(
                grid_origin.x + cell_width * column,
                grid_origin.y + line_height * row,
            )
        };

        let backgrounds = snapshot
            .backgrounds
            .iter()
            .map(|span| {
                (
                    Bounds::new(
                        cell_origin(span.row, span.columns.start),
                        // Overlap by a hair so adjacent spans never show seams.
                        size(cell_width * span.columns.len() + px(0.5), line_height),
                    ),
                    span.color,
                )
            })
            .collect();

        let shape = |text: String, style: CellStyle, color: Hsla| -> ShapedLine {
            let mut run_font = base_font.clone();
            if style.bold {
                run_font.weight = FontWeight::BOLD;
            }
            if style.italic {
                run_font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: text.len(),
                font: run_font,
                color,
                background_color: None,
                underline: style.underline.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: style.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            };
            // Forcing the advance keeps fallback-font glyphs on the grid.
            text_system.shape_line(
                SharedString::from(text),
                font_size,
                &[run],
                Some(cell_width),
            )
        };

        let lines = snapshot
            .batches
            .into_iter()
            .map(|batch| {
                (
                    cell_origin(batch.row, batch.column),
                    shape(batch.text, batch.style, batch.style.fg),
                )
            })
            .collect();

        let mut underlines = Vec::new();
        if let Some(link) = &hovered_link {
            for (line, columns) in &link.spans {
                let row = line + snapshot.display_offset as i32;
                if row < 0 || row as usize >= term_size.rows {
                    continue;
                }
                let origin = cell_origin(row as usize, columns.start);
                underlines.push(Bounds::new(
                    point(origin.x, origin.y + line_height - px(2.)),
                    size(cell_width * columns.len(), px(1.)),
                ));
            }
        }

        let mut cursor = Vec::new();
        let mut cursor_text = None;
        let mut cursor_bounds = None;
        if let Some(snapshot_cursor) = &snapshot.cursor {
            let origin = cell_origin(snapshot_cursor.row, snapshot_cursor.column);
            let width = if snapshot_cursor.wide {
                cell_width * 2.
            } else {
                cell_width
            };
            let block = Bounds::new(origin, size(width, line_height));
            cursor_bounds = Some(block);
            let color = rgb(theme::term_cursor());
            if !self.focused {
                cursor.push(outline(block, color, BorderStyle::Solid));
            } else {
                match snapshot_cursor.shape {
                    CursorShape::Block => {
                        cursor.push(fill(block, color));
                        if snapshot_cursor.text != ' ' {
                            let style = CellStyle {
                                fg: rgb(theme::term_bg()).into(),
                                bold: snapshot_cursor.bold,
                                italic: false,
                                underline: false,
                                strike: false,
                            };
                            cursor_text = Some((
                                origin,
                                shape(snapshot_cursor.text.to_string(), style, style.fg),
                            ));
                        }
                    }
                    CursorShape::Beam => {
                        cursor.push(fill(Bounds::new(origin, size(px(1.5), line_height)), color))
                    }
                    CursorShape::Underline => cursor.push(fill(
                        Bounds::new(
                            point(origin.x, origin.y + line_height - px(2.)),
                            size(width, px(2.)),
                        ),
                        color,
                    )),
                    CursorShape::HollowBlock => {
                        cursor.push(outline(block, color, BorderStyle::Solid))
                    }
                    CursorShape::Hidden => {}
                }
            }
        }

        // In-progress input method composition floats at the cursor.
        let marked_text = marked.and_then(|text| {
            let origin = cursor_bounds?.origin;
            let style = CellStyle {
                fg: rgb(theme::term_fg()).into(),
                bold: false,
                italic: false,
                underline: true,
                strike: false,
            };
            let run = TextRun {
                len: text.len(),
                font: base_font.clone(),
                color: style.fg,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(style.fg),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let line = text_system.shape_line(SharedString::from(text), font_size, &[run], None);
            let bounds = Bounds::new(origin, size(line.width.max(cell_width), line_height));
            Some((bounds, line))
        });

        let scrollbar = (snapshot.display_offset > 0 && snapshot.history_size > 0).then(|| {
            let total = (snapshot.history_size + term_size.rows) as f32;
            let visible = term_size.rows as f32 / total;
            let top = (snapshot.history_size - snapshot.display_offset) as f32 / total;
            let track = bounds.size.height;
            Bounds::new(
                point(bounds.right() - px(5.), bounds.top() + track * top),
                size(px(3.), (track * visible).max(px(18.))),
            )
        });

        PrepaintState {
            hitbox,
            grid_origin,
            cell,
            backgrounds,
            lines,
            underlines,
            cursor,
            cursor_text,
            marked_text,
            cursor_bounds,
            scrollbar,
            link_hovered: hovered_link.is_some(),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            TerminalInputHandler {
                view: self.view.clone(),
                cursor_bounds: state
                    .cursor_bounds
                    .unwrap_or(Bounds::new(state.grid_origin, state.cell)),
            },
            cx,
        );
        window.set_cursor_style(
            if state.link_hovered {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            },
            &state.hitbox,
        );

        let line_height = state.cell.height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (rect, color) in state.backgrounds.drain(..) {
                window.paint_quad(fill(rect, color));
            }
            for (origin, line) in &state.lines {
                let _ = line.paint(*origin, line_height, window, cx);
            }
            for rect in state.underlines.drain(..) {
                window.paint_quad(fill(rect, theme::accent()));
            }
            for quad in state.cursor.drain(..) {
                window.paint_quad(quad);
            }
            if let Some((origin, line)) = &state.cursor_text {
                let _ = line.paint(*origin, line_height, window, cx);
            }
            if let Some((rect, line)) = &state.marked_text {
                window.paint_quad(fill(*rect, rgb(theme::term_bg())));
                let _ = line.paint(rect.origin, line_height, window, cx);
            }
            if let Some(rect) = state.scrollbar {
                window.paint_quad(fill(
                    rect,
                    Rgba {
                        a: 0.35,
                        ..theme::text_muted()
                    },
                ));
            }
        });
    }
}

/// Copies what is needed to draw the visible grid, under the terminal lock.
fn snapshot(view: &TerminalView, size: TermSize) -> Snapshot {
    let term = view.term().lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let display_offset = content.display_offset;
    let selection = content.selection;
    let cursor_point = content.cursor.point;
    let cursor_shape = content.cursor.shape;
    let default_bg = AnsiColor::Named(NamedColor::Background);
    let selection_color: Hsla = theme::selection().into();

    let mut batches: Vec<Batch> = Vec::with_capacity(size.rows * 3);
    let mut backgrounds: Vec<BackgroundSpan> = Vec::new();
    let mut cursor = None;
    let mut current: Option<Batch> = None;
    // Color conversion is the hot path; consecutive cells usually share colors.
    let mut fg_cache: Option<(AnsiColor, Hsla)> = None;
    let mut bg_cache: Option<(AnsiColor, Hsla)> = None;

    let flush = |current: &mut Option<Batch>, batches: &mut Vec<Batch>| {
        if let Some(mut batch) = current.take() {
            let trimmed = batch.text.trim_end_matches(' ').len();
            batch.text.truncate(trimmed);
            if !batch.text.is_empty() {
                batches.push(batch);
            }
        }
    };

    for indexed in content.display_iter {
        let point: AlacPoint = indexed.point;
        let row = point.line.0 + display_offset as i32;
        if row < 0 || row as usize >= size.rows {
            continue;
        }
        let row = row as usize;
        let column = point.column.0;
        let cell = indexed.cell;
        let flags = cell.flags;

        let mut fg = match fg_cache {
            Some((color, hsla)) if color == cell.fg => hsla,
            _ => {
                let hsla = resolve(cell.fg, colors);
                fg_cache = Some((cell.fg, hsla));
                hsla
            }
        };
        let mut bg = if cell.bg == default_bg {
            None
        } else {
            Some(match bg_cache {
                Some((color, hsla)) if color == cell.bg => hsla,
                _ => {
                    let hsla = resolve(cell.bg, colors);
                    bg_cache = Some((cell.bg, hsla));
                    hsla
                }
            })
        };
        if flags.contains(Flags::INVERSE) {
            let old_fg = fg;
            fg = bg.unwrap_or_else(|| rgb(theme::term_bg()).into());
            bg = Some(old_fg);
        }
        if flags.contains(Flags::DIM) {
            fg.a *= 0.6;
        }
        if selection.is_some_and(|range| range.contains(point)) {
            bg = Some(selection_color);
        }

        if let Some(color) = bg {
            match backgrounds.last_mut() {
                Some(span)
                    if span.row == row && span.columns.end == column && span.color == color =>
                {
                    span.columns.end = column + 1
                }
                _ => backgrounds.push(BackgroundSpan {
                    row,
                    columns: column..column + 1,
                    color,
                }),
            }
        }

        if point == cursor_point && cursor_shape != CursorShape::Hidden {
            cursor = Some(CursorSnapshot {
                row,
                column,
                shape: cursor_shape,
                text: if flags.contains(Flags::HIDDEN) {
                    ' '
                } else {
                    cell.c
                },
                wide: flags.contains(Flags::WIDE_CHAR),
                bold: flags.contains(Flags::BOLD),
            });
        }

        // A wide glyph's trailing spacer keeps the one-glyph-per-cell layout.
        let spacer = flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        let c = if spacer || flags.contains(Flags::HIDDEN) || cell.c == '\t' {
            ' '
        } else {
            cell.c
        };
        let style = CellStyle {
            fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strike: flags.contains(Flags::STRIKEOUT),
        };
        let blank = c == ' ' && !style.underline && !style.strike;

        match current.as_mut() {
            Some(batch) if batch.row == row && batch.column + batch.cells == column => {
                if blank {
                    // Spaces never force a new batch; trailing ones are trimmed.
                    batch.text.push(' ');
                    batch.cells += 1;
                    continue;
                }
                if batch.style == style {
                    batch.text.push(c);
                    batch.cells += 1;
                    continue;
                }
            }
            _ => {}
        }
        flush(&mut current, &mut batches);
        if !blank {
            current = Some(Batch {
                row,
                column,
                cells: 1,
                text: c.to_string(),
                style,
            });
        }
    }
    flush(&mut current, &mut batches);

    Snapshot {
        batches,
        backgrounds,
        cursor,
        display_offset,
        history_size: term.grid().history_size(),
    }
}

fn resolve(color: AnsiColor, colors: &Colors) -> Hsla {
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Named(name) => colors[name].unwrap_or_else(|| default_color(name as usize)),
        AnsiColor::Indexed(index) => {
            colors[index as usize].unwrap_or_else(|| default_color(index as usize))
        }
    };
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// The built-in palette for an alacritty color index (0..=268).
pub fn default_color(index: usize) -> Rgb {
    fn hex(value: u32) -> Rgb {
        Rgb {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }
    fn dim(color: Rgb) -> Rgb {
        Rgb {
            r: (color.r as f32 * 0.66) as u8,
            g: (color.g as f32 * 0.66) as u8,
            b: (color.b as f32 * 0.66) as u8,
        }
    }
    match index {
        0..=15 => hex(theme::ansi(index)),
        16..=231 => {
            let index = index - 16;
            let level = |value: usize| {
                if value == 0 {
                    0
                } else {
                    (55 + 40 * value) as u8
                }
            };
            Rgb {
                r: level(index / 36),
                g: level((index / 6) % 6),
                b: level(index % 6),
            }
        }
        232..=255 => {
            let gray = (8 + 10 * (index - 232)) as u8;
            Rgb {
                r: gray,
                g: gray,
                b: gray,
            }
        }
        // NamedColor::Foreground = 256, Background, Cursor, Dim*, BrightForeground, DimForeground.
        256 => hex(theme::term_fg()),
        257 => hex(theme::term_bg()),
        258 => hex(theme::term_cursor()),
        259..=266 => dim(hex(theme::ansi(index - 259))),
        267 => hex(0xffffff),
        268 => dim(hex(theme::term_fg())),
        _ => hex(theme::term_fg()),
    }
}

/// Routes committed text and IME composition to the terminal. Implemented
/// directly (rather than through `EntityInputHandler`) so press-and-hold can
/// be turned off: held keys must repeat in a terminal.
struct TerminalInputHandler {
    view: Entity<TerminalView>,
    cursor_bounds: Bounds<Pixels>,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .read(cx)
            .marked_text()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| view.commit_text(text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.set_marked_text(Some(new_text.to_string()), cx)
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view
            .update(cx, |view, cx| view.set_marked_text(None, cx));
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        Some(self.cursor_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_covers_cube_and_grays() {
        assert_eq!(default_color(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            default_color(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(default_color(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(default_color(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            default_color(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }
}
