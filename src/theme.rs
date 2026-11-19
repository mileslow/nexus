//! Colors, fonts and metrics shared by every view.

use gpui::{px, rgb, rgba, Hsla, Pixels, Rgba};

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

pub const UI_TEXT_SIZE: Pixels = px(12.5);
pub const MONO_TEXT_SIZE: Pixels = px(12.5);
pub const TITLEBAR_HEIGHT: Pixels = px(36.);
pub const TAB_HEIGHT: Pixels = px(32.);
pub const ROW_HEIGHT: Pixels = px(24.);

macro_rules! colors {
    ($($name:ident = $hex:expr;)*) => {
        $(pub fn $name() -> Rgba { rgb($hex) })*
    };
}

colors! {
    bg_app = 0x0e1014;
    bg_sidebar = 0x13161b;
    bg_titlebar = 0x13161b;
    bg_tabbar = 0x13161b;
    bg_hover = 0x1c2129;
    bg_active = 0x242b36;
    bg_input = 0x0b0d10;
    border = 0x232831;
    border_strong = 0x323946;
    text = 0xd5dae3;
    text_muted = 0x8992a1;
    text_faint = 0x5a6372;
    accent = 0x6ea8fe;
    green = 0x7bd88f;
    yellow = 0xe5c07b;
    red = 0xf2777a;
    claude = 0xe0906a;
    codex = 0x5ad1c2;
    selection = 0x2c4470;
    line_highlight = 0x1b2230;
}

/// Row tints for the file view's inline diff.
pub fn diff_added_bg() -> Rgba {
    rgba(0x7bd88f24)
}

pub fn diff_removed_bg() -> Rgba {
    rgba(0xf2777a26)
}

pub fn hsla(color: Rgba) -> Hsla {
    color.into()
}

/// The 16 base ANSI colors used by the terminal.
pub const ANSI: [u32; 16] = [
    0x1b1f27, 0xf2777a, 0x7bd88f, 0xe5c07b, 0x6ea8fe, 0xc79bf2, 0x5ad1c2, 0xc5cad3, 0x5a6372,
    0xff8f92, 0x98e6a8, 0xf2d399, 0x8fbcff, 0xd8b5ff, 0x7ce3d6, 0xf2f4f8,
];

pub const TERM_FG: u32 = 0xd5dae3;
pub const TERM_BG: u32 = 0x0e1014;
pub const TERM_CURSOR: u32 = 0xd5dae3;
