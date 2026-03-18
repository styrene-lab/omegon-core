//! TUI Theme — Alpharius color system for ratatui.
//!
//! Derived from the Omegon style guide (skills/style/SKILL.md).
//! All TUI color references should go through this module.
//! To add a new theme, implement the Theme trait.

use ratatui::style::{Color, Modifier, Style};

/// Semantic color slots for the TUI.
pub trait Theme: Send + Sync {
    // ─── Core palette ───────────────────────────────────────────────
    fn bg(&self) -> Color;
    fn card_bg(&self) -> Color;
    fn surface_bg(&self) -> Color;
    fn border(&self) -> Color;
    fn border_dim(&self) -> Color;

    // ─── Text ───────────────────────────────────────────────────────
    fn fg(&self) -> Color;
    fn muted(&self) -> Color;
    fn dim(&self) -> Color;

    // ─── Brand ──────────────────────────────────────────────────────
    fn accent(&self) -> Color;
    fn accent_muted(&self) -> Color;
    fn accent_bright(&self) -> Color;

    // ─── Signal ─────────────────────────────────────────────────────
    fn success(&self) -> Color;
    fn error(&self) -> Color;
    fn warning(&self) -> Color;
    fn caution(&self) -> Color;

    // ─── Derived styles ─────────────────────────────────────────────

    fn style_fg(&self) -> Style {
        Style::default().fg(self.fg())
    }
    fn style_muted(&self) -> Style {
        Style::default().fg(self.muted())
    }
    fn style_dim(&self) -> Style {
        Style::default().fg(self.dim())
    }
    fn style_accent(&self) -> Style {
        Style::default().fg(self.accent())
    }
    fn style_accent_bold(&self) -> Style {
        Style::default().fg(self.accent()).add_modifier(Modifier::BOLD)
    }
    fn style_success(&self) -> Style {
        Style::default().fg(self.success())
    }
    fn style_error(&self) -> Style {
        Style::default().fg(self.error())
    }
    fn style_warning(&self) -> Style {
        Style::default().fg(self.warning())
    }
    fn style_heading(&self) -> Style {
        Style::default().fg(self.accent_bright()).add_modifier(Modifier::BOLD)
    }
    fn style_user_input(&self) -> Style {
        Style::default().fg(self.fg()).add_modifier(Modifier::BOLD)
    }
    fn style_footer_bg(&self) -> Style {
        Style::default().bg(self.card_bg())
    }
    fn style_border(&self) -> Style {
        Style::default().fg(self.border())
    }
    fn style_border_dim(&self) -> Style {
        Style::default().fg(self.border_dim())
    }
}

/// Alpharius — the default Omegon theme.
/// Dark, cold, precise. Deep void with iridescent ceramite teal.
pub struct Alpharius;

impl Theme for Alpharius {
    fn bg(&self) -> Color { Color::Rgb(6, 8, 14) }
    fn card_bg(&self) -> Color { Color::Rgb(14, 22, 34) }
    fn surface_bg(&self) -> Color { Color::Rgb(19, 30, 46) }
    fn border(&self) -> Color { Color::Rgb(26, 52, 72) }
    fn border_dim(&self) -> Color { Color::Rgb(14, 30, 48) }

    fn fg(&self) -> Color { Color::Rgb(196, 216, 228) }
    fn muted(&self) -> Color { Color::Rgb(96, 120, 136) }
    fn dim(&self) -> Color { Color::Rgb(52, 72, 88) }

    fn accent(&self) -> Color { Color::Rgb(42, 180, 200) }
    fn accent_muted(&self) -> Color { Color::Rgb(26, 136, 152) }
    fn accent_bright(&self) -> Color { Color::Rgb(110, 202, 216) }

    fn success(&self) -> Color { Color::Rgb(26, 184, 120) }
    fn error(&self) -> Color { Color::Rgb(200, 48, 48) }
    fn warning(&self) -> Color { Color::Rgb(200, 100, 24) }
    fn caution(&self) -> Color { Color::Rgb(184, 144, 32) }
}

/// The default theme instance.
pub fn default_theme() -> Box<dyn Theme> {
    Box::new(Alpharius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpharius_colors_are_distinct() {
        let t = Alpharius;
        // Core palette colors should all be different
        assert_ne!(t.bg(), t.fg());
        assert_ne!(t.accent(), t.success());
        assert_ne!(t.error(), t.warning());
    }

    #[test]
    fn derived_styles_have_correct_color() {
        let t = Alpharius;
        assert_eq!(t.style_accent().fg, Some(t.accent()));
    }
}
