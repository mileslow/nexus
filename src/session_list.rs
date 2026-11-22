//! The searchable list of Claude Code and Codex chats.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window,
};

use crate::app_state::{ChatMarks, SavedChat, Store};
use crate::assets::icon;
use crate::chat_search::{self, Match};
use crate::platform::{folder_key, is_within};
use crate::sessions::{self, Agent, Session};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

const RESCAN_INTERVAL: Duration = Duration::from_secs(20);
const ROW_HEIGHT: f32 = 44.;
/// Rows the agent found are taller: they say why.
const FOUND_ROW_HEIGHT: f32 = 76.;
const SEARCH_PLACEHOLDER: &str = "Search chats";
const ASK_PLACEHOLDER: &str = "Describe a chat, then press Return";

/// Stops a search by description when it is no longer wanted.
struct StopOnDrop(Arc<chat_search::Handle>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Where a search by description stands.
enum AgentSearch {
    Idle,
    Running {
        /// How many chats the agent is reading, and how many older ones
        /// were left out.
        reading: usize,
        left_out: usize,
        started: Instant,
        search: StopOnDrop,
        /// Keeps the elapsed time on screen moving.
        _tick: Task<()>,
    },
    Found {
        query: String,
        matches: Vec<Match>,
    },
    Failed(String),
}

pub enum SessionListEvent {
    Open(Session),
    /// Escape pressed in an empty search box: hand focus back to the panes.
    Dismissed,
}

pub struct SessionList {
    sessions: Arc<Vec<Session>>,
    /// Indices into `sessions` that match the current query and filters.
    filtered: Vec<usize>,
    search: Entity<TextInput>,
    agent_filter: Option<Agent>,
    root: PathBuf,
    /// When set, only chats started under `root` are listed.
    scope_to_root: bool,
    /// When set, only bookmarked chats are listed.
    bookmarks_only: bool,
    /// The pins and bookmarks the list was last arranged by.
    marks: ChatMarks,
    /// When set, the search box takes a description for an agent to find
    /// chats by, instead of words to filter titles with.
    ask: bool,
    agent_search: AgentSearch,
    /// What "This folder" was set to before asking turned it on.
    scope_before_ask: bool,
    highlighted: usize,
    open_ids: HashSet<String>,
    loaded: bool,
    scanning: bool,
    scroll: UniformListScrollHandle,
    _rescan: Task<()>,
}

impl EventEmitter<SessionListEvent> for SessionList {}

impl Focusable for SessionList {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// Returns indices of sessions matching every whitespace-separated term.
/// `among`, when given, limits the result to chats with those ids.
pub fn filter_sessions(
    sessions: &[Session],
    query: &str,
    agent: Option<Agent>,
    under: Option<&str>,
    among: Option<&BTreeSet<String>>,
) -> Vec<usize> {
    let query = query.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    let under = under.map(|root| folder_key(root, cfg!(windows)));
    sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| {
            agent.is_none_or(|agent| session.agent == agent)
                && among.is_none_or(|ids| ids.contains(&session.id))
                && under
                    .as_deref()
                    .is_none_or(|root| is_within(&folder_key(&session.cwd, cfg!(windows)), root))
                && terms.iter().all(|term| session.haystack.contains(term))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Moves pinned chats to the front, keeping the order within each group.
pub fn pinned_first(
    sessions: &[Session],
    rows: Vec<usize>,
    pinned: &BTreeSet<String>,
) -> Vec<usize> {
    if pinned.is_empty() {
        return rows;
    }
    let (mut front, rest): (Vec<usize>, Vec<usize>) = rows
        .into_iter()
        .partition(|&index| pinned.contains(&sessions[index].id));
    front.extend(rest);
    front
}

/// The rows for what an agent found, in its order, keeping only chats that
/// are still listed.
pub fn found_rows(sessions: &[Session], listed: &[usize], matches: &[Match]) -> Vec<usize> {
    matches
        .iter()
        .filter_map(|found| {
            listed
                .iter()
                .copied()
                .find(|&index| sessions[index].id == found.id)
        })
        .collect()
}

/// See [`SessionList::infer_chat`]. `created` reports when a session log was
/// created, in seconds since the Unix epoch.
pub fn infer_chat(
    sessions: &[Session],
    agent: Agent,
    cwd: &Path,
    agent_started: u64,
    claimed: &HashSet<String>,
    created: &dyn Fn(&Path) -> Option<u64>,
) -> Option<SavedChat> {
    // Clocks and file timestamps are only this precise.
    const SLACK: u64 = 5;
    let cwd = cwd.to_string_lossy();
    let cwd = folder_key(&cwd, cfg!(windows));
    sessions
        .iter()
        .filter(|session| {
            session.agent == agent
                && folder_key(&session.cwd, cfg!(windows)) == cwd
                && session.modified + SLACK >= agent_started
                && !claimed.contains(&session.id)
        })
        .filter(|session| created(&session.path).is_some_and(|at| at + SLACK >= agent_started))
        .max_by_key(|session| session.modified)
        .map(|session| SavedChat {
            agent,
            id: session.id.clone(),
            title: session.title.clone(),
        })
}

impl SessionList {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(SEARCH_PLACEHOLDER, cx));
        cx.subscribe(&search, |this, _, event: &TextInputEvent, cx| match event {
            // A description is for the agent: it filters nothing as it is
            // typed, and what was found stays until the next search.
            TextInputEvent::Changed if this.ask => cx.notify(),
            TextInputEvent::Changed => {
                this.highlighted = 0;
                this.refilter(cx);
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
            TextInputEvent::Submit => {
                if this.ask && !this.shows_answer_to(this.search.read(cx).text()) {
                    this.start_agent_search(cx);
                } else if let Some(session) = this.session_at(this.highlighted) {
                    cx.emit(SessionListEvent::Open(session.clone()));
                }
            }
            TextInputEvent::Cancel => {
                let empty = this.search.read(cx).text().is_empty();
                if matches!(this.agent_search, AgentSearch::Running { .. }) {
                    this.set_agent_search(AgentSearch::Idle, cx);
                } else if !matches!(this.agent_search, AgentSearch::Idle) {
                    this.set_agent_search(AgentSearch::Idle, cx);
                    this.search.update(cx, |search, cx| search.clear(cx));
                } else if empty {
                    cx.emit(SessionListEvent::Dismissed);
                } else {
                    this.search.update(cx, |search, cx| search.clear(cx));
                }
            }
            TextInputEvent::MoveDown => this.move_highlight(1, cx),
            TextInputEvent::MoveUp => this.move_highlight(-1, cx),
        })
        .detach();

        // Pins and bookmarks can change from any window.
        cx.observe_global::<Store>(|this, cx| {
            if *cx.global::<Store>().chat_marks() != this.marks {
                this.refilter(cx);
                cx.notify();
            }
        })
        .detach();

        let rescan = cx.spawn(async move |this, cx| loop {
            if this
                .update(cx, |this: &mut Self, cx| this.rescan(cx))
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(RESCAN_INTERVAL).await;
        });

        Self {
            sessions: Arc::new(Vec::new()),
            filtered: Vec::new(),
            search,
            agent_filter: None,
            root,
            scope_to_root: false,
            bookmarks_only: false,
            marks: cx.global::<Store>().chat_marks().clone(),
            ask: false,
            agent_search: AgentSearch::Idle,
            scope_before_ask: false,
            highlighted: 0,
            open_ids: HashSet::new(),
            loaded: false,
            scanning: false,
            scroll: UniformListScrollHandle::new(),
            _rescan: rescan,
        }
    }

    /// The listed chat with this id, as something that can be resumed.
    pub fn chat(&self, id: &str) -> Option<SavedChat> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| SavedChat {
                agent: session.agent,
                id: session.id.clone(),
                title: session.title.clone(),
            })
    }

    /// A fallback for agents that keep no record of their running chat:
    /// guesses it as the most recently active chat of that agent, in that
    /// folder, whose log was created after the agent started. Chats in
    /// `claimed` belong to other terminals or other programs.
    pub fn infer_chat(
        &self,
        agent: Agent,
        cwd: &Path,
        agent_started: u64,
        claimed: &HashSet<String>,
    ) -> Option<SavedChat> {
        infer_chat(
            &self.sessions,
            agent,
            cwd,
            agent_started,
            claimed,
            &|path| {
                let created = std::fs::metadata(path).ok()?.created().ok()?;
                Some(
                    created
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs(),
                )
            },
        )
    }

    /// Marks which chats are currently open in a terminal.
    pub fn set_open_ids(&mut self, ids: HashSet<String>, cx: &mut Context<Self>) {
        if ids != self.open_ids {
            self.open_ids = ids;
            cx.notify();
        }
    }

    /// `(shown, total)` counts for the section header.
    pub fn counts(&self) -> (usize, usize) {
        (self.filtered.len(), self.sessions.len())
    }

    /// Rescans session logs on a background thread.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        cx.spawn(async move |this, cx| {
            let sessions = cx.background_spawn(async move { sessions::scan() }).await;
            let _ = this.update(cx, |this, cx| {
                this.scanning = false;
                this.loaded = true;
                if *this.sessions != sessions {
                    this.sessions = Arc::new(sessions);
                    this.refilter(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refilter(&mut self, cx: &App) {
        self.marks = cx.global::<Store>().chat_marks().clone();
        let words = if self.ask {
            ""
        } else {
            self.search.read(cx).text()
        };
        let matching = self.listed(words);
        self.filtered = match &self.agent_search {
            AgentSearch::Found { matches, .. } => found_rows(&self.sessions, &matching, matches),
            _ => pinned_first(&self.sessions, matching, &self.marks.pinned),
        };
        self.highlighted = self.highlighted.min(self.filtered.len().saturating_sub(1));
    }

    /// The chats the chips allow whose titles hold every word of `words`,
    /// newest first.
    fn listed(&self, words: &str) -> Vec<usize> {
        let root = self.root.to_string_lossy();
        filter_sessions(
            &self.sessions,
            words,
            self.agent_filter,
            self.scope_to_root.then_some(root.as_ref()),
            self.bookmarks_only.then_some(&self.marks.bookmarked),
        )
    }

    /// Whether the list is showing what the agent found for `description`.
    fn shows_answer_to(&self, description: &str) -> bool {
        matches!(&self.agent_search, AgentSearch::Found { query, .. } if query == description.trim())
    }

    fn set_agent_search(&mut self, search: AgentSearch, cx: &mut Context<Self>) {
        self.agent_search = search;
        self.highlighted = 0;
        self.refilter(cx);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Switches the search box between filtering by words and describing a
    /// chat for an agent to find.
    fn toggle_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask = !self.ask;
        if self.ask {
            // The chats of this folder are the ones worth an agent's time;
            // "This folder" can still be turned off to search them all.
            self.scope_before_ask = self.scope_to_root;
            self.scope_to_root = true;
        } else {
            self.scope_to_root = self.scope_before_ask;
        }
        let placeholder = if self.ask {
            ASK_PLACEHOLDER
        } else {
            SEARCH_PLACEHOLDER
        };
        self.search
            .update(cx, |search, cx| search.set_placeholder(placeholder, cx));
        window.focus(&self.search.focus_handle(cx));
        self.set_agent_search(AgentSearch::Idle, cx);
    }

    /// Hands the description in the search box, and the chats now listed,
    /// to an agent.
    fn start_agent_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text().trim().to_string();
        if query.is_empty() {
            return;
        }
        let listed = self.listed("");
        let left_out = listed.len().saturating_sub(chat_search::MAX_CHATS);
        let chats: Vec<Session> = listed
            .iter()
            .take(chat_search::MAX_CHATS)
            .map(|&index| self.sessions[index].clone())
            .collect();
        if chats.is_empty() {
            let nothing = AgentSearch::Found {
                query,
                matches: Vec::new(),
            };
            return self.set_agent_search(nothing, cx);
        }

        let handle = Arc::new(chat_search::Handle::default());
        let reading = chats.len();
        cx.spawn({
            let handle = handle.clone();
            async move |this, cx| {
                let outcome = cx
                    .background_spawn({
                        let (handle, query) = (handle.clone(), query.clone());
                        async move { chat_search::run(&query, &chats, &handle) }
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    // A search that was stopped or replaced has nothing to say.
                    let current = matches!(
                        &this.agent_search,
                        AgentSearch::Running { search, .. } if Arc::ptr_eq(&search.0, &handle)
                    );
                    if current {
                        let search = match outcome {
                            Ok(matches) => AgentSearch::Found { query, matches },
                            Err(message) => AgentSearch::Failed(message),
                        };
                        this.set_agent_search(search, cx);
                    }
                });
            }
        })
        .detach();
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        });
        let running = AgentSearch::Running {
            reading,
            left_out,
            started: Instant::now(),
            search: StopOnDrop(handle),
            _tick: tick,
        };
        self.set_agent_search(running, cx);
    }

    fn session_at(&self, row: usize) -> Option<&Session> {
        self.filtered
            .get(row)
            .and_then(|&index| self.sessions.get(index))
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        let last = self.filtered.len() as isize - 1;
        self.highlighted = (self.highlighted as isize + delta).clamp(0, last) as usize;
        self.scroll
            .scroll_to_item(self.highlighted, ScrollStrategy::Center);
        cx.notify();
    }

    fn set_agent_filter(&mut self, filter: Option<Agent>, cx: &mut Context<Self>) {
        self.agent_filter = filter;
        self.highlighted = 0;
        self.refilter(cx);
        cx.notify();
    }

    fn toggle_scope(&mut self, cx: &mut Context<Self>) {
        self.scope_to_root = !self.scope_to_root;
        self.highlighted = 0;
        self.refilter(cx);
        cx.notify();
    }

    fn toggle_bookmarks_only(&mut self, cx: &mut Context<Self>) {
        self.bookmarks_only = !self.bookmarks_only;
        self.highlighted = 0;
        self.refilter(cx);
        cx.notify();
    }

    /// Why the agent picked this chat, when the list shows what it found.
    fn reason_for(&self, id: &str) -> Option<&str> {
        match &self.agent_search {
            AgentSearch::Found { matches, .. } => matches
                .iter()
                .find(|found| found.id == id)
                .map(|found| found.reason.as_str()),
            _ => None,
        }
    }

    fn row_height(&self) -> f32 {
        match self.agent_search {
            AgentSearch::Found { .. } => FOUND_ROW_HEIGHT,
            _ => ROW_HEIGHT,
        }
    }

    /// The button in the search box that turns asking an agent on and off.
    fn render_ask_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let on = self.ask;
        div()
            .id("ask-toggle")
            .flex_none()
            .h(px(20.))
            .px(px(5.))
            .flex()
            .items_center()
            .gap(px(3.))
            .rounded(px(4.))
            .text_size(px(11.))
            .cursor_pointer()
            .when(on, |toggle| {
                toggle.bg(theme::bg_active()).text_color(theme::text())
            })
            .when(!on, |toggle| {
                toggle
                    .text_color(theme::text_faint())
                    .hover(|style| style.bg(theme::bg_hover()).text_color(theme::text_muted()))
            })
            .child(icon(
                "sparkle",
                px(11.),
                if on {
                    theme::accent()
                } else {
                    theme::text_faint()
                },
            ))
            .child("Ask")
            .on_click(cx.listener(|this, _, window, cx| this.toggle_ask(window, cx)))
    }

    /// The line under the chips that says what the agent is up to.
    fn render_ask_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.ask {
            return None;
        }
        let (text, color, stoppable) = match &self.agent_search {
            AgentSearch::Idle => (
                format!(
                    "Say what the chat was about. {} reads the chats listed here and picks out the ones that match.",
                    chat_search::AGENT_NAME
                ),
                theme::text_faint(),
                false,
            ),
            AgentSearch::Running {
                reading,
                left_out,
                started,
                ..
            } => {
                let chats = match (*reading, *left_out) {
                    (1, 0) => "1 chat".to_string(),
                    (count, 0) => format!("{count} chats"),
                    (count, _) => format!("the {count} most recent chats"),
                };
                (
                    format!(
                        "{} is reading {chats}… {}s",
                        chat_search::AGENT_NAME,
                        started.elapsed().as_secs()
                    ),
                    theme::text_muted(),
                    true,
                )
            }
            AgentSearch::Found { .. } => match self.filtered.len() {
                // The empty list says so itself.
                0 => return None,
                1 => ("1 chat found".to_string(), theme::text_faint(), false),
                count => (
                    format!("{count} chats found, best match first"),
                    theme::text_faint(),
                    false,
                ),
            },
            AgentSearch::Failed(message) => (message.clone(), theme::red(), false),
        };
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .pb(px(6.))
                .flex()
                .items_start()
                .gap(px(6.))
                .text_size(px(11.))
                .child(div().flex_1().min_w_0().text_color(color).child(text))
                .when(stoppable, |line| {
                    line.child(
                        div()
                            .id("ask-stop")
                            .flex_none()
                            .px(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_color(theme::text_muted())
                            .bg(theme::bg_hover())
                            .hover(|style| style.bg(theme::bg_active()).text_color(theme::text()))
                            .child("Stop")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_agent_search(AgentSearch::Idle, cx)
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// A small button on a chat's row that sets or clears one of its marks.
    fn render_mark(
        id: (&'static str, usize),
        glyph: &'static str,
        on: bool,
        chat: &str,
        set: fn(&mut Store, &str, bool),
    ) -> impl IntoElement + use<> {
        let chat = chat.to_string();
        div()
            .id(id)
            .size(px(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .hover(|style| style.bg(theme::bg_active()))
            .child(icon(
                glyph,
                px(12.),
                if on {
                    theme::accent()
                } else {
                    theme::text_muted()
                },
            ))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.update_global::<Store, _>(|store, _| set(store, &chat, !on));
            })
    }

    fn render_chip(
        &self,
        id: &'static str,
        label: impl IntoElement,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px(px(7.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded(px(5.))
            .text_size(px(11.))
            .cursor_pointer()
            .when(active, |chip| {
                chip.bg(theme::bg_active()).text_color(theme::text())
            })
            .when(!active, |chip| {
                chip.text_color(theme::text_faint())
                    .hover(|style| style.bg(theme::bg_hover()).text_color(theme::text_muted()))
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
    }

    fn render_row(
        &self,
        row: usize,
        now: u64,
        search_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let session = &self.sessions[self.filtered[row]];
        let highlighted = search_focused && row == self.highlighted;
        let open = self.open_ids.contains(&session.id);
        let pinned = self.marks.pinned.contains(&session.id);
        let bookmarked = self.marks.bookmarked.contains(&session.id);
        let agent_color = match session.agent {
            Agent::Claude => theme::claude(),
            Agent::Codex => theme::codex(),
        };
        let title: SharedString = session.title.clone().into();
        let age = sessions::format_age(session.modified, now);
        // A chat the agent found says why, on up to three lines.
        let reason = self.reason_for(&session.id);
        let detail: SharedString = match reason {
            Some(reason) => format!("{age} · {reason}"),
            None => format!(
                "{} · {} · {age}",
                session.agent.label(),
                session.project_name()
            ),
        }
        .into();
        let found = reason.is_some();
        let opened = session.clone();

        div()
            .id(row)
            .group("chat-row")
            .relative()
            .h(px(self.row_height()))
            .w_full()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .when(highlighted, |row| row.bg(theme::bg_hover()))
            .hover(|style| style.bg(theme::bg_hover()))
            .child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .bg(agent_color)
                    .when(!open, |dot| dot.opacity(0.55)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_color(if open {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::text_faint())
                            .map(|line| {
                                if found {
                                    line.line_clamp(3).text_ellipsis()
                                } else {
                                    line.truncate()
                                }
                            })
                            .child(detail),
                    ),
            )
            // The marks a chat has show all the time.
            .when(pinned, |row| {
                row.child(icon("pin-filled", px(11.), theme::text_faint()))
            })
            .when(bookmarked, |row| {
                row.child(icon("bookmark-filled", px(11.), theme::accent()))
            })
            // Pointing at a row brings up the buttons that change them.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(6.))
                    .pl(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .bg(theme::bg_hover())
                    .invisible()
                    .group_hover("chat-row", |style| style.visible())
                    .child(Self::render_mark(
                        ("chat-pin", row),
                        if pinned { "pin-filled" } else { "pin" },
                        pinned,
                        &session.id,
                        Store::set_chat_pinned,
                    ))
                    .child(Self::render_mark(
                        ("chat-bookmark", row),
                        if bookmarked {
                            "bookmark-filled"
                        } else {
                            "bookmark"
                        },
                        bookmarked,
                        &session.id,
                        Store::set_chat_bookmarked,
                    )),
            )
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(SessionListEvent::Open(opened.clone()))),
            )
    }
}

impl Render for SessionList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = sessions::now_secs();
        let list = if self.filtered.is_empty() {
            let message = if !self.loaded {
                "Scanning chats…"
            } else if self.sessions.is_empty() {
                "No Claude Code or Codex chats found"
            } else if self.bookmarks_only && self.marks.bookmarked.is_empty() {
                "No bookmarked chats yet"
            } else if matches!(self.agent_search, AgentSearch::Found { .. }) {
                "No chats match that description"
            } else {
                "No matching chats"
            };
            div()
                .p_3()
                .text_color(theme::text_faint())
                .child(message)
                .into_any_element()
        } else {
            uniform_list(
                "session-list",
                self.filtered.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    let search_focused = this.search.focus_handle(cx).is_focused(window);
                    range
                        .map(|row| this.render_row(row, now, search_focused, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .size_full()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .mx(px(8.))
                    .mt(px(2.))
                    .h(px(28.))
                    .pl(px(8.))
                    .pr(px(3.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border())
                    .child(icon("search", px(13.), theme::text_faint()))
                    .child(self.search.clone())
                    .child(self.render_ask_toggle(cx)),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(self.render_chip(
                        "chip-all",
                        "All",
                        self.agent_filter.is_none(),
                        cx,
                        |this, cx| this.set_agent_filter(None, cx),
                    ))
                    .child(self.render_chip(
                        "chip-claude",
                        "Claude",
                        self.agent_filter == Some(Agent::Claude),
                        cx,
                        |this, cx| this.set_agent_filter(Some(Agent::Claude), cx),
                    ))
                    .child(self.render_chip(
                        "chip-codex",
                        "Codex",
                        self.agent_filter == Some(Agent::Codex),
                        cx,
                        |this, cx| this.set_agent_filter(Some(Agent::Codex), cx),
                    ))
                    .child(div().flex_1())
                    .child(self.render_chip(
                        "chip-bookmarks",
                        icon(
                            if self.bookmarks_only {
                                "bookmark-filled"
                            } else {
                                "bookmark"
                            },
                            px(12.),
                            if self.bookmarks_only {
                                theme::accent()
                            } else {
                                theme::text_faint()
                            },
                        ),
                        self.bookmarks_only,
                        cx,
                        |this, cx| this.toggle_bookmarks_only(cx),
                    ))
                    .child(self.render_chip(
                        "chip-scope",
                        "This folder",
                        self.scope_to_root,
                        cx,
                        |this, cx| this.toggle_scope(cx),
                    )),
            )
            .children(self.render_ask_status(cx))
            .child(div().flex_1().min_h_0().child(list))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(agent: Agent, title: &str, cwd: &str) -> Session {
        Session {
            agent,
            id: title.into(),
            title: title.into(),
            cwd: cwd.into(),
            modified: 0,
            path: PathBuf::new(),
            haystack: format!(
                "{} {} {}",
                title.to_lowercase(),
                cwd.to_lowercase(),
                agent.label().to_lowercase()
            ),
        }
    }

    fn dated(agent: Agent, title: &str, cwd: &str, modified: u64) -> Session {
        let mut session = session(agent, title, cwd);
        session.modified = modified;
        session.path = format!("/logs/{title}").into();
        session
    }

    #[test]
    fn guesses_the_chat_an_agent_started_itself() {
        let sessions = vec![
            dated(Agent::Claude, "older", "/work/app", 900),
            dated(Agent::Claude, "current", "/work/app/", 900),
            dated(Agent::Claude, "elsewhere", "/work/site", 950),
            dated(Agent::Codex, "codex", "/work/app", 960),
        ];
        // `older` is still being written to, but existed long before the
        // agent started at t=800: it belongs to something else.
        let created = |path: &Path| match path.to_str()? {
            "/logs/older" => Some(100),
            "/logs/current" => Some(810),
            _ => Some(900),
        };
        let infer = |started: u64, claimed: &[&str]| {
            let claimed: HashSet<String> = claimed.iter().map(|id| id.to_string()).collect();
            infer_chat(
                &sessions,
                Agent::Claude,
                Path::new("/work/app"),
                started,
                &claimed,
                &created,
            )
            .map(|chat| chat.title)
        };
        assert_eq!(infer(800, &[]).as_deref(), Some("current"));
        // Open in another terminal or another program: not this one's chat.
        assert_eq!(infer(800, &["current"]), None);
        // Nothing was created after an agent that started later.
        assert_eq!(infer(2000, &[]), None);
    }

    #[test]
    fn filters_by_terms_agent_and_folder() {
        let sessions = vec![
            session(Agent::Claude, "Fix login bug", "/work/app"),
            session(Agent::Codex, "Add dark mode", "/work/app/web"),
            session(Agent::Claude, "Write docs", "/work/application"),
        ];
        assert_eq!(filter_sessions(&sessions, "", None, None, None), vec![0, 1, 2]);
        assert_eq!(filter_sessions(&sessions, "LOGIN fix", None, None, None), vec![0]);
        assert_eq!(filter_sessions(&sessions, "codex", None, None, None), vec![1]);
        assert_eq!(
            filter_sessions(&sessions, "", Some(Agent::Claude), None, None),
            vec![0, 2]
        );
        // `/work/app` must not match the sibling `/work/application`.
        assert_eq!(
            filter_sessions(&sessions, "", None, Some("/work/app/"), None),
            vec![0, 1]
        );
        assert!(filter_sessions(&sessions, "nothing", None, None, None).is_empty());
    }

    #[test]
    fn bookmarks_narrow_the_list_and_pins_lead_it() {
        let sessions = vec![
            session(Agent::Claude, "newest", "/work/app"),
            session(Agent::Codex, "middle", "/work/app"),
            session(Agent::Claude, "older", "/work/app"),
            session(Agent::Claude, "oldest", "/work/site"),
        ];
        let ids = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|name| name.to_string()).collect()
        };

        let bookmarked = ids(&["middle", "oldest", "gone"]);
        assert_eq!(
            filter_sessions(&sessions, "", None, None, Some(&bookmarked)),
            vec![1, 3]
        );
        // The other filters still apply to bookmarked chats.
        assert_eq!(
            filter_sessions(&sessions, "", Some(Agent::Claude), None, Some(&bookmarked)),
            vec![3]
        );
        assert!(filter_sessions(&sessions, "", None, None, Some(&ids(&[]))).is_empty());

        // Pinned chats come first; each group keeps its newest-first order.
        let pinned = ids(&["older", "middle"]);
        assert_eq!(
            pinned_first(&sessions, vec![0, 1, 2, 3], &pinned),
            vec![1, 2, 0, 3]
        );
        // A pinned chat the search left out stays out.
        assert_eq!(pinned_first(&sessions, vec![0, 3], &pinned), vec![0, 3]);
        assert_eq!(pinned_first(&sessions, vec![0, 1], &ids(&[])), vec![0, 1]);
    }

    #[test]
    fn found_chats_keep_the_agents_order() {
        let sessions = vec![
            session(Agent::Claude, "newest", "/work/app"),
            session(Agent::Codex, "middle", "/work/app"),
            session(Agent::Claude, "oldest", "/work/site"),
        ];
        let found = |ids: &[&str]| -> Vec<Match> {
            ids.iter()
                .map(|id| Match {
                    id: id.to_string(),
                    reason: String::new(),
                })
                .collect()
        };
        assert_eq!(
            found_rows(&sessions, &[0, 1, 2], &found(&["oldest", "newest"])),
            vec![2, 0]
        );
        // Chats the chips have since hidden, or that are gone, drop out.
        assert_eq!(
            found_rows(&sessions, &[0, 1], &found(&["oldest", "gone", "middle"])),
            vec![1]
        );
        assert!(found_rows(&sessions, &[0, 1, 2], &[]).is_empty());
    }
}
