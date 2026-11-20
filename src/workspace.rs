//! The window's root view: title bar, sidebar (terminals, files, chats) and
//! the tiled panes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    actions, anchored, deferred, div, prelude::*, px, relative, AnyElement, App, Axis,
    ClipboardItem, Context,
    DragMoveEvent, EmptyView, Entity, EntityId, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Point, SharedString, Task, WeakEntity,
    Window, WindowControlArea,
};

use crate::app_state::{SavedAxis, SavedChat, SavedLayout, SavedNode, SavedTab, Store};
use crate::assets::icon;
use crate::chrome::{self, AppMenu};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::file_view::FileView;
use crate::find::{Find, FindNext, FindPrevious};
use crate::home::display_path;
use crate::layout::{Direction, Node};
use crate::links::{self, LinkTarget};
use crate::pane::{
    icon_button, status_color, terminal_glyph, Pane, PaneEvent, PaneItem, TabPlacement,
};
use crate::session_list::{SessionList, SessionListEvent};
use crate::sessions::{self, Agent, Session};
use crate::terminal::{SpawnOptions, TerminalView};
use crate::theme;
use crate::theme_picker::ThemePicker;
use crate::update::UpdateBadge;
use crate::windows;

actions!(
    nexus,
    [
        NewTerminal,
        NewClaude,
        NewCodex,
        CloseTab,
        SplitRight,
        SplitDown,
        NextTab,
        PrevTab,
        NextPane,
        PrevPane,
        ToggleSidebar,
        SearchChats,
        OpenFolder,
        NewWindow,
        Quit,
        SelectTerminal1,
        SelectTerminal2,
        SelectTerminal3,
        SelectTerminal4,
        SelectTerminal5,
        SelectTerminal6,
        SelectTerminal7,
        SelectTerminal8,
        SelectTerminal9,
    ]
);

const TICK: Duration = Duration::from_millis(700);
/// Save the layout every this many ticks (about two seconds).
const SAVE_EVERY: usize = 3;
const SIDEBAR_MIN: f32 = 190.;
const SIDEBAR_MAX: f32 = 620.;

const SECTION_TERMINALS: usize = 0;
const SECTION_FILES: usize = 1;
const SECTION_CHATS: usize = 2;

#[derive(Clone, Copy)]
enum MenuChoice {
    Copy,
    Split(Direction),
    Close,
}

/// Drag payload for the divider between two panes.
struct SplitDrag {
    split_id: usize,
    index: usize,
}

/// Drag payload for the sidebar's right edge.
struct SidebarDrag;

pub struct Workspace {
    root: PathBuf,
    focus_handle: FocusHandle,
    file_tree: Entity<FileTree>,
    sessions: Entity<SessionList>,
    center: Node<Entity<Pane>>,
    active_pane: Entity<Pane>,
    /// The pane that most recently received a file, so files gather there.
    file_pane: Option<WeakEntity<Pane>>,
    sidebar_width: Pixels,
    sidebar_visible: bool,
    collapsed: [bool; 3],
    next_split_id: usize,
    window_active: bool,
    /// The right-click menu for a terminal, while it is open.
    terminal_menu: Option<TerminalMenu>,
    theme_picker: Entity<ThemePicker>,
    update_badge: Entity<UpdateBadge>,
    app_menu: Entity<AppMenu>,
    ticks: usize,
    _tick: Task<()>,
}

struct TerminalMenu {
    position: Point<Pixels>,
    pane: Entity<Pane>,
    terminal: EntityId,
    /// Whether the menu was opened over the terminal's text rather than its
    /// row in the sidebar; only then does it offer Copy.
    over_text: bool,
    /// What was selected when the menu opened. Held here because output
    /// arriving while the menu is up can clear the selection.
    selection: Option<String>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let file_tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        cx.subscribe_in(
            &file_tree,
            window,
            |this, _, event: &FileTreeEvent, window, cx| match event {
                FileTreeEvent::OpenFile(path) => {
                    this.open_file(path.clone(), None, None, window, cx)
                }
            },
        )
        .detach();

        let sessions = cx.new(|cx| SessionList::new(root.clone(), cx));
        cx.subscribe_in(
            &sessions,
            window,
            |this, _, event: &SessionListEvent, window, cx| match event {
                SessionListEvent::Open(session) => this.open_session(session, window, cx),
                SessionListEvent::Dismissed => this.focus_active_pane(window, cx),
            },
        )
        .detach();

        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if this.window_active {
                // Catch up on whatever changed while the app was in the background.
                this.file_tree.update(cx, |tree, cx| tree.refresh(cx));
                this.sessions.update(cx, |sessions, cx| sessions.rescan(cx));
            }
        })
        .detach();

        let tick = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this
                .update_in(cx, |this: &mut Self, _, cx| this.tick(cx))
                .is_err()
            {
                break;
            }
        });

        let pane = cx.new(|cx| {
            let mut pane = Pane::new(window, cx);
            pane.set_active(true, cx);
            pane
        });
        Self::observe_pane(&pane, window, cx);
        window.set_window_title(&window_title(&root));

        let mut this = Self {
            root,
            focus_handle: cx.focus_handle(),
            file_tree,
            sessions,
            center: Node::Leaf(pane.clone()),
            active_pane: pane.clone(),
            file_pane: None,
            sidebar_width: px(280.),
            sidebar_visible: true,
            collapsed: [false; 3],
            next_split_id: 0,
            window_active: true,
            terminal_menu: None,
            theme_picker: cx.new(|_| ThemePicker::new()),
            update_badge: cx.new(UpdateBadge::new),
            app_menu: cx.new(|_| AppMenu::new()),
            ticks: 0,
            _tick: tick,
        };

        // Come back the way this workspace was left, if it has been open before.
        match cx.global::<Store>().layout_for(&this.root) {
            Some(layout) => this.restore(layout, window, cx),
            None => this.spawn_terminal(&pane, None, window, cx),
        }

        let root = this.root.clone();
        let handle = window.window_handle();
        cx.update_global::<Store, _>(|store, _| store.workspace_opened(&root, handle));
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |workspace, cx| workspace.window_closing(cx));
            true
        });
        cx.on_app_quit(|workspace, cx| {
            workspace.save_layout(cx);
            async {}
        })
        .detach();
        this
    }

    // ---- remembered layout --------------------------------------------------

    /// Describes the current panes, tabs and sidebar so they can be rebuilt.
    fn snapshot(&self, cx: &App) -> SavedLayout {
        let chats = self.running_chats(cx);
        let tree = self.snapshot_node(&self.center, &chats, cx);
        SavedLayout {
            sidebar_width: f32::from(self.sidebar_width),
            sidebar_visible: self.sidebar_visible,
            collapsed: self.collapsed,
            tree,
            active_pane: self
                .panes()
                .iter()
                .position(|pane| *pane == self.active_pane)
                .unwrap_or(0),
        }
    }

    /// The chat running in each agent terminal, so it can be resumed later.
    fn running_chats(&self, cx: &App) -> HashMap<EntityId, SavedChat> {
        let sessions = self.sessions.read(cx);
        let agents: Vec<_> = self
            .terminals(cx)
            .into_iter()
            .filter_map(|(_, _, entity)| {
                let terminal = entity.read(cx);
                Some((entity.entity_id(), terminal.agent?, entity))
            })
            .collect();
        let mut chats = HashMap::new();
        if agents.is_empty() {
            return chats;
        }
        // Chats already tied to a terminal, or open in any other program,
        // cannot be guessed as another terminal's chat.
        let live = sessions::live_claude_sessions();
        let mut claimed: HashSet<String> = live.values().cloned().collect();
        let mut unknown = Vec::new();

        for (id, agent, entity) in &agents {
            let terminal = entity.read(cx);
            // Claude Code says exactly which chat a process has open, which
            // stays right after `/resume` or `/clear` inside the agent.
            let recorded = (*agent == Agent::Claude)
                .then(|| live.get(&terminal.foreground_pid()))
                .flatten()
                .and_then(|session| sessions.chat(session));
            let resumed = terminal.session_id.as_ref().map(|session| SavedChat {
                agent: *agent,
                id: session.clone(),
                title: terminal
                    .chat_title()
                    .map(str::to_owned)
                    .unwrap_or_else(|| terminal.title()),
            });
            match recorded.or(resumed) {
                Some(chat) => {
                    claimed.insert(chat.id.clone());
                    chats.insert(*id, chat);
                }
                None => unknown.push((*id, *agent, terminal.cwd(), terminal.agent_started_secs())),
            }
        }
        for (id, agent, cwd, started) in unknown {
            let Some(started) = started else { continue };
            if let Some(chat) = sessions.infer_chat(agent, &cwd, started, &claimed) {
                claimed.insert(chat.id.clone());
                chats.insert(id, chat);
            }
        }
        chats
    }

    fn snapshot_node(
        &self,
        node: &Node<Entity<Pane>>,
        chats: &HashMap<EntityId, SavedChat>,
        cx: &App,
    ) -> SavedNode {
        match node {
            Node::Leaf(pane) => {
                let pane = pane.read(cx);
                let tabs = pane
                    .items()
                    .iter()
                    .map(|item| match item {
                        PaneItem::Terminal(terminal) => SavedTab::Terminal {
                            cwd: terminal.read(cx).cwd(),
                            chat: chats.get(&terminal.entity_id()).cloned(),
                        },
                        PaneItem::File(file) => SavedTab::File {
                            path: file.read(cx).path().to_path_buf(),
                        },
                    })
                    .collect();
                SavedNode::Pane {
                    tabs,
                    active: pane.active_index(),
                }
            }
            Node::Split {
                axis,
                children,
                sizes,
                ..
            } => SavedNode::Split {
                axis: match axis {
                    Axis::Horizontal => SavedAxis::Horizontal,
                    Axis::Vertical => SavedAxis::Vertical,
                },
                sizes: sizes.clone(),
                children: children
                    .iter()
                    .map(|child| self.snapshot_node(child, chats, cx))
                    .collect(),
            },
        }
    }

    fn save_layout(&mut self, cx: &mut Context<Self>) {
        let layout = self.snapshot(cx);
        if cx.global::<Store>().layout_for(&self.root).as_ref() != Some(&layout) {
            let root = self.root.clone();
            cx.update_global::<Store, _>(|store, _| store.save_layout(&root, layout));
        }
    }

    /// The user is closing this workspace's window.
    fn window_closing(&mut self, cx: &mut Context<Self>) {
        self.save_layout(cx);
        let root = self.root.clone();
        cx.update_global::<Store, _>(|store, _| store.workspace_closed(&root));
    }

    /// Rebuilds panes, tabs and sidebar from a remembered layout. Terminals
    /// start fresh shells in their old folders; chats are resumed.
    fn restore(&mut self, layout: SavedLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_width = px(layout.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX));
        self.sidebar_visible = layout.sidebar_visible;
        self.collapsed = layout.collapsed;
        self.center = self.restore_node(&layout.tree, window, cx);

        let panes = self.panes();
        let active = panes
            .get(layout.active_pane)
            .or_else(|| panes.first())
            .cloned()
            .expect("a layout always has a pane");
        self.active_pane = active.clone();
        active.update(cx, |pane, cx| pane.set_active(true, cx));
        // A workspace with nothing in it would be a dead end.
        if panes.iter().all(|pane| pane.read(cx).is_empty()) {
            self.spawn_terminal(&active, None, window, cx);
        }
        self.sync_open_sessions(cx);
        active.read(cx).focus_active(window, cx);
    }

    fn restore_node(
        &mut self,
        node: &SavedNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Node<Entity<Pane>> {
        match node {
            SavedNode::Pane { tabs, active } => {
                let pane = self.create_pane(window, cx);
                for tab in tabs {
                    match tab {
                        SavedTab::Terminal { cwd, chat } => {
                            let options = SpawnOptions {
                                cwd: cwd.clone(),
                                command: chat
                                    .as_ref()
                                    .map(|chat| sessions::resume_command(chat.agent, &chat.id)),
                                label: chat.as_ref().map(|chat| chat.title.clone()),
                                agent: chat.as_ref().map(|chat| chat.agent),
                                session_id: chat.as_ref().map(|chat| chat.id.clone()),
                                workspace_root: self.root.clone(),
                            };
                            self.spawn_terminal(&pane, Some(options), window, cx);
                        }
                        SavedTab::File { path } => {
                            if path.is_file() {
                                let root = self.root.clone();
                                let view =
                                    cx.new(|cx| FileView::new(path.clone(), &root, None, cx));
                                pane.update(cx, |pane, cx| {
                                    pane.add_item(PaneItem::File(view), window, cx)
                                });
                            }
                        }
                    }
                }
                let active = *active;
                pane.update(cx, |pane, cx| {
                    if active < pane.items().len() {
                        pane.activate(active, window, cx);
                    }
                });
                Node::Leaf(pane)
            }
            SavedNode::Split {
                axis,
                sizes,
                children,
            } => {
                let mut restored: Vec<Node<Entity<Pane>>> = children
                    .iter()
                    .map(|child| self.restore_node(child, window, cx))
                    .collect();
                match restored.len() {
                    0 => Node::Leaf(self.create_pane(window, cx)),
                    1 => restored.remove(0),
                    count => {
                        // Trust the saved sizes only if they are sane.
                        let total: f32 = sizes.iter().sum();
                        let usable = sizes.len() == count
                            && sizes.iter().all(|size| size.is_finite() && *size > 0.01)
                            && total > 0.;
                        let sizes = if usable {
                            sizes.iter().map(|size| size / total).collect()
                        } else {
                            vec![1. / count as f32; count]
                        };
                        self.next_split_id += 1;
                        Node::Split {
                            id: self.next_split_id,
                            axis: match axis {
                                SavedAxis::Horizontal => Axis::Horizontal,
                                SavedAxis::Vertical => Axis::Vertical,
                            },
                            children: restored,
                            sizes,
                        }
                    }
                }
            }
        }
    }

    fn observe_pane(pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(pane, window, Self::on_pane_event).detach();
    }

    fn create_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new(window, cx));
        Self::observe_pane(&pane, window, cx);
        pane
    }

    fn panes(&self) -> Vec<Entity<Pane>> {
        self.center.leaves()
    }

    fn set_active_pane(&mut self, pane: &Entity<Pane>, cx: &mut Context<Self>) {
        if self.active_pane != *pane {
            self.active_pane
                .update(cx, |pane, cx| pane.set_active(false, cx));
            self.active_pane = pane.clone();
        }
        pane.update(cx, |pane, cx| pane.set_active(true, cx));
    }

    // ---- periodic upkeep ----------------------------------------------------

    /// Refreshes process names, activity dots and on-disk file changes.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.ticks += 1;
        if self.ticks % SAVE_EVERY == 0 {
            self.save_layout(cx);
        }
        let mut any_changed = false;
        for pane in self.panes() {
            let (items, active) = {
                let pane = pane.read(cx);
                (pane.items().to_vec(), pane.active_index())
            };
            let pane_is_active = pane == self.active_pane;
            let mut pane_changed = false;
            for (index, item) in items.iter().enumerate() {
                match item {
                    PaneItem::Terminal(terminal) => {
                        let in_view = self.window_active && pane_is_active && index == active;
                        pane_changed |= terminal.update(cx, |terminal, _| {
                            let process_changed = terminal.refresh_process();
                            let status_changed = terminal.update_status(in_view);
                            process_changed || status_changed
                        });
                    }
                    PaneItem::File(file) => {
                        if index == active {
                            file.update(cx, |file, cx| file.check_for_changes(cx));
                        }
                    }
                }
            }
            if pane_changed {
                any_changed = true;
                pane.update(cx, |_, cx| cx.notify());
            }
        }
        if any_changed {
            // An agent exiting frees its chat to be resumed again.
            self.sync_open_sessions(cx);
            cx.notify();
        }
    }

    // ---- terminals ----------------------------------------------------------

    /// The directory new terminals in `pane` should start in.
    fn default_cwd(&self, pane: &Entity<Pane>, cx: &App) -> PathBuf {
        match pane.read(cx).active_item() {
            Some(PaneItem::Terminal(terminal)) => terminal.read(cx).cwd(),
            _ => self.root.clone(),
        }
    }

    fn spawn_terminal(
        &mut self,
        pane: &Entity<Pane>,
        options: Option<SpawnOptions>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = options.unwrap_or_else(|| SpawnOptions {
            cwd: self.default_cwd(pane, cx),
            command: None,
            label: None,
            agent: None,
            session_id: None,
            workspace_root: self.root.clone(),
        });
        match TerminalView::spawn(options, window, cx) {
            Ok(terminal) => pane.update(cx, |pane, cx| {
                pane.add_item(PaneItem::Terminal(terminal), window, cx)
            }),
            Err(error) => eprintln!("nexus: failed to start terminal: {error:#}"),
        }
    }

    fn spawn_agent(&mut self, agent: Agent, window: &mut Window, cx: &mut Context<Self>) {
        let command = match agent {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        };
        let pane = self.active_pane.clone();
        let options = SpawnOptions {
            cwd: self.root.clone(),
            command: Some(command.to_string()),
            label: None,
            agent: Some(agent),
            session_id: None,
            workspace_root: self.root.clone(),
        };
        self.spawn_terminal(&pane, Some(options), window, cx);
    }

    /// Shows a chat in the active pane, resuming it in a new terminal unless
    /// it is already running somewhere, in which case that tab is focused.
    fn open_session(&mut self, session: &Session, window: &mut Window, cx: &mut Context<Self>) {
        for pane in self.panes() {
            let existing = pane.read(cx).index_of_session(&session.id, cx);
            if let Some(index) = existing {
                pane.update(cx, |pane, cx| pane.activate(index, window, cx));
                return;
            }
        }
        let cwd = if session.cwd.is_empty() {
            self.root.clone()
        } else {
            PathBuf::from(&session.cwd)
        };
        let pane = self.active_pane.clone();
        let options = SpawnOptions {
            cwd,
            command: Some(session.resume_command()),
            label: Some(session.title.clone()),
            agent: Some(session.agent),
            session_id: Some(session.id.clone()),
            workspace_root: self.root.clone(),
        };
        self.spawn_terminal(&pane, Some(options), window, cx);
    }

    /// Every terminal in sidebar order, with the pane and tab that hold it.
    fn terminals(&self, cx: &App) -> Vec<(Entity<Pane>, usize, Entity<TerminalView>)> {
        let mut out = Vec::new();
        for pane in self.panes() {
            for (index, item) in pane.read(cx).items().iter().enumerate() {
                if let Some(terminal) = item.as_terminal() {
                    out.push((pane.clone(), index, terminal.clone()));
                }
            }
        }
        out
    }

    /// Jumps to the nth terminal listed in the sidebar (zero-based).
    fn select_terminal(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, index, _)) = self.terminals(cx).into_iter().nth(position) {
            pane.update(cx, |pane, cx| pane.activate(index, window, cx));
        }
    }

    fn sync_open_sessions(&mut self, cx: &mut Context<Self>) {
        let mut ids = HashSet::new();
        for pane in self.panes() {
            for item in pane.read(cx).items() {
                if let PaneItem::Terminal(terminal) = item {
                    if let Some(id) = &terminal.read(cx).session_id {
                        ids.insert(id.clone());
                    }
                }
            }
        }
        self.sessions
            .update(cx, |sessions, cx| sessions.set_open_ids(ids, cx));
    }

    // ---- files --------------------------------------------------------------

    /// Picks where a file should open: next to the files already open, and
    /// never on top of a running terminal.
    fn file_target(
        &mut self,
        source: Option<&Entity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        if let Some(pane) = self.file_pane.as_ref().and_then(|pane| pane.upgrade()) {
            if self.center.contains(&pane) {
                return pane;
            }
        }
        if let Some(pane) = self
            .panes()
            .into_iter()
            .find(|pane| pane.read(cx).has_files())
        {
            return pane;
        }
        let base = source.cloned().unwrap_or_else(|| self.active_pane.clone());
        if base.read(cx).is_empty() {
            return base;
        }
        let pane = self.create_pane(window, cx);
        self.center.split(
            &base,
            pane.clone(),
            Direction::Right,
            &mut self.next_split_id,
        );
        pane
    }

    pub fn open_file(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        source: Option<&Entity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = links::normalize(&path);
        for pane in self.panes() {
            let existing = pane.read(cx).index_of_file(&path, cx);
            if let Some(index) = existing {
                pane.update(cx, |pane, cx| {
                    pane.activate(index, window, cx);
                    if let Some(PaneItem::File(file)) = pane.active_item() {
                        file.update(cx, |file, cx| file.go_to_line(line, cx));
                    }
                });
                self.file_pane = Some(pane.downgrade());
                self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
                return;
            }
        }

        let target = self.file_target(source, window, cx);
        let root = self.root.clone();
        let view = cx.new(|cx| FileView::new(path.clone(), &root, line, cx));
        target.update(cx, |pane, cx| {
            pane.add_item(PaneItem::File(view), window, cx)
        });
        self.file_pane = Some(target.downgrade());
        self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        cx.notify();
    }

    fn open_link(
        &mut self,
        target: &LinkTarget,
        source: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            LinkTarget::Url(url) => cx.open_url(url),
            LinkTarget::File { path, is_dir, .. } if *is_dir => {
                self.sidebar_visible = true;
                self.collapsed[SECTION_FILES] = false;
                self.file_tree.update(cx, |tree, cx| tree.reveal(path, cx));
                cx.notify();
            }
            LinkTarget::File { path, line, .. } => {
                self.open_file(path.clone(), *line, Some(source), window, cx)
            }
        }
    }

    // ---- panes --------------------------------------------------------------

    fn on_pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PaneEvent::Focused => {
                self.set_active_pane(pane, cx);
                self.reveal_active_file(cx);
                cx.notify();
            }
            PaneEvent::Emptied => self.remove_pane(pane, window, cx),
            PaneEvent::Changed => {
                self.sync_open_sessions(cx);
                if *pane == self.active_pane {
                    self.reveal_active_file(cx);
                }
                cx.notify();
            }
            PaneEvent::Open(target) => self.open_link(target, pane, window, cx),
            PaneEvent::Split(direction) => self.split_pane(pane, *direction, window, cx),
            PaneEvent::ContextMenu { position, item } => {
                self.open_terminal_menu(*position, pane.clone(), *item, true, cx)
            }
            PaneEvent::TabDropped {
                item,
                from,
                placement,
            } => self.move_tab(*item, *from, pane, *placement, window, cx),
            PaneEvent::NewTerminal => self.spawn_terminal(pane, None, window, cx),
        }
    }

    fn reveal_active_file(&mut self, cx: &mut Context<Self>) {
        let path = match self.active_pane.read(cx).active_item() {
            Some(PaneItem::File(file)) => Some(file.read(cx).path().to_path_buf()),
            _ => None,
        };
        if let Some(path) = path {
            self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        }
    }

    /// Moves a tab out of the pane it was dragged from and into `target`,
    /// either among its tabs or in a new pane split off one of its sides.
    fn move_tab(
        &mut self,
        item: EntityId,
        from: EntityId,
        target: &Entity<Pane>,
        placement: TabPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self
            .panes()
            .into_iter()
            .find(|pane| pane.entity_id() == from)
        else {
            return;
        };
        let same_pane = source == *target;
        match placement {
            TabPlacement::Split(direction) => {
                // Splitting a pane's only tab off itself would change nothing.
                if same_pane && source.read(cx).items().len() < 2 {
                    return;
                }
                let Some(moved) = source.update(cx, |pane, cx| pane.take_item(item, window, cx))
                else {
                    return;
                };
                let new_pane = self.create_pane(window, cx);
                let destination = if self.center.split(
                    target,
                    new_pane.clone(),
                    direction,
                    &mut self.next_split_id,
                ) {
                    new_pane
                } else {
                    target.clone()
                };
                destination.update(cx, |pane, cx| pane.add_item(moved, window, cx));
                self.set_active_pane(&destination, cx);
            }
            TabPlacement::Before(_) | TabPlacement::End => {
                if same_pane {
                    return;
                }
                let Some(moved) = source.update(cx, |pane, cx| pane.take_item(item, window, cx))
                else {
                    return;
                };
                let index = match placement {
                    TabPlacement::Before(index) => index,
                    _ => usize::MAX,
                };
                target.update(cx, |pane, cx| pane.insert_item(index, moved, window, cx));
                self.set_active_pane(target, cx);
            }
        }
        self.sync_open_sessions(cx);
        cx.notify();
    }

    /// Opens a new terminal pane beside `pane`, on the given side.
    fn split_pane(
        &mut self,
        pane: &Entity<Pane>,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.default_cwd(pane, cx);
        let new_pane = self.create_pane(window, cx);
        if !self
            .center
            .split(pane, new_pane.clone(), direction, &mut self.next_split_id)
        {
            return;
        }
        let options = SpawnOptions {
            cwd,
            command: None,
            label: None,
            agent: None,
            session_id: None,
            workspace_root: self.root.clone(),
        };
        self.spawn_terminal(&new_pane, Some(options), window, cx);
        self.set_active_pane(&new_pane, cx);
        cx.notify();
    }

    // ---- terminal right-click menu ------------------------------------------

    fn open_terminal_menu(
        &mut self,
        position: Point<Pixels>,
        pane: Entity<Pane>,
        terminal: EntityId,
        over_text: bool,
        cx: &mut Context<Self>,
    ) {
        let selection = if over_text {
            let pane = pane.read(cx);
            pane.index_of(terminal)
                .and_then(|index| pane.items()[index].as_terminal())
                .and_then(|terminal| terminal.read(cx).selected_text())
        } else {
            None
        };
        self.terminal_menu = Some(TerminalMenu {
            position,
            pane,
            terminal,
            over_text,
            selection,
        });
        cx.notify();
    }

    fn dismiss_terminal_menu(&mut self, cx: &mut Context<Self>) {
        if self.terminal_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Runs a menu choice against the terminal the menu was opened on.
    fn choose_from_terminal_menu(
        &mut self,
        choice: MenuChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.terminal_menu.take() else {
            return;
        };
        cx.notify();
        if let MenuChoice::Copy = choice {
            if let Some(text) = menu.selection {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        // The pane may have been closed while the menu was open.
        if !self.center.contains(&menu.pane) {
            return;
        }
        match choice {
            MenuChoice::Copy => {}
            MenuChoice::Split(direction) => self.split_pane(&menu.pane, direction, window, cx),
            MenuChoice::Close => menu.pane.update(cx, |pane, cx| {
                if let Some(index) = pane.index_of(menu.terminal) {
                    pane.close_item(index, window, cx);
                }
            }),
        }
    }

    fn render_terminal_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let menu = self.terminal_menu.as_ref()?;
        let row =
            |id: &'static str, glyph: &'static str, label: &'static str, choice: MenuChoice| {
                div()
                    .id(id)
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(icon(glyph, px(13.), theme::text_muted()))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_from_terminal_menu(choice, window, cx)
                    }))
            };
        // Copy stays in the menu with nothing selected, dimmed, so the other
        // rows keep their places.
        let copy = menu.over_text.then(|| {
            let separator = div().h(px(1.)).my(px(4.)).mx(px(4.)).bg(theme::border());
            let item = if menu.selection.is_some() {
                row("menu-copy", "copy", "Copy", MenuChoice::Copy).into_any_element()
            } else {
                div()
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_color(theme::text_faint())
                    .child(icon("copy", px(13.), theme::text_faint()))
                    .child("Copy")
                    .into_any_element()
            };
            [item, separator.into_any_element()]
        });
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("terminal-menu")
                            .occlude()
                            .w(px(168.))
                            .p(px(4.))
                            .flex()
                            .flex_col()
                            .bg(theme::bg_sidebar())
                            .border_1()
                            .border_color(theme::border_strong())
                            .rounded(px(8.))
                            .shadow_lg()
                            .text_color(theme::text())
                            .on_mouse_down_out(
                                cx.listener(|this, _, _, cx| this.dismiss_terminal_menu(cx)),
                            )
                            .children(copy.into_iter().flatten())
                            .child(row(
                                "menu-split-up",
                                "arrow-up",
                                "Split Up",
                                MenuChoice::Split(Direction::Up),
                            ))
                            .child(row(
                                "menu-split-right",
                                "arrow-right",
                                "Split Right",
                                MenuChoice::Split(Direction::Right),
                            ))
                            .child(row(
                                "menu-split-down",
                                "arrow-down",
                                "Split Down",
                                MenuChoice::Split(Direction::Down),
                            ))
                            .child(row(
                                "menu-split-left",
                                "arrow-left",
                                "Split Left",
                                MenuChoice::Split(Direction::Left),
                            ))
                            .child(div().h(px(1.)).my(px(4.)).mx(px(4.)).bg(theme::border()))
                            .child(row("menu-close", "close", "Close", MenuChoice::Close)),
                    ),
            )
            .with_priority(1),
        )
    }

    fn remove_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.panes();
        let Some(position) = panes.iter().position(|candidate| candidate == pane) else {
            return;
        };
        if !self.center.remove(pane) {
            // The last pane stays open, showing its empty state.
            pane.read(cx).focus_active(window, cx);
            cx.notify();
            return;
        }
        if self.active_pane == *pane {
            let remaining = self.panes();
            let next = remaining[position.min(remaining.len() - 1)].clone();
            self.set_active_pane(&next, cx);
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn focus_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane.read(cx).focus_active(window, cx);
    }

    fn cycle_pane(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.panes();
        let Some(position) = panes.iter().position(|pane| *pane == self.active_pane) else {
            return;
        };
        let next = (position as isize + delta).rem_euclid(panes.len() as isize) as usize;
        let next = panes[next].clone();
        self.set_active_pane(&next, cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    // ---- workspace folder ---------------------------------------------------

    /// Asks for a folder and opens it as a workspace in its own window.
    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = cx.update(|_, cx| windows::open_workspace_window(&path, cx));
                }
            }
        })
        .detach();
    }

    // ---- sidebar ------------------------------------------------------------

    fn toggle_section(&mut self, section: usize, cx: &mut Context<Self>) {
        self.collapsed[section] = !self.collapsed[section];
        cx.notify();
    }

    fn render_section_header(
        &self,
        section: usize,
        title: &'static str,
        detail: SharedString,
        actions: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chevron = if self.collapsed[section] {
            "chevron-right"
        } else {
            "chevron-down"
        };
        div()
            .id(("section", section))
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .pl(px(8.))
            .pr(px(4.))
            .when(section > 0, |header| {
                header.border_t_1().border_color(theme::border())
            })
            .cursor_pointer()
            .child(icon(chevron, px(11.), theme::text_faint()))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme::text_faint())
                    .child(detail),
            )
            .children(actions)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
    }

    fn render_terminal_rows(&self, cx: &mut Context<Self>) -> (usize, Vec<AnyElement>) {
        let mut rows = Vec::new();
        for pane in self.panes() {
            let pane_ref = pane.read(cx);
            let pane_is_active = pane == self.active_pane;
            for (index, item) in pane_ref.items().iter().enumerate() {
                let Some(terminal) = item.as_terminal() else {
                    continue;
                };
                let terminal = terminal.read(cx);
                let row_id = terminal.id;
                let selected = pane_is_active && index == pane_ref.active_index();
                let (glyph, glyph_color) = terminal_glyph(terminal.agent);
                let title: SharedString = terminal.title().into();
                let process: SharedString = terminal.process_label().into();
                let status = status_color(terminal.status());
                let activate_pane = pane.clone();
                let close_pane = pane.clone();
                let menu_pane = pane.clone();
                let terminal_id = item.entity_id();

                rows.push(
                    div()
                        .id(("terminal-row", row_id))
                        .group("terminal-row")
                        .h(px(26.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .pl(px(12.))
                        .pr(px(6.))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme::bg_active()))
                        .hover(|style| style.bg(theme::bg_hover()))
                        .child(icon(glyph, px(13.), glyph_color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(if selected {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(title),
                        )
                        .when_some(status, |row, color| {
                            row.child(div().flex_none().size(px(7.)).rounded_full().bg(color))
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(theme::text_faint())
                                .group_hover("terminal-row", |style| style.invisible())
                                .child(process),
                        )
                        .child(
                            div()
                                .id(("terminal-close", row_id))
                                .absolute()
                                .right(px(6.))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .invisible()
                                .group_hover("terminal-row", |style| style.visible())
                                .hover(|style| style.bg(theme::bg_active()))
                                .child(icon("close", px(11.), theme::text_muted()))
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    close_pane
                                        .update(cx, |pane, cx| pane.close_item(index, window, cx));
                                }),
                        )
                        .relative()
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_terminal_menu(
                                    event.position,
                                    menu_pane.clone(),
                                    terminal_id,
                                    false,
                                    cx,
                                )
                            }),
                        )
                        .on_click(move |_, window, cx| {
                            activate_pane.update(cx, |pane, cx| pane.activate(index, window, cx));
                        })
                        .into_any_element(),
                );
            }
        }
        (rows.len(), rows)
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (terminal_count, terminal_rows) = self.render_terminal_rows(cx);
        let (shown, total) = self.sessions.read(cx).counts();
        let chat_detail: SharedString = if shown == total {
            format!("{total}").into()
        } else {
            format!("{shown} of {total}").into()
        };
        let root_name: SharedString = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned())
            .into();

        let terminal_actions = vec![icon_button("sidebar-new-terminal", "plus")
            .size(px(20.))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                let pane = this.active_pane.clone();
                this.spawn_terminal(&pane, None, window, cx);
            }))
            .into_any_element()];
        let file_actions = vec![icon_button("sidebar-open-folder", "folder-open")
            .size(px(20.))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.open_folder(&OpenFolder, window, cx);
            }))
            .into_any_element()];
        let chat_actions = vec![icon_button("sidebar-rescan", "refresh")
            .size(px(20.))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.sessions.update(cx, |sessions, cx| sessions.rescan(cx));
            }))
            .into_any_element()];

        div()
            .relative()
            .w(self.sidebar_width)
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::bg_sidebar())
            .border_r_1()
            .border_color(theme::border())
            .child(self.render_section_header(
                SECTION_TERMINALS,
                "TERMINALS",
                format!("{terminal_count}").into(),
                terminal_actions,
                cx,
            ))
            .when(!self.collapsed[SECTION_TERMINALS], |sidebar| {
                sidebar.child(
                    div()
                        .id("terminal-rows")
                        .flex_none()
                        .max_h(px(236.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .pb(px(4.))
                        .children(terminal_rows),
                )
            })
            .child(self.render_section_header(SECTION_FILES, "FILES", root_name, file_actions, cx))
            .when(!self.collapsed[SECTION_FILES], |sidebar| {
                sidebar.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(self.file_tree.clone()),
                )
            })
            .child(self.render_section_header(
                SECTION_CHATS,
                "CHATS",
                chat_detail,
                chat_actions,
                cx,
            ))
            .when(!self.collapsed[SECTION_CHATS], |sidebar| {
                sidebar.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(self.sessions.clone()),
                )
            })
            // Keeps collapsed sections stacked at the top.
            .when(
                self.collapsed[SECTION_FILES] && self.collapsed[SECTION_CHATS],
                |sidebar| sidebar.child(div().flex_1()),
            )
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-3.))
                    .w(px(7.))
                    .cursor_col_resize()
                    .on_drag(SidebarDrag, |_, _, _, cx| cx.new(|_| EmptyView)),
            )
    }

    // ---- title bar and panes ------------------------------------------------

    fn render_launch_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        color: gpui::Rgba,
        label: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .occlude()
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border())
            .text_size(px(11.5))
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| {
                style
                    .bg(theme::bg_hover())
                    .border_color(theme::border_strong())
                    .text_color(theme::text())
            })
            .child(icon(glyph, px(12.), color))
            .child(label)
    }

    fn render_titlebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Leave room for the traffic lights unless the window is full screen.
        let leading = if window.is_fullscreen() || chrome::CUSTOM {
            10.
        } else {
            80.
        };
        let root_path: SharedString = display_path(&self.root).into();
        div()
            .id("titlebar")
            .h(theme::TITLEBAR_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(leading))
            .when(!chrome::CUSTOM, |bar| bar.pr(px(10.)))
            .bg(theme::bg_titlebar())
            .border_b_1()
            .border_color(theme::border())
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            // Windows moves the window by the bar; everything on it that
            // takes clicks occludes it.
            .when(chrome::CUSTOM, |bar| {
                bar.window_control_area(WindowControlArea::Drag)
                    .child(self.app_menu.clone())
            })
            .child(icon_button("toggle-sidebar", "sidebar").occlude().on_click(
                cx.listener(|this, _, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx)),
            ))
            .child(
                div()
                    .id("titlebar-root")
                    .occlude()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_muted())
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::text()))
                    .child(root_path)
                    // The workspace name leads back to the home view.
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        cx.defer(windows::open_home_window);
                    }),
            )
            .child(div().flex_1())
            .child(self.update_badge.clone())
            .child(
                self.render_launch_button("launch-claude", "sparkle", theme::claude(), "Claude")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.spawn_agent(Agent::Claude, window, cx);
                    })),
            )
            .child(
                self.render_launch_button("launch-codex", "sparkle", theme::codex(), "Codex")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.spawn_agent(Agent::Codex, window, cx);
                    })),
            )
            .child(
                self.render_launch_button(
                    "launch-terminal",
                    "terminal",
                    theme::text_muted(),
                    "Terminal",
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_terminal(&NewTerminal, window, cx);
                })),
            )
            .child(self.theme_picker.clone())
            .when(chrome::CUSTOM, |bar| {
                bar.child(chrome::caption_buttons(window))
            })
    }

    fn render_divider(
        &self,
        split_id: usize,
        index: usize,
        axis: Axis,
    ) -> impl IntoElement + use<> {
        let handle = div()
            .id(("split-handle", split_id * 64 + index))
            .absolute()
            .on_drag(SplitDrag { split_id, index }, |_, _, _, cx| {
                cx.new(|_| EmptyView)
            });
        let line = div().relative().flex_none().bg(theme::border());
        match axis {
            Axis::Horizontal => line.w(px(1.)).h_full().child(
                handle
                    .top_0()
                    .bottom_0()
                    .left(px(-3.))
                    .w(px(7.))
                    .cursor_col_resize(),
            ),
            Axis::Vertical => line.h(px(1.)).w_full().child(
                handle
                    .left_0()
                    .right_0()
                    .top(px(-3.))
                    .h(px(7.))
                    .cursor_row_resize(),
            ),
        }
    }

    fn render_node(&self, node: &Node<Entity<Pane>>, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(pane) => pane.clone().into_any_element(),
            Node::Split {
                id,
                axis,
                children,
                sizes,
            } => {
                let split_id = *id;
                let axis = *axis;
                let mut container = div().id(("split", split_id)).size_full().flex();
                if axis == Axis::Vertical {
                    container = container.flex_col();
                }
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        container = container.child(self.render_divider(split_id, index - 1, axis));
                    }
                    let cell = div()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .child(self.render_node(child, cx));
                    container = container.child(match axis {
                        Axis::Horizontal => cell.h_full().w(relative(sizes[index])),
                        Axis::Vertical => cell.w_full().h(relative(sizes[index])),
                    });
                }
                container
                    .on_drag_move::<SplitDrag>(cx.listener(
                        move |this, event: &DragMoveEvent<SplitDrag>, _, cx| {
                            let drag = event.drag(cx);
                            if drag.split_id != split_id {
                                return;
                            }
                            let index = drag.index;
                            let bounds = event.bounds;
                            let position = event.event.position;
                            let fraction = match axis {
                                Axis::Horizontal => {
                                    (position.x - bounds.left()) / bounds.size.width
                                }
                                Axis::Vertical => (position.y - bounds.top()) / bounds.size.height,
                            };
                            if this.center.resize(split_id, index, fraction) {
                                cx.notify();
                            }
                        },
                    ))
                    .into_any_element()
            }
        }
    }

    // ---- actions ------------------------------------------------------------

    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        self.spawn_terminal(&pane, None, window, cx);
    }

    fn new_claude(&mut self, _: &NewClaude, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_agent(Agent::Claude, window, cx);
    }

    fn new_codex(&mut self, _: &NewCodex, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_agent(Agent::Codex, window, cx);
    }

    /// `⌘F` searches whatever the active pane is showing.
    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_pane.read(cx).active_item().cloned() {
            Some(PaneItem::Terminal(terminal)) => {
                terminal.update(cx, |terminal, cx| terminal.open_find(window, cx))
            }
            Some(PaneItem::File(file)) => file.update(cx, |file, cx| file.open_find(window, cx)),
            None => {}
        }
    }

    /// `⌘G` and `⌘⇧G` repeat the search without going back to the bar.
    fn find_again(&mut self, onward: bool, cx: &mut Context<Self>) {
        match self.active_pane.read(cx).active_item().cloned() {
            // In a terminal, onward is up: toward older output.
            Some(PaneItem::Terminal(terminal)) => {
                terminal.update(cx, |terminal, cx| terminal.find_step(!onward, cx))
            }
            Some(PaneItem::File(file)) => file.update(cx, |file, cx| file.find_step(onward, cx)),
            None => {}
        }
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        if pane.read(cx).is_empty() {
            self.remove_pane(&pane, window, cx);
        } else {
            pane.update(cx, |pane, cx| pane.close_active(window, cx));
        }
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        self.split_pane(&pane, Direction::Right, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        self.split_pane(&pane, Direction::Down, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane
            .update(cx, |pane, cx| pane.cycle(1, window, cx));
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane
            .update(cx, |pane, cx| pane.cycle(-1, window, cx));
    }

    fn next_pane(&mut self, _: &NextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_pane(1, window, cx);
    }

    fn prev_pane(&mut self, _: &PrevPane, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_pane(-1, window, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        if !self.sidebar_visible {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    fn search_chats(&mut self, _: &SearchChats, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = true;
        self.collapsed[SECTION_CHATS] = false;
        window.focus(&self.sessions.focus_handle(cx));
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let center = self.render_node(&self.center, cx);
        div()
            .id("workspace")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_app())
            .text_color(theme::text())
            .text_size(theme::UI_TEXT_SIZE)
            .font_family(theme::UI_FONT)
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            // Any key dismisses the terminal menu; Escape does only that.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.terminal_menu.is_some() {
                    this.dismiss_terminal_menu(cx);
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_claude))
            .on_action(cx.listener(Self::new_codex))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.find_again(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.find_again(false, cx)))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::next_pane))
            .on_action(cx.listener(Self::prev_pane))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::search_chats))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(|this, _: &SelectTerminal1, window, cx| {
                this.select_terminal(0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal2, window, cx| {
                this.select_terminal(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal3, window, cx| {
                this.select_terminal(2, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal4, window, cx| {
                this.select_terminal(3, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal5, window, cx| {
                this.select_terminal(4, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal6, window, cx| {
                this.select_terminal(5, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal7, window, cx| {
                this.select_terminal(6, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal8, window, cx| {
                this.select_terminal(7, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectTerminal9, window, cx| {
                this.select_terminal(8, window, cx)
            }))
            .on_drag_move::<SidebarDrag>(cx.listener(
                |this, event: &DragMoveEvent<SidebarDrag>, _, cx| {
                    let width = f32::from(event.event.position.x - event.bounds.left());
                    let width = px(width.clamp(SIDEBAR_MIN, SIDEBAR_MAX));
                    if width != this.sidebar_width {
                        this.sidebar_width = width;
                        cx.notify();
                    }
                },
            ))
            .child(self.render_titlebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.sidebar_visible, |body| {
                        body.child(self.render_sidebar(cx))
                    })
                    .child(div().flex_1().min_w_0().h_full().child(center)),
            )
            .children(self.render_terminal_menu(cx))
    }
}

fn window_title(root: &Path) -> String {
    match root.file_name() {
        Some(name) => format!("{} — Nexus", name.to_string_lossy()),
        None => "Nexus".to_string(),
    }
}
