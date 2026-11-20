//! The searchable list of Claude Code and Codex chats.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, uniform_list, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window,
};

use crate::app_state::SavedChat;
use crate::assets::icon;
use crate::platform::{folder_key, is_within};
use crate::sessions::{self, Agent, Session};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

const RESCAN_INTERVAL: Duration = Duration::from_secs(20);
const ROW_HEIGHT: f32 = 44.;

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
pub fn filter_sessions(
    sessions: &[Session],
    query: &str,
    agent: Option<Agent>,
    under: Option<&str>,
) -> Vec<usize> {
    let query = query.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    let under = under.map(|root| folder_key(root, cfg!(windows)));
    sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| {
            agent.is_none_or(|agent| session.agent == agent)
                && under
                    .as_deref()
                    .is_none_or(|root| is_within(&folder_key(&session.cwd, cfg!(windows)), root))
                && terms.iter().all(|term| session.haystack.contains(term))
        })
        .map(|(index, _)| index)
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
        let search = cx.new(|cx| TextInput::new("Search chats", cx));
        cx.subscribe(&search, |this, _, event: &TextInputEvent, cx| match event {
            TextInputEvent::Changed => {
                this.highlighted = 0;
                this.refilter(cx);
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
            TextInputEvent::Submit => {
                if let Some(session) = this.session_at(this.highlighted) {
                    cx.emit(SessionListEvent::Open(session.clone()));
                }
            }
            TextInputEvent::Cancel => {
                if this.search.read(cx).text().is_empty() {
                    cx.emit(SessionListEvent::Dismissed);
                } else {
                    this.search.update(cx, |search, cx| search.clear(cx));
                }
            }
            TextInputEvent::MoveDown => this.move_highlight(1, cx),
            TextInputEvent::MoveUp => this.move_highlight(-1, cx),
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
        let root = self.root.to_string_lossy();
        self.filtered = filter_sessions(
            &self.sessions,
            self.search.read(cx).text(),
            self.agent_filter,
            self.scope_to_root.then_some(root.as_ref()),
        );
        self.highlighted = self.highlighted.min(self.filtered.len().saturating_sub(1));
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

    fn render_chip(
        &self,
        id: &'static str,
        label: &'static str,
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
        let agent_color = match session.agent {
            Agent::Claude => theme::claude(),
            Agent::Codex => theme::codex(),
        };
        let title: SharedString = session.title.clone().into();
        let detail: SharedString = format!(
            "{} · {} · {}",
            session.agent.label(),
            session.project_name(),
            sessions::format_age(session.modified, now)
        )
        .into();
        let opened = session.clone();

        div()
            .id(row)
            .h(px(ROW_HEIGHT))
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
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme::text_faint())
                            .child(detail),
                    ),
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
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border())
                    .child(icon("search", px(13.), theme::text_faint()))
                    .child(self.search.clone()),
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
                        "chip-scope",
                        "This folder",
                        self.scope_to_root,
                        cx,
                        |this, cx| this.toggle_scope(cx),
                    )),
            )
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
        assert_eq!(filter_sessions(&sessions, "", None, None), vec![0, 1, 2]);
        assert_eq!(filter_sessions(&sessions, "LOGIN fix", None, None), vec![0]);
        assert_eq!(filter_sessions(&sessions, "codex", None, None), vec![1]);
        assert_eq!(
            filter_sessions(&sessions, "", Some(Agent::Claude), None),
            vec![0, 2]
        );
        // `/work/app` must not match the sibling `/work/application`.
        assert_eq!(
            filter_sessions(&sessions, "", None, Some("/work/app/")),
            vec![0, 1]
        );
        assert!(filter_sessions(&sessions, "nothing", None, None).is_empty());
    }
}
