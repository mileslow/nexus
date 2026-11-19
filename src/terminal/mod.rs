//! A terminal pane: an `alacritty_terminal` grid fed by a PTY, drawn by
//! [`element::TerminalElement`].

pub mod element;
pub mod keys;
mod proc;

use std::collections::HashMap;
use std::ops::Range;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point as AlacPoint};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
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

use crate::links::{self, Link, LinkTarget};
use crate::sessions::{shell_quote, Agent};
use crate::theme;

actions!(terminal, [Copy, Paste, Clear]);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        KeyBinding::new("cmd-k", Clear, ctx),
    ]);
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

const MAX_LINK_ROWS: usize = 24;

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
    ContextMenu(Point<Pixels>),
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
    spawned_at: Instant,
    pub session_id: Option<String>,
    initial_cwd: PathBuf,
    workspace_root: PathBuf,
    child_pid: i32,
    pty_fd: RawFd,
    foreground_pid: i32,
    process_name: String,
    process_cwd: Option<PathBuf>,

    exited: bool,
    focused: bool,
    last_output: Instant,
    last_input: Instant,
    working_since: Option<Instant>,
    status: Status,

    selecting: bool,
    mouse_captured: bool,
    last_reported_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    hovered_link: Option<HoveredLink>,
    hover_cell: Option<AlacPoint>,
    marked_text: Option<String>,
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
            // Hang up the shell (which hangs up its own jobs), but leave the
            // reader thread running: it keeps draining output, so nothing
            // blocks on a full PTY while exiting, and it stops by itself once
            // the shell is gone. Stopping it first can leave the shell stuck
            // mid-exit for many seconds.
            unsafe {
                libc::kill(self.child_pid, libc::SIGHUP);
            }
            // Programs that ignore SIGHUP must not keep the reader alive forever.
            let pty_tx = self.pty_tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                let _ = pty_tx.send(Msg::Shutdown);
            });
        }
        unsafe {
            libc::close(self.pty_fd);
        }
    }
}

/// Marks a descriptor close-on-exec so shells started later do not inherit it.
fn set_cloexec(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
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

        let shell = std::env::var("SHELL")
            .ok()
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "/bin/zsh".to_string());
        let args = match &options.command {
            // A login + interactive shell loads the user's PATH so agent CLIs
            // resolve, and `exec` leaves a usable shell behind afterwards.
            Some(command) => vec![
                "-l".to_string(),
                "-i".to_string(),
                "-c".to_string(),
                format!("{command}; exec {} -l -i", shell_quote(&shell)),
            ],
            None => vec!["-l".to_string()],
        };
        let cwd = if options.cwd.is_dir() {
            options.cwd.clone()
        } else {
            links::home_dir().unwrap_or_else(|| PathBuf::from("/"))
        };
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "Nexus".to_string());
        let pty_options = tty::Options {
            shell: Some(tty::Shell::new(shell, args)),
            working_directory: Some(cwd.clone()),
            drain_on_exit: false,
            env,
        };

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let pty = tty::new(&pty_options, window_size, id as u64)?;
        let child_pid = pty.child().id() as i32;
        // Without this every later terminal's shell would inherit this PTY's
        // master side, keeping it open after its own tab is closed.
        set_cloexec(pty.file().as_raw_fd());
        // Our own handle on the PTY, used only to query its foreground process.
        let pty_fd = unsafe { libc::fcntl(pty.file().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };

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
                spawned_at: now,
                session_id: options.session_id,
                initial_cwd: cwd,
                workspace_root: options.workspace_root,
                child_pid,
                pty_fd,
                foreground_pid: child_pid,
                process_name: String::new(),
                process_cwd: None,
                exited: false,
                focused: false,
                last_output: now,
                last_input: now,
                working_since: None,
                status: Status::Idle,
                selecting: false,
                mouse_captured: false,
                last_reported_cell: None,
                scroll_remainder: 0.,
                hovered_link: None,
                hover_cell: None,
                marked_text: None,
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
            if !title.is_empty() {
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
        let pid = proc::foreground_pid(self.pty_fd).unwrap_or(self.child_pid);
        let name = proc::process_name(pid).unwrap_or_default();
        let cwd = proc::process_cwd(pid).or_else(|| proc::process_cwd(self.child_pid));
        let mut changed = false;
        if pid != self.foreground_pid || name != self.process_name {
            self.foreground_pid = pid;
            match proc::detect_agent(&name, pid) {
                Some(agent) => {
                    self.agent = Some(agent);
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

    /// The agent exited back to the shell: this is a plain terminal again.
    fn agent_finished(&mut self) {
        self.agent = None;
        self.launch_agent = None;
        self.agent_seen = false;
        self.label = None;
        self.session_id = None;
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

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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
        if event.modifiers.platform {
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

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.term.lock().selection_to_string();
        if let Some(text) = text.filter(|text| !text.is_empty()) {
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
        // Ctrl-L asks the foreground program to redraw a clean screen.
        self.input(vec![0x0c], cx);
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

        if event.modifiers.platform {
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

        if event.modifiers.platform {
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
                        LinkTarget::File {
                            path: PathBuf::from(path),
                            is_dir: std::path::Path::new(path).is_dir(),
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
            .bg(gpui::rgb(theme::TERM_BG))
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
                    cx.emit(TerminalEvent::ContextMenu(event.position));
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
            .child(element::TerminalElement::new(cx.entity(), focused))
    }
}
