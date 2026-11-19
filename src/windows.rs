//! Opening windows: the home view, and one window per workspace.

use std::path::{Path, PathBuf};

use gpui::{
    point, prelude::*, px, size, App, Bounds, TitlebarOptions, Window, WindowBounds, WindowOptions,
};

use crate::app_state::Store;
use crate::home::Home;
use crate::workspace::Workspace;

/// Resolves symlinks so one folder is always one workspace.
pub fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn window_options(cx: &mut App) -> WindowOptions {
    // `NEXUS_BACKGROUND=1` opens windows without taking keyboard focus, for
    // launching from scripts.
    let background = std::env::var_os("NEXUS_BACKGROUND").is_some_and(|value| value == "1");
    // Step each new window down and right so it does not hide the last one.
    let step = px(28. * (cx.windows().len() % 8) as f32);
    let mut bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    bounds.origin = bounds.origin + point(step, step);
    WindowOptions {
        focus: !background,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Nexus".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(13.), px(11.))),
        }),
        window_min_size: Some(size(px(640.), px(400.))),
        ..Default::default()
    }
}

fn activate_app(cx: &mut App) {
    if std::env::var_os("NEXUS_BACKGROUND").is_none_or(|value| value != "1") {
        cx.activate(true);
    }
}

/// Opens a new window on the home view.
pub fn open_home_window(cx: &mut App) {
    let options = window_options(cx);
    if let Err(error) = cx.open_window(options, |window, cx| cx.new(|cx| Home::new(window, cx))) {
        eprintln!("nexus: failed to open window: {error:#}");
    }
    activate_app(cx);
}

/// Brings the window already showing `root` to the front, if there is one.
fn focus_existing(root: &Path, cx: &mut App) -> bool {
    let Some(handle) = cx.global::<Store>().window_for(root) else {
        return false;
    };
    handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

/// Opens `root` in its own window, or focuses the window that already has it.
pub fn open_workspace_window(root: &Path, cx: &mut App) {
    let root = canonical(root);
    if focus_existing(&root, cx) {
        return;
    }
    let options = window_options(cx);
    if let Err(error) = cx.open_window(options, |window, cx| {
        cx.new(|cx| Workspace::new(root.clone(), window, cx))
    }) {
        eprintln!("nexus: failed to open window: {error:#}");
    }
    activate_app(cx);
}

/// Turns the home window into the workspace for `root`. If another window
/// already has that workspace, focuses it and closes this one instead.
pub fn enter_workspace(root: &Path, window: &mut Window, cx: &mut App) {
    let root = canonical(root);
    if focus_existing(&root, cx) {
        window.remove_window();
        return;
    }
    window.replace_root(cx, |window, cx| Workspace::new(root, window, cx));
}
