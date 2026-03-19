//! Secret management for Omegon.
//!
//! Layers:
//! 1. Resolution — resolve secrets from env vars, keychain, shell commands
//! 2. Redaction — scrub known secret values from tool output before it reaches the LLM
//! 3. Tool guards — block/confirm tool calls accessing sensitive paths
//! 4. Audit log — append-only record of guard decisions
//!
//! Design: secrets are never stored in memory longer than needed. Recipes
//! (instructions for *how* to resolve a secret) are persisted, but values
//! are resolved on-demand and held only in the redaction set.

mod audit;
mod guards;
mod recipes;
mod redact;
mod resolve;

pub use audit::AuditLog;
pub use guards::{GuardDecision, PathGuard};
pub use recipes::{Recipe, RecipeStore};

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Central secrets manager — owns the redaction set, recipes, and guards.
pub struct SecretsManager {
    /// Resolved secret values for redaction (name → value).
    /// Values are kept in memory only for the duration of the session.
    redaction_set: Arc<RwLock<HashMap<String, String>>>,
    /// Recipe store (persisted to ~/.omegon/secrets.json)
    recipes: RecipeStore,
    /// Path guard for sensitive file access
    path_guard: PathGuard,
    /// Audit log
    audit: AuditLog,
}

impl SecretsManager {
    /// Create a new secrets manager, loading recipes from the config directory.
    pub fn new(config_dir: &std::path::Path) -> anyhow::Result<Self> {
        let recipes = RecipeStore::load(config_dir)?;
        let audit = AuditLog::new(config_dir);
        let path_guard = PathGuard::new();

        let mut mgr = Self {
            redaction_set: Arc::new(RwLock::new(HashMap::new())),
            recipes,
            path_guard,
            audit,
        };

        // Pre-resolve all known secrets into the redaction set
        mgr.refresh_redaction_set();

        Ok(mgr)
    }

    /// Resolve a secret by name. Checks env vars first, then recipes.
    pub fn resolve(&self, name: &str) -> Option<String> {
        resolve::resolve_secret(name, &self.recipes)
    }

    /// Redact all known secret values from a string.
    pub fn redact(&self, input: &str) -> String {
        let set = self.redaction_set.read().unwrap();
        redact::redact_string(input, &set)
    }

    /// Redact secrets from tool result content blocks.
    pub fn redact_content(&self, content: &mut Vec<omegon_traits::ContentBlock>) {
        let set = self.redaction_set.read().unwrap();
        redact::redact_content_blocks(content, &set);
    }

    /// Check if a tool call should be guarded (sensitive path access).
    pub fn check_guard(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<GuardDecision> {
        let decision = self.path_guard.check(tool_name, args);
        if let Some(ref d) = decision {
            self.audit.log_guard(tool_name, args, d);
        }
        decision
    }

    /// Get a clone of the redaction set for use in other components.
    pub fn redaction_handle(&self) -> Arc<RwLock<HashMap<String, String>>> {
        self.redaction_set.clone()
    }

    /// Re-resolve all secrets and update the redaction set.
    fn refresh_redaction_set(&mut self) {
        let mut set = self.redaction_set.write().unwrap();
        set.clear();

        // Resolve from recipes
        for (name, recipe) in self.recipes.iter() {
            if let Some(value) = resolve::execute_recipe(recipe) {
                if !value.is_empty() {
                    set.insert(name.clone(), value);
                }
            }
        }

        // Also grab well-known env vars that might contain secrets
        for env_name in resolve::WELL_KNOWN_SECRET_ENVS {
            if let Ok(value) = std::env::var(env_name) {
                if !value.is_empty() && !set.values().any(|v| v == &value) {
                    set.insert(env_name.to_string(), value);
                }
            }
        }

        tracing::info!(count = set.len(), "redaction set refreshed");
    }
}
