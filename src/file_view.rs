//! A read-only file viewer: virtualized lines, background syntax
//! highlighting, jump-to-line, and automatic reload when the file changes.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::{
    div, img, prelude::*, px, uniform_list, App, ClipboardItem, Context, FocusHandle, Focusable,
    HighlightStyle, Hsla, ListHorizontalSizingBehavior, ObjectFit, Rgba, ScrollStrategy,
    SharedString, StyledText, Task, UniformListScrollHandle, Window,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

use crate::theme;

const MAX_FILE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 3000;
const MAX_HIGHLIGHT_LINES: usize = 40_000;
const HIGHLIGHT_CHUNK: usize = 400;
const LINE_HEIGHT: f32 = 19.;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];

type LineHighlights = Arc<[(Range<usize>, Hsla)]>;

enum Content {
    Loading,
    Text {
        lines: Arc<Vec<SharedString>>,
        highlights: Vec<Option<LineHighlights>>,
        longest: usize,
    },
    Image,
    Message(SharedString),
}

enum Loaded {
    Text {
        lines: Vec<SharedString>,
        longest: usize,
    },
    Image,
    Message(SharedString),
}

pub struct FileView {
    path: PathBuf,
    /// Shown above the content; relative to the workspace when possible.
    display_path: SharedString,
    focus_handle: FocusHandle,
    content: Content,
    scroll: UniformListScrollHandle,
    highlighted_line: Option<usize>,
    pending_scroll: Option<usize>,
    mtime: Option<SystemTime>,
    checking: bool,
    cancel_highlight: Arc<AtomicBool>,
    _load: Option<Task<()>>,
    _highlight: Option<Task<()>>,
}

impl Focusable for FileView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for FileView {
    fn drop(&mut self) {
        self.cancel_highlight.store(true, Ordering::Relaxed);
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn syntax_theme() -> &'static Theme {
    static THEMES: OnceLock<two_face::theme::EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES
        .get_or_init(two_face::theme::extra)
        .get(two_face::theme::EmbeddedThemeName::OneHalfDark)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn load(path: &Path) -> (Loaded, Option<SystemTime>) {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            return (
                Loaded::Message(format!("Cannot open file: {error}").into()),
                None,
            )
        }
    };
    let mtime = meta.modified().ok();
    if meta.is_dir() {
        return (Loaded::Message("This is a folder.".into()), mtime);
    }
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return (Loaded::Image, mtime);
    }
    if meta.len() > MAX_FILE_BYTES {
        let megabytes = meta.len() as f64 / (1024. * 1024.);
        return (
            Loaded::Message(format!("File is too large to preview ({megabytes:.0} MB).").into()),
            mtime,
        );
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                Loaded::Message(format!("Cannot read file: {error}").into()),
                mtime,
            )
        }
    };
    if bytes.iter().take(8192).any(|&byte| byte == 0) {
        return (Loaded::Message("Binary file — no preview.".into()), mtime);
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut longest = 0;
    let mut longest_len = 0;
    let mut lines: Vec<SharedString> = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = prepare_line(raw.strip_suffix('\r').unwrap_or(raw));
        if line.len() > longest_len {
            longest_len = line.len();
            longest = index;
        }
        lines.push(line.into());
    }
    // A trailing newline is not an extra empty line.
    if lines.len() > 1 && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
        longest = longest.min(lines.len() - 1);
    }
    (Loaded::Text { lines, longest }, mtime)
}

/// Expands tabs and bounds pathological line lengths (minified bundles).
fn prepare_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut column = 0;
    for c in raw.chars() {
        if column >= MAX_LINE_CHARS {
            out.push('…');
            break;
        }
        if c == '\t' {
            let width = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else if c.is_control() {
            out.push(' ');
            column += 1;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

/// Highlights `lines` in chunks so the first screens color in quickly.
fn highlight_lines(
    path: &Path,
    lines: &[SharedString],
    cancel: &AtomicBool,
    mut emit: impl FnMut(usize, Vec<LineHighlights>),
) {
    let set = syntaxes();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    let syntax = set
        .find_syntax_by_extension(&name)
        .or_else(|| set.find_syntax_by_extension(&extension))
        .or_else(|| {
            lines
                .first()
                .and_then(|line| set.find_syntax_by_first_line(line))
        });
    let Some(syntax) = syntax else { return };
    if syntax.name == "Plain Text" {
        return;
    }

    let mut highlighter = HighlightLines::new(syntax, syntax_theme());
    let default_color = theme::text();
    let mut buffer = String::new();
    let limit = lines.len().min(MAX_HIGHLIGHT_LINES);
    let mut start = 0;
    while start < limit {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let end = (start + HIGHLIGHT_CHUNK).min(limit);
        let mut chunk = Vec::with_capacity(end - start);
        for line in &lines[start..end] {
            buffer.clear();
            buffer.push_str(line);
            buffer.push('\n');
            let mut spans: Vec<(Range<usize>, Hsla)> = Vec::new();
            if let Ok(regions) = highlighter.highlight_line(&buffer, set) {
                let mut offset = 0;
                for (style, text) in regions {
                    let len = text.len();
                    let span_end = (offset + len).min(line.len());
                    let color = Rgba {
                        r: style.foreground.r as f32 / 255.,
                        g: style.foreground.g as f32 / 255.,
                        b: style.foreground.b as f32 / 255.,
                        a: 1.,
                    };
                    if span_end > offset && color != default_color {
                        let color: Hsla = color.into();
                        match spans.last_mut() {
                            Some((range, last)) if range.end == offset && *last == color => {
                                range.end = span_end
                            }
                            _ => spans.push((offset..span_end, color)),
                        }
                    }
                    offset += len;
                }
            }
            chunk.push(spans.into());
        }
        emit(start, chunk);
        start = end;
    }
}

impl FileView {
    pub fn new(path: PathBuf, root: &Path, line: Option<u32>, cx: &mut Context<Self>) -> Self {
        let display_path = path
            .strip_prefix(root)
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let mut this = Self {
            path,
            display_path: display_path.into(),
            focus_handle: cx.focus_handle(),
            content: Content::Loading,
            scroll: UniformListScrollHandle::new(),
            highlighted_line: None,
            pending_scroll: None,
            mtime: None,
            checking: false,
            cancel_highlight: Arc::new(AtomicBool::new(false)),
            _load: None,
            _highlight: None,
        };
        this.go_to_line(line, cx);
        this.reload(cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
            .into()
    }

    /// Scrolls to and highlights a 1-based line number.
    pub fn go_to_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        let Some(line) = line.filter(|&line| line > 0) else {
            return;
        };
        let index = line as usize - 1;
        self.highlighted_line = Some(index);
        match &self.content {
            Content::Text { lines, .. } => {
                self.scroll.scroll_to_item_strict(
                    index.min(lines.len().saturating_sub(1)),
                    ScrollStrategy::Center,
                );
            }
            _ => self.pending_scroll = Some(index),
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let (loaded, mtime) = cx.background_spawn(async move { load(&path) }).await;
            let _ = this.update(cx, |this, cx| this.apply(loaded, mtime, cx));
        }));
    }

    fn apply(&mut self, loaded: Loaded, mtime: Option<SystemTime>, cx: &mut Context<Self>) {
        self.mtime = mtime;
        self.cancel_highlight.store(true, Ordering::Relaxed);
        self._highlight = None;
        self.content = match loaded {
            Loaded::Image => Content::Image,
            Loaded::Message(message) => Content::Message(message),
            Loaded::Text { lines, longest } => {
                let lines = Arc::new(lines);
                if let Some(index) = self.pending_scroll.take() {
                    self.scroll.scroll_to_item_strict(
                        index.min(lines.len().saturating_sub(1)),
                        ScrollStrategy::Center,
                    );
                }
                self.start_highlighting(lines.clone(), cx);
                Content::Text {
                    highlights: vec![None; lines.len()],
                    lines,
                    longest,
                }
            }
        };
        cx.notify();
    }

    fn start_highlighting(&mut self, lines: Arc<Vec<SharedString>>, cx: &mut Context<Self>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_highlight = cancel.clone();
        let path = self.path.clone();
        let (tx, mut rx) = unbounded::<(usize, Vec<LineHighlights>)>();
        let worker_lines = lines.clone();
        cx.background_spawn(async move {
            highlight_lines(&path, &worker_lines, &cancel, |start, chunk| {
                let _ = tx.unbounded_send((start, chunk));
            });
        })
        .detach();
        self._highlight = Some(cx.spawn(async move |this, cx| {
            while let Some((start, chunk)) = rx.next().await {
                let applied = this.update(cx, |this, cx| {
                    let Content::Text {
                        lines: current,
                        highlights,
                        ..
                    } = &mut this.content
                    else {
                        return false;
                    };
                    // A reload replaced the text these spans were computed for.
                    if !Arc::ptr_eq(current, &lines) {
                        return false;
                    }
                    for (offset, spans) in chunk.into_iter().enumerate() {
                        if let Some(slot) = highlights.get_mut(start + offset) {
                            *slot = Some(spans);
                        }
                    }
                    cx.notify();
                    true
                });
                if !matches!(applied, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Reloads the file if it changed on disk since it was read.
    pub fn check_for_changes(&mut self, cx: &mut Context<Self>) {
        if self.checking || matches!(self.content, Content::Loading) {
            return;
        }
        self.checking = true;
        let path = self.path.clone();
        let known = self.mtime;
        cx.spawn(async move |this, cx| {
            let current = cx.background_spawn(async move { file_mtime(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                this.checking = false;
                if current != known {
                    this.reload(cx);
                }
            });
        })
        .detach();
    }

    pub fn copy_path(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.path.to_string_lossy().into_owned(),
        ));
    }

    fn render_lines(
        &self,
        range: Range<usize>,
        gutter_width: gpui::Pixels,
    ) -> Vec<impl IntoElement + use<>> {
        let Content::Text {
            lines, highlights, ..
        } = &self.content
        else {
            return Vec::new();
        };
        range
            .map(|index| {
                let line = lines[index].clone();
                let text = match highlights.get(index).and_then(|spans| spans.as_ref()) {
                    Some(spans) if !spans.is_empty() => {
                        StyledText::new(line).with_highlights(spans.iter().map(|(range, color)| {
                            (
                                range.clone(),
                                HighlightStyle {
                                    color: Some(*color),
                                    ..Default::default()
                                },
                            )
                        }))
                    }
                    _ => StyledText::new(line),
                };
                div()
                    .h(px(LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .when(self.highlighted_line == Some(index), |row| {
                        row.bg(theme::line_highlight())
                    })
                    .child(
                        div()
                            .w(gutter_width)
                            .flex_none()
                            .flex()
                            .justify_end()
                            .pr(px(14.))
                            .text_color(if self.highlighted_line == Some(index) {
                                theme::text_muted()
                            } else {
                                theme::text_faint()
                            })
                            .child(SharedString::from((index + 1).to_string())),
                    )
                    .child(div().pr(px(24.)).child(text))
            })
            .collect()
    }
}

impl Render for FileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            Content::Loading => div().into_any_element(),
            Content::Message(message) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::text_faint())
                .child(message.clone())
                .into_any_element(),
            Content::Image => div()
                .size_full()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(self.path.clone())
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Content::Text { lines, longest, .. } => {
                let digits = lines.len().max(1).to_string().len().max(3);
                let gutter_width = px(digits as f32 * 7.6 + 30.);
                uniform_list(
                    "file-lines",
                    lines.len(),
                    cx.processor(move |this, range: Range<usize>, _window, _cx| {
                        this.render_lines(range, gutter_width)
                    }),
                )
                .track_scroll(self.scroll.clone())
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(Some(*longest))
                .size_full()
                .into_any_element()
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_app())
            .track_focus(&self.focus_handle)
            .key_context("FileView")
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(px(11.5))
                    .text_color(theme::text_faint())
                    .child(
                        div()
                            .id("file-path")
                            .truncate()
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::text_muted()))
                            .child(self.display_path.clone())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_path(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::MONO_TEXT_SIZE)
                    .text_color(theme::text())
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_expand_to_stops_and_long_lines_truncate() {
        assert_eq!(prepare_line("\ta\tb"), "    a   b");
        let long = "x".repeat(MAX_LINE_CHARS + 50);
        assert_eq!(prepare_line(&long).chars().count(), MAX_LINE_CHARS + 1);
    }

    #[test]
    fn loads_text_and_reports_binary() {
        let dir = std::env::temp_dir().join(format!("nexus-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let text_path = dir.join("a.txt");
        fs::write(&text_path, "one\r\ntwo longer\n").unwrap();
        match load(&text_path).0 {
            Loaded::Text { lines, longest } => {
                assert_eq!(lines, vec![SharedString::from("one"), "two longer".into()]);
                assert_eq!(longest, 1);
            }
            _ => panic!("expected text"),
        }
        let binary_path = dir.join("b.bin");
        fs::write(&binary_path, [1u8, 0, 2]).unwrap();
        assert!(matches!(load(&binary_path).0, Loaded::Message(_)));
        assert!(matches!(load(&dir.join("missing")).0, Loaded::Message(_)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn highlights_rust_source() {
        let lines: Vec<SharedString> =
            vec!["fn main() {".into(), "    let x = 1;".into(), "}".into()];
        let mut seen = Vec::new();
        highlight_lines(
            Path::new("main.rs"),
            &lines,
            &AtomicBool::new(false),
            |start, chunk| seen.push((start, chunk)),
        );
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1.len(), 3);
        assert!(!seen[0].1[0].is_empty());
        for (line, spans) in lines.iter().zip(&seen[0].1) {
            for (range, _) in spans.iter() {
                assert!(range.end <= line.len() && line.is_char_boundary(range.end));
            }
        }
    }
}
