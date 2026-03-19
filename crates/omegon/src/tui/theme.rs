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
    // ── Backgrounds — stepped contrast for visual layering ──────
    fn bg(&self) -> Color { Color::Rgb(8, 10, 18) }           // void
    fn card_bg(&self) -> Color { Color::Rgb(16, 24, 38) }     // raised card
    fn surface_bg(&self) -> Color { Color::Rgb(22, 34, 52) }  // code/result blocks
    fn border(&self) -> Color { Color::Rgb(36, 64, 88) }      // visible borders
    fn border_dim(&self) -> Color { Color::Rgb(20, 38, 56) }  // subtle dividers

    // ── Text — clear hierarchy from bright to barely visible ────
    fn fg(&self) -> Color { Color::Rgb(200, 218, 230) }       // primary text
    fn muted(&self) -> Color { Color::Rgb(120, 148, 168) }    // secondary text
    fn dim(&self) -> Color { Color::Rgb(64, 88, 108) }        // tertiary/chrome

    // ── Brand — iridescent ceramite teal ────────────────────────
    fn accent(&self) -> Color { Color::Rgb(42, 180, 200) }
    fn accent_muted(&self) -> Color { Color::Rgb(28, 140, 160) }
    fn accent_bright(&self) -> Color { Color::Rgb(120, 210, 224) }

    // ── Signal — status colors ──────────────────────────────────
    fn success(&self) -> Color { Color::Rgb(32, 192, 128) }
    fn error(&self) -> Color { Color::Rgb(220, 56, 56) }
    fn warning(&self) -> Color { Color::Rgb(210, 120, 32) }
    fn caution(&self) -> Color { Color::Rgb(196, 160, 40) }
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
