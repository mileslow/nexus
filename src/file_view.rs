//! A read-only file viewer: virtualized lines, background syntax
//! highlighting, jump-to-line, automatic reload when the file changes, and an
//! inline diff of what changed since the last commit.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::{
    actions, canvas, combine_highlights, div, img, list, point, prelude::*, px, uniform_list,
    AnyElement, App, Bounds, ClipboardItem, Context, DispatchPhase, Entity, EventEmitter,
    FocusHandle, Focusable, HighlightStyle, Hsla, KeyBinding, ListAlignment,
    ListHorizontalSizingBehavior, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, Pixels, Rgba, ScrollStrategy, SharedString, StyledText, Task,
    UniformListScrollHandle, Window,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

use crate::assets::icon;
use crate::diff::{self, Baseline, Diff, RowKind};
use crate::find::{self, FindBar, FindEvent};
use crate::markdown::{self, Target};
use crate::platform::keys;
use crate::selection::{self, Selection};
use crate::terminal;
use crate::theme;

actions!(file_view, [SelectAll]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("FileView");
    cx.bind_keys([
        // The same Copy the Edit menu offers.
        KeyBinding::new(keys("cmd-c", "ctrl-c"), terminal::Copy, ctx),
        KeyBinding::new(keys("cmd-a", "ctrl-a"), SelectAll, ctx),
    ]);
}

const MAX_FILE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 3000;
const MAX_HIGHLIGHT_LINES: usize = 40_000;
const HIGHLIGHT_CHUNK: usize = 400;
const LINE_HEIGHT: f32 = 19.;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd", "mdx"];
/// A Markdown file bigger than this opens as its source; the formatted
/// page lays out all of it at once.
const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;
/// Code blocks longer than this are left uncolored.
const MAX_CODE_BLOCK_BYTES: usize = 256 * 1024;

pub enum FileViewEvent {
    /// A link in the file points at this file or folder.
    OpenPath(PathBuf),
    /// The file's path, above its content, was right-clicked here.
    PathMenu(gpui::Point<Pixels>),
}

type LineHighlights = Arc<[(Range<usize>, Hsla)]>;
type Lines = Arc<Vec<SharedString>>;

/// How the file differs from its baseline.
struct Changes {
    base: Lines,
    diff: Diff,
    /// The widest row of the unified view, for horizontal scrolling.
    longest_row: usize,
}

struct TextContent {
    lines: Lines,
    highlights: Vec<Option<LineHighlights>>,
    longest: usize,
    changes: Option<Changes>,
    /// What the file is compared against, shown beside the path.
    baseline_note: Option<&'static str>,
}

enum Content {
    Loading,
    Text(TextContent),
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

/// Everything read from disk for one (re)load, gathered off the UI thread.
struct Snapshot {
    loaded: Loaded,
    mtime: Option<SystemTime>,
    changes: Option<Changes>,
    baseline_note: Option<&'static str>,
    in_repository: bool,
    git_watch: Option<PathBuf>,
    git_stamp: Option<SystemTime>,
}

pub struct FileView {
    path: PathBuf,
    /// Shown above the content; relative to the workspace when possible.
    display_path: SharedString,
    focus_handle: FocusHandle,
    content: Content,
    scroll: UniformListScrollHandle,
    /// Zero-based line (of the current file) to highlight.
    highlighted_line: Option<usize>,
    pending_scroll: Option<usize>,
    /// Show removed lines inline. Off shows the plain file with change marks.
    show_diff: bool,
    hunk_cursor: Option<usize>,
    /// The text when the view first loaded: the baseline outside git.
    opened: Option<Lines>,
    mtime: Option<SystemTime>,
    git_watch: Option<PathBuf>,
    git_stamp: Option<SystemTime>,
    checking: bool,
    /// The theme the current highlights were computed for.
    highlight_theme: usize,
    cancel_highlight: Arc<AtomicBool>,
    find_bar: Entity<FindBar>,
    find_open: bool,
    /// Every match of the find query: its line and byte range, in order.
    find_matches: Vec<(usize, Range<usize>)>,
    find_current: Option<usize>,
    /// Measured while drawing, to bring a match on a long line into view.
    gutter_width: Pixels,
    char_width: Pixels,
    /// Whether this is a Markdown file, which can be shown formatted.
    is_markdown: bool,
    /// Show the formatted page rather than the source.
    preview: bool,
    /// The file read as Markdown, once that is done.
    markdown: Option<markdown::Document>,
    /// The formatted page: one row for each top-level block.
    page: ListState,
    /// The text selected with the mouse, in whichever of the page and the
    /// source is showing.
    selection: Selection,
    _load: Option<Task<()>>,
    _highlight: Option<Task<()>>,
    _markdown: Option<Task<()>>,
}

impl EventEmitter<FileViewEvent> for FileView {}

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

/// Builds a syntect theme from a color theme's own token colors.
fn custom_syntax_theme(colors: &theme::SyntaxColors, text: u32) -> Theme {
    use std::str::FromStr;
    use syntect::highlighting::{Color, ScopeSelectors, StyleModifier, ThemeItem, ThemeSettings};
    let color = |hex: u32| Color {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
        a: 0xff,
    };
    // Later rules win over earlier ones of the same specificity.
    let rules = [
        ("comment, punctuation.definition.comment", colors.comment),
        ("string", colors.string),
        ("constant.numeric", colors.number),
        (
            "constant.language, constant.character.escape, constant.other",
            colors.constant,
        ),
        ("keyword, storage", colors.keyword),
        ("keyword.operator", colors.operator),
        (
            "entity.name.function, support.function, variable.function",
            colors.function,
        ),
        (
            "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
             entity.name.trait, support.type, support.class, entity.other.inherited-class",
            colors.type_name,
        ),
        ("entity.name.tag", colors.tag),
        ("entity.other.attribute-name", colors.attribute),
    ];
    Theme {
        name: None,
        author: None,
        settings: ThemeSettings {
            // Unmatched text reports this color, which marks it as plain.
            foreground: Some(color(text)),
            ..ThemeSettings::default()
        },
        scopes: rules
            .into_iter()
            .filter_map(|(selector, hex)| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(selector).ok()?,
                    style: StyleModifier {
                        foreground: Some(color(hex)),
                        background: None,
                        font_style: None,
                    },
                })
            })
            .collect(),
    }
}

/// The syntect theme that goes with the active color theme.
fn syntax_theme() -> &'static Theme {
    use two_face::theme::EmbeddedThemeName as Name;
    static BUNDLED: OnceLock<two_face::theme::EmbeddedLazyThemeSet> = OnceLock::new();
    static CUSTOM: OnceLock<std::sync::Mutex<std::collections::HashMap<usize, &'static Theme>>> =
        OnceLock::new();
    let bundled = |name: Name| BUNDLED.get_or_init(two_face::theme::extra).get(name);
    match theme::active().syntax {
        theme::Syntax::OneHalfDark => bundled(Name::OneHalfDark),
        theme::Syntax::TwoDark => bundled(Name::TwoDark),
        theme::Syntax::CatppuccinMocha => bundled(Name::CatppuccinMocha),
        theme::Syntax::GruvboxDark => bundled(Name::GruvboxDark),
        theme::Syntax::Nord => bundled(Name::Nord),
        theme::Syntax::Github => bundled(Name::Github),
        theme::Syntax::CatppuccinLatte => bundled(Name::CatppuccinLatte),
        theme::Syntax::Dracula => bundled(Name::Dracula),
        theme::Syntax::SolarizedDark => bundled(Name::SolarizedDark),
        theme::Syntax::SolarizedLight => bundled(Name::SolarizedLight),
        theme::Syntax::Custom(colors) => {
            // Built once per theme and kept for the life of the app.
            let mut built = CUSTOM
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            built.entry(theme::active_index()).or_insert_with(|| {
                Box::leak(Box::new(custom_syntax_theme(colors, theme::term_fg())))
            })
        }
    }
}

fn rgba(color: syntect::highlighting::Color) -> Rgba {
    Rgba {
        r: color.r as f32 / 255.,
        g: color.g as f32 / 255.,
        b: color.b as f32 / 255.,
        a: 1.,
    }
}

/// Colors for a block of code in a named language, as byte ranges of it.
pub fn highlight_code(language: &str, text: &str) -> Vec<(Range<usize>, Hsla)> {
    let mut spans: Vec<(Range<usize>, Hsla)> = Vec::new();
    let set = syntaxes();
    let syntax = set
        .find_syntax_by_token(language)
        .filter(|syntax| syntax.name != "Plain Text");
    let Some(syntax) = syntax.filter(|_| !language.is_empty() && text.len() <= MAX_CODE_BLOCK_BYTES)
    else {
        return spans;
    };
    let syntax_theme = syntax_theme();
    let default_color = syntax_theme.settings.foreground.map(rgba);
    let mut highlighter = HighlightLines::new(syntax, syntax_theme);
    let mut buffer = String::new();
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        // The highlighter wants every line to end with a line break.
        buffer.clear();
        buffer.push_str(line);
        if !line.ends_with('\n') {
            buffer.push('\n');
        }
        if let Ok(regions) = highlighter.highlight_line(&buffer, set) {
            let mut offset = line_start;
            for (style, piece) in regions {
                let end = (offset + piece.len()).min(line_start + line.len());
                let color = rgba(style.foreground);
                if end > offset && Some(color) != default_color {
                    let color: Hsla = color.into();
                    match spans.last_mut() {
                        Some((range, last)) if range.end == offset && *last == color => {
                            range.end = end
                        }
                        _ => spans.push((offset..end, color)),
                    }
                }
                offset += piece.len();
            }
        }
        line_start += line.len();
    }
    spans
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        MARKDOWN_EXTENSIONS.contains(&extension.to_string_lossy().to_lowercase().as_str())
    })
}

/// Where a file opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// As a tab: text, and the pictures the viewer can show.
    Here,
    /// In whatever program the system opens such a file with.
    System,
    /// Shown in the file manager: "opening" a program would run it.
    Reveal,
}

/// Files the system runs rather than shows, which their bytes do not
/// give away.
const RUNNABLE_EXTENSIONS: &[&str] = &["msi", "msp", "lnk", "pif", "jar", "appref-ms"];

/// Decides where a file opens from its extension (lowercase) and its
/// first bytes.
fn opening(extension: &str, head: &[u8]) -> Opening {
    if IMAGE_EXTENSIONS.contains(&extension) || !head.contains(&0) {
        return Opening::Here;
    }
    // Compiled programs and libraries: Windows, Linux, and the Mac's
    // several byte orders.
    const PROGRAMS: &[&[u8]] = &[
        b"MZ",
        b"\x7fELF",
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xce\xfa\xed\xfe",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
    ];
    let program = PROGRAMS.iter().any(|magic| head.starts_with(magic))
        || RUNNABLE_EXTENSIONS.contains(&extension);
    if program {
        Opening::Reveal
    } else {
        Opening::System
    }
}

/// Where `path` opens: here when the viewer can show it, otherwise in the
/// program the system has for it. Reads the start of the file.
pub fn opening_for(path: &Path) -> Opening {
    use std::io::Read as _;
    let mut head = Vec::new();
    let read = fs::File::open(path).and_then(|file| file.take(8192).read_to_end(&mut head));
    if read.is_err() {
        // The viewer says what went wrong.
        return Opening::Here;
    }
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    opening(&extension, &head)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// Splits file bytes into display lines and finds the longest. `None` for
/// binary data.
fn text_lines(bytes: &[u8]) -> Option<(Vec<SharedString>, usize)> {
    if bytes.iter().take(8192).any(|&byte| byte == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
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
    Some((lines, longest))
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
    match text_lines(&bytes) {
        Some((lines, longest)) => (Loaded::Text { lines, longest }, mtime),
        None => (Loaded::Message("Binary file — no preview.".into()), mtime),
    }
}

/// Diffs `lines` against `base`. `None` when nothing changed.
fn changes_against(base: Lines, lines: &[SharedString], longest: usize) -> Option<Changes> {
    let diff = diff::diff_lines(&base, lines)?;
    let mut longest_row = diff.row_of_line.get(longest).copied().unwrap_or(0) as usize;
    let mut longest_len = lines.get(longest).map_or(0, |line| line.len());
    for (row_index, row) in diff.rows.iter().enumerate() {
        if row.kind == RowKind::Removed {
            let len = base[row.index as usize].len();
            if len > longest_len {
                longest_len = len;
                longest_row = row_index;
            }
        }
    }
    Some(Changes {
        base,
        diff,
        longest_row,
    })
}

/// Reads the file and works out what changed. `opened` is the text from when
/// the view first loaded, used as the baseline for files outside git.
fn snapshot(path: &Path, opened: Option<Lines>) -> Snapshot {
    let (loaded, mtime) = load(path);
    let mut snapshot = Snapshot {
        loaded,
        mtime,
        changes: None,
        baseline_note: None,
        in_repository: false,
        git_watch: None,
        git_stamp: None,
    };
    let Loaded::Text { lines, longest } = &snapshot.loaded else {
        return snapshot;
    };

    let git = diff::git_state(path);
    snapshot.git_stamp = git.watch.as_deref().and_then(file_mtime);
    snapshot.git_watch = git.watch;
    snapshot.in_repository = git.baseline != Baseline::NoRepository;
    let base = match git.baseline {
        Baseline::Head(bytes) => {
            snapshot.baseline_note = Some("vs last commit");
            (bytes.len() as u64 <= MAX_FILE_BYTES)
                .then(|| text_lines(&bytes))
                .flatten()
                .map(|(base, _)| Arc::new(base))
        }
        Baseline::Untracked => {
            snapshot.baseline_note = Some("not committed yet");
            None
        }
        Baseline::NoRepository => {
            if opened.is_some() {
                snapshot.baseline_note = Some("since opened");
            }
            opened
        }
    };
    snapshot.changes = base.and_then(|base| changes_against(base, lines, *longest));
    snapshot
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

    let syntax_theme = syntax_theme();
    let mut highlighter = HighlightLines::new(syntax, syntax_theme);
    // Text in the syntax theme's plain color takes the UI text color instead.
    let default_color = syntax_theme.settings.foreground.map(|color| Rgba {
        r: color.r as f32 / 255.,
        g: color.g as f32 / 255.,
        b: color.b as f32 / 255.,
        a: 1.,
    });
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
                    if span_end > offset && Some(color) != default_color {
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
        let focus_handle = cx.focus_handle();
        let find_bar = cx.new(|cx| FindBar::new(focus_handle.clone(), false, cx));
        cx.subscribe(&find_bar, Self::on_find_event).detach();
        let is_markdown = is_markdown(&path);
        let small = fs::metadata(&path).is_ok_and(|meta| meta.len() <= MAX_PREVIEW_BYTES);
        let mut this = Self {
            is_markdown,
            preview: is_markdown && small,
            markdown: None,
            page: ListState::new(0, ListAlignment::Top, px(600.)),
            selection: Selection::default(),
            _markdown: None,
            path,
            display_path: display_path.into(),
            focus_handle,
            content: Content::Loading,
            scroll: UniformListScrollHandle::new(),
            highlighted_line: None,
            pending_scroll: None,
            show_diff: true,
            hunk_cursor: None,
            opened: None,
            mtime: None,
            git_watch: None,
            git_stamp: None,
            checking: false,
            highlight_theme: theme::active_index(),
            cancel_highlight: Arc::new(AtomicBool::new(false)),
            find_bar,
            find_open: false,
            find_matches: Vec::new(),
            find_current: None,
            gutter_width: px(0.),
            char_width: px(7.5),
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

    /// The changes being shown inline, if the unified view is on.
    fn shown_changes(&self) -> Option<&Changes> {
        match &self.content {
            Content::Text(text) if self.show_diff => text.changes.as_ref(),
            _ => None,
        }
    }

    /// The list row that displays a zero-based line of the current file.
    fn row_for_line(&self, line: usize) -> usize {
        let Content::Text(text) = &self.content else {
            return line;
        };
        let line = line.min(text.lines.len().saturating_sub(1));
        match self.shown_changes() {
            Some(changes) => changes.diff.row_of_line[line] as usize,
            None => line,
        }
    }

    /// The list row currently at the top of the viewport.
    fn top_row(&self) -> usize {
        let offset = self.scroll.0.borrow().base_handle.offset().y;
        (-f32::from(offset) / LINE_HEIGHT).max(0.).round() as usize
    }

    /// Scrolls to and highlights a 1-based line number.
    pub fn go_to_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        let Some(line) = line.filter(|&line| line > 0) else {
            return;
        };
        let index = line as usize - 1;
        // A line number means the source, not the formatted page.
        self.set_preview(false);
        self.highlighted_line = Some(index);
        if matches!(self.content, Content::Text(_)) {
            self.scroll
                .scroll_to_item_strict(self.row_for_line(index), ScrollStrategy::Center);
        } else {
            self.pending_scroll = Some(index);
        }
        cx.notify();
    }

    // ---- find -----------------------------------------------------------------

    /// Opens the find bar, or returns to it if it is already open.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Find searches the source, and marks what it finds there.
        self.set_preview(false);
        self.find_open = true;
        self.find_bar.update(cx, |bar, cx| bar.focus(window, cx));
        self.refresh_find(true, cx);
    }

    fn on_find_event(&mut self, _: Entity<FindBar>, event: &FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed => self.refresh_find(true, cx),
            FindEvent::Next => self.find_step(true, cx),
            FindEvent::Previous => self.find_step(false, cx),
            FindEvent::Dismissed => {
                self.find_open = false;
                self.find_matches.clear();
                self.find_current = None;
                cx.notify();
            }
        }
    }

    /// Searches the file again. `jump` moves to the first match at or below
    /// the top of the view; without it the view stays where it is.
    fn refresh_find(&mut self, jump: bool, cx: &mut Context<Self>) {
        let query = self.find_bar.read(cx).query(cx);
        let mut matches = Vec::new();
        if let (true, Content::Text(text)) = (self.find_open, &self.content) {
            let case_sensitive = find::is_case_sensitive(&query);
            'lines: for (index, line) in text.lines.iter().enumerate() {
                for range in find::find_in_line(line, &query, case_sensitive) {
                    matches.push((index, range));
                    if matches.len() >= find::MAX_MATCHES {
                        break 'lines;
                    }
                }
            }
        }
        self.find_matches = matches;
        self.find_current = if self.find_matches.is_empty() {
            None
        } else if jump {
            let top = self.top_row();
            let below = self
                .find_matches
                .iter()
                .position(|(line, _)| self.row_for_line(*line) >= top);
            Some(below.unwrap_or(0))
        } else {
            self.find_current
                .map(|current| current.min(self.find_matches.len() - 1))
        };
        if jump {
            self.reveal_current_match();
        }
        self.report_find(cx);
    }

    /// Steps to the match below (`forward`) or above the current one.
    pub fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.find_open {
            return;
        }
        self.find_current = find::step(self.find_current, self.find_matches.len(), forward);
        self.reveal_current_match();
        self.report_find(cx);
    }

    fn report_find(&mut self, cx: &mut Context<Self>) {
        let (current, total) = (self.find_current, self.find_matches.len());
        self.find_bar
            .update(cx, |bar, cx| bar.set_results(current, total, cx));
        cx.notify();
    }

    /// Scrolls so the current match is on screen, both down and across.
    fn reveal_current_match(&mut self) {
        let Some((line, range)) = self
            .find_current
            .and_then(|current| self.find_matches.get(current))
            .cloned()
        else {
            return;
        };
        let Content::Text(text) = &self.content else {
            return;
        };
        let handle = self.scroll.0.borrow().base_handle.clone();
        let viewport = handle.bounds().size;
        let offset = handle.offset();

        let row = self.row_for_line(line);
        let top = self.top_row();
        let visible_rows = (f32::from(viewport.height) / LINE_HEIGHT).floor() as usize;
        if row < top || row + 1 >= top + visible_rows.max(1) {
            self.scroll
                .scroll_to_item_strict(row, ScrollStrategy::Center);
        }

        // The font is monospaced, so columns are a matter of counting.
        let Some(line_text) = text.lines.get(line) else {
            return;
        };
        let columns_before = line_text[..range.start.min(line_text.len())]
            .chars()
            .count();
        let columns = line_text[range.start.min(line_text.len())..range.end.min(line_text.len())]
            .chars()
            .count();
        let start = self.gutter_width + self.char_width * columns_before as f32;
        let end = start + self.char_width * columns as f32;
        let left = -offset.x;
        let right = left + viewport.width;
        if viewport.width > px(0.) && (end > right - px(24.) || start < left + self.gutter_width) {
            let target = (start - viewport.width / 2.).max(px(0.));
            handle.set_offset(point(-target, offset.y));
        }
    }

    /// The matches on one line: the index of the first, and their ranges.
    fn matches_on_line(&self, line: usize) -> (usize, &[(usize, Range<usize>)]) {
        let start = self.find_matches.partition_point(|(at, _)| *at < line);
        let end = self.find_matches.partition_point(|(at, _)| *at <= line);
        (start, &self.find_matches[start..end])
    }

    /// Switches between the unified diff and the plain file, keeping the
    /// same part of the file on screen.
    fn toggle_diff(&mut self, cx: &mut Context<Self>) {
        let Content::Text(text) = &self.content else {
            return;
        };
        let Some(changes) = &text.changes else {
            return;
        };
        let top = self.top_row();
        let new_top = if self.show_diff {
            changes.diff.line_of_row(top)
        } else {
            let line = top.min(text.lines.len().saturating_sub(1));
            changes.diff.row_of_line[line] as usize
        };
        self.show_diff = !self.show_diff;
        // The rows it was made on are not the rows showing now.
        self.selection.clear();
        self.scroll
            .scroll_to_item_strict(new_top, ScrollStrategy::Top);
        cx.notify();
    }

    /// Scrolls to the next (`+1`) or previous (`-1`) run of changes.
    fn step_change(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Content::Text(text) = &self.content else {
            return;
        };
        let Some(changes) = &text.changes else {
            return;
        };
        let count = changes.diff.hunks.len() as isize;
        if count == 0 {
            return;
        }
        let next = match self.hunk_cursor {
            Some(current) => (current as isize + delta).rem_euclid(count),
            None if delta < 0 => count - 1,
            None => 0,
        } as usize;
        let row = changes.diff.hunks[next] as usize;
        let target = if self.show_diff {
            row
        } else {
            changes.diff.line_of_row(row)
        };
        self.hunk_cursor = Some(next);
        self.scroll
            .scroll_to_item_strict(target, ScrollStrategy::Center);
        cx.notify();
    }

    /// Reads the file as Markdown again, off the UI thread.
    fn read_markdown(&mut self, cx: &mut Context<Self>) {
        if !self.is_markdown {
            return;
        }
        let path = self.path.clone();
        self._markdown = Some(cx.spawn(async move |this, cx| {
            let document = cx
                .background_spawn(async move {
                    let bytes = fs::read(&path).unwrap_or_default();
                    markdown::parse(&String::from_utf8_lossy(&bytes), &highlight_code)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A file that changed while open keeps its place on screen.
                let top = this.page.logical_scroll_top();
                this.page.reset(document.blocks.len());
                this.page.scroll_to(top);
                this.markdown = Some(document);
                if this.preview {
                    this.selection.clear();
                }
                cx.notify();
            });
        }));
    }

    fn render_page_row(&self, index: usize) -> AnyElement {
        let Some(document) = &self.markdown else {
            return div().into_any_element();
        };
        let folder = self.path.parent().unwrap_or(Path::new(""));
        markdown::render_block(document, index, folder, &self.selection)
    }

    /// Shows the formatted page or the source. A selection made in one
    /// means nothing in the other.
    fn set_preview(&mut self, preview: bool) {
        if self.preview != preview {
            self.preview = preview;
            self.selection.clear();
        }
    }

    /// Switches a Markdown file between the formatted page and its source.
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.set_preview(!self.preview);
        cx.notify();
    }

    // ---- selection ------------------------------------------------------------

    /// Whether the formatted page, rather than the source, is showing.
    fn previewing(&self) -> bool {
        self.preview && matches!(self.content, Content::Text(_))
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.selection
            .press(event.position, event.click_count, event.modifiers.shift);
        cx.notify();
    }

    /// Carries the selection to where the mouse was dragged. `bounds` is
    /// the area the text is shown in.
    fn on_drag(&mut self, event: &MouseMoveEvent, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if !self.selection.is_dragging() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The button was let go where the window did not hear of it.
            self.selection.release(event.position);
            return;
        }
        // Dragging past the top or bottom scrolls on to more of the text.
        let y = event.position.y;
        let past = (y - bounds.bottom()).max(px(0.)) + (y - bounds.top()).min(px(0.));
        if past != px(0.) {
            if self.previewing() {
                self.page.scroll_by(past);
            } else {
                let handle = self.scroll.0.borrow().base_handle.clone();
                let offset = handle.offset();
                let top = (offset.y - past).clamp(-handle.max_offset().height, px(0.));
                handle.set_offset(point(offset.x, top));
            }
        }
        if self.selection.drag(event.position) || past != px(0.) {
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if !self.selection.is_dragging() {
            return;
        }
        // Pressing and letting go of a link, without selecting, follows it.
        if let Some(address) = self.selection.release(event.position) {
            self.follow_link(&address, cx);
        }
        cx.notify();
    }

    /// The text of a row of the source: a line of the file or, among its
    /// changes, one that was removed from it.
    fn row_text<'a>(&'a self, text: &'a TextContent, row: usize) -> Option<&'a SharedString> {
        let Some(changes) = self.shown_changes() else {
            return text.lines.get(row);
        };
        let shown = changes.diff.rows.get(row)?;
        match shown.kind {
            RowKind::Removed => changes.base.get(shown.index as usize),
            _ => text.lines.get(shown.index as usize),
        }
    }

    /// The text selected with the mouse, if any.
    fn selected_text(&self) -> Option<String> {
        let selected = self.selection.range()?;
        let Content::Text(text) = &self.content else {
            return None;
        };
        if self.preview {
            return Some(self.markdown.as_ref()?.text_between(&selected));
        }
        let rows = match self.shown_changes() {
            Some(changes) => changes.diff.rows.len(),
            None => text.lines.len(),
        };
        let mut out = String::new();
        for row in selected.start.row..rows.min(selected.end.row.saturating_add(1)) {
            let Some(line) = self.row_text(text, row) else {
                break;
            };
            let Some(part) = selection::part(&selected, row, 0, line) else {
                continue;
            };
            out.push_str(&line[part.range]);
            if part.continues && row + 1 < rows {
                out.push('\n');
            }
        }
        Some(out)
    }

    fn copy(&mut self, _: &terminal::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.content, Content::Text(_)) {
            self.selection.select_all();
            cx.notify();
        }
    }

    /// Goes where a link in the formatted page points.
    fn follow_link(&mut self, address: &str, cx: &mut Context<Self>) {
        let folder = self.path.parent().unwrap_or(Path::new(""));
        match markdown::target(address, folder) {
            Target::Url(url) => cx.open_url(&url),
            Target::File(path) if path.exists() => cx.emit(FileViewEvent::OpenPath(path)),
            Target::File(_) | Target::Anchor => {}
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let opened = self.opened.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_spawn(async move { snapshot(&path, opened) })
                .await;
            let _ = this.update(cx, |this, cx| this.apply(snapshot, cx));
        }));
    }

    fn apply(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.mtime = snapshot.mtime;
        self.git_watch = snapshot.git_watch;
        self.git_stamp = snapshot.git_stamp;
        self.cancel_highlight.store(true, Ordering::Relaxed);
        self._highlight = None;
        // What was selected may not be there any more.
        self.selection.clear();
        self.content = match snapshot.loaded {
            Loaded::Image => Content::Image,
            Loaded::Message(message) => Content::Message(message),
            Loaded::Text { lines, longest } => {
                let lines = Arc::new(lines);
                if !snapshot.in_repository && self.opened.is_none() {
                    self.opened = Some(lines.clone());
                }
                self.start_highlighting(lines.clone(), cx);
                Content::Text(TextContent {
                    highlights: vec![None; lines.len()],
                    lines,
                    longest,
                    changes: snapshot.changes,
                    baseline_note: snapshot.baseline_note,
                })
            }
        };
        if matches!(self.content, Content::Text(_)) {
            self.read_markdown(cx);
        }
        // The set of changes may be different now, and so may the matches.
        self.hunk_cursor = None;
        if self.find_open {
            self.refresh_find(false, cx);
        }
        if matches!(self.content, Content::Text(_)) {
            if let Some(index) = self.pending_scroll.take() {
                self.scroll
                    .scroll_to_item_strict(self.row_for_line(index), ScrollStrategy::Center);
            }
        }
        cx.notify();
    }

    fn start_highlighting(&mut self, lines: Lines, cx: &mut Context<Self>) {
        self.cancel_highlight.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_highlight = cancel.clone();
        self.highlight_theme = theme::active_index();
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
                    let Content::Text(text) = &mut this.content else {
                        return false;
                    };
                    // A reload replaced the text these spans were computed for.
                    if !Arc::ptr_eq(&text.lines, &lines) {
                        return false;
                    }
                    for (offset, spans) in chunk.into_iter().enumerate() {
                        if let Some(slot) = text.highlights.get_mut(start + offset) {
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

    /// Reloads if the file changed on disk, or if a commit, checkout or
    /// reset moved the baseline it is compared against.
    pub fn check_for_changes(&mut self, cx: &mut Context<Self>) {
        if self.checking || matches!(self.content, Content::Loading) {
            return;
        }
        self.checking = true;
        let path = self.path.clone();
        let git_watch = self.git_watch.clone();
        let known = (self.mtime, self.git_stamp);
        cx.spawn(async move |this, cx| {
            let current = cx
                .background_spawn(async move {
                    (file_mtime(&path), git_watch.as_deref().and_then(file_mtime))
                })
                .await;
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
        let Content::Text(text) = &self.content else {
            return Vec::new();
        };
        let shown = self.shown_changes();
        range
            .map(|row_index| {
                // Resolve the row to a kind, a line number and its text.
                let (kind, line_index, number, content) = match shown {
                    Some(changes) => {
                        let row = changes.diff.rows[row_index];
                        let index = row.index as usize;
                        match row.kind {
                            RowKind::Removed => {
                                (row.kind, None, index + 1, changes.base[index].clone())
                            }
                            kind => (kind, Some(index), index + 1, text.lines[index].clone()),
                        }
                    }
                    None => {
                        let added = text
                            .changes
                            .as_ref()
                            .is_some_and(|changes| changes.diff.line_added[row_index]);
                        let kind = if added { RowKind::Added } else { RowKind::Same };
                        (
                            kind,
                            Some(row_index),
                            row_index + 1,
                            text.lines[row_index].clone(),
                        )
                    }
                };

                let spans = line_index
                    .and_then(|index| text.highlights.get(index))
                    .and_then(|spans| spans.as_ref())
                    .filter(|spans| !spans.is_empty());
                let syntax = spans.into_iter().flat_map(|spans| {
                    spans.iter().map(|(range, color)| {
                        (
                            range.clone(),
                            HighlightStyle {
                                color: Some(*color),
                                ..Default::default()
                            },
                        )
                    })
                });
                let (first_match, found) = match line_index {
                    Some(index) if !self.find_matches.is_empty() => self.matches_on_line(index),
                    _ => (0, &[][..]),
                };
                let line = content.clone();
                let styled = if found.is_empty() {
                    match spans {
                        Some(_) => StyledText::new(content).with_highlights(syntax),
                        None => StyledText::new(content),
                    }
                } else {
                    let length = content.len();
                    let found = found.iter().enumerate().filter_map(|(offset, (_, range))| {
                        // A reload can leave a match pointing past a shorter line.
                        if range.end > length || !content.is_char_boundary(range.start) {
                            return None;
                        }
                        let style = if Some(first_match + offset) == self.find_current {
                            let (background, text) = find::current_match_colors();
                            HighlightStyle {
                                color: Some(text),
                                background_color: Some(background),
                                ..Default::default()
                            }
                        } else {
                            HighlightStyle {
                                background_color: Some(find::match_color()),
                                ..Default::default()
                            }
                        };
                        Some((range.clone(), style))
                    });
                    let combined: Vec<_> = combine_highlights(syntax, found).collect();
                    StyledText::new(content).with_highlights(combined)
                };

                let highlighted = line_index.is_some() && line_index == self.highlighted_line;
                let unified = shown.is_some();
                let background = if highlighted {
                    Some(theme::line_highlight())
                } else if unified && kind == RowKind::Added {
                    Some(theme::diff_added_bg())
                } else if kind == RowKind::Removed {
                    Some(theme::diff_removed_bg())
                } else {
                    None
                };
                let marker = match (kind, unified) {
                    (RowKind::Added, true) => div()
                        .text_color(theme::green())
                        .child("+")
                        .into_any_element(),
                    (RowKind::Removed, _) => {
                        div().text_color(theme::red()).child("−").into_any_element()
                    }
                    // With removed lines hidden, a bar marks what was added.
                    (RowKind::Added, false) => div()
                        .w(px(2.))
                        .h(px(LINE_HEIGHT - 4.))
                        .bg(theme::green())
                        .into_any_element(),
                    (RowKind::Same, _) => div().into_any_element(),
                };
                let number_color = match kind {
                    _ if highlighted => theme::text_muted(),
                    RowKind::Removed => Rgba {
                        a: 0.55,
                        ..theme::red()
                    },
                    _ => theme::text_faint(),
                };

                div()
                    .h(px(LINE_HEIGHT))
                    // Fill the pane so row tints reach its right edge.
                    .w_full()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .when_some(background, |row, color| row.bg(color))
                    .child(
                        div()
                            .w(gutter_width)
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_end()
                            .text_color(number_color)
                            .child(SharedString::from(number.to_string()))
                            .child(
                                div()
                                    .w(px(20.))
                                    .flex_none()
                                    .flex()
                                    .justify_center()
                                    .child(marker),
                            ),
                    )
                    .child(
                        div()
                            .pr(px(24.))
                            .when(kind == RowKind::Removed, |line| {
                                line.text_color(theme::text_muted())
                            })
                            .child(self.selection.text(row_index, 0, line, styled)),
                    )
            })
            .collect()
    }

    /// The change summary and controls shown beside the path.
    fn render_change_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let Content::Text(text) = &self.content else {
            return None;
        };
        let note = text.baseline_note;
        let Some(changes) = &text.changes else {
            // Nothing changed: just say what it was compared against, if
            // that is itself informative.
            return note
                .filter(|note| *note == "not committed yet")
                .map(|note| div().flex_none().child(note).into_any_element());
        };

        let small_button = |id: &'static str, name: &'static str| {
            div()
                .id(id)
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::bg_active()))
                .child(icon(name, px(12.), theme::text_muted()))
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_color(theme::green())
                        .child(SharedString::from(format!("+{}", changes.diff.added))),
                )
                .child(
                    div()
                        .text_color(theme::red())
                        .child(SharedString::from(format!("−{}", changes.diff.removed))),
                )
                .when_some(note, |bar, note| bar.child(note))
                .child(
                    small_button("previous-change", "chevron-up")
                        .on_click(cx.listener(|this, _, _, cx| this.step_change(-1, cx))),
                )
                .child(
                    small_button("next-change", "chevron-down")
                        .on_click(cx.listener(|this, _, _, cx| this.step_change(1, cx))),
                )
                .child(
                    div()
                        .id("toggle-diff")
                        .h(px(18.))
                        .px(px(7.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .map(|chip| {
                            if self.show_diff {
                                chip.bg(theme::bg_active()).text_color(theme::text())
                            } else {
                                chip.text_color(theme::text_muted())
                                    .hover(|style| style.bg(theme::bg_hover()))
                            }
                        })
                        .child("Diff")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_diff(cx))),
                )
                .into_any_element(),
        )
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&gpui::font(theme::MONO_FONT));
        if let Ok(advance) = text_system.advance(font_id, theme::MONO_TEXT_SIZE, 'm') {
            self.char_width = advance.width;
        }
        // Syntax colors belong to the theme they were computed for.
        if self.highlight_theme != theme::active_index() {
            if let Content::Text(text) = &mut self.content {
                text.highlights = vec![None; text.lines.len()];
                let lines = text.lines.clone();
                self.start_highlighting(lines, cx);
                self.read_markdown(cx);
            } else {
                self.highlight_theme = theme::active_index();
            }
        }
        let previewing = self.previewing();
        // The text is about to say where it is drawn this time.
        self.selection.begin_frame();
        let page = previewing.then(|| match &self.markdown {
            Some(_) => list(
                self.page.clone(),
                cx.processor(|this, index: usize, _window, _cx| this.render_page_row(index)),
            )
            .size_full()
            .into_any_element(),
            // Still being read.
            None => div().into_any_element(),
        });
        let body = match &self.content {
            _ if page.is_some() => page.unwrap_or_else(|| div().into_any_element()),
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
            Content::Text(text) => {
                let (row_count, widest_row, last_number) = match self.shown_changes() {
                    Some(changes) => (
                        changes.diff.rows.len(),
                        changes.longest_row,
                        text.lines.len().max(changes.base.len()),
                    ),
                    None => (text.lines.len(), text.longest, text.lines.len()),
                };
                let digits = last_number.max(1).to_string().len().max(3);
                let gutter_width = px(digits as f32 * 7.6 + 36.);
                self.gutter_width = gutter_width;
                uniform_list(
                    "file-lines",
                    row_count,
                    cx.processor(move |this, range: Range<usize>, _window, _cx| {
                        this.render_lines(range, gutter_width)
                    }),
                )
                .track_scroll(self.scroll.clone())
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(Some(widest_row))
                .size_full()
                .into_any_element()
            }
        };
        // The change controls step through the source.
        let change_bar = if previewing {
            None
        } else {
            self.render_change_bar(cx)
        };
        let preview_toggle = self.is_markdown.then(|| {
            div()
                .id("toggle-preview")
                .flex_none()
                .h(px(18.))
                .px(px(7.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .map(|chip| {
                    if self.preview {
                        chip.bg(theme::bg_active()).text_color(theme::text())
                    } else {
                        chip.text_color(theme::text_muted())
                            .hover(|style| style.bg(theme::bg_hover()))
                    }
                })
                .child("Preview")
                .on_click(cx.listener(|this, _, _, cx| this.toggle_preview(cx)))
        });
        let find_bar = self.find_open.then(|| {
            div()
                .absolute()
                .top(px(8.))
                .right(px(18.))
                .child(self.find_bar.clone())
        });

        // Once a drag has begun it is followed wherever the pointer goes,
        // which an element only hears about while the pointer is over it.
        let view = cx.weak_entity();
        let drag = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let dragged = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = dragged.update(cx, |this, cx| this.on_drag(event, bounds, cx));
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        let _ = view.update(cx, |this, cx| this.on_mouse_up(event, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_app())
            .track_focus(&self.focus_handle)
            .key_context("FileView")
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(px(11.5))
                    .text_color(theme::text_faint())
                    .child(
                        div()
                            .id("file-path")
                            .min_w_0()
                            .truncate()
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::text_muted()))
                            .child(self.display_path.clone())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_path(cx)))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|_, event: &MouseDownEvent, _, cx| {
                                    cx.emit(FileViewEvent::PathMenu(event.position))
                                }),
                            ),
                    )
                    .child(div().flex_1())
                    .children(change_bar)
                    .children(preview_toggle),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::MONO_TEXT_SIZE)
                    .text_color(theme::text())
                    .relative()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .child(body)
                    .child(drag)
                    .children(find_bar),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tabs_expand_to_stops_and_long_lines_truncate() {
        assert_eq!(prepare_line("\ta\tb"), "    a   b");
        let long = "x".repeat(MAX_LINE_CHARS + 50);
        assert_eq!(prepare_line(&long).chars().count(), MAX_LINE_CHARS + 1);
    }

    #[test]
    fn what_the_viewer_cannot_show_opens_elsewhere() {
        // Text, whatever it is called, and pictures open as a tab.
        assert_eq!(opening("rs", b"fn main() {}"), Opening::Here);
        assert_eq!(opening("pdf", b"plain text after all"), Opening::Here);
        assert_eq!(opening("", b""), Opening::Here);
        assert_eq!(opening("png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Opening::Here);
        // Anything else with binary content goes to the system's program.
        assert_eq!(opening("pdf", b"%PDF-1.7\n%\xe2\xe3\0\0"), Opening::System);
        assert_eq!(opening("zip", b"PK\x03\x04\x14\0\0\0"), Opening::System);
        assert_eq!(opening("", b"\0\x01\x02"), Opening::System);
        // A program is pointed at, never run.
        assert_eq!(opening("", b"\xcf\xfa\xed\xfe\x0c\0\0\x01"), Opening::Reveal);
        assert_eq!(opening("so", b"\x7fELF\x02\x01\x01\0"), Opening::Reveal);
        assert_eq!(opening("exe", b"MZ\x90\0\x03\0"), Opening::Reveal);
        assert_eq!(opening("msi", b"\xd0\xcf\x11\xe0\0\0"), Opening::Reveal);
        // The same container as an installer, but a document.
        assert_eq!(opening("doc", b"\xd0\xcf\x11\xe0\0\0"), Opening::System);

        let dir = temp_dir("opening");
        let pdf = dir.join("report.PDF");
        fs::write(&pdf, b"%PDF-1.7\n\0\0").unwrap();
        assert_eq!(opening_for(&pdf), Opening::System);
        let notes = dir.join("notes.txt");
        fs::write(&notes, "hello").unwrap();
        assert_eq!(opening_for(&notes), Opening::Here);
        assert_eq!(opening_for(&dir.join("missing.bin")), Opening::Here);
        assert_eq!(opening_for(&dir), Opening::Here);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_text_and_reports_binary() {
        let dir = temp_dir("load");
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
    fn outside_git_changes_are_measured_from_when_the_file_was_opened() {
        let dir = temp_dir("opened");
        let path = dir.join("notes.txt");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();

        // First load: nothing to compare against yet.
        let first = snapshot(&path, None);
        assert!(!first.in_repository);
        assert!(first.changes.is_none());
        let Loaded::Text { lines, .. } = first.loaded else {
            panic!("expected text")
        };
        let opened: Lines = Arc::new(lines);

        fs::write(&path, "one\n2\nthree\nfour\n").unwrap();
        let second = snapshot(&path, Some(opened));
        assert_eq!(second.baseline_note, Some("since opened"));
        let changes = second.changes.expect("file changed");
        assert_eq!((changes.diff.added, changes.diff.removed), (2, 1));
        assert_eq!(changes.base[1], SharedString::from("two"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_widest_row_can_be_a_removed_line() {
        let base: Lines = Arc::new(vec!["short".into(), "a much longer removed line".into()]);
        let lines: Vec<SharedString> = vec!["short".into(), "new".into()];
        let changes = changes_against(base, &lines, 0).unwrap();
        // Rows: same "short", removed long line, added "new".
        assert_eq!(changes.longest_row, 1);
        assert_eq!(changes.diff.rows.len(), 3);
    }

    #[test]
    fn custom_syntax_colors_reach_the_right_tokens() {
        use syntect::highlighting::Color;
        let colors = theme::SyntaxColors {
            comment: 0x010101,
            string: 0x020202,
            number: 0x030303,
            constant: 0x040404,
            keyword: 0x050505,
            function: 0x060606,
            type_name: 0x070707,
            tag: 0x080808,
            attribute: 0x090909,
            operator: 0x0a0a0a,
        };
        let custom = custom_syntax_theme(&colors, 0xeeeeee);
        assert_eq!(custom.scopes.len(), 10, "every selector parsed");

        let set = syntaxes();
        let syntax = set.find_syntax_by_extension("rs").unwrap();
        let mut highlighter = HighlightLines::new(syntax, &custom);
        let line = "fn main() { let s = \"hi\"; let n = 42; } // done\n";
        let regions = highlighter.highlight_line(line, set).unwrap();
        let color_of = |needle: &str| -> Color {
            regions
                .iter()
                .find(|(_, text)| text.contains(needle))
                .map(|(style, _)| style.foreground)
                .unwrap_or_else(|| panic!("no region for {needle:?}"))
        };
        let gray = |value: u8| Color {
            r: value,
            g: value,
            b: value,
            a: 0xff,
        };
        assert_eq!(color_of("fn"), gray(5));
        assert_eq!(color_of("main"), gray(6));
        assert_eq!(color_of("hi"), gray(2));
        assert_eq!(color_of("42"), gray(3));
        assert_eq!(color_of("done"), gray(1));
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
