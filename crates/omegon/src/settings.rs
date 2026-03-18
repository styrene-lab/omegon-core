//! Runtime settings — mutable configuration shared between TUI and agent loop.
//!
//! This replaces pi's `sharedState` global. All runtime-mutable values live here.
//! The TUI reads for display. Commands write via the shared Arc<Mutex>.
//! The agent loop reads before each turn.
//!
//! Settings persist for the session. Serialized to session snapshot on save.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Runtime settings that can change mid-session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Active model (provider:model-id format).
    pub model: String,

    /// Thinking level: off, low, medium, high.
    pub thinking: ThinkingLevel,

    /// Maximum turns per agent invocation. 0 = no limit.
    pub max_turns: u32,

    /// Context compaction threshold (fraction of context window).
    pub compaction_threshold: f32,

    /// Context window size (tokens). Inferred from model.
    pub context_window: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "anthropic:claude-sonnet-4-20250514".into(),
            thinking: ThinkingLevel::Medium,
            max_turns: 50,
            compaction_threshold: 0.75,
            context_window: 200_000,
        }
    }
}

impl Settings {
    pub fn new(model: &str) -> Self {
        let context_window = infer_context_window(model);
        Self {
            model: model.to_string(),
            context_window,
            ..Default::default()
        }
    }

    pub fn model_short(&self) -> &str {
        self.model.split(':').next_back()
            .or_else(|| self.model.split('/').next_back())
            .unwrap_or(&self.model)
    }

    pub fn provider(&self) -> &str {
        self.model.split(':').next().unwrap_or("anthropic")
    }
}

/// Thinking level — controls extended thinking budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "low" | "min" | "minimal" => Some(Self::Low),
            "medium" | "med" | "default" => Some(Self::Medium),
            "high" | "max" => Some(Self::High),
            _ => None,
        }
    }

    pub fn budget_tokens(&self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(5_000),
            Self::Medium => Some(10_000),
            Self::High => Some(50_000),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Off => "○",
            Self::Low => "◔",
            Self::Medium => "◑",
            Self::High => "◉",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Off, Self::Low, Self::Medium, Self::High]
    }
}

/// Infer context window from model identifier.
fn infer_context_window(model: &str) -> usize {
    let name = model.split(':').next_back().unwrap_or(model);
    if name.contains("opus") { return 200_000; }
    if name.contains("sonnet") { return 200_000; }
    if name.contains("haiku") { return 200_000; }
    if name.contains("gpt-4") { return 128_000; }
    if name.contains("o3") { return 200_000; }
    200_000 // safe default
}

/// Thread-safe shared settings handle.
pub type SharedSettings = Arc<Mutex<Settings>>;

pub fn shared(model: &str) -> SharedSettings {
    Arc::new(Mutex::new(Settings::new(model)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default() {
        let s = Settings::default();
        assert_eq!(s.thinking, ThinkingLevel::Medium);
        assert_eq!(s.context_window, 200_000);
    }

    #[test]
    fn model_short_extracts_name() {
        let s = Settings::new("anthropic:claude-opus-4-20250514");
        assert_eq!(s.model_short(), "claude-opus-4-20250514");
        assert_eq!(s.provider(), "anthropic");
    }

    #[test]
    fn thinking_level_round_trip() {
        for level in ThinkingLevel::all() {
            let s = level.as_str();
            assert_eq!(ThinkingLevel::parse(s), Some(*level));
        }
    }

    #[test]
    fn context_window_inference() {
        assert_eq!(infer_context_window("anthropic:claude-sonnet-4-20250514"), 200_000);
        assert_eq!(infer_context_window("openai:gpt-4.1"), 128_000);
    }

    #[test]
    fn thinking_budget() {
        assert_eq!(ThinkingLevel::Off.budget_tokens(), None);
        assert_eq!(ThinkingLevel::High.budget_tokens(), Some(50_000));
    }
}
