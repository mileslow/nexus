//! Window chrome that Windows leaves to the app: the minimize, maximize
//! and close buttons, and a menu standing in for the Mac menu bar.

use gpui::{
    anchored, deferred, div, prelude::*, px, Action, Context, Corner, SharedString, Window,
    WindowControlArea,
};

use crate::assets::icon;
use crate::theme;
use crate::update::{self, Updater};
use crate::workspace::{NewWindow, OpenFolder, SearchChats, SearchFiles, ToggleSidebar};

/// True where the app draws its own title bar buttons and menu.
pub const CUSTOM: bool = cfg!(windows);

/// The minimize, maximize and close buttons at the right of the title bar.
/// Windows does the pressing; these only show where the buttons are.
pub fn caption_buttons(window: &Window) -> impl IntoElement {
    let button = |id: &'static str, glyph: &'static str, area: WindowControlArea| {
        let close = area == WindowControlArea::Close;
        div()
            .id(id)
            .group(id)
            .w(px(46.))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            // Keeps the title bar under it from claiming the click as a drag.
            .occlude()
            .window_control_area(area)
            .hover(|style| {
                if close {
                    style.bg(gpui::rgb(0xc42b1c))
                } else {
                    style.bg(theme::bg_active())
                }
            })
            .child(
                icon(glyph, px(12.), theme::text_muted()).when(close, |glyph| {
                    glyph.group_hover(id, |style| style.text_color(gpui::white()))
                }),
            )
    };
    let maximize = if window.is_maximized() {
        "window-restore"
    } else {
        "window-maximize"
    };
    div()
        .h_full()
        .flex_none()
        .flex()
        .child(button(
            "caption-minimize",
            "window-minimize",
            WindowControlArea::Min,
        ))
        .child(button("caption-maximize", maximize, WindowControlArea::Max))
        .child(button("caption-close", "close", WindowControlArea::Close))
}

/// A title-bar button that opens the commands a Mac keeps in its menu bar.
pub struct AppMenu {
    open: bool,
}

enum Entry {
    Run(&'static str, &'static str, Box<dyn Action>),
    CheckForUpdates,
    About,
    Separator,
}

impl AppMenu {
    pub fn new() -> Self {
        Self { open: false }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::Run("New Window", "Ctrl+Shift+N", Box::new(NewWindow)),
            Entry::Run("Open Folder…", "Ctrl+Shift+O", Box::new(OpenFolder)),
            Entry::Separator,
            Entry::Run("Search Files", "Alt+Shift+F", Box::new(SearchFiles)),
            Entry::Run("Search Chats", "Ctrl+Shift+P", Box::new(SearchChats)),
            Entry::Run("Find…", "Ctrl+Shift+F", Box::new(crate::find::Find)),
            Entry::Run("Toggle Sidebar", "Ctrl+Shift+B", Box::new(ToggleSidebar)),
            Entry::Separator,
            Entry::CheckForUpdates,
            Entry::About,
        ]
    }
}

impl Render for AppMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.open.then(|| {
            let rows = Self::entries()
                .into_iter()
                .enumerate()
                .map(|(index, entry)| {
                    let (label, hint): (SharedString, &'static str) = match &entry {
                        Entry::Run(label, hint, _) => ((*label).into(), hint),
                        Entry::CheckForUpdates => ("Check for Updates…".into(), ""),
                        Entry::About => (format!("About Nexus {}", update::VERSION).into(), ""),
                        Entry::Separator => {
                            return div()
                                .h(px(1.))
                                .my(px(4.))
                                .mx(px(4.))
                                .bg(theme::border())
                                .into_any_element()
                        }
                    };
                    div()
                        .id(("app-menu-row", index))
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::bg_active()))
                        .child(div().flex_1().child(label))
                        .child(div().text_color(theme::text_faint()).child(hint))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close(cx);
                            match &entry {
                                Entry::Run(_, _, action) => {
                                    window.dispatch_action(action.boxed_clone(), cx)
                                }
                                Entry::CheckForUpdates => Updater::global(cx)
                                    .update(cx, |updater, cx| updater.check(true, cx)),
                                Entry::About => cx.open_url(update::RELEASES_URL),
                                Entry::Separator => {}
                            }
                        }))
                        .into_any_element()
                });
            // Anchored below the button's left edge, above everything else.
            div().absolute().top(px(28.)).left_0().child(
                deferred(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .id("app-menu")
                                .occlude()
                                .w(px(250.))
                                .p(px(4.))
                                .flex()
                                .flex_col()
                                .bg(theme::bg_sidebar())
                                .border_1()
                                .border_color(theme::border_strong())
                                .rounded(px(8.))
                                .shadow_lg()
                                .text_size(theme::UI_TEXT_SIZE)
                                .text_color(theme::text())
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
                                .children(rows),
                        ),
                )
                .with_priority(1),
            )
        });

        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id("app-menu-button")
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .occlude()
                    .when(self.open, |button| button.bg(theme::bg_active()))
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(icon("menu", px(14.), theme::text_muted()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.open = !this.open;
                        cx.notify();
                    })),
            )
            .children(menu)
    }
}
