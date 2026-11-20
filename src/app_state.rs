//! What Nexus remembers between launches: the theme, the workspaces that
//! have been opened, and how each one was laid out.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::{AnyWindowHandle, Global};
use serde::{Deserialize, Serialize};

use crate::sessions::{now_secs, Agent};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedChat {
    pub agent: Agent,
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedTab {
    Terminal {
        cwd: PathBuf,
        /// The chat to resume in this terminal, if it was running one.
        #[serde(default)]
        chat: Option<SavedChat>,
    },
    File {
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedNode {
    Pane {
        tabs: Vec<SavedTab>,
        active: usize,
    },
    Split {
        axis: SavedAxis,
        sizes: Vec<f32>,
        children: Vec<SavedNode>,
    },
}

impl SavedNode {
    fn count(&self, terminals: &mut usize, files: &mut usize) {
        match self {
            SavedNode::Pane { tabs, .. } => {
                for tab in tabs {
                    match tab {
                        SavedTab::Terminal { .. } => *terminals += 1,
                        SavedTab::File { .. } => *files += 1,
                    }
                }
            }
            SavedNode::Split { children, .. } => {
                for child in children {
                    child.count(terminals, files);
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub sidebar_width: f32,
    pub sidebar_visible: bool,
    pub collapsed: [bool; 3],
    pub tree: SavedNode,
    /// Index of the active pane, counting panes left to right, top to bottom.
    pub active_pane: usize,
}

impl SavedLayout {
    /// `(terminals, files)` open in this layout.
    pub fn counts(&self) -> (usize, usize) {
        let (mut terminals, mut files) = (0, 0);
        self.tree.count(&mut terminals, &mut files);
        (terminals, files)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub root: PathBuf,
    /// Seconds since the Unix epoch.
    pub last_opened: u64,
    #[serde(default)]
    pub layout: Option<SavedLayout>,
}

impl WorkspaceRecord {
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceRecord>,
    /// Workspaces whose windows are open, reopened on the next launch.
    #[serde(default)]
    pub open: Vec<PathBuf>,
    /// The single folder remembered by early versions.
    #[serde(default, skip_serializing)]
    root: Option<PathBuf>,
}

impl AppState {
    fn parse(bytes: &[u8]) -> Self {
        let mut state: AppState = serde_json::from_slice(bytes).unwrap_or_default();
        if let Some(root) = state.root.take() {
            if state.workspaces.is_empty() {
                state.workspaces.push(WorkspaceRecord {
                    root,
                    last_opened: 0,
                    layout: None,
                });
            }
        }
        state
    }

    /// Known workspaces, most recently opened first.
    pub fn recent(&self) -> Vec<WorkspaceRecord> {
        let mut recent = self.workspaces.clone();
        recent.sort_by(|a, b| {
            b.last_opened
                .cmp(&a.last_opened)
                .then_with(|| a.root.cmp(&b.root))
        });
        recent
    }

    pub fn layout_for(&self, root: &Path) -> Option<SavedLayout> {
        self.workspaces
            .iter()
            .find(|record| record.root == root)
            .and_then(|record| record.layout.clone())
    }

    fn record_mut(&mut self, root: &Path) -> &mut WorkspaceRecord {
        let index = match self.workspaces.iter().position(|r| r.root == root) {
            Some(index) => index,
            None => {
                self.workspaces.push(WorkspaceRecord {
                    root: root.to_path_buf(),
                    last_opened: 0,
                    layout: None,
                });
                self.workspaces.len() - 1
            }
        };
        &mut self.workspaces[index]
    }

    fn mark_opened(&mut self, root: &Path, now: u64) {
        self.record_mut(root).last_opened = now;
        if !self.open.iter().any(|open| open == root) {
            self.open.push(root.to_path_buf());
        }
    }

    fn mark_closed(&mut self, root: &Path) {
        self.open.retain(|open| open != root);
    }

    /// Returns true when the stored layout actually changed.
    fn set_layout(&mut self, root: &Path, layout: SavedLayout) -> bool {
        let record = self.record_mut(root);
        if record.layout.as_ref() == Some(&layout) {
            return false;
        }
        record.layout = Some(layout);
        true
    }

    fn forget(&mut self, root: &Path) {
        self.workspaces.retain(|record| record.root != root);
        self.open.retain(|open| open != root);
    }
}

/// The app-wide store: persisted state plus which window shows which workspace.
pub struct Store {
    state: AppState,
    path: Option<PathBuf>,
    windows: HashMap<PathBuf, AnyWindowHandle>,
}

impl Global for Store {}

/// Where remembered windows are kept. `NEXUS_STATE_FILE` points the app at
/// a different file, so scripted runs never touch the real one.
pub fn state_path() -> Option<PathBuf> {
    std::env::var_os("NEXUS_STATE_FILE")
        .map(PathBuf::from)
        .or_else(|| Some(crate::platform::data_dir()?.join("state.json")))
}

impl Store {
    pub fn load() -> Self {
        let path = state_path();
        let state = path
            .as_deref()
            .and_then(|path| fs::read(path).ok())
            .map(|bytes| AppState::parse(&bytes))
            .unwrap_or_default();
        Self {
            state,
            path,
            windows: HashMap::new(),
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.state) {
            let tmp = path.with_extension("json.tmp");
            if fs::write(&tmp, bytes).is_ok() {
                let _ = fs::rename(tmp, path);
            }
        }
    }

    pub fn theme(&self) -> Option<&str> {
        self.state.theme.as_deref()
    }

    pub fn set_theme(&mut self, name: &str) {
        if self.state.theme.as_deref() != Some(name) {
            self.state.theme = Some(name.to_string());
            self.save();
        }
    }

    pub fn recent(&self) -> Vec<WorkspaceRecord> {
        self.state.recent()
    }

    /// Workspaces that were open when the app last ran and still exist.
    pub fn reopen_list(&self) -> Vec<PathBuf> {
        self.state
            .open
            .iter()
            .filter(|root| root.is_dir())
            .cloned()
            .collect()
    }

    pub fn layout_for(&self, root: &Path) -> Option<SavedLayout> {
        self.state.layout_for(root)
    }

    pub fn window_for(&self, root: &Path) -> Option<AnyWindowHandle> {
        self.windows.get(root).copied()
    }

    pub fn workspace_opened(&mut self, root: &Path, window: AnyWindowHandle) {
        self.windows.insert(root.to_path_buf(), window);
        self.state.mark_opened(root, now_secs());
        self.save();
    }

    /// The user closed this workspace's window.
    pub fn workspace_closed(&mut self, root: &Path) {
        self.windows.remove(root);
        self.state.mark_closed(root);
        self.save();
    }

    pub fn save_layout(&mut self, root: &Path, layout: SavedLayout) {
        if self.state.set_layout(root, layout) {
            self.save();
        }
    }

    pub fn forget(&mut self, root: &Path) {
        self.state.forget(root);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> SavedLayout {
        SavedLayout {
            sidebar_width: 300.,
            sidebar_visible: true,
            collapsed: [false, true, false],
            tree: SavedNode::Split {
                axis: SavedAxis::Horizontal,
                sizes: vec![0.4, 0.6],
                children: vec![
                    SavedNode::Pane {
                        tabs: vec![
                            SavedTab::Terminal {
                                cwd: "/work/app".into(),
                                chat: Some(SavedChat {
                                    agent: Agent::Claude,
                                    id: "abc".into(),
                                    title: "Fix login".into(),
                                }),
                            },
                            SavedTab::Terminal {
                                cwd: "/work/app/web".into(),
                                chat: None,
                            },
                        ],
                        active: 1,
                    },
                    SavedNode::Pane {
                        tabs: vec![SavedTab::File {
                            path: "/work/app/src/main.rs".into(),
                        }],
                        active: 0,
                    },
                ],
            },
            active_pane: 1,
        }
    }

    #[test]
    fn layouts_survive_a_round_trip() {
        let mut state = AppState::default();
        state.mark_opened(Path::new("/work/app"), 10);
        assert!(state.set_layout(Path::new("/work/app"), layout()));
        assert!(
            !state.set_layout(Path::new("/work/app"), layout()),
            "unchanged"
        );
        state.theme = Some("Nord".into());

        let bytes = serde_json::to_vec(&state).unwrap();
        let restored = AppState::parse(&bytes);
        assert_eq!(restored, state);
        assert_eq!(restored.layout_for(Path::new("/work/app")), Some(layout()));
        assert_eq!(layout().counts(), (2, 1));
    }

    #[test]
    fn recents_are_newest_first_and_track_open_windows() {
        let mut state = AppState::default();
        state.mark_opened(Path::new("/a"), 5);
        state.mark_opened(Path::new("/b"), 9);
        state.mark_opened(Path::new("/a"), 12);
        let names: Vec<String> = state.recent().iter().map(|r| r.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(state.open, vec![PathBuf::from("/a"), PathBuf::from("/b")]);

        state.mark_closed(Path::new("/a"));
        assert_eq!(state.open, vec![PathBuf::from("/b")]);
        assert_eq!(state.workspaces.len(), 2, "closing keeps the record");

        state.forget(Path::new("/b"));
        assert!(state.open.is_empty());
        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn reads_the_old_single_folder_format_and_tolerates_garbage() {
        let state = AppState::parse(br#"{"root": "/old/project"}"#);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].root, PathBuf::from("/old/project"));
        assert_eq!(AppState::parse(b"not json"), AppState::default());
        // Unknown fields from a newer version are ignored.
        let state = AppState::parse(br#"{"theme": "Nord", "future": 1}"#);
        assert_eq!(state.theme.as_deref(), Some("Nord"));
    }
}
