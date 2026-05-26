//! The visual theme. One source of truth for colors and styles, so a later theme/accent
//! picker is just a swap. v1 is "modern dark" (a Tokyo-Night-ish palette).

use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub border: Color,
}

impl Theme {
    pub fn modern_dark() -> Self {
        Self {
            bg: Color::Rgb(0x1a, 0x1b, 0x26),
            panel: Color::Rgb(0x24, 0x28, 0x3b),
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            dim: Color::Rgb(0x56, 0x5f, 0x89),
            accent: Color::Rgb(0x7d, 0xcf, 0xff),
            accent2: Color::Rgb(0xbb, 0x9a, 0xf7),
            border: Color::Rgb(0x29, 0x2e, 0x42),
        }
    }

    /// Base fill for the whole screen.
    pub fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }
    pub fn body(&self) -> Style {
        Style::default().fg(self.fg)
    }
    pub fn heading(&self) -> Style {
        Style::default().fg(self.accent).add_modifier(Modifier::BOLD)
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
    /// Fenced code block body.
    pub fn code_block(&self) -> Style {
        Style::default().fg(self.fg).bg(self.panel)
    }
    /// The selected row in a list.
    pub fn selected(&self) -> Style {
        Style::default().fg(self.bg).bg(self.accent).add_modifier(Modifier::BOLD)
    }
}
