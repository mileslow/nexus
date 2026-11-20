//! Nexus: a fast agent multiplexer / IDE built on GPUI.

mod app_state;
mod assets;
mod diff;
mod file_tree;
mod file_view;
mod home;
mod layout;
mod links;
mod pane;
mod platform;
mod session_list;
mod sessions;
mod terminal;
mod text_input;
mod theme;
mod theme_picker;
mod update;
mod windows;
mod workspace;

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui::{actions, App, Application, KeyBinding, Menu, MenuItem, SystemMenuType};

use app_state::Store;
use update::Updater;
use workspace::{
    CloseTab, NewClaude, NewCodex, NewTerminal, NewWindow, NextPane, NextTab, OpenFolder, PrevPane,
    PrevTab, Quit, SearchChats, SelectTerminal1, SelectTerminal2, SelectTerminal3, SelectTerminal4,
    SelectTerminal5, SelectTerminal6, SelectTerminal7, SelectTerminal8, SelectTerminal9, SplitDown,
    SplitRight, ToggleSidebar,
};

actions!(
    nexus,
    [
        About,
        CheckForUpdates,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
    ]
);

/// The folder named on the command line, if any. A file opens its folder.
fn requested_root() -> Option<PathBuf> {
    // Flags are not folders; old systems pass `-psn_…` to apps they launch.
    let argument = std::env::args_os().nth(1)?;
    if argument.to_string_lossy().starts_with('-') {
        return None;
    }
    platform::folder_for(&PathBuf::from(argument))
}

/// Names of environment variables that mark this process as running inside
/// a Claude Code session.
fn inherited_agent_variables(
    names: impl Iterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    names
        .filter(|name| name.to_string_lossy().starts_with("CLAUDE"))
        .collect()
}

fn bind_keys(cx: &mut App) {
    text_input::bind_keys(cx);
    terminal::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-shift-n", NewWindow, None),
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
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
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
            items: vec![
                MenuItem::action("About Nexus", About),
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Nexus", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Nexus", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Window", NewWindow),
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::separator(),
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("New Claude Chat", NewClaude),
                MenuItem::action("New Codex Chat", NewCodex),
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
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        },
    ]
}

/// Actions that belong to the app rather than to any one view.
fn register_app_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &NewWindow, cx| windows::open_home_window(cx));
    cx.on_action(|_: &About, _| platform::show_about_panel());
    cx.on_action(|_: &CheckForUpdates, cx| {
        Updater::global(cx).update(cx, |updater, cx| updater.check(true, cx));
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
}

fn main() {
    // When Nexus is launched from inside an agent session it inherits that
    // session's variables, and every Claude started in a Nexus terminal would
    // then run as a child of it. Terminals are login shells, so anything set
    // in the user's own shell profile comes back.
    for name in inherited_agent_variables(std::env::vars_os().map(|(name, _)| name)) {
        std::env::remove_var(name);
    }
    platform::ensure_locale();
    let requested = requested_root();

    // Folders opened from Finder, the Dock or `open -a Nexus` arrive as
    // URLs, possibly before the app has finished launching.
    let (opened_tx, mut opened_rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
    let app = Application::new().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        let _ = opened_tx.unbounded_send(urls);
    });
    app.run(move |cx: &mut App| {
        let store = Store::load();
        if let Some(name) = store.theme() {
            theme::set_active_by_name(name);
        }
        let reopen = store.reopen_list();
        cx.set_global(store);
        Updater::init(cx);

        bind_keys(cx);
        cx.set_menus(menus());
        cx.set_dock_menu(vec![MenuItem::action("New Window", NewWindow)]);
        register_app_actions(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Folders the app was launched to open are already waiting.
        let mut requested: Vec<PathBuf> = requested.into_iter().collect();
        while let Ok(urls) = opened_rx.try_recv() {
            requested.extend(opened_folders(&urls));
        }

        // A folder named at launch wins; otherwise pick up where the last
        // run left off, and failing that show the home view.
        if !requested.is_empty() {
            for root in requested {
                windows::open_workspace_window(&root, cx);
            }
        } else if !reopen.is_empty() {
            for root in reopen {
                windows::open_workspace_window(&root, cx);
            }
        } else {
            windows::open_home_window(cx);
        }

        cx.spawn(async move |cx| {
            while let Some(urls) = opened_rx.next().await {
                let opened = cx.update(|cx| {
                    for root in opened_folders(&urls) {
                        windows::open_workspace_window(&root, cx);
                    }
                });
                if opened.is_err() {
                    break;
                }
            }
        })
        .detach();
    });
}

/// The folders named by URLs the system asked the app to open.
fn opened_folders(urls: &[String]) -> Vec<PathBuf> {
    urls.iter()
        .filter_map(|url| platform::path_from_file_url(url))
        .filter_map(|path| platform::folder_for(&path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_agent_session_variables_are_scrubbed() {
        let names = [
            "PATH",
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_PID",
            "ANTHROPIC_API_KEY",
            "HOME",
        ];
        let scrubbed = inherited_agent_variables(names.iter().map(std::ffi::OsString::from));
        assert_eq!(
            scrubbed,
            vec!["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_PID"]
        );
    }
}
