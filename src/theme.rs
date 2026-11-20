//! Color themes, fonts and metrics shared by every view.
//!
//! The active theme is process-wide: views read colors through the accessor
//! functions below on every render, so switching themes only needs a redraw.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{px, rgb, rgba, Hsla, Pixels, Rgba};

pub const UI_FONT: &str = ".SystemUIFont";
/// A monospaced font every copy of the system has.
pub const MONO_FONT: &str = if cfg!(windows) { "Consolas" } else { "Menlo" };

pub const UI_TEXT_SIZE: Pixels = px(12.5);
pub const MONO_TEXT_SIZE: Pixels = px(12.5);
pub const TITLEBAR_HEIGHT: Pixels = px(36.);
pub const TAB_HEIGHT: Pixels = px(32.);
pub const ROW_HEIGHT: Pixels = px(24.);

/// How a theme colors source code: one of the bundled syntax themes, or its
/// own set of colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    OneHalfDark,
    TwoDark,
    CatppuccinMocha,
    GruvboxDark,
    Nord,
    Github,
    CatppuccinLatte,
    Dracula,
    SolarizedDark,
    SolarizedLight,
    Custom(&'static SyntaxColors),
}

/// Colors for the main kinds of token, as `0xRRGGBB`.
#[derive(Debug, PartialEq, Eq)]
pub struct SyntaxColors {
    pub comment: u32,
    pub string: u32,
    pub number: u32,
    /// `true`, `null`, and other built-in constants.
    pub constant: u32,
    pub keyword: u32,
    pub function: u32,
    /// Class and type names.
    pub type_name: u32,
    /// Markup tag names.
    pub tag: u32,
    /// Markup attribute names.
    pub attribute: u32,
    pub operator: u32,
}

// Taken from the "Owlet (Default)" VS Code theme.
const OWLET_SYNTAX: SyntaxColors = SyntaxColors {
    comment: 0x697478,
    string: 0xecc48d,
    number: 0xf78c6c,
    constant: 0xff5874,
    keyword: 0xc792ea,
    function: 0x82aaff,
    type_name: 0xffcb8b,
    tag: 0x82aaff,
    attribute: 0x78ccf0,
    operator: 0x78ccf0,
};

const NIGHT_OWL_SYNTAX: SyntaxColors = SyntaxColors {
    comment: 0x637777,
    string: 0xecc48d,
    number: 0xf78c6c,
    constant: 0xff5874,
    keyword: 0xc792ea,
    function: 0x82aaff,
    type_name: 0xffcb8b,
    tag: 0xcaece6,
    attribute: 0xc5e478,
    operator: 0x7fdbca,
};

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
    Theme {
        name: "Owlet",
        bg_app: 0x1e222a,
        bg_sidebar: 0x191c23,
        bg_hover: 0x262b35,
        bg_active: 0x2d333f,
        bg_input: 0x14171d,
        border: 0x2c313a,
        border_strong: 0x3f4653,
        text: 0xd6deeb,
        text_muted: 0x8a9499,
        text_faint: 0x555d61,
        accent: 0x82aaff,
        green: 0x22da6e,
        yellow: 0xecc48d,
        red: 0xef5350,
        claude: 0xf78c6c,
        codex: 0x21c7a8,
        selection: 0x3a4354,
        line_highlight: 0x252a33,
        diff_added: 0x22da6e22,
        diff_removed: 0xef535026,
        ansi: [
            0x2d333f, 0xef5350, 0x22da6e, 0xecc48d, 0x82aaff, 0xc792ea, 0x21c7a8, 0xffffff,
            0x575656, 0xef5350, 0x22da6e, 0xffeb95, 0x82aaff, 0xc792ea, 0x7fdbca, 0xffffff,
        ],
        syntax: Syntax::Custom(&OWLET_SYNTAX),
    },
    Theme {
        name: "Night Owl",
        bg_app: 0x011627,
        bg_sidebar: 0x010e1a,
        bg_hover: 0x0b2942,
        bg_active: 0x13344f,
        bg_input: 0x0b253a,
        border: 0x122d42,
        border_strong: 0x1d3b53,
        text: 0xd6deeb,
        text_muted: 0x89a4bb,
        text_faint: 0x4b6479,
        accent: 0x82aaff,
        green: 0xaddb67,
        yellow: 0xecc48d,
        red: 0xef5350,
        claude: 0xf78c6c,
        codex: 0x7fdbca,
        selection: 0x1d3b53,
        line_highlight: 0x0a2133,
        diff_added: 0xaddb6722,
        diff_removed: 0xef535026,
        ansi: [
            0x1d3b53, 0xef5350, 0x22da6e, 0xc5e478, 0x82aaff, 0xc792ea, 0x21c7a8, 0xffffff,
            0x575656, 0xef5350, 0x22da6e, 0xffeb95, 0x82aaff, 0xc792ea, 0x7fdbca, 0xffffff,
        ],
        syntax: Syntax::Custom(&NIGHT_OWL_SYNTAX),
    },
    Theme {
        name: "Dracula",
        bg_app: 0x282a36,
        bg_sidebar: 0x21222c,
        bg_hover: 0x313442,
        bg_active: 0x44475a,
        bg_input: 0x1e1f29,
        border: 0x343746,
        border_strong: 0x565a73,
        text: 0xf8f8f2,
        text_muted: 0xa3a8c3,
        text_faint: 0x6272a4,
        accent: 0xbd93f9,
        green: 0x50fa7b,
        yellow: 0xf1fa8c,
        red: 0xff5555,
        claude: 0xffb86c,
        codex: 0x8be9fd,
        selection: 0x44475a,
        line_highlight: 0x2f313d,
        diff_added: 0x50fa7b20,
        diff_removed: 0xff555526,
        ansi: [
            0x44475a, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
        syntax: Syntax::Dracula,
    },
    Theme {
        name: "One Dark",
        bg_app: 0x282c34,
        bg_sidebar: 0x21252b,
        bg_hover: 0x2c313a,
        bg_active: 0x3a3f4b,
        bg_input: 0x1d1f23,
        border: 0x343841,
        border_strong: 0x4b5263,
        text: 0xabb2bf,
        text_muted: 0x8b93a1,
        text_faint: 0x5c6370,
        accent: 0x61afef,
        green: 0x98c379,
        yellow: 0xe5c07b,
        red: 0xe06c75,
        claude: 0xd19a66,
        codex: 0x56b6c2,
        selection: 0x3e4451,
        line_highlight: 0x2c313c,
        diff_added: 0x98c37924,
        diff_removed: 0xe06c7526,
        ansi: [
            0x3f4451, 0xe06c75, 0x98c379, 0xd19a66, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
            0x5c6370, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
        ],
        syntax: Syntax::TwoDark,
    },
    Theme {
        name: "Solarized Dark",
        bg_app: 0x002b36,
        bg_sidebar: 0x00212b,
        bg_hover: 0x04313c,
        bg_active: 0x073642,
        bg_input: 0x001f27,
        border: 0x0a3d4a,
        border_strong: 0x1c5361,
        text: 0x93a1a1,
        text_muted: 0x7c8f93,
        text_faint: 0x586e75,
        accent: 0x268bd2,
        green: 0x859900,
        yellow: 0xb58900,
        red: 0xdc322f,
        claude: 0xcb4b16,
        codex: 0x2aa198,
        selection: 0x083a46,
        line_highlight: 0x04313c,
        diff_added: 0x85990030,
        diff_removed: 0xdc322f30,
        ansi: [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x586e75, 0xcb4b16, 0x859900, 0xb58900, 0x268bd2, 0x6c71c4, 0x2aa198, 0xfdf6e3,
        ],
        syntax: Syntax::SolarizedDark,
    },
    Theme {
        name: "Solarized Light",
        bg_app: 0xfdf6e3,
        bg_sidebar: 0xeee8d5,
        bg_hover: 0xe8e1cd,
        bg_active: 0xe4ddc8,
        bg_input: 0xfffbee,
        border: 0xddd6c1,
        border_strong: 0xb8b8a6,
        text: 0x4a6068,
        text_muted: 0x657b83,
        text_faint: 0x93a1a1,
        accent: 0x268bd2,
        green: 0x859900,
        yellow: 0xb58900,
        red: 0xdc322f,
        claude: 0xcb4b16,
        codex: 0x2aa198,
        selection: 0xd5e2e2,
        line_highlight: 0xf3ecd8,
        diff_added: 0x85990033,
        diff_removed: 0xdc322f26,
        ansi: [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0x93a1a1,
            0x586e75, 0xcb4b16, 0x859900, 0xb58900, 0x268bd2, 0x6c71c4, 0x2aa198, 0x657b83,
        ],
        syntax: Syntax::SolarizedLight,
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
