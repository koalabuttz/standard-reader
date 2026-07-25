//! The visual theme. One source of truth for colours and styles.
//!
//! [`Theme`] is the *resolved* palette the renderer reads (seven `ratatui` colours + style
//! helpers). [`ThemeColors`] is its serde-friendly mirror — seven `#rrggbb` hex strings — used
//! for the built-in presets and for the user's editable global/per-blog custom palettes persisted
//! in `prefs.toml`. The hex→colour parse is tolerant: a malformed value falls back to the matching
//! `modern_dark` slot, so a hand-edited file never panics or renders garbage.
//!
//! Invariant: every slot resolves to `Color::Rgb`. The reader's callout-tint blending
//! (`reader::blend`/`rgb_of`) assumes RGB and degrades non-RGB colours to black — so presets and
//! conversions must only ever emit `Color::Rgb`.

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub border: Color,
}

/// The seven theme slots as `#rrggbb` hex strings — the serde/persistence form of a [`Theme`],
/// and the unit a preset or the in-app editor works with.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct ThemeColors {
    pub bg: String,
    pub panel: String,
    pub fg: String,
    pub dim: String,
    pub accent: String,
    pub accent2: String,
    pub border: String,
}

/// The seven slots in display order, for the editor and any slot-wise iteration.
pub const SLOTS: [&str; 7] = [
    "background",
    "panel",
    "foreground",
    "dim",
    "accent",
    "accent 2",
    "border",
];

/// Built-in preset names, in picker order. The first is the default.
pub const PRESETS: [&str; 3] = ["modern-dark", "light", "high-contrast"];

impl ThemeColors {
    pub fn modern_dark() -> Self {
        Self {
            bg: "#1a1b26".into(),
            panel: "#24283b".into(),
            fg: "#c0caf5".into(),
            dim: "#565f89".into(),
            accent: "#7dcfff".into(),
            accent2: "#bb9af7".into(),
            border: "#292e42".into(),
        }
    }

    pub fn light() -> Self {
        Self {
            bg: "#fafafa".into(),
            panel: "#eceff4".into(),
            fg: "#2e3440".into(),
            dim: "#7b8394".into(),
            accent: "#1a73e8".into(),
            accent2: "#8250df".into(),
            border: "#d6dae0".into(),
        }
    }

    pub fn high_contrast() -> Self {
        Self {
            bg: "#000000".into(),
            panel: "#141414".into(),
            fg: "#ffffff".into(),
            dim: "#b0b0b0".into(),
            accent: "#ffd000".into(),
            accent2: "#00e5ff".into(),
            border: "#ffffff".into(),
        }
    }

    /// The preset palette for a name, or `None` if it isn't a built-in.
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "modern-dark" => Some(Self::modern_dark()),
            "light" => Some(Self::light()),
            "high-contrast" => Some(Self::high_contrast()),
            _ => None,
        }
    }

    /// The hex string for slot `i` (0..7), in [`SLOTS`] order.
    pub fn slot(&self, i: usize) -> &str {
        match i {
            0 => &self.bg,
            1 => &self.panel,
            2 => &self.fg,
            3 => &self.dim,
            4 => &self.accent,
            5 => &self.accent2,
            _ => &self.border,
        }
    }

    /// Set slot `i` (0..7) to an RGB triple, written back as `#rrggbb`.
    pub fn set_slot(&mut self, i: usize, rgb: [u8; 3]) {
        let hex = to_hex(rgb);
        match i {
            0 => self.bg = hex,
            1 => self.panel = hex,
            2 => self.fg = hex,
            3 => self.dim = hex,
            4 => self.accent = hex,
            5 => self.accent2 = hex,
            _ => self.border = hex,
        }
    }

    /// Slot `i` as an RGB triple (tolerant parse; falls back to the `modern_dark` slot).
    pub fn slot_rgb(&self, i: usize) -> [u8; 3] {
        parse_hex(self.slot(i)).unwrap_or_else(|| parse_hex(Self::modern_dark().slot(i)).unwrap())
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::modern_dark()
    }
}

impl From<&ThemeColors> for Theme {
    fn from(c: &ThemeColors) -> Self {
        let color = |i: usize| {
            let [r, g, b] = c.slot_rgb(i);
            Color::Rgb(r, g, b)
        };
        Theme {
            bg: color(0),
            panel: color(1),
            fg: color(2),
            dim: color(3),
            accent: color(4),
            accent2: color(5),
            border: color(6),
        }
    }
}

/// Parse a `#rrggbb` / `rrggbb` hex colour into an RGB triple. Tolerant: returns `None` on any
/// malformed input (wrong length, non-hex), leaving the caller to substitute a default.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ])
}

/// Format an RGB triple as a lowercase `#rrggbb` hex string.
pub fn to_hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl Theme {
    /// The default uniform theme (a Tokyo-Night-ish modern dark).
    pub fn modern_dark() -> Self {
        Theme::from(&ThemeColors::modern_dark())
    }

    /// Base fill for the whole screen.
    pub fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }
    pub fn body(&self) -> Style {
        Style::default().fg(self.fg)
    }
    pub fn heading(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }
    pub fn dim_style(&self) -> Style {
        Style::default().fg(self.dim)
    }
    pub fn accent_style(&self) -> Style {
        Style::default().fg(self.accent)
    }
    /// Inline `code`.
    pub fn code_inline(&self) -> Style {
        Style::default().fg(self.accent2)
    }
    /// Highlighted / marker text with no authored colour: a neutral tint behind readable text.
    /// Derived from the palette (no extra configurable slot) and distinct from links (accent fg),
    /// inline code (accent2 fg), and the selected row (accent bg + bold).
    pub fn highlight(&self) -> Style {
        Style::default().bg(self.accent2).fg(self.bg)
    }
    /// Blend `color` over the background at `alpha` (0..=1) — for subtle tints like a highlighter
    /// mark, so the author's colour shows while normal-fg text stays readable. `bg` is always RGB
    /// (theme invariant); a non-RGB bg degrades to black.
    pub fn tint(&self, (r, g, b): (u8, u8, u8), alpha: f32) -> Color {
        let (br, bg, bb) = match self.bg {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => (0, 0, 0),
        };
        let mix = |fg: u8, bg: u8| (fg as f32 * alpha + bg as f32 * (1.0 - alpha)).round() as u8;
        Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb))
    }
    /// Fenced code block body.
    pub fn code_block(&self) -> Style {
        Style::default().fg(self.fg).bg(self.panel)
    }
    /// The selected row in a list.
    pub fn selected(&self) -> Style {
        Style::default()
            .fg(self.bg)
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_tolerates_garbage() {
        assert_eq!(parse_hex("#1a1b26"), Some([0x1a, 0x1b, 0x26]));
        assert_eq!(parse_hex("1a1b26"), Some([0x1a, 0x1b, 0x26]));
        assert_eq!(to_hex([0x1a, 0x1b, 0x26]), "#1a1b26");
        // Malformed → None (caller substitutes a default).
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn colors_resolve_to_rgb_for_every_preset() {
        for name in PRESETS {
            let theme = Theme::from(&ThemeColors::preset(name).unwrap());
            for c in [theme.bg, theme.fg, theme.accent, theme.border] {
                assert!(
                    matches!(c, Color::Rgb(..)),
                    "{name}: non-RGB slot breaks blending"
                );
            }
        }
    }

    #[test]
    fn bad_slot_falls_back_to_modern_dark() {
        let mut c = ThemeColors::modern_dark();
        c.accent = "not a colour".into();
        // The accent slot index is 4; tolerant parse → modern_dark's accent.
        assert_eq!(c.slot_rgb(4), parse_hex("#7dcfff").unwrap());
    }
}
