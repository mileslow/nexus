//! Nexus: a fast agent multiplexer / IDE built on GPUI.

mod assets;
mod diff;
mod file_tree;
mod file_view;
mod layout;
mod links;
mod pane;
mod session_list;
mod sessions;
mod state;
mod terminal;
mod text_input;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, KeyBinding, Menu, MenuItem,
    TitlebarOptions, WindowBounds, WindowOptions,
};

use workspace::{
    CloseTab, NewClaude, NewCodex, NewTerminal, NextPane, NextTab, OpenFolder, PrevPane, PrevTab,
    Quit, SearchChats, SelectTerminal1, SelectTerminal2, SelectTerminal3, SelectTerminal4,
    SelectTerminal5, SelectTerminal6, SelectTerminal7, SelectTerminal8, SelectTerminal9, SplitDown,
    SplitRight, ToggleSidebar, Workspace,
};

/// Folder to open: an explicit argument, else the last one used, else the
/// directory the app was launched from.
fn initial_root() -> PathBuf {
    if let Some(arg) = std::env::args_os().nth(1) {
        let path = PathBuf::from(arg);
        if let Ok(path) = path.canonicalize() {
            if path.is_dir() {
                return path;
            }
            if let Some(parent) = path.parent() {
                return parent.to_path_buf();
            }
        }
    }
    let launched_from = std::env::current_dir().ok().filter(|dir| {
        // Finder launches apps from `/`, which is never the folder you want.
        dir.parent().is_some()
    });
    launched_from
        .or_else(state::saved_root)
        .or_else(links::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn bind_keys(cx: &mut App) {
    text_input::bind_keys(cx);
    terminal::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTerminal, None),
        KeyBinding::new("cmd-shift-c", NewClaude, None),
        KeyBinding::new("cmd-shift-x", NewCodex, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-d", SplitRight, None),
        KeyBinding::new("cmd-shift-d", SplitDown, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-]", NextPane, None),
        KeyBinding::new("cmd-[", PrevPane, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-p", SearchChats, None),
        KeyBinding::new("cmd-shift-f", SearchChats, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-1", SelectTerminal1, None),
        KeyBinding::new("cmd-2", SelectTerminal2, None),
        KeyBinding::new("cmd-3", SelectTerminal3, None),
        KeyBinding::new("cmd-4", SelectTerminal4, None),
        KeyBinding::new("cmd-5", SelectTerminal5, None),
        KeyBinding::new("cmd-6", SelectTerminal6, None),
        KeyBinding::new("cmd-7", SelectTerminal7, None),
        KeyBinding::new("cmd-8", SelectTerminal8, None),
        KeyBinding::new("cmd-9", SelectTerminal9, None),
    ]);
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Nexus".into(),
            items: vec![MenuItem::action("Quit Nexus", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("New Claude Chat", NewClaude),
                MenuItem::action("New Codex Chat", NewCodex),
                MenuItem::separator(),
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Copy", terminal::Copy),
                MenuItem::action("Paste", terminal::Paste),
                MenuItem::separator(),
                MenuItem::action("Clear Terminal", terminal::Clear),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Search Chats", SearchChats),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
                MenuItem::action("Next Pane", NextPane),
                MenuItem::action("Previous Pane", PrevPane),
            ],
        },
    ]
}

fn main() {
    // Agent CLIs started inside Nexus must not think they are nested inside
    // whichever agent session happened to launch the app.
    for name in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
    ] {
        std::env::remove_var(name);
    }
    let root = initial_root();
    state::save_root(&root);

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            bind_keys(cx);
            cx.set_menus(menus());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            // `NEXUS_BACKGROUND=1` opens the window without taking keyboard
            // focus, for launching from scripts.
            let background = std::env::var_os("NEXUS_BACKGROUND").is_some_and(|v| v == "1");

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let options = WindowOptions {
                focus: !background,
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Nexus".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(13.), px(11.))),
                }),
                window_min_size: Some(size(px(640.), px(400.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                cx.new(|cx| Workspace::new(root.clone(), window, cx))
            })
            .expect("failed to open window");
            if !background {
                cx.activate(true);
            }
        });
}
