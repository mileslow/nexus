//! A pane: a strip of tabs over one visible terminal or file.

use std::collections::HashMap;
use std::path::Path;

use gpui::{
    div, prelude::*, px, AnyElement, App, Axis, Context, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, MouseButton, SharedString, Subscription, Window,
};

use crate::assets::icon;
use crate::file_view::FileView;
use crate::links::LinkTarget;
use crate::sessions::Agent;
use crate::terminal::{Status, TerminalEvent, TerminalView};
use crate::theme;

#[derive(Clone)]
pub enum PaneItem {
    Terminal(Entity<TerminalView>),
    File(Entity<FileView>),
}

impl PaneItem {
    pub fn entity_id(&self) -> EntityId {
        match self {
            PaneItem::Terminal(terminal) => terminal.entity_id(),
            PaneItem::File(file) => file.entity_id(),
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneItem::Terminal(terminal) => terminal.focus_handle(cx),
            PaneItem::File(file) => file.focus_handle(cx),
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            PaneItem::Terminal(terminal) => terminal.read(cx).title().into(),
            PaneItem::File(file) => file.read(cx).title(),
        }
    }

    pub fn as_terminal(&self) -> Option<&Entity<TerminalView>> {
        match self {
            PaneItem::Terminal(terminal) => Some(terminal),
            PaneItem::File(_) => None,
        }
    }
}

pub enum PaneEvent {
    /// Keyboard focus moved into this pane.
    Focused,
    /// The last tab closed.
    Emptied,
    /// Tabs or their titles changed.
    Changed,
    /// A link was Cmd-clicked in one of this pane's terminals.
    Open(LinkTarget),
    Split(Axis),
    NewTerminal,
}

pub struct Pane {
    items: Vec<PaneItem>,
    active: usize,
    /// Whether this is the pane that sidebar actions and shortcuts target.
    is_active: bool,
    focus_handle: FocusHandle,
    subscriptions: HashMap<EntityId, Subscription>,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Icon and tint for a terminal, by the agent it runs.
pub fn terminal_glyph(agent: Option<Agent>) -> (&'static str, gpui::Rgba) {
    match agent {
        Some(Agent::Claude) => ("sparkle", theme::claude()),
        Some(Agent::Codex) => ("sparkle", theme::codex()),
        None => ("terminal", theme::text_faint()),
    }
}

pub fn status_color(status: Status) -> Option<gpui::Rgba> {
    match status {
        Status::Working => Some(theme::green()),
        Status::Attention => Some(theme::yellow()),
        Status::Done => Some(theme::accent()),
        Status::Idle => None,
    }
}

/// A small square button holding one icon.
pub fn icon_button(
    id: impl Into<gpui::ElementId>,
    name: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|style| style.bg(theme::bg_active()))
        .child(icon(name, px(14.), theme::text_muted()))
}

impl Pane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus_in(&focus_handle, window, |_, _, cx| {
            cx.emit(PaneEvent::Focused);
            cx.notify();
        })
        .detach();
        Self {
            items: Vec::new(),
            active: 0,
            is_active: false,
            focus_handle,
            subscriptions: HashMap::new(),
        }
    }

    pub fn set_active(&mut self, is_active: bool, cx: &mut Context<Self>) {
        if self.is_active != is_active {
            self.is_active = is_active;
            cx.notify();
        }
    }

    pub fn items(&self) -> &[PaneItem] {
        &self.items
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_item(&self) -> Option<&PaneItem> {
        self.items.get(self.active)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_files(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, PaneItem::File(_)))
    }

    pub fn index_of_file(&self, path: &Path, cx: &App) -> Option<usize> {
        self.items.iter().position(|item| match item {
            PaneItem::File(file) => file.read(cx).path() == path,
            PaneItem::Terminal(_) => false,
        })
    }

    pub fn index_of_session(&self, session_id: &str, cx: &App) -> Option<usize> {
        self.items.iter().position(|item| match item {
            PaneItem::Terminal(terminal) => {
                terminal.read(cx).session_id.as_deref() == Some(session_id)
            }
            PaneItem::File(_) => false,
        })
    }

    pub fn index_of(&self, id: EntityId) -> Option<usize> {
        self.items.iter().position(|item| item.entity_id() == id)
    }

    /// Adds a tab after the active one and switches to it.
    pub fn add_item(&mut self, item: PaneItem, window: &mut Window, cx: &mut Context<Self>) {
        if let PaneItem::Terminal(terminal) = &item {
            let subscription = cx.subscribe_in(
                terminal,
                window,
                |this, terminal, event: &TerminalEvent, window, cx| match event {
                    TerminalEvent::Closed => {
                        if let Some(index) = this.index_of(terminal.entity_id()) {
                            this.close_item(index, window, cx);
                        }
                    }
                    TerminalEvent::TitleChanged => {
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                    }
                    TerminalEvent::Open(target) => cx.emit(PaneEvent::Open(target.clone())),
                },
            );
            self.subscriptions
                .insert(terminal.entity_id(), subscription);
        }
        let index = if self.items.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.items.insert(index, item);
        self.activate(index, window, cx);
    }

    /// Shows a tab and gives it keyboard focus.
    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.items.len() {
            return;
        }
        self.active = index;
        window.focus(&self.items[index].focus_handle(cx));
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    pub fn focus_active(&self, window: &mut Window, cx: &App) {
        match self.active_item() {
            Some(item) => window.focus(&item.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    pub fn close_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.items.len() {
            return;
        }
        let had_focus = self.focus_handle.contains_focused(window, cx);
        let item = self.items.remove(index);
        self.subscriptions.remove(&item.entity_id());
        drop(item);

        if self.items.is_empty() {
            self.active = 0;
            cx.emit(PaneEvent::Emptied);
        } else {
            if index < self.active || self.active >= self.items.len() {
                self.active = self.active.saturating_sub(1);
            }
            if had_focus {
                window.focus(&self.items[self.active].focus_handle(cx));
            }
        }
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_item(self.active, window, cx);
    }

    /// Cycles tabs; `delta` is +1 or -1.
    pub fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.items.len() as isize;
        if count > 1 {
            let next = (self.active as isize + delta).rem_euclid(count) as usize;
            self.activate(next, window, cx);
        }
    }

    fn render_tab(
        &self,
        index: usize,
        pane_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let item = &self.items[index];
        let active = index == self.active;
        let title = item.title(cx);
        let (glyph, status) = match item {
            PaneItem::Terminal(terminal) => {
                let terminal = terminal.read(cx);
                let (name, color) = terminal_glyph(terminal.agent);
                (icon(name, px(13.), color), status_color(terminal.status()))
            }
            PaneItem::File(_) => (icon("file", px(13.), theme::text_faint()), None),
        };

        div()
            .id(("tab", index))
            .group("tab")
            .h_full()
            // Tabs shrink to share the strip before it has to scroll.
            .flex_shrink()
            .min_w(px(96.))
            .max_w(px(230.))
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(10.))
            .pr(px(6.))
            .relative()
            .border_r_1()
            .border_color(theme::border())
            .cursor_pointer()
            .map(|tab| {
                if active {
                    tab.bg(theme::bg_app()).text_color(theme::text())
                } else {
                    tab.text_color(theme::text_muted())
                        .hover(|style| style.bg(theme::bg_hover()))
                }
            })
            // The focused pane's active tab carries an accent line.
            .when(active && pane_focused, |tab| {
                tab.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(theme::accent()),
                )
            })
            .child(glyph)
            .child(div().min_w_0().truncate().child(title))
            .when_some(status, |tab, color| {
                tab.child(div().flex_none().size(px(6.)).rounded_full().bg(color))
            })
            .child(
                div()
                    .id(("tab-close", index))
                    .size(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .when(!active, |close| {
                        close
                            .invisible()
                            .group_hover("tab", |style| style.visible())
                    })
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(icon("close", px(11.), theme::text_muted()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_item(index, window, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.activate(index, window, cx)))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| this.close_item(index, window, cx)),
            )
    }

    fn render_tab_bar(
        &self,
        pane_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tabs: Vec<AnyElement> = (0..self.items.len())
            .map(|index| self.render_tab(index, pane_focused, cx).into_any_element())
            .collect();
        div()
            .h(theme::TAB_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .bg(theme::bg_tabbar())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("tabs")
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(1.))
                    .px(px(4.))
                    .child(
                        icon_button("new-terminal", "plus")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaneEvent::NewTerminal))),
                    )
                    .child(icon_button("split-right", "split-right").on_click(
                        cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Split(Axis::Horizontal))),
                    ))
                    .child(icon_button("split-down", "split-down").on_click(
                        cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Split(Axis::Vertical))),
                    )),
            )
    }
}

impl Render for Pane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane_focused = self.is_active;
        let body = match self.active_item() {
            Some(PaneItem::Terminal(terminal)) => terminal.clone().into_any_element(),
            Some(PaneItem::File(file)) => file.clone().into_any_element(),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .text_color(theme::text_faint())
                .child("No open tabs")
                .child(
                    div()
                        .text_size(px(11.5))
                        .child("⌘T new terminal · click a file or chat in the sidebar"),
                )
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_app())
            .track_focus(&self.focus_handle)
            .child(self.render_tab_bar(pane_focused, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    // Dim every pane but the active one, like a multiplexer.
                    .when(!pane_focused, |body| body.opacity(0.72))
                    .child(body),
            )
    }
}
