//! Icons are small inline SVGs served through GPUI's asset pipeline.

use std::borrow::Cow;

use gpui::{svg, AssetSource, Hsla, Pixels, SharedString, Styled, Svg};

macro_rules! icon_svg {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

const ICONS: &[(&str, &str)] = &[
    (
        "chevron-right",
        icon_svg!(r#"<path d="M6 3.5 10.5 8 6 12.5"/>"#),
    ),
    (
        "chevron-up",
        icon_svg!(r#"<path d="M3.5 10 8 5.5 12.5 10"/>"#),
    ),
    (
        "chevron-down",
        icon_svg!(r#"<path d="M3.5 6 8 10.5 12.5 6"/>"#),
    ),
    (
        "folder",
        icon_svg!(
            r#"<path d="M1.8 4.2c0-.7.5-1.2 1.2-1.2h3l1.4 1.6H13c.7 0 1.2.5 1.2 1.2v6c0 .7-.5 1.2-1.2 1.2H3c-.7 0-1.2-.5-1.2-1.2z"/>"#
        ),
    ),
    (
        "file",
        icon_svg!(
            r#"<path d="M4 1.8h5l3.2 3.2v8c0 .7-.5 1.2-1.2 1.2H4c-.7 0-1.2-.5-1.2-1.2V3c0-.7.5-1.2 1.2-1.2z"/><path d="M9 1.8V5h3.2"/>"#
        ),
    ),
    (
        "terminal",
        icon_svg!(
            r#"<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.6"/><path d="M4.4 6.2 6.6 8l-2.2 1.8M8.4 10h3"/>"#
        ),
    ),
    ("plus", icon_svg!(r#"<path d="M8 3v10M3 8h10"/>"#)),
    ("close", icon_svg!(r#"<path d="M4 4l8 8M12 4l-8 8"/>"#)),
    (
        "split-right",
        icon_svg!(
            r#"<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.6"/><path d="M8 2.6v10.8"/>"#
        ),
    ),
    (
        "split-down",
        icon_svg!(
            r#"<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.6"/><path d="M1.6 8h12.8"/>"#
        ),
    ),
    (
        "search",
        icon_svg!(r#"<circle cx="7" cy="7" r="4.2"/><path d="m10.2 10.2 3.2 3.2"/>"#),
    ),
    (
        "chat",
        icon_svg!(
            r#"<path d="M2 4c0-.7.5-1.2 1.2-1.2h9.6c.7 0 1.2.5 1.2 1.2v6c0 .7-.5 1.2-1.2 1.2H7l-3 2.4v-2.4h-.8C2.5 11.2 2 10.700 2 10z"/>"#
        ),
    ),
    (
        "sidebar",
        icon_svg!(
            r#"<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.6"/><path d="M6 2.6v10.8"/>"#
        ),
    ),
    (
        "refresh",
        icon_svg!(r#"<path d="M13 8a5 5 0 1 1-1.5-3.600M13 2.800v2.800h-2.800"/>"#),
    ),
    (
        "folder-open",
        icon_svg!(
            r#"<path d="M1.800 12V4.200c0-.7.5-1.2 1.2-1.2h3l1.400 1.600H12c.7 0 1.200.5 1.200 1.200V7"/><path d="M1.800 12l1.800-4.400c.2-.4.500-.6.900-.6h9.100c.6 0 1 .6.8 1.200L13 12c-.2.600-.6 1-1.200 1H3c-.7 0-1.200-.4-1.200-1z"/>"#
        ),
    ),
    (
        "sparkle",
        icon_svg!(
            r#"<path d="M8 1.800 9.500 6.500 14.200 8 9.500 9.500 8 14.200 6.500 9.500 1.800 8 6.500 6.500z"/>"#
        ),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"));
        Ok(name.and_then(|name| {
            ICONS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, body)| Cow::Borrowed(body.as_bytes()))
        }))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(n, _)| format!("icons/{n}.svg").into())
            .collect())
    }
}

/// A square icon tinted with `color`.
pub fn icon(name: &'static str, size: Pixels, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(size)
        .flex_none()
        .text_color(color.into())
}
