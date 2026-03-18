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

// ─── Profile persistence ────────────────────────────────────────────────────

/// Profile: settings that persist with the project in .pi/config.json.
/// Read on startup, written on change. Travels with git.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_model: Option<ProfileModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileModel {
    pub provider: String,
    pub model_id: String,
}

impl Profile {
    /// Load profile. Project-level (`.omegon/profile.json`) overrides
    /// global (`~/.config/omegon/profile.json`). Both are optional.
    pub fn load(cwd: &std::path::Path) -> Self {
        // Project-level first
        let project_path = cwd.join(".omegon/profile.json");
        if let Ok(content) = std::fs::read_to_string(&project_path)
            && let Ok(profile) = serde_json::from_str(&content) {
                tracing::debug!(path = %project_path.display(), "project profile loaded");
                return profile;
            }

        // Global fallback
        if let Some(global_path) = global_profile_path()
            && let Ok(content) = std::fs::read_to_string(&global_path)
                && let Ok(profile) = serde_json::from_str(&content) {
                    tracing::debug!(path = %global_path.display(), "global profile loaded");
                    return profile;
                }

        Self { last_used_model: None, thinking_level: None, max_turns: None }
    }

    /// Save to the project-level profile.
    pub fn save(&self, cwd: &std::path::Path) -> anyhow::Result<()> {
        let dir = cwd.join(".omegon");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("profile.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        tracing::debug!(path = %path.display(), "project profile saved");
        Ok(())
    }

    /// Save to the global profile (~/.config/omegon/profile.json).
    pub fn save_global(&self) -> anyhow::Result<()> {
        let path = global_profile_path()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        tracing::debug!(path = %path.display(), "global profile saved");
        Ok(())
    }

    /// Apply profile to settings (called at startup).
    pub fn apply_to(&self, settings: &mut Settings) {
        if let Some(ref m) = self.last_used_model {
            settings.model = format!("{}:{}", m.provider, m.model_id);
            settings.context_window = infer_context_window(&settings.model);
        }
        if let Some(ref t) = self.thinking_level
            && let Some(level) = ThinkingLevel::parse(t) {
                settings.thinking = level;
            }
        if let Some(turns) = self.max_turns {
            settings.max_turns = turns;
        }
    }

    /// Capture current settings into the profile (called on change).
    pub fn capture_from(&mut self, settings: &Settings) {
        self.last_used_model = Some(ProfileModel {
            provider: settings.provider().to_string(),
            model_id: settings.model_short().to_string(),
        });
        self.thinking_level = Some(settings.thinking.as_str().to_string());
        self.max_turns = Some(settings.max_turns);
    }
}

fn global_profile_path() -> Option<std::path::PathBuf> {
    // XDG on Linux, ~/Library/Application Support on macOS
    dirs::config_dir().map(|d| d.join("omegon/profile.json"))
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
