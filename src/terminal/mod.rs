//! A terminal pane: an `alacritty_terminal` grid fed by a PTY, drawn by
//! [`element::TerminalElement`].

pub mod element;
pub mod keys;
mod proc;
pub mod shell;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point as AlacPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, KeyBinding, KeyDownEvent, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    Size, Task, TouchPhase, Window,
};

use crate::file_tree::DraggedPath;
use crate::find::{self, FindBar, FindEvent};
use crate::links::{self, Link, LinkTarget};
use crate::platform::keys;
use crate::sessions::{shell_quote, Agent};
use crate::theme;

actions!(terminal, [Copy, Paste, Clear]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Terminal");
    // On Windows plain Ctrl+C belongs to the program in the terminal.
    cx.bind_keys([
        KeyBinding::new(keys("cmd-c", "ctrl-shift-c"), Copy, ctx),
        KeyBinding::new(keys("cmd-v", "ctrl-shift-v"), Paste, ctx),
        KeyBinding::new(keys("cmd-k", "ctrl-shift-k"), Clear, ctx),
    ]);
    if cfg!(windows) {
        cx.bind_keys([
            KeyBinding::new("ctrl-v", Paste, ctx),
            KeyBinding::new("shift-insert", Paste, ctx),
        ]);
    }
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

const MAX_LINK_ROWS: usize = 24;

/// How long a shell that is still starting up gets to finish the command it
/// is in the middle of before it is hung up.
const STARTUP_SETTLE: Duration = Duration::from_secs(2);
/// A shell this old is taken to have finished starting, whatever it runs.
const STARTUP_LIMIT: Duration = Duration::from_secs(120);

/// How many shells are being hung up on a thread of their own.
static HANGING_UP: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// Counts one shell in `HANGING_UP` for as long as it lives.
struct HangingUp;

impl HangingUp {
    fn begin() -> Self {
        *HANGING_UP.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        Self
    }
}

impl Drop for HangingUp {
    fn drop(&mut self) {
        *HANGING_UP.0.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        HANGING_UP.1.notify_all();
    }
}

/// Waits for the shells that are in the middle of being hung up. The app
/// calls this on its way out: exiting sooner would end them wherever they
/// are, which is what waiting for a quiet moment is there to avoid.
pub fn finish_hang_ups() {
    let (count, changed) = &HANGING_UP;
    let pending = count.lock().unwrap_or_else(|e| e.into_inner());
    let limit = STARTUP_SETTLE + Duration::from_millis(250);
    let _ = changed.wait_timeout_while(pending, limit, |pending| *pending > 0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Forwards PTY-thread events to the UI thread. Wakeups are coalesced so a
/// flood of output schedules at most one repaint at a time.
#[derive(Clone)]
pub struct Listener {
    tx: UnboundedSender<AlacEvent>,
    wake_pending: Arc<AtomicBool>,
}

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        if matches!(event, AlacEvent::Wakeup) && self.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.tx.unbounded_send(event);
    }
}

pub struct SpawnOptions {
    pub cwd: PathBuf,
    /// Run this command first; the tab drops back to a shell when it exits.
    pub command: Option<String>,
    pub label: Option<String>,
    pub agent: Option<Agent>,
    pub session_id: Option<String>,
    pub workspace_root: PathBuf,
}

pub enum TerminalEvent {
    TitleChanged,
    Closed,
    Open(LinkTarget),
    /// Right-clicked at this window position.
    ContextMenu {
        position: Point<Pixels>,
        /// The file or folder named by the text that was right-clicked.
        path: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Producing output right now.
    Working,
    /// Rang the bell while out of view.
    Attention,
    /// Finished a burst of work while out of view.
    Done,
    Idle,
}

/// What to type when a path is dropped on a terminal. An agent gets an
/// `@` mention relative to its folder, which attaches the file to the chat;
/// a shell gets the quoted absolute path.
pub fn dropped_path_text(path: &std::path::Path, cwd: &std::path::Path, agent: bool) -> String {
    if agent {
        let shown = path.strip_prefix(cwd).unwrap_or(path).to_string_lossy();
        // Mentions end at whitespace, so such paths fall back to quoting.
        if !shown.is_empty() && !shown.contains(char::is_whitespace) {
            return format!("@{shown} ");
        }
    }
    format!("{} ", shell_quote(&path.to_string_lossy()))
}

/// Windows shells title the terminal with the path of their own program
/// (`Administrator: C:\\…\\pwsh.exe`), which says nothing. Titles worth
/// showing come from what is run in them.
fn is_program_path_title(title: &str) -> bool {
    title.to_ascii_lowercase().ends_with(".exe")
}

/// A link under the pointer while Cmd is held.
#[derive(Clone, Debug, PartialEq)]
pub struct HoveredLink {
    pub target: LinkTarget,
    /// `(grid line, column range)` spans to underline.
    pub spans: Vec<(i32, Range<usize>)>,
}

pub struct TerminalView {
    pub id: usize,
    term: Arc<FairMutex<Term<Listener>>>,
    pty_tx: EventLoopSender,
    wake_pending: Arc<AtomicBool>,
    focus_handle: FocusHandle,
    size: TermSize,
    cell: Size<Pixels>,
    grid_origin: Point<Pixels>,
    bounds: Bounds<Pixels>,

    osc_title: Option<String>,
    /// The chat's title, while a resumed chat is running here.
    label: Option<String>,
    /// The agent CLI currently in the foreground (or about to start).
    pub agent: Option<Agent>,
    /// What this tab was launched to run, until that agent shows up.
    launch_agent: Option<Agent>,
    agent_seen: bool,
    agent_since: Option<u64>,
    spawned_at: Instant,
    pub session_id: Option<String>,
    initial_cwd: PathBuf,
    workspace_root: PathBuf,
    processes: Arc<proc::Watch>,
    foreground_pid: i32,
    process_name: String,
    process_cwd: Option<PathBuf>,

    exited: bool,
    focused: bool,
    last_output: Instant,
    last_input: Instant,
    /// Whether anything has been typed, pasted or dropped here.
    had_input: bool,
    working_since: Option<Instant>,
    status: Status,

    selecting: bool,
    mouse_captured: bool,
    last_reported_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    hovered_link: Option<HoveredLink>,
    hover_cell: Option<AlacPoint>,
    marked_text: Option<String>,
    find_bar: Entity<FindBar>,
    find_open: bool,
    /// The compiled find query. Drawing borrows it to mark what is on screen.
    find_regex: RefCell<Option<RegexSearch>>,
    find_current: Option<Match>,
    _events: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        if !self.exited {
            // A shell closed while its startup files are still running is
            // not cut off in the middle of one of their commands.
            let settle = if self.starting() {
                STARTUP_SETTLE
            } else {
                Duration::ZERO
            };
            // Programs that ignore the hang-up must not keep the reader
            // alive forever. On Windows closing the terminal is what ends
            // them, so there is nothing to wait for.
            let grace = if cfg!(windows) {
                Duration::ZERO
            } else {
                Duration::from_secs(3)
            };
            let processes = self.processes.clone();
            let pty_tx = self.pty_tx.clone();
            let hanging_up = HangingUp::begin();
            std::thread::spawn(move || {
                // Hang up the shell (which hangs up its own jobs), but leave
                // the reader thread running: it keeps draining output, so
                // nothing blocks on a full PTY while exiting, and it stops by
                // itself once the shell is gone. Stopping it first can leave
                // the shell stuck mid-exit for many seconds.
                processes.hang_up_between_commands(settle);
                drop(hanging_up);
                std::thread::sleep(grace);
                let _ = pty_tx.send(Msg::Shutdown);
            });
        }
    }
}

impl TerminalView {
    pub fn spawn(
        options: SpawnOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let size = TermSize {
            cols: 100,
            rows: 30,
        };
        let window_size = WindowSize {
            num_lines: size.rows as u16,
            num_cols: size.cols as u16,
            cell_width: 8,
            cell_height: 17,
        };

        let shell = shell::program();
        let args = shell::arguments(&shell, options.command.as_deref());
        let cwd = if options.cwd.is_dir() {
            options.cwd.clone()
        } else {
            links::home_dir().unwrap_or_else(|| PathBuf::from("/"))
        };
        // Windows resolves folders to a spelling shells cannot start in.
        let cwd = PathBuf::from(crate::platform::strip_verbatim(&cwd.to_string_lossy()));
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "Nexus".to_string());
        let pty_options = tty::Options {
            shell: Some(tty::Shell::new(shell, args)),
            working_directory: Some(cwd.clone()),
            drain_on_exit: false,
            env,
            #[cfg(windows)]
            escape_args: true,
        };

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let pty = tty::new(&pty_options, window_size, id as u64)?;
        let processes = Arc::new(proc::Watch::new(&pty));
        let child_pid = processes.shell_pid();

        let (tx, mut rx) = unbounded();
        let wake_pending = Arc::new(AtomicBool::new(false));
        let listener = Listener {
            tx,
            wake_pending: wake_pending.clone(),
        };
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let pty_tx = event_loop.channel();
        event_loop.spawn();

        Ok(cx.new(|cx| {
            let events = cx.spawn(async move |this, cx| {
                while let Some(event) = rx.next().await {
                    if this
                        .update(cx, |this: &mut Self, cx| this.handle_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });

            let focus_handle = cx.focus_handle();
            cx.on_focus(&focus_handle, window, |this: &mut Self, _, cx| {
                this.focus_changed(true, cx)
            })
            .detach();
            cx.on_blur(&focus_handle, window, |this: &mut Self, _, cx| {
                this.focus_changed(false, cx)
            })
            .detach();
            // A search works back from the newest output, so Enter steps up.
            let find_bar = cx.new(|cx| FindBar::new(focus_handle.clone(), true, cx));
            cx.subscribe(&find_bar, Self::on_find_event).detach();

            let now = Instant::now();
            let mut this = Self {
                id,
                term,
                pty_tx,
                wake_pending,
                focus_handle,
                size,
                cell: Size {
                    width: px(8.),
                    height: px(17.),
                },
                grid_origin: Point::default(),
                bounds: Bounds::default(),
                osc_title: None,
                label: options.label,
                agent: options.agent,
                launch_agent: options.agent,
                agent_seen: false,
                agent_since: None,
                spawned_at: now,
                session_id: options.session_id,
                initial_cwd: cwd,
                workspace_root: options.workspace_root,
                processes,
                foreground_pid: child_pid,
                process_name: String::new(),
                process_cwd: None,
                exited: false,
                focused: false,
                last_output: now,
                last_input: now,
                had_input: false,
                working_since: None,
                status: Status::Idle,
                selecting: false,
                mouse_captured: false,
                last_reported_cell: None,
                scroll_remainder: 0.,
                hovered_link: None,
                hover_cell: None,
                marked_text: None,
                find_bar,
                find_open: false,
                find_regex: RefCell::new(None),
                find_current: None,
                _events: events,
            };
            this.refresh_process();
            this
        }))
    }

    // ---- state shown in tabs and the sidebar -------------------------------

    pub fn title(&self) -> String {
        // A resumed chat keeps its own name; otherwise prefer whatever title
        // the running program set.
        if let Some(label) = &self.label {
            return label.clone();
        }
        if let Some(title) = self.osc_title.as_deref().map(str::trim) {
            if !title.is_empty() && !is_program_path_title(title) {
                return title.to_string();
            }
        }
        let dir = self.cwd();
        let dir_name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let process = self.process_label();
        if process.is_empty() {
            dir_name
        } else {
            format!("{process} · {dir_name}")
        }
    }

    /// A short name for what is running: the agent, else the process.
    pub fn process_label(&self) -> String {
        match self.agent {
            Some(agent) => agent.label().to_lowercase(),
            None => self.process_name.clone(),
        }
    }

    /// The directory the foreground program is working in.
    pub fn cwd(&self) -> PathBuf {
        self.process_cwd
            .clone()
            .unwrap_or_else(|| self.initial_cwd.clone())
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn hovered_link(&self) -> Option<&HoveredLink> {
        self.hovered_link.as_ref()
    }

    pub fn marked_text(&self) -> Option<&str> {
        self.marked_text.as_deref()
    }

    pub(crate) fn term(&self) -> &Arc<FairMutex<Term<Listener>>> {
        &self.term
    }

    /// Polls the PTY's foreground process. Returns true when anything shown
    /// in the UI changed.
    pub fn refresh_process(&mut self) -> bool {
        if self.exited {
            return false;
        }
        let pid = self.processes.foreground();
        let name = proc::process_name(pid).unwrap_or_default();
        let cwd = proc::process_cwd(pid).or_else(|| proc::process_cwd(self.processes.shell_pid()));
        let mut changed = false;
        if pid != self.foreground_pid || name != self.process_name {
            self.foreground_pid = pid;
            match proc::detect_agent(&name, pid) {
                Some(agent) => {
                    self.agent = Some(agent);
                    if !self.agent_seen {
                        self.agent_since = Some(crate::sessions::now_secs());
                    }
                    self.agent_seen = true;
                }
                None if self.agent_seen => self.agent_finished(),
                None => self.agent = self.launch_agent,
            }
            self.process_name = name;
            changed = true;
        }
        // A launch that never produced its agent (say, the CLI is missing)
        // should not keep wearing the agent's name.
        if !self.agent_seen
            && self.launch_agent.is_some()
            && self.spawned_at.elapsed() > Duration::from_secs(20)
        {
            self.agent_finished();
            changed = true;
        }
        if cwd != self.process_cwd {
            self.process_cwd = cwd;
            changed = true;
        }
        changed
    }

    /// Whether only the shell's own startup can have run here so far: no
    /// agent has shown up, nothing was typed, and the tab is new.
    fn starting(&self) -> bool {
        !self.agent_seen && !self.had_input && self.spawned_at.elapsed() < STARTUP_LIMIT
    }

    /// The agent exited back to the shell: this is a plain terminal again.
    fn agent_finished(&mut self) {
        self.agent = None;
        self.launch_agent = None;
        self.agent_seen = false;
        self.agent_since = None;
        self.label = None;
        self.session_id = None;
    }

    /// The name of the chat resumed here, if any.
    pub fn chat_title(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// When the agent CLI running here was first seen, in Unix seconds.
    pub fn agent_started_secs(&self) -> Option<u64> {
        self.agent_since
    }

    /// The process currently in the terminal's foreground.
    pub fn foreground_pid(&self) -> i32 {
        self.foreground_pid
    }

    /// Recomputes the activity indicator. `in_view` is true while the user is
    /// looking at this terminal. Returns true when the status changed.
    pub fn update_status(&mut self, in_view: bool) -> bool {
        let now = Instant::now();
        let recent = now.duration_since(self.last_output) < Duration::from_millis(1200);
        // Output right after a keystroke is just echo, not the program working.
        let echo = self.last_output < self.last_input + Duration::from_millis(350);
        let working = recent && !echo;

        let mut status = self.status;
        if working {
            self.working_since.get_or_insert(now);
            status = Status::Working;
        } else {
            let worked_for = self
                .working_since
                .take()
                .map(|since| self.last_output.saturating_duration_since(since));
            if status == Status::Working {
                let long_task = worked_for.is_some_and(|d| d > Duration::from_secs(2));
                status = if long_task && !in_view {
                    Status::Done
                } else {
                    Status::Idle
                };
            }
        }
        if in_view && matches!(status, Status::Done | Status::Attention) {
            status = Status::Idle;
        }
        let changed = status != self.status;
        self.status = status;
        changed
    }

    // ---- PTY plumbing -------------------------------------------------------

    fn handle_event(&mut self, event: AlacEvent, cx: &mut Context<Self>) {
        match event {
            AlacEvent::Wakeup => {
                self.wake_pending.store(false, Ordering::Release);
                self.last_output = Instant::now();
                cx.notify();
            }
            AlacEvent::Title(title) => {
                self.osc_title = Some(title);
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::ResetTitle => {
                self.osc_title = None;
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::Bell => {
                if !self.focused {
                    self.status = Status::Attention;
                    cx.emit(TerminalEvent::TitleChanged);
                }
            }
            AlacEvent::PtyWrite(text) => self.write(text.into_bytes()),
            AlacEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            AlacEvent::ClipboardLoad(_, format) => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .unwrap_or_default();
                self.write(format(&text).into_bytes());
            }
            AlacEvent::ColorRequest(index, format) => {
                let color = {
                    let term = self.term.lock();
                    term.colors()[index].unwrap_or_else(|| element::default_color(index))
                };
                self.write(format(color).into_bytes());
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                self.write(format(self.window_size()).into_bytes());
            }
            AlacEvent::ChildExit(_) | AlacEvent::Exit => {
                if !self.exited {
                    self.exited = true;
                    cx.emit(TerminalEvent::Closed);
                }
            }
            AlacEvent::CursorBlinkingChange | AlacEvent::MouseCursorDirty => {}
        }
    }

    fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.pty_tx.send(Msg::Input(bytes.into()));
        }
    }

    /// Sends user-originated input and snaps the view back to the prompt.
    pub fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        {
            let mut term = self.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        self.last_input = Instant::now();
        self.had_input = true;
        self.write(bytes);
        cx.notify();
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.rows as u16,
            num_cols: self.size.cols as u16,
            cell_width: f32::from(self.cell.width).round().max(1.) as u16,
            cell_height: f32::from(self.cell.height).round().max(1.) as u16,
        }
    }

    /// Called by the element each frame with its measured geometry.
    pub(crate) fn set_layout(
        &mut self,
        bounds: Bounds<Pixels>,
        grid_origin: Point<Pixels>,
        cell: Size<Pixels>,
        size: TermSize,
    ) {
        self.bounds = bounds;
        self.grid_origin = grid_origin;
        self.cell = cell;
        if size != self.size {
            self.size = size;
            self.term.lock().resize(size);
            let _ = self.pty_tx.send(Msg::Resize(self.window_size()));
        }
    }

    fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            });
        }
        if focused {
            self.update_status(true);
        } else {
            self.set_hovered_link(None, cx);
        }
        cx.emit(TerminalEvent::TitleChanged);
        cx.notify();
    }

    // ---- keyboard -----------------------------------------------------------

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into the find bar belong to it, not to the shell.
        if !self.focus_handle.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let mode = self.mode();

        // Shift+PageUp/PageDown browse scrollback unless a full-screen app owns the screen.
        if keystroke.modifiers.shift && !mode.contains(TermMode::ALT_SCREEN) {
            let scroll = match keystroke.key.as_str() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.term.lock().scroll_display(scroll);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

        if let Some(bytes) = keys::to_bytes(keystroke, mode) {
            self.input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.secondary() {
            let position = window.mouse_position();
            if self.bounds.contains(&position) {
                let (point, _) = self.grid_point(position);
                self.hover_cell = Some(point);
                let link = self.link_at(point);
                self.set_hovered_link(link, cx);
            }
        } else {
            self.set_hovered_link(None, cx);
        }
    }

    /// Committed text from the keyboard or an input method.
    pub(crate) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.input(text.as_bytes().to_vec(), cx);
    }

    pub(crate) fn set_marked_text(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        self.marked_text = text.filter(|text| !text.is_empty());
        cx.notify();
    }

    /// The text currently selected with the mouse, if any.
    pub fn selected_text(&self) -> Option<String> {
        let text = self.term.lock().selection_to_string();
        text.filter(|text| !text.is_empty())
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let bytes = keys::paste_bytes(&text, self.mode());
            self.input(bytes, cx);
        }
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        self.term.lock().grid_mut().clear_history();
        self.find_current = None;
        // Ctrl-L asks the foreground program to redraw a clean screen.
        self.input(vec![0x0c], cx);
    }

    // ---- find -----------------------------------------------------------------

    /// Opens the find bar, or returns to it if it is already open.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = true;
        self.find_bar.update(cx, |bar, cx| bar.focus(window, cx));
        self.set_find_query(cx);
    }

    fn on_find_event(&mut self, _: Entity<FindBar>, event: &FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed => self.set_find_query(cx),
            FindEvent::Next => self.find_step(true, cx),
            FindEvent::Previous => self.find_step(false, cx),
            FindEvent::Dismissed => {
                self.find_open = false;
                *self.find_regex.borrow_mut() = None;
                self.find_current = None;
                cx.notify();
            }
        }
    }

    /// Compiles the query in the bar and moves to the nearest match at or
    /// above the bottom of the screen.
    fn set_find_query(&mut self, cx: &mut Context<Self>) {
        let query = self.find_bar.read(cx).query(cx);
        *self.find_regex.borrow_mut() = if query.is_empty() {
            None
        } else {
            RegexSearch::new(&literal_pattern(&query)).ok()
        };
        self.find_current = None;
        self.find_step(false, cx);
    }

    /// Steps to the match below (`forward`) or above the current one,
    /// wrapping around the ends of the scrollback.
    pub fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.find_open {
            return;
        }
        let mut found = None;
        let mut position = None;
        let mut total = 0;
        if let Some(regex) = self.find_regex.borrow_mut().as_mut() {
            let mut term = self.term.lock();
            // Output and resizing move the grid under a remembered match.
            let last = AlacPoint::new(term.bottommost_line(), term.last_column());
            let first = AlacPoint::new(term.topmost_line(), Column(0));
            let current = self.find_current.take().filter(|current| {
                *current.start() >= first
                    && *current.end() <= last
                    && current.start().column <= last.column
                    && current.end().column <= last.column
            });
            let (origin, direction, side) = match (&current, forward) {
                (Some(current), true) => (
                    current.end().add(&*term, Boundary::None, 1),
                    Direction::Right,
                    Side::Left,
                ),
                (Some(current), false) => (
                    current.start().sub(&*term, Boundary::None, 1),
                    Direction::Left,
                    Side::Right,
                ),
                (None, _) => {
                    let offset = term.grid().display_offset() as i32;
                    let bottom = Line(term.screen_lines() as i32 - 1 - offset);
                    (
                        AlacPoint::new(bottom, term.last_column()),
                        Direction::Left,
                        Side::Right,
                    )
                }
            };
            found = term.search_next(regex, origin, direction, side, None);
            if let Some(found) = &found {
                // Bring an off-screen match a third of the way down, clear of
                // the find bar and with some of what led up to it.
                let offset = term.grid().display_offset() as i32;
                let rows = term.screen_lines() as i32;
                let line = found.start().line.0;
                if !(3..rows).contains(&(line + offset)) {
                    let wanted = (rows / 3 - line).clamp(0, term.grid().history_size() as i32);
                    term.scroll_display(Scroll::Delta(wanted - offset));
                }
                for other in RegexIter::new(first, last, Direction::Right, &*term, regex) {
                    if other == *found {
                        position = Some(total);
                    }
                    total += 1;
                    if total >= find::MAX_MATCHES {
                        break;
                    }
                }
            }
        }
        self.find_current = found;
        self.find_bar
            .update(cx, |bar, cx| bar.set_results(position, total, cx));
        cx.notify();
    }

    /// The matches on screen, top to bottom, and the one the search is on.
    pub(crate) fn visible_matches(&self, term: &Term<Listener>) -> (Vec<Match>, Option<Match>) {
        let mut regex = self.find_regex.borrow_mut();
        let Some(regex) = regex.as_mut().filter(|_| self.find_open) else {
            return (Vec::new(), None);
        };
        let offset = term.grid().display_offset() as i32;
        let top = AlacPoint::new(Line(-offset), Column(0));
        let bottom = AlacPoint::new(
            Line(term.screen_lines() as i32 - 1 - offset),
            term.last_column(),
        );
        let matches = RegexIter::new(top, bottom, Direction::Right, term, regex)
            .take(2_000)
            .collect();
        (matches, self.find_current.clone())
    }

    fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = paths
            .paths()
            .iter()
            .map(|path| shell_quote(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            window.focus(&self.focus_handle);
            let bytes = keys::paste_bytes(&format!("{text} "), self.mode());
            self.input(bytes, cx);
        }
    }

    /// A file or folder dragged in from the sidebar.
    fn on_drop_dragged_path(
        &mut self,
        dragged: &DraggedPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = dropped_path_text(&dragged.path, &self.cwd(), self.agent.is_some());
        window.focus(&self.focus_handle);
        let bytes = keys::paste_bytes(&text, self.mode());
        self.input(bytes, cx);
    }

    // ---- mouse --------------------------------------------------------------

    /// Maps a window position to a grid cell and the half of the cell hit.
    fn grid_point(&self, position: Point<Pixels>) -> (AlacPoint, Direction) {
        let (column, row, side) = self.viewport_cell(position);
        let display_offset = self.term.lock().grid().display_offset();
        (
            AlacPoint::new(Line(row as i32 - display_offset as i32), Column(column)),
            side,
        )
    }

    fn viewport_cell(&self, position: Point<Pixels>) -> (usize, usize, Direction) {
        let x = (position.x - self.grid_origin.x) / self.cell.width;
        let y = (position.y - self.grid_origin.y) / self.cell.height;
        let column = (x.floor().max(0.) as usize).min(self.size.cols.saturating_sub(1));
        let row = (y.floor().max(0.) as usize).min(self.size.rows.saturating_sub(1));
        let side = if x - x.floor() < 0.5 {
            Direction::Left
        } else {
            Direction::Right
        };
        (column, row, side)
    }

    fn report_mouse(
        &mut self,
        button: u8,
        pressed: bool,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) {
        let (column, row, _) = self.viewport_cell(position);
        self.last_reported_cell = Some((column, row));
        if let Some(bytes) = keys::mouse_report(
            button,
            pressed,
            column,
            row,
            modifiers.shift,
            modifiers.alt,
            modifiers.control,
            self.mode(),
        ) {
            self.write(bytes);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let (point, side) = self.grid_point(event.position);

        if event.modifiers.secondary() {
            if let Some(link) = self.link_at(point) {
                cx.emit(TerminalEvent::Open(link.target));
                return;
            }
        }

        if self.mode().intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            self.mouse_captured = true;
            self.report_mouse(0, true, event.position, event.modifiers);
            return;
        }

        let kind = match event.click_count {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.term.lock().selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            let (point, side) = self.grid_point(event.position);
            let mut term = self.term.lock();
            if event.position.y < self.bounds.top() {
                term.scroll_display(Scroll::Delta(1));
            } else if event.position.y > self.bounds.bottom() {
                term.scroll_display(Scroll::Delta(-1));
            }
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
            drop(term);
            cx.notify();
            return;
        }

        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let dragging = event.pressed_button == Some(MouseButton::Left);
            let wants_motion = mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && dragging);
            if wants_motion {
                let (column, row, _) = self.viewport_cell(event.position);
                if self.last_reported_cell != Some((column, row)) {
                    let button = if dragging { 32 } else { 35 };
                    self.report_mouse(button, true, event.position, event.modifiers);
                }
            }
        }

        if event.modifiers.secondary() {
            let (point, _) = self.grid_point(event.position);
            if self.hover_cell != Some(point) {
                self.hover_cell = Some(point);
                let link = self.link_at(point);
                self.set_hovered_link(link, cx);
            }
        } else if self.hovered_link.is_some() {
            self.set_hovered_link(None, cx);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_captured {
            self.mouse_captured = false;
            self.report_mouse(0, false, event.position, event.modifiers);
        }
        if self.selecting {
            self.selecting = false;
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(event.touch_phase, TouchPhase::Started) {
            self.scroll_remainder = 0.;
        }
        let delta = event.delta.pixel_delta(self.cell.height).y;
        self.scroll_remainder += delta / self.cell.height;
        let lines = self.scroll_remainder.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32;

        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs().min(8) {
                self.report_mouse(button, true, event.position, event.modifiers);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !event.modifiers.shift
        {
            // Full-screen programs without mouse support scroll with arrow keys.
            let letter = if lines > 0 { 'A' } else { 'B' };
            let sequence = if mode.contains(TermMode::APP_CURSOR) {
                format!("\x1bO{letter}")
            } else {
                format!("\x1b[{letter}")
            };
            self.write(
                sequence
                    .repeat(lines.unsigned_abs().min(8) as usize)
                    .into_bytes(),
            );
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    // ---- links --------------------------------------------------------------

    fn set_hovered_link(&mut self, link: Option<HoveredLink>, cx: &mut Context<Self>) {
        if link.is_none() {
            self.hover_cell = None;
        }
        if self.hovered_link != link {
            self.hovered_link = link;
            cx.notify();
        }
    }

    /// Directories a relative path printed in this terminal may be relative to.
    fn link_bases(&self) -> Vec<PathBuf> {
        let mut bases = Vec::with_capacity(3);
        for base in [
            self.process_cwd.clone(),
            Some(self.initial_cwd.clone()),
            Some(self.workspace_root.clone()),
        ]
        .into_iter()
        .flatten()
        {
            if !bases.contains(&base) {
                bases.push(base);
            }
        }
        bases
    }

    /// Finds a file path or URL at a grid cell.
    /// The file or folder named by the text at `position`, if it names one.
    fn path_at(&self, position: Point<Pixels>) -> Option<PathBuf> {
        let (point, _) = self.grid_point(position);
        match self.link_at(point)?.target {
            LinkTarget::File { path, .. } => Some(path),
            LinkTarget::Url(_) => None,
        }
    }

    fn link_at(&self, point: AlacPoint) -> Option<HoveredLink> {
        // Copy the logical line out so no disk access happens under the lock.
        let (chars, positions, index, hyperlink) = {
            let term = self.term.lock();
            let grid = term.grid();
            let last_column = Column(grid.columns().checked_sub(1)?);
            if point.line < grid.topmost_line()
                || point.line > grid.bottommost_line()
                || point.column > last_column
            {
                return None;
            }

            // Programs can attach explicit hyperlinks to cells (OSC 8).
            let hyperlink = grid[point].hyperlink().map(|link| link.uri().to_string());

            let wraps = |line: Line| grid[line][last_column].flags.contains(Flags::WRAPLINE);
            let mut first = point.line;
            let mut last = point.line;
            let mut budget = MAX_LINK_ROWS;
            while first > grid.topmost_line() && wraps(first - 1) && budget > 0 {
                first -= 1;
                budget -= 1;
            }
            while last < grid.bottommost_line() && wraps(last) && budget > 0 {
                last += 1;
                budget -= 1;
            }

            let mut chars = Vec::new();
            let mut positions = Vec::new();
            let mut index = 0;
            let mut line = first;
            while line <= last {
                for column in 0..=last_column.0 {
                    let cell = &grid[line][Column(column)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    if line < point.line || (line == point.line && column <= point.column.0) {
                        index = chars.len();
                    }
                    chars.push(cell.c);
                    positions.push((line.0, column));
                }
                line += 1;
            }
            (chars, positions, index, hyperlink)
        };

        let link = match hyperlink {
            Some(uri) => {
                // Underline the word under the pointer for explicit hyperlinks.
                let mut start = index;
                while start > 0 && !chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                let mut end = index + 1;
                while end < chars.len() && !chars[end].is_whitespace() {
                    end += 1;
                }
                let target = match uri.strip_prefix("file://") {
                    Some(path) => {
                        // `file://host/path`: drop an optional host component.
                        let path = match path.find('/') {
                            Some(0) | None => path,
                            Some(slash) => &path[slash..],
                        };
                        let path = crate::platform::local_path(path, cfg!(windows));
                        LinkTarget::File {
                            is_dir: path.is_dir(),
                            path,
                            line: None,
                            column: None,
                        }
                    }
                    None => LinkTarget::Url(uri),
                };
                Link {
                    range: start..end,
                    target,
                }
            }
            None => links::find_link_at(&chars, index, &self.link_bases(), &links::fs_probe)?,
        };

        let mut spans: Vec<(i32, Range<usize>)> = Vec::new();
        for &(line, column) in &positions[link.range.clone()] {
            match spans.last_mut() {
                Some((last_line, range)) if *last_line == line => range.end = column + 1,
                _ => spans.push((line, column..column + 1)),
            }
        }
        Some(HoveredLink {
            target: link.target,
            spans,
        })
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .id(("terminal", self.id))
            .size_full()
            .bg(gpui::rgb(theme::term_bg()))
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle);
                    cx.emit(TerminalEvent::ContextMenu {
                        position: event.position,
                        path: this.path_at(event.position),
                    });
                }),
            )
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered {
                    this.set_hovered_link(None, cx);
                }
            }))
            .on_drop(cx.listener(Self::on_drop_paths))
            .on_drop(cx.listener(Self::on_drop_dragged_path))
            .drag_over::<DraggedPath>(|style, _, _, _| style.bg(theme::bg_hover()))
            .child(element::TerminalElement::new(cx.entity(), focused))
            .when(self.find_open, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(px(8.))
                        .right(px(18.))
                        .child(self.find_bar.clone()),
                )
            })
    }
}

/// A regular expression that matches `query` as plain text.
fn literal_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 8);
    for c in query.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_shell_naming_its_own_program_is_not_a_title() {
        assert!(is_program_path_title(
            r"Administrator: C:\Program Files\PowerShell\7\pwsh.exe"
        ));
        assert!(is_program_path_title(r"C:\WINDOWS\system32\cmd.EXE"));
        assert!(!is_program_path_title("✳ Fix the login bug"));
        assert!(!is_program_path_title("vim notes.txt"));
    }

    #[test]
    fn find_queries_are_searched_as_plain_text() {
        assert_eq!(literal_pattern("main.rs"), r"main\.rs");
        assert_eq!(literal_pattern("f(x) [y]"), r"f\(x\) \[y\]");
        assert_eq!(literal_pattern(r"a\b"), r"a\\b");
        assert_eq!(literal_pattern("plain words"), "plain words");
        // Every escaped pattern compiles.
        assert!(RegexSearch::new(&literal_pattern("*?+{[(|^$.#&-~")).is_ok());
    }

    #[test]
    fn dropped_paths_become_mentions_for_agents_and_quoted_paths_for_shells() {
        let cwd = Path::new("/work/app");
        assert_eq!(
            dropped_path_text(Path::new("/work/app/src/main.rs"), cwd, true),
            "@src/main.rs "
        );
        // Outside the agent's folder the mention is absolute.
        assert_eq!(
            dropped_path_text(Path::new("/etc/hosts"), cwd, true),
            "@/etc/hosts "
        );
        assert_eq!(
            dropped_path_text(Path::new("/work/app/my notes.md"), cwd, true),
            "'/work/app/my notes.md' "
        );
        assert_eq!(
            dropped_path_text(Path::new("/work/app/src/main.rs"), cwd, false),
            "/work/app/src/main.rs "
        );
        // Quoting follows the shell terminals start; see `shell::quote`.
        #[cfg(not(windows))]
        assert_eq!(
            dropped_path_text(Path::new("/work/it's here/a.txt"), cwd, false),
            r"'/work/it'\''s here/a.txt' "
        );
    }
}
