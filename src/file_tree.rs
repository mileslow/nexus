//! The file explorer: a lazily loaded, virtualized directory tree, and a
//! search box that finds files by name and lines by their text.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    div, prelude::*, px, uniform_list, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, ScrollStrategy, SharedString, StyledText, Task, UniformListScrollHandle,
    Window,
};

use crate::assets::icon;
use crate::file_search::{self, Indexed, LineHit};
use crate::find;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// A list of the folder's files older than this is read again.
const INDEX_LIFETIME: Duration = Duration::from_secs(10);
/// How long typing must pause before the files' text is searched.
const TEXT_SEARCH_DELAY: Duration = Duration::from_millis(140);
const MAX_NAME_HITS: usize = 50;

/// Directories that are usually generated; shown dimmed.
const NOISY_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    "vendor",
];

pub enum FileTreeEvent {
    /// Open a file, at a line (counted from 1) when one was picked.
    OpenFile(PathBuf, Option<u32>),
    /// Escape pressed in an empty search box: hand focus back to the panes.
    Dismissed,
}

/// A row of search results.
enum Hit {
    /// The heading over a group of results.
    Heading(SharedString),
    /// A file found by name, or one whose lines follow.
    File { file: usize, opens: bool },
    Line(LineHit),
}

impl Hit {
    /// Whether the row can be picked.
    fn opens(&self) -> bool {
        match self {
            Hit::Heading(_) => false,
            Hit::File { opens, .. } => *opens,
            Hit::Line(_) => true,
        }
    }
}

/// Lays out the rows for what a search found. `lines` come grouped by file.
fn result_rows(names: &[usize], lines: Vec<LineHit>) -> Vec<Hit> {
    let mut rows = Vec::new();
    if !names.is_empty() {
        rows.push(Hit::Heading("FILES".into()));
        rows.extend(names.iter().map(|&file| Hit::File { file, opens: true }));
    }
    if !lines.is_empty() {
        let capped = if lines.len() >= file_search::MAX_LINES {
            "+"
        } else {
            ""
        };
        rows.push(Hit::Heading(format!("TEXT  {}{capped}", lines.len()).into()));
        let mut last = None;
        for line in lines {
            if last != Some(line.file) {
                last = Some(line.file);
                rows.push(Hit::File {
                    file: line.file,
                    opens: false,
                });
            }
            rows.push(Hit::Line(line));
        }
    }
    rows
}

/// A file or folder being dragged out of the tree.
#[derive(Clone)]
pub struct DraggedPath {
    pub path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

/// The chip that follows the pointer while a path is dragged.
struct PathPreview(DraggedPath);

impl Render for PathPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(26.))
            .max_w(px(260.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .bg(theme::bg_active())
            .border_1()
            .border_color(theme::border_strong())
            .text_size(theme::UI_TEXT_SIZE)
            .font_family(theme::UI_FONT)
            .text_color(theme::text())
            .shadow_md()
            .opacity(0.92)
            .child(icon(
                if self.0.is_dir { "folder" } else { "file" },
                px(13.),
                theme::text_muted(),
            ))
            .child(div().min_w_0().truncate().child(self.0.name.clone()))
    }
}

#[derive(Clone)]
struct Child {
    name: SharedString,
    path: PathBuf,
    is_dir: bool,
    dimmed: bool,
}

struct Row {
    path: PathBuf,
    name: SharedString,
    depth: usize,
    is_dir: bool,
    expanded: bool,
    dimmed: bool,
}

pub struct FileTree {
    root: PathBuf,
    children: HashMap<PathBuf, Arc<Vec<Child>>>,
    mtimes: HashMap<PathBuf, SystemTime>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    search: Entity<TextInput>,
    /// Every file in the folder, as of `indexed_at`.
    index: Arc<Vec<Indexed>>,
    indexed_at: Option<Instant>,
    indexing: bool,
    /// What the search box's text found; empty while it is empty.
    hits: Vec<Hit>,
    /// The files found by name, kept while their text is still being read.
    name_hits: Vec<usize>,
    highlighted: usize,
    searching_text: bool,
    stop_text_search: Arc<AtomicBool>,
    hit_scroll: UniformListScrollHandle,
    _text_search: Option<Task<()>>,
    _poll: Task<()>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// Lists a directory: folders first, then case-insensitive by name.
fn read_children(dir: &Path) -> Vec<Child> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut children: Vec<Child> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".DS_Store" || name == ".git" {
                return None;
            }
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            let is_dir = if kind.is_symlink() {
                fs::metadata(&path)
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
            } else {
                kind.is_dir()
            };
            let dimmed = name.starts_with('.') || (is_dir && NOISY_DIRS.contains(&name.as_str()));
            Some(Child {
                name: name.into(),
                path,
                is_dir,
                dimmed,
            })
        })
        .collect();
    children.sort_by_cached_key(|child| (!child.is_dir, child.name.to_lowercase()));
    children
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    fs::metadata(dir).and_then(|meta| meta.modified()).ok()
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        // Directory mtimes change when entries are added or removed, so a
        // cheap periodic stat keeps the tree fresh while agents edit files.
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let Ok(watched) = this.read_with(cx, |this: &Self, _| this.watched_dirs()) else {
                break;
            };
            let changed = cx
                .background_spawn(async move {
                    watched
                        .into_iter()
                        .filter(|(dir, mtime)| dir_mtime(dir) != *mtime)
                        .map(|(dir, _)| dir)
                        .collect::<Vec<_>>()
                })
                .await;
            if changed.is_empty() {
                continue;
            }
            if this
                .update(cx, |this, cx| {
                    for dir in changed {
                        this.load_dir(dir, cx);
                    }
                })
                .is_err()
            {
                break;
            }
        });

        let search = cx.new(|cx| TextInput::new("Search files", cx));
        cx.subscribe(&search, |this, _, event: &TextInputEvent, cx| match event {
            TextInputEvent::Changed => this.search_changed(cx),
            TextInputEvent::Submit => this.open_hit(this.highlighted, cx),
            TextInputEvent::Cancel => {
                if this.search.read(cx).text().is_empty() {
                    cx.emit(FileTreeEvent::Dismissed);
                } else {
                    this.search.update(cx, |search, cx| search.clear(cx));
                }
            }
            TextInputEvent::MoveDown => this.move_highlight(true, cx),
            TextInputEvent::MoveUp => this.move_highlight(false, cx),
        })
        .detach();

        let mut this = Self {
            search,
            index: Arc::new(Vec::new()),
            indexed_at: None,
            indexing: false,
            hits: Vec::new(),
            name_hits: Vec::new(),
            highlighted: 0,
            searching_text: false,
            stop_text_search: Arc::new(AtomicBool::new(false)),
            hit_scroll: UniformListScrollHandle::new(),
            _text_search: None,
            root: root.clone(),
            children: HashMap::new(),
            mtimes: HashMap::new(),
            expanded: HashSet::new(),
            loading: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _poll: poll,
        };
        this.expanded.insert(root.clone());
        this.load_dir(root, cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = self
            .watched_dirs()
            .into_iter()
            .map(|(dir, _)| dir)
            .collect();
        for dir in dirs {
            self.load_dir(dir, cx);
        }
    }

    fn watched_dirs(&self) -> Vec<(PathBuf, Option<SystemTime>)> {
        self.expanded
            .iter()
            .filter(|dir| self.children.contains_key(*dir))
            .map(|dir| (dir.clone(), self.mtimes.get(dir).copied()))
            .collect()
    }

    fn load_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.loading.insert(dir.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let read_dir = dir.clone();
            let (children, mtime) = cx
                .background_spawn(async move {
                    let mtime = dir_mtime(&read_dir);
                    (read_children(&read_dir), mtime)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading.remove(&dir);
                this.apply_children(dir, children, mtime);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_children(&mut self, dir: PathBuf, children: Vec<Child>, mtime: Option<SystemTime>) {
        match mtime {
            Some(mtime) => {
                self.mtimes.insert(dir.clone(), mtime);
            }
            None => {
                self.mtimes.remove(&dir);
            }
        }
        self.children.insert(dir, Arc::new(children));
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        fn visit(tree: &FileTree, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
            let Some(children) = tree.children.get(dir) else {
                return;
            };
            for child in children.iter() {
                let expanded = child.is_dir && tree.expanded.contains(&child.path);
                rows.push(Row {
                    path: child.path.clone(),
                    name: child.name.clone(),
                    depth,
                    is_dir: child.is_dir,
                    expanded,
                    dimmed: child.dimmed,
                });
                if expanded {
                    visit(tree, &child.path, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        visit(self, &self.root, 0, &mut rows);
        self.rows = rows;
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path.clone());
            // Show cached children immediately, then refresh in the background.
            self.load_dir(path, cx);
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn click_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let path = row.path.clone();
        self.selected = Some(path.clone());
        if row.is_dir {
            self.toggle_dir(path, cx);
        } else {
            cx.emit(FileTreeEvent::OpenFile(path, None));
            cx.notify();
        }
    }

    // ---- search ---------------------------------------------------------------

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text().trim().to_string()
    }

    /// Lists the folder's files again, off the UI thread, then searches again.
    fn reindex(&mut self, cx: &mut Context<Self>) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_spawn(async move { file_search::list_files(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.indexing = false;
                this.indexed_at = Some(Instant::now());
                if *this.index != files {
                    this.index = Arc::new(files);
                }
                this.run_search(cx);
            });
        })
        .detach();
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        self.highlighted = 0;
        self.hit_scroll.scroll_to_item(0, ScrollStrategy::Top);
        // Agents add and remove files all the time.
        let stale = self
            .indexed_at
            .is_none_or(|at| at.elapsed() > INDEX_LIFETIME);
        if stale && !self.query(cx).is_empty() {
            self.reindex(cx);
        }
        self.run_search(cx);
    }

    /// Finds names at once, and text shortly after typing stops.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.stop_text_search.store(true, Ordering::Relaxed);
        self._text_search = None;
        self.searching_text = false;

        let query = self.query(cx);
        self.name_hits = file_search::name_matches(&self.index, &query, MAX_NAME_HITS);
        self.hits = result_rows(&self.name_hits, Vec::new());
        self.settle_highlight();
        cx.notify();
        if query.chars().count() < file_search::MIN_TEXT_QUERY || self.index.is_empty() {
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        self.stop_text_search = stop.clone();
        self.searching_text = true;
        let files = self.index.clone();
        self._text_search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TEXT_SEARCH_DELAY).await;
            let lines = cx
                .background_spawn(async move {
                    file_search::text_matches(&files, &query, &stop)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.searching_text = false;
                this.hits = result_rows(&this.name_hits, lines);
                this.settle_highlight();
                cx.notify();
            });
        }));
    }

    /// Keeps the highlight on a row that can be picked.
    fn settle_highlight(&mut self) {
        let last = self.hits.len().saturating_sub(1);
        self.highlighted = self.highlighted.min(last);
        if !self.hits.get(self.highlighted).is_some_and(Hit::opens) {
            self.highlighted = (self.highlighted..self.hits.len())
                .find(|&row| self.hits[row].opens())
                .unwrap_or(self.highlighted);
        }
    }

    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let next = if down {
            (self.highlighted + 1..self.hits.len()).find(|&row| self.hits[row].opens())
        } else {
            (0..self.highlighted)
                .rev()
                .find(|&row| self.hits[row].opens())
        };
        if let Some(next) = next {
            self.highlighted = next;
            self.hit_scroll
                .scroll_to_item(next, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn open_hit(&mut self, row: usize, cx: &mut Context<Self>) {
        let (file, line) = match self.hits.get(row) {
            Some(Hit::File { file, .. }) => (*file, None),
            Some(Hit::Line(hit)) => (hit.file, Some(hit.line)),
            _ => return,
        };
        if let Some(file) = self.index.get(file) {
            self.highlighted = row;
            cx.emit(FileTreeEvent::OpenFile(file.path.clone(), line));
            cx.notify();
        }
    }

    fn render_hit(
        &self,
        row: usize,
        search_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let base = div()
            .id(("file-hit", row))
            .h(theme::ROW_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .gap(px(5.))
            .pr_2();
        let file_of = |index: usize| self.index.get(index);
        let content = match &self.hits[row] {
            Hit::Heading(label) => {
                return base
                    .pl(px(10.))
                    .text_size(px(10.5))
                    .text_color(theme::text_faint())
                    .child(label.clone());
            }
            Hit::File { file, opens } => {
                let (name, folder) = file_of(*file)
                    .map(|file| (file.name().to_string(), file.folder().to_string()))
                    .unwrap_or_default();
                base.pl(px(10.))
                    .text_color(if *opens {
                        theme::text_muted()
                    } else {
                        theme::text()
                    })
                    .child(icon("file", px(14.), theme::text_faint()))
                    .child(div().flex_none().child(SharedString::from(name)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme::text_faint())
                            .child(SharedString::from(folder)),
                    )
            }
            Hit::Line(hit) => {
                let marks = hit.found.clone().map(|range| {
                    (
                        range,
                        HighlightStyle {
                            background_color: Some(find::match_color()),
                            color: Some(theme::text().into()),
                            ..HighlightStyle::default()
                        },
                    )
                });
                base.pl(px(30.))
                    .text_color(theme::text_muted())
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(22.))
                            .text_size(px(11.))
                            .text_color(theme::text_faint())
                            .child(SharedString::from(hit.line.to_string())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(11.5))
                            .child(
                                StyledText::new(SharedString::from(hit.text.clone()))
                                    .with_highlights(marks),
                            ),
                    )
            }
        };
        if !self.hits[row].opens() {
            return content;
        }
        content
            .cursor_pointer()
            .when(search_focused && row == self.highlighted, |row| {
                row.bg(theme::bg_hover())
            })
            .hover(|style| style.bg(theme::bg_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.open_hit(row, cx)))
    }

    fn render_results(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.hits.is_empty() {
            let message = if self.indexing || self.searching_text || self.indexed_at.is_none() {
                "Searching…"
            } else {
                "No files or text match"
            };
            return div()
                .p_3()
                .text_color(theme::text_faint())
                .child(message)
                .into_any_element();
        }
        uniform_list(
            "file-hits",
            self.hits.len(),
            cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                let search_focused = this.search.focus_handle(cx).is_focused(window);
                range
                    .map(|row| this.render_hit(row, search_focused, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.hit_scroll.clone())
        .size_full()
        .into_any_element()
    }

    /// Highlights `path`, expanding its ancestors and scrolling it into view.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path) {
            return;
        }
        self.selected = Some(path.to_path_buf());
        if let Ok(relative) = path.strip_prefix(&self.root) {
            // Ancestors are few, so load any missing ones synchronously.
            let mut dir = self.root.clone();
            let mut ancestors = vec![dir.clone()];
            let mut components = relative.components().peekable();
            while let Some(component) = components.next() {
                dir.push(component);
                if components.peek().is_some() || dir.is_dir() {
                    ancestors.push(dir.clone());
                }
            }
            for dir in ancestors {
                self.expanded.insert(dir.clone());
                if !self.children.contains_key(&dir) {
                    let mtime = dir_mtime(&dir);
                    self.apply_children(dir.clone(), read_children(&dir), mtime);
                }
            }
            self.rebuild_rows();
            if let Some(index) = self.rows.iter().position(|row| row.path == path) {
                self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            }
        }
        cx.notify();
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let row = &self.rows[index];
        let selected = self.selected.as_deref() == Some(row.path.as_path());
        let text_color = if row.dimmed {
            theme::text_faint()
        } else if selected {
            theme::text()
        } else {
            theme::text_muted()
        };
        let chevron = if !row.is_dir {
            div().size(px(12.)).flex_none().into_any_element()
        } else if row.expanded {
            icon("chevron-down", px(12.), theme::text_faint()).into_any_element()
        } else {
            icon("chevron-right", px(12.), theme::text_faint()).into_any_element()
        };
        let glyph = if row.is_dir {
            icon(
                if row.expanded {
                    "folder-open"
                } else {
                    "folder"
                },
                px(14.),
                theme::accent(),
            )
        } else {
            icon("file", px(14.), theme::text_faint())
        };

        div()
            .id(index)
            .h(theme::ROW_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .gap(px(5.))
            .pl(px(8. + row.depth as f32 * 12.))
            .pr_2()
            .text_color(text_color)
            .when(selected, |row| row.bg(theme::bg_active()))
            .hover(|style| style.bg(theme::bg_hover()))
            .cursor_pointer()
            .child(chevron)
            .child(glyph)
            .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.click_row(index, cx)))
            // Drag onto a terminal to type the path there.
            .on_drag(
                DraggedPath {
                    path: row.path.clone(),
                    name: row.name.clone(),
                    is_dir: row.is_dir,
                },
                |dragged, _, _, cx| cx.new(|_| PathPreview(dragged.clone())),
            )
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = !self.search.read(cx).text().trim().is_empty();
        let body = if searching {
            self.render_results(cx)
        } else if self.rows.is_empty() {
            let message = if self.loading.is_empty() {
                "Empty folder"
            } else {
                "Loading…"
            };
            div()
                .p_3()
                .text_color(theme::text_faint())
                .child(message)
                .into_any_element()
        } else {
            uniform_list(
                "file-tree",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .map(|index| this.render_row(index, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .size_full()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .mx(px(8.))
                    .mt(px(2.))
                    .mb(px(4.))
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border())
                    .child(icon("search", px(13.), theme::text_faint()))
                    .child(self.search.clone()),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(file: usize, line: u32) -> LineHit {
        LineHit {
            file,
            line,
            text: String::new(),
            found: None,
        }
    }

    /// One letter per row: `h`eading, `f`ile to open, file `n`ame over its
    /// lines, `l`ine.
    fn shape(rows: &[Hit]) -> String {
        rows.iter()
            .map(|row| match row {
                Hit::Heading(_) => 'h',
                Hit::File { opens: true, .. } => 'f',
                Hit::File { opens: false, .. } => 'n',
                Hit::Line(_) => 'l',
            })
            .collect()
    }

    #[test]
    fn results_group_names_then_lines_by_file() {
        let rows = result_rows(&[4, 2], vec![line(1, 3), line(1, 9), line(5, 1)]);
        assert_eq!(shape(&rows), "hffhnllnl");
        // Headings and the names over lines are not things to open.
        assert_eq!(
            rows.iter().map(Hit::opens).collect::<Vec<_>>(),
            vec![false, true, true, false, false, true, true, false, true]
        );
        assert_eq!(shape(&result_rows(&[], vec![line(0, 1)])), "hnl");
        assert_eq!(shape(&result_rows(&[7], Vec::new())), "hf");
        assert!(result_rows(&[], Vec::new()).is_empty());
    }
}
