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
mod session_list;
mod sessions;
mod terminal;
mod text_input;
mod theme;
mod theme_picker;
mod windows;
mod workspace;

use std::path::PathBuf;

use gpui::{App, Application, KeyBinding, Menu, MenuItem};

use app_state::Store;
use workspace::{
    CloseTab, NewClaude, NewCodex, NewTerminal, NewWindow, NextPane, NextTab, OpenFolder, PrevPane,
    PrevTab, Quit, SearchChats, SelectTerminal1, SelectTerminal2, SelectTerminal3, SelectTerminal4,
    SelectTerminal5, SelectTerminal6, SelectTerminal7, SelectTerminal8, SelectTerminal9, SplitDown,
    SplitRight, ToggleSidebar,
};

/// The folder named on the command line, if any. A file opens its folder.
fn requested_root() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::args_os().nth(1)?)
        .canonicalize()
        .ok()?;
    if path.is_dir() {
        Some(path)
    } else {
        path.parent().map(PathBuf::from)
    }
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
    ]
}

fn main() {
    // When Nexus is launched from inside an agent session it inherits that
    // session's variables, and every Claude started in a Nexus terminal would
    // then run as a child of it. Terminals are login shells, so anything set
    // in the user's own shell profile comes back.
    for name in inherited_agent_variables(std::env::vars_os().map(|(name, _)| name)) {
        std::env::remove_var(name);
    }
    let requested = requested_root();

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            let store = Store::load();
            if let Some(name) = store.theme() {
                theme::set_active_by_name(name);
            }
            let reopen = store.reopen_list();
            cx.set_global(store);

            bind_keys(cx);
            cx.set_menus(menus());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &NewWindow, cx| windows::open_home_window(cx));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            // A folder on the command line wins; otherwise pick up where the
            // last run left off, and failing that show the home view.
            match requested {
                Some(root) => windows::open_workspace_window(&root, cx),
                None if !reopen.is_empty() => {
                    for root in reopen {
                        windows::open_workspace_window(&root, cx);
                    }
                }
                None => windows::open_home_window(cx),
            }
        });
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
