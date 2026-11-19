//! The file explorer: a lazily loaded, virtualized directory tree.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{
    div, prelude::*, px, uniform_list, Context, EventEmitter, ScrollStrategy, SharedString, Task,
    UniformListScrollHandle, Window,
};

use crate::assets::icon;
use crate::theme;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
    OpenFile(PathBuf),
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
    _poll: Task<()>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

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

        let mut this = Self {
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
            cx.emit(FileTreeEvent::OpenFile(path));
            cx.notify();
        }
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
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            let message = if self.loading.is_empty() {
                "Empty folder"
            } else {
                "Loading…"
            };
            return div()
                .size_full()
                .p_3()
                .text_color(theme::text_faint())
                .child(message)
                .into_any_element();
        }
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
    }
}
