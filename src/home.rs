//! The home view: pick a folder or a remembered workspace to enter.

use std::path::{Path, PathBuf};

use gpui::{
    div, img, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, FontWeight,
    PathPromptOptions, SharedString, Window,
};

use crate::app_state::{Store, WorkspaceRecord};
use crate::assets::{icon, logo};
use crate::links::home_dir;
use crate::sessions::{format_age, now_secs};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;
use crate::theme_picker::ThemePicker;
use crate::update::UpdateBadge;
use crate::windows;
use crate::workspace::OpenFolder;

pub struct Home {
    search: Entity<TextInput>,
    theme_picker: Entity<ThemePicker>,
    update_badge: Entity<UpdateBadge>,
    highlighted: usize,
}

impl Focusable for Home {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// Abbreviates the home directory as `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.to_string_lossy())
            };
        }
    }
    path.to_string_lossy().into_owned()
}

/// Keeps the workspaces whose name or path contains every search term.
pub fn filter_workspaces(records: Vec<WorkspaceRecord>, query: &str) -> Vec<WorkspaceRecord> {
    let query = query.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    records
        .into_iter()
        .filter(|record| {
            let haystack = record.root.to_string_lossy().to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}

/// "2 terminals · 1 file", or nothing when no layout is remembered.
pub fn layout_summary(record: &WorkspaceRecord) -> Option<String> {
    let (terminals, files) = record.layout.as_ref()?.counts();
    let plural = |count: usize, noun: &str| match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    };
    let mut parts = Vec::new();
    if terminals > 0 {
        parts.push(plural(terminals, "terminal"));
    }
    if files > 0 {
        parts.push(plural(files, "file"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

impl Home {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search workspaces", cx));
        cx.subscribe_in(
            &search,
            window,
            |this, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed => {
                    this.highlighted = 0;
                    cx.notify();
                }
                TextInputEvent::Submit => {
                    if let Some(record) = this.visible(cx).get(this.highlighted) {
                        this.enter(record.root.clone(), window, cx);
                    }
                }
                TextInputEvent::Cancel => this.search.update(cx, |search, cx| search.clear(cx)),
                TextInputEvent::MoveDown => this.move_highlight(1, cx),
                TextInputEvent::MoveUp => this.move_highlight(-1, cx),
            },
        )
        .detach();
        // Another window opening or closing a workspace changes this list.
        cx.observe_global::<Store>(|_, cx| cx.notify()).detach();

        window.set_window_title("Nexus");
        window.focus(&search.focus_handle(cx));
        Self {
            search,
            theme_picker: cx.new(|_| ThemePicker::new()),
            update_badge: cx.new(UpdateBadge::new),
            highlighted: 0,
        }
    }

    fn visible(&self, cx: &App) -> Vec<WorkspaceRecord> {
        filter_workspaces(cx.global::<Store>().recent(), self.search.read(cx).text())
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible(cx).len() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + delta).clamp(0, count - 1) as usize;
            cx.notify();
        }
    }

    fn enter(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !root.is_dir() {
            return;
        }
        // Replacing the window's root drops this view, so do it after the
        // current update finishes.
        window.defer(cx, move |window, cx| {
            windows::enter_workspace(&root, window, cx)
        });
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, window, cx| this.enter(path, window, cx));
                }
            }
        })
        .detach();
    }

    fn render_row(
        &self,
        index: usize,
        record: &WorkspaceRecord,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let exists = record.root.is_dir();
        let is_open = cx.global::<Store>().window_for(&record.root).is_some();
        let name: SharedString = record.name().into();
        let path: SharedString = display_path(&record.root).into();
        let mut details = Vec::new();
        if !exists {
            details.push("folder missing".to_string());
        } else if is_open {
            details.push("open".to_string());
        }
        details.extend(layout_summary(record));
        if record.last_opened > 0 {
            details.push(match format_age(record.last_opened, now).as_str() {
                "now" => "just now".to_string(),
                age => format!("{age} ago"),
            });
        }
        let details: SharedString = details.join(" · ").into();
        let enter_root = record.root.clone();
        let forget_root = record.root.clone();

        div()
            .id(("workspace", index))
            .group("workspace-row")
            .h(px(52.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(index == self.highlighted, |row| row.bg(theme::bg_hover()))
            .hover(|style| style.bg(theme::bg_hover()))
            .child(icon(
                "folder",
                px(16.),
                if exists {
                    theme::accent()
                } else {
                    theme::text_faint()
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.5))
                            .text_color(if exists {
                                theme::text()
                            } else {
                                theme::text_faint()
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme::text_faint())
                            .child(path),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme::text_muted())
                    .child(details),
            )
            .child(
                div()
                    .id(("forget", index))
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .invisible()
                    .group_hover("workspace-row", |style| style.visible())
                    .hover(|style| style.bg(theme::bg_active()))
                    .child(icon("close", px(11.), theme::text_muted()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.update_global::<Store, _>(|store, _| store.forget(&forget_root));
                    })),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.enter(enter_root.clone(), window, cx)),
            )
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_secs();
        let records = self.visible(cx);
        self.highlighted = self.highlighted.min(records.len().saturating_sub(1));
        let has_any = !cx.global::<Store>().recent().is_empty();
        let rows: Vec<_> = records
            .iter()
            .enumerate()
            .map(|(index, record)| self.render_row(index, record, now, cx).into_any_element())
            .collect();
        let empty_message = if !has_any {
            Some("No workspaces yet. Open a folder to start one.")
        } else if rows.is_empty() {
            Some("No matching workspaces")
        } else {
            None
        };

        div()
            .id("home")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg_app())
            .text_color(theme::text())
            .text_size(theme::UI_TEXT_SIZE)
            .font_family(theme::UI_FONT)
            .key_context("Home")
            .on_action(cx.listener(Self::open_folder))
            .child(
                // Title bar strip: leaves room for the traffic lights.
                div()
                    .id("home-titlebar")
                    .h(theme::TITLEBAR_HEIGHT)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(10.))
                    .on_click(|event, window, _| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    })
                    .child(self.update_badge.clone())
                    .child(self.theme_picker.clone()),
            )
            .child(
                div().flex_1().min_h_0().flex().justify_center().child(
                    div()
                        .w(px(640.))
                        .max_w_full()
                        .h_full()
                        .px(px(20.))
                        .pt(px(
                            (f32::from(window.viewport_size().height) * 0.08).min(72.)
                        ))
                        .pb(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(14.))
                                .child(img(logo()).size(px(56.)).flex_none())
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(
                                            div()
                                                .text_size(px(22.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("Nexus"),
                                        )
                                        .child(
                                            div().text_color(theme::text_muted()).child(
                                                "Pick a workspace to enter, or open a folder.",
                                            ),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .h(px(34.))
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(8.))
                                .bg(theme::bg_input())
                                .border_1()
                                .border_color(theme::border())
                                .child(icon("search", px(14.), theme::text_faint()))
                                .child(self.search.clone()),
                        )
                        .child(
                            div()
                                .id("workspace-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .children(rows)
                                .when_some(empty_message, |list, message| {
                                    list.child(
                                        div()
                                            .p(px(12.))
                                            .text_color(theme::text_faint())
                                            .child(message),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .id("open-folder")
                                        .h(px(30.))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .rounded(px(7.))
                                        .border_1()
                                        .border_color(theme::border_strong())
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::bg_hover()))
                                        .child(icon("folder-open", px(14.), theme::text_muted()))
                                        .child("Open Folder…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_folder(&OpenFolder, window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(theme::text_faint())
                                        .child(
                                        "⌘O open folder · ⌘⇧N new window · ↑↓ and Enter to pick",
                                    ),
                                ),
                        ),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::{SavedLayout, SavedNode, SavedTab};

    fn record(root: &str, tabs: Vec<SavedTab>) -> WorkspaceRecord {
        WorkspaceRecord {
            root: root.into(),
            last_opened: 1,
            layout: (!tabs.is_empty()).then(|| SavedLayout {
                sidebar_width: 280.,
                sidebar_visible: true,
                collapsed: [false; 3],
                tree: SavedNode::Pane { tabs, active: 0 },
                active_pane: 0,
            }),
        }
    }

    fn terminal() -> SavedTab {
        SavedTab::Terminal {
            cwd: "/x".into(),
            chat: None,
        }
    }

    #[test]
    fn summarizes_a_remembered_layout() {
        assert_eq!(layout_summary(&record("/a", vec![])), None);
        assert_eq!(
            layout_summary(&record("/a", vec![terminal()])).as_deref(),
            Some("1 terminal")
        );
        let tabs = vec![
            terminal(),
            terminal(),
            SavedTab::File {
                path: "/x/a.rs".into(),
            },
        ];
        assert_eq!(
            layout_summary(&record("/a", tabs)).as_deref(),
            Some("2 terminals · 1 file")
        );
    }

    #[test]
    fn searches_names_and_paths() {
        let records = vec![
            record("/work/ansa", vec![]),
            record("/work/nexus", vec![]),
            record("/play/Ansa-site", vec![]),
        ];
        let names = |query: &str| -> Vec<String> {
            filter_workspaces(records.clone(), query)
                .iter()
                .map(|record| record.name())
                .collect()
        };
        assert_eq!(names(""), vec!["ansa", "nexus", "Ansa-site"]);
        assert_eq!(names("ANSA"), vec!["ansa", "Ansa-site"]);
        assert_eq!(names("work ans"), vec!["ansa"]);
        assert!(names("zzz").is_empty());
    }
}
