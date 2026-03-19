//! TUI effects — tachyonfx-powered visual polish.
//!
//! Manages animated effects that run as post-processing passes on the ratatui buffer.
//! Effects auto-expire after their duration — no manual cleanup needed.
//!
//! Integration: `App::draw()` renders widgets normally, then calls
//! `effects.process(delta, buf, area)` to apply active effects.

use std::time::Instant;

use ratatui::prelude::*;
use tachyonfx::{fx, EffectManager, EffectTimer, Interpolation, Motion};

use super::theme::Theme;

/// Named effect slots for unique effects (only one per slot at a time).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectSlot {
    #[default]
    /// Sweep-in on the footer zone.
    FooterReveal,
    /// Fade-in on the conversation zone.
    ConversationFade,
    /// Spinner verb hsl shift.
    SpinnerGlow,
    /// Brief flash on a footer value change.
    FooterPing,
}

/// Manages active effects and tracks frame timing.
pub struct Effects {
    manager: EffectManager<EffectSlot>,
    last_frame: Instant,
    /// Areas updated each draw — effects process against these.
    footer_area: Option<Rect>,
    conversation_area: Option<Rect>,
}

impl Effects {
    pub fn new() -> Self {
        Self {
            manager: EffectManager::default(),
            last_frame: Instant::now(),
            footer_area: None,
            conversation_area: None,
        }
    }

    /// Record the areas for this frame (call before process).
    pub fn set_areas(&mut self, conversation: Rect, footer: Rect) {
        self.conversation_area = Some(conversation);
        self.footer_area = Some(footer);
    }

    /// Process all active effects on the buffer. Call after rendering widgets.
    pub fn process(&mut self, buf: &mut Buffer, area: Rect) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame);
        self.last_frame = now;

        let duration = tachyonfx::Duration::from_millis(delta.as_millis() as u32);
        self.manager.process_effects(duration, buf, area);
    }

    /// Queue the initial startup reveal effects.
    pub fn queue_startup(&mut self, t: &dyn Theme) {
        // Footer sweeps in from bottom
        let footer_sweep = self.manager.unique(
            EffectSlot::FooterReveal,
            fx::sweep_in(
                Motion::DownToUp,
                3,   // gradient length
                1,   // randomness
                t.bg(),
                EffectTimer::from_ms(400, Interpolation::CubicOut),
            ),
        );
        self.manager.add_effect(footer_sweep);

        // Conversation fades in from void
        let conv_fade = self.manager.unique(
            EffectSlot::ConversationFade,
            fx::fade_from(
                t.bg(),
                t.bg(),
                EffectTimer::from_ms(600, Interpolation::CubicOut),
            ),
        );
        self.manager.add_effect(conv_fade);
    }

    /// Flash effect when a footer value changes (fact count, context %, etc.).
    pub fn ping_footer(&mut self, t: &dyn Theme) {
        let ping = self.manager.unique(
            EffectSlot::FooterPing,
            fx::fade_from_fg(
                t.accent_bright(),
                EffectTimer::from_ms(300, Interpolation::CubicOut),
            ),
        );
        self.manager.add_effect(ping);
    }

    /// HSL cycling glow on the spinner/working area.
    pub fn start_spinner_glow(&mut self) {
        let glow = self.manager.unique(
            EffectSlot::SpinnerGlow,
            fx::ping_pong(fx::hsl_shift_fg(
                [30.0, 0.0, 0.15],
                EffectTimer::from_ms(2000, Interpolation::SineInOut),
            )),
        );
        self.manager.add_effect(glow);
    }

    /// Stop the spinner glow.
    pub fn stop_spinner_glow(&mut self) {
        self.manager.cancel_unique_effect(EffectSlot::SpinnerGlow);
    }

    /// True if any effects are active (drives render timing).
    pub fn has_active(&self) -> bool {
        self.manager.is_running()
    }
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Alpharius;

    #[test]
    fn effects_new_has_no_active() {
        let fx = Effects::new();
        assert!(!fx.has_active());
    }

    #[test]
    fn queue_startup_adds_effects() {
        let mut fx = Effects::new();
        let t = Alpharius;
        fx.queue_startup(&t);
        assert!(fx.has_active());
    }

    #[test]
    fn ping_footer_adds_effect() {
        let mut fx = Effects::new();
        let t = Alpharius;
        fx.ping_footer(&t);
        assert!(fx.has_active());
    }

    #[test]
    fn spinner_glow_lifecycle() {
        let mut fx = Effects::new();
        fx.start_spinner_glow();
        assert!(fx.has_active());
        fx.stop_spinner_glow();
        // Effect still active until processed — cancel marks it for removal
        // on next process cycle
    }
}
