//! The little bit of state remembered between launches.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::links::home_dir;

#[derive(Default, Serialize, Deserialize)]
struct State {
    root: Option<PathBuf>,
}

fn state_path() -> Option<PathBuf> {
    Some(home_dir()?.join("Library/Application Support/Nexus/state.json"))
}

/// The folder that was open last time, if it still exists.
pub fn saved_root() -> Option<PathBuf> {
    let bytes = fs::read(state_path()?).ok()?;
    let state: State = serde_json::from_slice(&bytes).ok()?;
    state.root.filter(|root| root.is_dir())
}

pub fn save_root(root: &Path) {
    let Some(path) = state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let state = State {
        root: Some(root.to_path_buf()),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&state) {
        let _ = fs::write(path, bytes);
    }
}
