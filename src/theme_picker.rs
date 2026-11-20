//! A title-bar button that opens the list of color themes.

use gpui::{anchored, deferred, div, prelude::*, px, Context, Corner, SharedString, Window};

use crate::app_state::Store;
use crate::assets::icon;
use crate::theme;

pub struct ThemePicker {
    open: bool,
}

impl ThemePicker {
    pub fn new() -> Self {
        Self { open: false }
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open = false;
        theme::set_active(index);
        cx.update_global::<Store, _>(|store, _| store.set_theme(theme::active().name));
        // Every window draws with the new colors.
        cx.refresh_windows();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }
}

impl Render for ThemePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = theme::active_index();
        let menu = self.open.then(|| {
            let rows = theme::THEMES.iter().enumerate().map(|(index, entry)| {
                div()
                    .id(("theme", index))
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(div().w(px(13.)).flex_none().when(index == active, |mark| {
                        mark.child(icon("check", px(13.), theme::accent()))
                    }))
                    .child(SharedString::from(entry.name))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.choose(index, cx)
                    }))
            });
            // Anchored below the button's right edge, above everything else.
            div().absolute().top(px(28.)).right_0().child(
                deferred(
                    anchored()
                        .anchor(Corner::TopRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .id("theme-menu")
                                .occlude()
                                .w(px(190.))
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
                    .id("theme-button")
                    .occlude()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(self.open, |button| button.bg(theme::bg_active()))
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(icon("palette", px(14.), theme::text_muted()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.open = !this.open;
                        cx.notify();
                    })),
            )
            .children(menu)
    }
}
