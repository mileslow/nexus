//! A pane: a strip of tabs over one visible terminal or file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, Pixels, Point, Rgba, SharedString,
    Subscription, Window,
};

use crate::assets::icon;
use crate::file_view::FileView;
use crate::layout::Direction;
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
    Split(Direction),
    NewTerminal,
    /// A terminal in this pane was right-clicked.
    ContextMenu {
        position: Point<Pixels>,
        item: EntityId,
        /// The file or folder named by the text under the pointer.
        path: Option<PathBuf>,
    },
    /// A tab in this pane's header was right-clicked.
    TabMenu {
        position: Point<Pixels>,
        item: EntityId,
    },
    /// A tab dragged from another pane (or onto an edge) was dropped here.
    TabDropped {
        item: EntityId,
        from: EntityId,
        placement: TabPlacement,
    },
}

/// Where a dropped tab should go in the pane it was dropped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPlacement {
    /// Before the tab currently at this index.
    Before(usize),
    /// After the last tab.
    End,
    /// In a new pane split off on this side.
    Split(Direction),
}

/// The payload carried while a tab is being dragged.
#[derive(Clone)]
pub struct DraggedTab {
    pane: EntityId,
    item: EntityId,
    title: SharedString,
    glyph: &'static str,
    color: Rgba,
}

/// The chip that follows the pointer during a tab drag.
struct TabPreview(DraggedTab);

impl Render for TabPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.))
            .max_w(px(230.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .bg(theme::bg_active())
            .border_1()
            .border_color(theme::border_strong())
            .text_size(theme::UI_TEXT_SIZE)
            .font_family(theme::UI_FONT)
            .text_color(theme::text())
            .shadow_md()
            .opacity(0.92)
            .child(icon(self.0.glyph, px(13.), self.0.color))
            .child(div().min_w_0().truncate().child(self.0.title.clone()))
    }
}

/// The part of a pane's body a dragged tab is hovering over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    /// Join this pane's tabs.
    Center,
    /// Split a new pane off on this side.
    Edge(Direction),
}

/// Edges claim the outer quarter of the pane; the middle joins its tabs.
pub fn drop_zone(bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<DropZone> {
    if !bounds.contains(&position) || bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
        return None;
    }
    let x = (position.x - bounds.left()) / bounds.size.width;
    let y = (position.y - bounds.top()) / bounds.size.height;
    let edges = [
        (x, Direction::Left),
        (1. - x, Direction::Right),
        (y, Direction::Up),
        (1. - y, Direction::Down),
    ];
    let (distance, direction) = edges
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("four edges");
    Some(if distance < 0.25 {
        DropZone::Edge(direction)
    } else {
        DropZone::Center
    })
}

pub struct Pane {
    items: Vec<PaneItem>,
    active: usize,
    /// Whether this is the pane that sidebar actions and shortcuts target.
    is_active: bool,
    focus_handle: FocusHandle,
    subscriptions: HashMap<EntityId, Subscription>,
    /// Where a tab being dragged over this pane's body would land.
    drop_zone: Option<DropZone>,
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
            drop_zone: None,
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
        let index = if self.items.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.insert_item(index, item, window, cx);
    }

    /// Adds a tab at `index` (clamped to the end) and switches to it.
    pub fn insert_item(
        &mut self,
        index: usize,
        item: PaneItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    TerminalEvent::ContextMenu { position, path } => {
                        cx.emit(PaneEvent::ContextMenu {
                            position: *position,
                            item: terminal.entity_id(),
                            path: path.clone(),
                        })
                    }
                },
            );
            self.subscriptions
                .insert(terminal.entity_id(), subscription);
        }
        let index = index.min(self.items.len());
        self.items.insert(index, item);
        self.activate(index, window, cx);
    }

    /// Removes a tab without closing what it shows, so it can be placed in
    /// another pane.
    pub fn take_item(
        &mut self,
        id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneItem> {
        let index = self.index_of(id)?;
        let had_focus = self.focus_handle.contains_focused(window, cx);
        let item = self.items.remove(index);
        self.subscriptions.remove(&id);
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
        Some(item)
    }

    /// Moves one of this pane's own tabs so it sits before the tab that is
    /// currently at `before` (or at the end).
    fn reorder(
        &mut self,
        id: EntityId,
        before: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.index_of(id) else {
            return;
        };
        let to = reordered_index(from, before, self.items.len());
        if to != from {
            let item = self.items.remove(from);
            self.items.insert(to, item);
        }
        self.activate(to, window, cx);
    }

    /// A dragged tab was dropped on the tab strip.
    fn drop_on_tabs(
        &mut self,
        tab: &DraggedTab,
        before: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_zone = None;
        if tab.pane == cx.entity_id() {
            self.reorder(tab.item, before.unwrap_or(self.items.len()), window, cx);
        } else {
            cx.emit(PaneEvent::TabDropped {
                item: tab.item,
                from: tab.pane,
                placement: before.map_or(TabPlacement::End, TabPlacement::Before),
            });
        }
    }

    /// A dragged tab was dropped on the pane's contents.
    fn drop_on_body(&mut self, tab: &DraggedTab, window: &mut Window, cx: &mut Context<Self>) {
        let zone = self.drop_zone.take().unwrap_or(DropZone::Center);
        cx.notify();
        let own = tab.pane == cx.entity_id();
        match zone {
            // Dropping a tab on its own pane changes nothing.
            DropZone::Center if own => {
                if let Some(index) = self.index_of(tab.item) {
                    self.activate(index, window, cx);
                }
            }
            DropZone::Center => cx.emit(PaneEvent::TabDropped {
                item: tab.item,
                from: tab.pane,
                placement: TabPlacement::End,
            }),
            DropZone::Edge(direction) => cx.emit(PaneEvent::TabDropped {
                item: tab.item,
                from: tab.pane,
                placement: TabPlacement::Split(direction),
            }),
        }
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
        let (glyph_name, glyph_color, status) = match item {
            PaneItem::Terminal(terminal) => {
                let terminal = terminal.read(cx);
                let (name, color) = terminal_glyph(terminal.agent);
                (name, color, status_color(terminal.status()))
            }
            PaneItem::File(_) => ("file", theme::text_faint(), None),
        };
        let glyph = icon(glyph_name, px(13.), glyph_color);
        let item_id = item.entity_id();
        let dragged = DraggedTab {
            pane: cx.entity_id(),
            item: item.entity_id(),
            title: title.clone(),
            glyph: glyph_name,
            color: glyph_color,
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                    // Keep the strip behind from opening the active tab's menu.
                    cx.stop_propagation();
                    cx.emit(PaneEvent::TabMenu {
                        position: event.position,
                        item: item_id,
                    });
                }),
            )
            // Tabs can be dragged to reorder them, onto another pane to move
            // them there, or onto a pane's edge to split it.
            .on_drag(dragged, |tab, _, _, cx| cx.new(|_| TabPreview(tab.clone())))
            .drag_over::<DraggedTab>(|style, _, _, _| style.bg(theme::bg_active()))
            .on_drop(cx.listener(move |this, tab: &DraggedTab, window, cx| {
                this.drop_on_tabs(tab, Some(index), window, cx)
            }))
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
                    .children(tabs)
                    // Right-clicking the empty part of the strip is about the
                    // tab that is showing.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            if let Some(item) = this.active_item() {
                                cx.emit(PaneEvent::TabMenu {
                                    position: event.position,
                                    item: item.entity_id(),
                                });
                            }
                        }),
                    )
                    // The empty part of the strip takes drops at the end.
                    .drag_over::<DraggedTab>(|style, _, _, _| style.bg(theme::bg_hover()))
                    .on_drop(cx.listener(|this, tab: &DraggedTab, window, cx| {
                        this.drop_on_tabs(tab, None, window, cx)
                    })),
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
                        cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Split(Direction::Right))),
                    ))
                    .child(icon_button("split-down", "split-down").on_click(
                        cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Split(Direction::Down))),
                    )),
            )
    }
}

impl Render for Pane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.drop_zone = None;
        }
        // Shade the part of the pane a dragged tab would land in.
        let drop_overlay = self.drop_zone.map(|zone| {
            let overlay = div()
                .absolute()
                .bg(Rgba {
                    a: 0.16,
                    ..theme::accent()
                })
                .border_1()
                .border_color(theme::accent());
            match zone {
                DropZone::Center => overlay.inset_0(),
                DropZone::Edge(Direction::Left) => overlay.top_0().bottom_0().left_0().w_1_2(),
                DropZone::Edge(Direction::Right) => overlay.top_0().bottom_0().right_0().w_1_2(),
                DropZone::Edge(Direction::Up) => overlay.left_0().right_0().top_0().h_1_2(),
                DropZone::Edge(Direction::Down) => overlay.left_0().right_0().bottom_0().h_1_2(),
            }
        });
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
                .child(div().text_size(px(11.5)).child(crate::platform::keys(
                    "⌘T new terminal · click a file or chat in the sidebar",
                    "Ctrl+Shift+T new terminal · click a file or chat in the sidebar",
                )))
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
                    .id("pane-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    // Dim every pane but the active one, like a multiplexer.
                    .when(!pane_focused, |body| body.opacity(0.72))
                    .child(body)
                    .children(drop_overlay)
                    .on_drag_move::<DraggedTab>(cx.listener(
                        |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                            let zone = drop_zone(event.bounds, event.event.position);
                            if this.drop_zone != zone {
                                this.drop_zone = zone;
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(|this, tab: &DraggedTab, window, cx| {
                        this.drop_on_body(tab, window, cx)
                    })),
            )
    }
}

/// Where a tab ends up when moved from `from` to sit before the tab that was
/// at `before`, in a strip of `len` tabs.
fn reordered_index(from: usize, before: usize, len: usize) -> usize {
    let before = before.min(len);
    // Removing the tab first shifts everything after it left by one.
    if before > from {
        before - 1
    } else {
        before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn dragging_a_tab_within_its_strip() {
        // [a b c d]: drop `a` on `c` puts it just before c.
        assert_eq!(reordered_index(0, 2, 4), 1);
        // Drop `d` on `b`.
        assert_eq!(reordered_index(3, 1, 4), 1);
        // Drop on itself, or on its right-hand neighbour, changes nothing.
        assert_eq!(reordered_index(1, 1, 4), 1);
        assert_eq!(reordered_index(1, 2, 4), 1);
        // Drop on the empty end of the strip.
        assert_eq!(reordered_index(0, 4, 4), 3);
        assert_eq!(reordered_index(3, 9, 4), 3);
    }

    #[test]
    fn pane_body_drop_zones() {
        let bounds = Bounds::new(point(px(100.), px(50.)), size(px(400.), px(200.)));
        let zone = |x: f32, y: f32| drop_zone(bounds, point(px(x), px(y)));
        assert_eq!(zone(300., 150.), Some(DropZone::Center));
        assert_eq!(zone(120., 150.), Some(DropZone::Edge(Direction::Left)));
        assert_eq!(zone(480., 150.), Some(DropZone::Edge(Direction::Right)));
        assert_eq!(zone(300., 60.), Some(DropZone::Edge(Direction::Up)));
        assert_eq!(zone(300., 240.), Some(DropZone::Edge(Direction::Down)));
        // In a corner the nearer edge wins.
        assert_eq!(zone(110., 100.), Some(DropZone::Edge(Direction::Left)));
        // Outside the pane there is no target.
        assert_eq!(zone(50., 150.), None);
        assert_eq!(zone(300., 300.), None);
    }
}
