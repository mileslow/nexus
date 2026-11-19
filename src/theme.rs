//! Color themes, fonts and metrics shared by every view.
//!
//! The active theme is process-wide: views read colors through the accessor
//! functions below on every render, so switching themes only needs a redraw.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{px, rgb, rgba, Hsla, Pixels, Rgba};

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

pub const UI_TEXT_SIZE: Pixels = px(12.5);
pub const MONO_TEXT_SIZE: Pixels = px(12.5);
pub const TITLEBAR_HEIGHT: Pixels = px(36.);
pub const TAB_HEIGHT: Pixels = px(32.);
pub const ROW_HEIGHT: Pixels = px(24.);

/// Syntax-highlighting palettes bundled with the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    OneHalfDark,
    TwoDark,
    CatppuccinMocha,
    GruvboxDark,
    Nord,
    Github,
    CatppuccinLatte,
}

pub struct Theme {
    pub name: &'static str,
    bg_app: u32,
    bg_sidebar: u32,
    bg_hover: u32,
    bg_active: u32,
    bg_input: u32,
    border: u32,
    border_strong: u32,
    text: u32,
    text_muted: u32,
    text_faint: u32,
    accent: u32,
    green: u32,
    yellow: u32,
    red: u32,
    claude: u32,
    codex: u32,
    selection: u32,
    line_highlight: u32,
    /// `0xRRGGBBAA` tints for the file view's inline diff.
    diff_added: u32,
    diff_removed: u32,
    /// The 16 base ANSI colors used by terminals.
    ansi: [u32; 16],
    pub syntax: Syntax,
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "Nexus Dark",
        bg_app: 0x0e1014,
        bg_sidebar: 0x13161b,
        bg_hover: 0x1c2129,
        bg_active: 0x242b36,
        bg_input: 0x0b0d10,
        border: 0x232831,
        border_strong: 0x323946,
        text: 0xd5dae3,
        text_muted: 0x8992a1,
        text_faint: 0x5a6372,
        accent: 0x6ea8fe,
        green: 0x7bd88f,
        yellow: 0xe5c07b,
        red: 0xf2777a,
        claude: 0xe0906a,
        codex: 0x5ad1c2,
        selection: 0x2c4470,
        line_highlight: 0x1b2230,
        diff_added: 0x7bd88f24,
        diff_removed: 0xf2777a26,
        ansi: [
            0x1b1f27, 0xf2777a, 0x7bd88f, 0xe5c07b, 0x6ea8fe, 0xc79bf2, 0x5ad1c2, 0xc5cad3,
            0x5a6372, 0xff8f92, 0x98e6a8, 0xf2d399, 0x8fbcff, 0xd8b5ff, 0x7ce3d6, 0xf2f4f8,
        ],
        syntax: Syntax::OneHalfDark,
    },
    Theme {
        name: "Tokyo Night",
        bg_app: 0x1a1b26,
        bg_sidebar: 0x16161e,
        bg_hover: 0x202330,
        bg_active: 0x2a2f45,
        bg_input: 0x13131a,
        border: 0x262a3d,
        border_strong: 0x3b4261,
        text: 0xc0caf5,
        text_muted: 0x9099c0,
        text_faint: 0x565f89,
        accent: 0x7aa2f7,
        green: 0x9ece6a,
        yellow: 0xe0af68,
        red: 0xf7768e,
        claude: 0xff9e64,
        codex: 0x73daca,
        selection: 0x33467c,
        line_highlight: 0x24283b,
        diff_added: 0x9ece6a24,
        diff_removed: 0xf7768e26,
        ansi: [
            0x32344a, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x565f89, 0xff7a93, 0xb9f27c, 0xff9e64, 0x7da6ff, 0xc7a9ff, 0x0db9d7, 0xc0caf5,
        ],
        syntax: Syntax::TwoDark,
    },
    Theme {
        name: "Catppuccin Mocha",
        bg_app: 0x1e1e2e,
        bg_sidebar: 0x181825,
        bg_hover: 0x262637,
        bg_active: 0x313244,
        bg_input: 0x11111b,
        border: 0x2b2b3c,
        border_strong: 0x45475a,
        text: 0xcdd6f4,
        text_muted: 0xa6adc8,
        text_faint: 0x6c7086,
        accent: 0x89b4fa,
        green: 0xa6e3a1,
        yellow: 0xf9e2af,
        red: 0xf38ba8,
        claude: 0xfab387,
        codex: 0x94e2d5,
        selection: 0x414668,
        line_highlight: 0x2a2b3c,
        diff_added: 0xa6e3a124,
        diff_removed: 0xf38ba826,
        ansi: [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xcdd6f4,
        ],
        syntax: Syntax::CatppuccinMocha,
    },
    Theme {
        name: "Gruvbox Dark",
        bg_app: 0x282828,
        bg_sidebar: 0x1d2021,
        bg_hover: 0x32302f,
        bg_active: 0x3c3836,
        bg_input: 0x1b1d1e,
        border: 0x3c3836,
        border_strong: 0x504945,
        text: 0xebdbb2,
        text_muted: 0xa89984,
        text_faint: 0x7c6f64,
        accent: 0x83a598,
        green: 0xb8bb26,
        yellow: 0xfabd2f,
        red: 0xfb4934,
        claude: 0xfe8019,
        codex: 0x8ec07c,
        selection: 0x504945,
        line_highlight: 0x32302f,
        diff_added: 0xb8bb2626,
        diff_removed: 0xfb493426,
        ansi: [
            0x3c3836, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
        syntax: Syntax::GruvboxDark,
    },
    Theme {
        name: "Nord",
        bg_app: 0x2e3440,
        bg_sidebar: 0x292e39,
        bg_hover: 0x353c4a,
        bg_active: 0x434c5e,
        bg_input: 0x262b35,
        border: 0x3b4252,
        border_strong: 0x4c566a,
        text: 0xd8dee9,
        text_muted: 0xa3adbf,
        text_faint: 0x6d7890,
        accent: 0x88c0d0,
        green: 0xa3be8c,
        yellow: 0xebcb8b,
        red: 0xbf616a,
        claude: 0xd08770,
        codex: 0x8fbcbb,
        selection: 0x4c566a,
        line_highlight: 0x38404f,
        diff_added: 0xa3be8c2b,
        diff_removed: 0xbf616a33,
        ansi: [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
        syntax: Syntax::Nord,
    },
    Theme {
        name: "GitHub Light",
        bg_app: 0xffffff,
        bg_sidebar: 0xf6f8fa,
        bg_hover: 0xeaeef2,
        bg_active: 0xdce3ea,
        bg_input: 0xffffff,
        border: 0xd8dee4,
        border_strong: 0xafb8c1,
        text: 0x1f2328,
        text_muted: 0x59636e,
        text_faint: 0x8c959f,
        accent: 0x0969da,
        green: 0x1a7f37,
        yellow: 0x9a6700,
        red: 0xcf222e,
        claude: 0xbc4c00,
        codex: 0x0e8a7b,
        selection: 0xb6d6fd,
        line_highlight: 0xeaf2ff,
        diff_added: 0x2da44e2e,
        diff_removed: 0xcf222e26,
        ansi: [
            0x24292f, 0xcf222e, 0x116329, 0x9a6700, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
            0x57606a, 0xa40e26, 0x1a7f37, 0x7d4e00, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
        syntax: Syntax::Github,
    },
    Theme {
        name: "Catppuccin Latte",
        bg_app: 0xeff1f5,
        bg_sidebar: 0xe6e9ef,
        bg_hover: 0xdce0e8,
        bg_active: 0xccd0da,
        bg_input: 0xf7f8fb,
        border: 0xd3d7e0,
        border_strong: 0xacb0be,
        text: 0x4c4f69,
        text_muted: 0x6c6f85,
        text_faint: 0x9ca0b0,
        accent: 0x1e66f5,
        green: 0x40a02b,
        yellow: 0xdf8e1d,
        red: 0xd20f39,
        claude: 0xfe640b,
        codex: 0x179299,
        selection: 0xbcc8ee,
        line_highlight: 0xe2e7f3,
        diff_added: 0x40a02b2e,
        diff_removed: 0xd20f3926,
        ansi: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
        syntax: Syntax::CatppuccinLatte,
    },
];

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn active() -> &'static Theme {
    &THEMES[active_index()]
}

pub fn active_index() -> usize {
    ACTIVE.load(Ordering::Relaxed).min(THEMES.len() - 1)
}

/// Switches the theme. Callers redraw their windows afterwards.
pub fn set_active(index: usize) {
    ACTIVE.store(index.min(THEMES.len() - 1), Ordering::Relaxed);
}

/// Switches to the theme with this name, if there is one.
pub fn set_active_by_name(name: &str) -> bool {
    match THEMES.iter().position(|theme| theme.name == name) {
        Some(index) => {
            set_active(index);
            true
        }
        None => false,
    }
}

macro_rules! colors {
    ($($name:ident;)*) => {
        $(pub fn $name() -> Rgba { rgb(active().$name) })*
    };
}

colors! {
    bg_app;
    bg_sidebar;
    bg_hover;
    bg_active;
    bg_input;
    border;
    border_strong;
    text;
    text_muted;
    text_faint;
    accent;
    green;
    yellow;
    red;
    claude;
    codex;
    selection;
    line_highlight;
}

pub fn bg_titlebar() -> Rgba {
    bg_sidebar()
}

pub fn bg_tabbar() -> Rgba {
    bg_sidebar()
}

/// Row tints for the file view's inline diff.
pub fn diff_added_bg() -> Rgba {
    rgba(active().diff_added)
}

pub fn diff_removed_bg() -> Rgba {
    rgba(active().diff_removed)
}

pub fn hsla(color: Rgba) -> Hsla {
    color.into()
}

/// One of the 16 base ANSI colors, as `0xRRGGBB`.
pub fn ansi(index: usize) -> u32 {
    active().ansi[index % 16]
}

pub fn term_fg() -> u32 {
    active().text
}

pub fn term_bg() -> u32 {
    active().bg_app
}

pub fn term_cursor() -> u32 {
    active().text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_are_unique() {
        for (index, theme) in THEMES.iter().enumerate() {
            assert_eq!(
                THEMES.iter().position(|other| other.name == theme.name),
                Some(index),
                "duplicate theme name {}",
                theme.name
            );
        }
    }

    /// Text must stay readable on every surface it is drawn on.
    #[test]
    fn text_contrasts_with_backgrounds() {
        fn luminance(hex: u32) -> f32 {
            let channel = |shift: u32| {
                let value = ((hex >> shift) & 0xff) as f32 / 255.;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        }
        fn contrast(a: u32, b: u32) -> f32 {
            let (la, lb) = (luminance(a), luminance(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        for theme in THEMES {
            for background in [theme.bg_app, theme.bg_sidebar, theme.bg_active] {
                assert!(
                    contrast(theme.text, background) >= 4.5,
                    "{}: text on {background:06x}",
                    theme.name
                );
                assert!(
                    contrast(theme.text_muted, background) >= 3.,
                    "{}: muted text on {background:06x}",
                    theme.name
                );
            }
            assert!(
                contrast(theme.text, theme.selection) >= 4.5,
                "{}: text on selection",
                theme.name
            );
        }
    }
}
