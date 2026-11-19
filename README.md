# Nexus

A fast agent multiplexer / IDE for macOS, built with Rust and [GPUI](https://gpui.rs).

Run Claude Code, Codex and plain shells side by side in tiled panes, browse the
project's files next to them, and jump back into any past chat from a
searchable list.

## Features

- **Tiled terminals.** Split right or down, drag dividers to resize, and keep
  several tabs per pane. Every terminal is listed in the sidebar with a dot
  showing whether it is working, finished while you were elsewhere, or rang
  the bell. `⌘1`–`⌘9` jump straight to one.
- **File explorer.** A lazily loaded tree of the open folder. Click a file to
  view it with syntax highlighting; the view reloads when the file changes on
  disk, so you can watch an agent edit it.
- **⌘-click paths.** Hold `⌘` over a path an agent printed (`src/main.rs:42`,
  `Update(app/page.tsx)`, `b/src/lib.rs`, quoted paths with spaces, URLs) and
  click to open it at that line. Paths are resolved against the terminal's
  current directory and only light up if they exist.
- **Chats.** Claude Code and Codex sessions found on this machine, newest
  first, searchable by title or folder. Clicking one resumes it in the active
  pane, or focuses it if it is already open.
- **Stays out of the way of files.** Files open beside the terminals rather
  than on top of them, and gather in one pane.

## Run

```sh
cargo run --release              # open the current directory
cargo run --release -- ~/code/app
```

Requires macOS. The Rust toolchain is pinned in `rust-toolchain.toml`
(rustup installs it on first build). Only the Xcode Command Line Tools are
needed: shaders are compiled at runtime, so full Xcode is not required.

The `claude` and `codex` commands must be on the `PATH` of your login shell.

## Keys

| Key | Action |
| --- | --- |
| `⌘T` | New terminal tab |
| `⌘⇧C` / `⌘⇧X` | New Claude / Codex chat |
| `⌘D` / `⌘⇧D` | Split right / down |
| `⌘W` | Close tab (closes the pane with its last tab) |
| `⌘]` / `⌘[` | Next / previous pane |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab |
| `⌘1`–`⌘9` | Jump to the nth terminal in the sidebar |
| `⌘P` | Search chats (`↑`/`↓` to move, `Enter` to open, `Esc` to leave) |
| `⌘B` | Toggle the sidebar |
| `⌘O` | Open a folder |
| `⌘`-click | Open the path or URL under the pointer |
| `⌘C` / `⌘V` / `⌘K` | Copy selection / paste / clear the terminal |
| `⇧PageUp` / `⇧PageDown` | Scroll back through terminal history |
| `⇧Enter` | Insert a newline in an agent prompt |

Option acts as Meta in terminals (`⌥←`/`⌥→` move by word). Dropping files
onto a terminal pastes their paths.

## How it is built

| Module | Role |
| --- | --- |
| `terminal/` | PTY + VT emulation (`alacritty_terminal`), a custom GPUI element that paints the grid, key/mouse encoding, foreground-process detection |
| `links.rs` | Finds file paths and URLs in a line of terminal text |
| `sessions.rs` | Discovers Claude Code and Codex chats on disk |
| `session_list.rs` | The searchable chat list |
| `file_tree.rs`, `file_view.rs` | File explorer and viewer |
| `layout.rs`, `pane.rs` | The split tree and tabbed panes |
| `workspace.rs` | Window root: title bar, sidebar, actions |

Performance choices worth knowing about:

- Terminal output is parsed on a dedicated thread per PTY. Wakeups are
  coalesced so a flood of output schedules at most one repaint at a time, and
  the grid is snapshotted under the lock but shaped and painted after it is
  released.
- Every list (files, chats, file lines) is virtualized.
- Chat logs can be hundreds of megabytes each, so only a bounded head and tail
  of each file is read, files are parsed in parallel, and results are cached by
  path, modification time and size in `~/Library/Caches/nexus`.
- Directory listing, file loading and syntax highlighting all run off the UI
  thread; highlighting streams in chunk by chunk.

## Tests

```sh
cargo test
```

Unit tests cover the non-UI logic: path detection, key and mouse encoding,
session-log parsing, chat filtering and the pane layout tree.

## Not yet

- The file view is read-only and has no text selection.
- macOS only.
