//! Find (`⌘F`): the bar that floats over a terminal or file, and the text
//! matching behind it.

use std::ops::Range;

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    KeyBinding, MouseButton, SharedString, Window,
};

use crate::assets::icon;
use crate::text_input::{self, TextInput, TextInputEvent};
use crate::theme;

actions!(find, [Find, FindNext, FindPrevious]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-f", Find, None),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrevious, None),
        KeyBinding::new("shift-enter", FindPrevious, Some("FindBar")),
    ]);
}

/// Stop counting here; nobody steps through more.
pub const MAX_MATCHES: usize = 9_999;

/// Background of the match the view is parked on, and the text over it.
pub fn current_match_colors() -> (Hsla, Hsla) {
    (gpui::rgb(0xf5c542).into(), gpui::rgb(0x15161c).into())
}

/// Background of every other match.
pub fn match_color() -> Hsla {
    gpui::rgba(0xf5c54255).into()
}

/// Lowercase queries ignore case; one capital letter makes the search exact.
pub fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Where `needle` occurs in `line`, as byte ranges that never overlap.
pub fn find_in_line(line: &str, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if needle.is_empty() || needle.len() > line.len() {
        return found;
    }
    if case_sensitive {
        found.extend(
            line.match_indices(needle)
                .map(|(start, text)| start..start + text.len()),
        );
    } else if needle.is_ascii() {
        // An ASCII needle can only match at ASCII bytes, which are always
        // character boundaries.
        let (hay, needle) = (line.as_bytes(), needle.as_bytes());
        let mut start = 0;
        while start + needle.len() <= hay.len() {
            if hay[start..start + needle.len()].eq_ignore_ascii_case(needle) {
                found.push(start..start + needle.len());
                start += needle.len();
            } else {
                start += 1;
            }
        }
    } else {
        let needle: Vec<char> = needle.chars().collect();
        let mut start = 0;
        while start < line.len() {
            let mut end = start;
            let mut hay = line[start..].chars();
            let matched = needle.iter().all(|&wanted| {
                hay.next().is_some_and(|c| {
                    end += c.len_utf8();
                    c.to_lowercase().eq(wanted.to_lowercase())
                })
            });
            if matched {
                found.push(start..end);
                start = end;
            } else {
                start += line[start..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    found
}

/// The match to move to from `current` when stepping forward or back
/// through `total` matches, wrapping around at either end.
pub fn step(current: Option<usize>, total: usize, forward: bool) -> Option<usize> {
    if total == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => total - 1,
        (Some(current), true) => (current + 1) % total,
        (Some(current), false) => (current + total - 1) % total,
    })
}

pub enum FindEvent {
    /// The query was edited.
    Changed,
    /// Move to the match below the current one.
    Next,
    /// Move to the match above the current one.
    Previous,
    Dismissed,
}

/// The floating find bar. The view that owns it does the searching and
/// reports back what it found.
pub struct FindBar {
    input: Entity<TextInput>,
    /// Where the keyboard goes when the bar is dismissed.
    return_focus: FocusHandle,
    /// Enter steps up instead of down: in a terminal the newest output is
    /// at the bottom, so a search works its way back from there.
    enter_steps_up: bool,
    /// One-based position of the current match, and how many there are.
    position: Option<usize>,
    total: usize,
}

impl EventEmitter<FindEvent> for FindBar {}

impl FindBar {
    pub fn new(return_focus: FocusHandle, enter_steps_up: bool, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Find", cx));
        cx.subscribe(&input, |_, _, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::Changed) {
                cx.emit(FindEvent::Changed);
            }
        })
        .detach();
        Self {
            input,
            return_focus,
            enter_steps_up,
            position: None,
            total: 0,
        }
    }

    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }

    /// Puts the keyboard in the bar with the old query selected, ready to
    /// be typed over.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle);
        self.input
            .update(cx, |input, cx| input.select_everything(cx));
    }

    /// Reports the zero-based current match and the number of matches.
    pub fn set_results(&mut self, current: Option<usize>, total: usize, cx: &mut Context<Self>) {
        let position = current.map(|index| index + 1);
        if (self.position, self.total) != (position, total) {
            self.position = position;
            self.total = total;
            cx.notify();
        }
    }

    /// Steps from the keyboard: `onward` is Enter or `⌘G`, otherwise the
    /// shifted key that goes the other way.
    fn step(&mut self, onward: bool, cx: &mut Context<Self>) {
        cx.emit(if onward == self.enter_steps_up {
            FindEvent::Previous
        } else {
            FindEvent::Next
        });
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.return_focus);
        cx.emit(FindEvent::Dismissed);
    }
}

impl Render for FindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_query = !self.input.read(cx).text().is_empty();
        let total = if self.total >= MAX_MATCHES {
            format!("{MAX_MATCHES}+")
        } else {
            self.total.to_string()
        };
        let (summary, missing) = match (has_query, self.total, self.position) {
            (false, _, _) => (String::new(), false),
            (true, 0, _) => ("No results".to_string(), true),
            (true, _, Some(position)) => (format!("{position} of {total}"), false),
            (true, _, None) => (total, false),
        };
        let button = |id: &'static str, glyph: &'static str| {
            div()
                .id(id)
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::bg_active()))
                .child(icon(glyph, px(13.), theme::text_muted()))
        };

        div()
            .id("find-bar")
            .key_context("FindBar")
            // Clicks and scrolling stay in the bar instead of reaching the
            // terminal or file underneath.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // The input would otherwise keep Enter and Escape to itself.
            .capture_action(cx.listener(|this, _: &text_input::Submit, _, cx| {
                cx.stop_propagation();
                this.step(true, cx);
            }))
            .capture_action(cx.listener(|this, _: &text_input::Cancel, window, cx| {
                cx.stop_propagation();
                this.dismiss(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| {
                cx.stop_propagation();
                this.step(false, cx);
            }))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| {
                cx.stop_propagation();
                this.step(true, cx);
            }))
            .w(px(340.))
            .h(px(32.))
            .pl(px(9.))
            .pr(px(5.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(8.))
            .bg(theme::bg_sidebar())
            .border_1()
            .border_color(theme::border_strong())
            .shadow_lg()
            .font_family(theme::UI_FONT)
            .text_size(theme::UI_TEXT_SIZE)
            .text_color(theme::text())
            .child(icon("search", px(13.), theme::text_faint()))
            .child(div().w(px(2.)).flex_none())
            .child(self.input.clone())
            .child(
                div()
                    .flex_none()
                    .px(px(4.))
                    .text_size(px(11.5))
                    .text_color(if missing {
                        theme::red()
                    } else {
                        theme::text_faint()
                    })
                    .child(SharedString::from(summary)),
            )
            .child(
                button("find-previous", "chevron-up")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindEvent::Previous))),
            )
            .child(
                button("find-next", "chevron-down")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindEvent::Next))),
            )
            .child(
                button("find-close", "close")
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_queries_ignore_case_and_capitals_make_them_exact() {
        assert!(!is_case_sensitive("error"));
        assert!(is_case_sensitive("Error"));

        assert_eq!(
            find_in_line("Error: error ERROR", "error", false),
            vec![0..5, 7..12, 13..18]
        );
        assert_eq!(
            find_in_line("Error: error ERROR", "Error", true),
            vec![0..5]
        );
    }

    #[test]
    fn matches_never_overlap_and_empty_queries_match_nothing() {
        assert_eq!(find_in_line("aaaa", "aa", false), vec![0..2, 2..4]);
        assert_eq!(find_in_line("aaaa", "aa", true), vec![0..2, 2..4]);
        assert!(find_in_line("anything", "", false).is_empty());
        assert!(find_in_line("ab", "abc", false).is_empty());
    }

    #[test]
    fn ranges_are_in_bytes_and_respect_character_boundaries() {
        // An ASCII needle after multi-byte text.
        let line = "café → Done";
        let found = find_in_line(line, "done", false);
        assert_eq!(found.len(), 1);
        assert_eq!(&line[found[0].clone()], "Done");

        // A needle that is not ASCII, in another case.
        let line = "naïve CAFÉ café";
        let found = find_in_line(line, "café", false);
        let texts: Vec<&str> = found.iter().map(|range| &line[range.clone()]).collect();
        assert_eq!(texts, vec!["CAFÉ", "café"]);
    }

    #[test]
    fn stepping_wraps_around_both_ends() {
        assert_eq!(step(None, 0, true), None);
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(None, 3, false), Some(2));
        assert_eq!(step(Some(2), 3, true), Some(0));
        assert_eq!(step(Some(0), 3, false), Some(2));
        assert_eq!(step(Some(1), 3, true), Some(2));
    }
}
