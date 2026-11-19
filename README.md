# Nexus

A fast agent multiplexer / IDE built with Rust and [GPUI](https://gpui.rs).

Work in progress. Planned:

- File tree sidebar; open files in any pane
- Tiled terminal panes that are quick to create and split
- Cmd-click file paths printed by agents to open them in the file viewer
- Searchable list of Claude Code and Codex chats; click one to resume it in the active pane

## Build

```sh
cargo run --release
```

Requires macOS. The Rust toolchain is pinned in `rust-toolchain.toml`.
