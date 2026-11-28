//! Nexus: a fast agent multiplexer / IDE built on GPUI.

// A Windows app, not a console program that would open a terminal window.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app_state;
mod assets;
mod chat_search;
mod chrome;
mod diff;
mod editor;
mod file_search;
mod file_tree;
mod file_view;
mod find;
mod home;
#[cfg(any(windows, test))]
mod instance;
mod layout;
mod links;
mod markdown;
mod pane;
mod platform;
mod selection;
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

use platform::keys;

use app_state::Store;
use update::Updater;
use workspace::{
    CloseTab, NewClaude, NewCodex, NewTerminal, NewWindow, NextPane, NextTab, OpenFolder, PrevPane,
    PrevTab, Quit, SearchChats, SearchFiles, SelectTerminal1, SelectTerminal2, SelectTerminal3, SelectTerminal4,
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

/// Shortcuts are `⌘` on a Mac. On Windows plain `Ctrl` keys belong to the
/// shell or agent in the terminal, so the app takes `Ctrl+Shift` and `Alt`,
/// the way Windows Terminal does.
fn bind_keys(cx: &mut App) {
    text_input::bind_keys(cx);
    find::bind_keys(cx);
    terminal::bind_keys(cx);
    file_view::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new(keys("cmd-shift-n", "ctrl-shift-n"), NewWindow, None),
        KeyBinding::new(keys("cmd-t", "ctrl-shift-t"), NewTerminal, None),
        KeyBinding::new(keys("cmd-shift-c", "alt-shift-c"), NewClaude, None),
        KeyBinding::new(keys("cmd-shift-x", "alt-shift-x"), NewCodex, None),
        KeyBinding::new(keys("cmd-w", "ctrl-shift-w"), CloseTab, None),
        KeyBinding::new(keys("cmd-d", "ctrl-shift-d"), SplitRight, None),
        KeyBinding::new(keys("cmd-shift-d", "ctrl-shift-e"), SplitDown, None),
        KeyBinding::new(keys("cmd-shift-]", "ctrl-pagedown"), NextTab, None),
        KeyBinding::new(keys("cmd-shift-[", "ctrl-pageup"), PrevTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new(keys("cmd-]", "alt-right"), NextPane, None),
        KeyBinding::new(keys("cmd-[", "alt-left"), PrevPane, None),
        KeyBinding::new(keys("cmd-b", "ctrl-shift-b"), ToggleSidebar, None),
        KeyBinding::new(keys("cmd-p", "ctrl-shift-p"), SearchChats, None),
        KeyBinding::new(keys("cmd-shift-f", "alt-shift-f"), SearchFiles, None),
        KeyBinding::new(keys("cmd-o", "ctrl-shift-o"), OpenFolder, None),
        KeyBinding::new(keys("cmd-q", "ctrl-shift-q"), Quit, None),
        KeyBinding::new(keys("cmd-1", "alt-1"), SelectTerminal1, None),
        KeyBinding::new(keys("cmd-2", "alt-2"), SelectTerminal2, None),
        KeyBinding::new(keys("cmd-3", "alt-3"), SelectTerminal3, None),
        KeyBinding::new(keys("cmd-4", "alt-4"), SelectTerminal4, None),
        KeyBinding::new(keys("cmd-5", "alt-5"), SelectTerminal5, None),
        KeyBinding::new(keys("cmd-6", "alt-6"), SelectTerminal6, None),
        KeyBinding::new(keys("cmd-7", "alt-7"), SelectTerminal7, None),
        KeyBinding::new(keys("cmd-8", "alt-8"), SelectTerminal8, None),
        KeyBinding::new(keys("cmd-9", "alt-9"), SelectTerminal9, None),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
        ]);
    }
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
                MenuItem::action("Find…", find::Find),
                MenuItem::action("Find Next", find::FindNext),
                MenuItem::action("Find Previous", find::FindPrevious),
                MenuItem::separator(),
                MenuItem::action("Clear Terminal", terminal::Clear),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Search Files", SearchFiles),
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
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &About, _| platform::show_about_panel());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    }
    cx.on_action(|_: &CheckForUpdates, cx| {
        Updater::global(cx).update(cx, |updater, cx| updater.check(true, cx));
    });
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

/// On Windows, hands what this copy was asked to open to the copy that is
/// already running, if there is one. Returns true when that happened and
/// this copy should exit.
#[cfg(windows)]
fn handed_to_running_copy(
    requested: Option<&PathBuf>,
    opened: &futures::channel::mpsc::UnboundedSender<Vec<PathBuf>>,
) -> bool {
    let Some(lock) = app_state::state_path().map(|path| path.with_file_name("instance")) else {
        return false;
    };
    let request = requested
        .map(|root| root.to_string_lossy().into_owned())
        .unwrap_or_default();
    match instance::claim(&lock, &request) {
        Ok(instance::Claim::HandedOff) => true,
        Ok(instance::Claim::Primary(server)) => {
            let opened = opened.clone();
            server.serve(move |request| {
                let folders = platform::folder_for(std::path::Path::new(&request));
                let _ = opened.unbounded_send(folders.into_iter().collect());
            });
            false
        }
        Err(_) => false,
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Nexus {}", update::VERSION);
        return;
    }
    update::wait_for_previous();
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
    // URLs, possibly before the app has finished launching. On Windows they
    // arrive from later copies of the app. No folders means "a new window".
    let (opened_tx, mut opened_rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
    #[cfg(windows)]
    if handed_to_running_copy(requested.as_ref(), &opened_tx) {
        return;
    }
    let app = Application::new().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        let folders = opened_folders(&urls);
        if !folders.is_empty() {
            let _ = opened_tx.unbounded_send(folders);
        }
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
        if cfg!(target_os = "macos") {
            cx.set_menus(menus());
            cx.set_dock_menu(vec![MenuItem::action("New Window", NewWindow)]);
        }
        register_app_actions(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // Quitting closes every terminal. This runs once they are closed, so
        // their shells are hung up in their own time before the app is gone.
        cx.on_app_quit(|_| async { terminal::finish_hang_ups() })
            .detach();

        // Folders the app was launched to open are already waiting.
        let mut requested: Vec<PathBuf> = requested.into_iter().collect();
        while let Ok(folders) = opened_rx.try_recv() {
            requested.extend(folders);
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
            while let Some(folders) = opened_rx.next().await {
                let opened = cx.update(|cx| {
                    if folders.is_empty() {
                        windows::open_home_window(cx);
                    }
                    for root in folders {
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
