//! Secret management for Omegon.
//!
//! Layers:
//! 1. Resolution — resolve secrets from env vars, keyring, shell commands
//! 2. Redaction — scrub known secret values from tool output (Aho-Corasick single-pass)
//! 3. Tool guards — block/confirm tool calls accessing sensitive paths
//! 4. Audit log — append-only record of guard decisions
//!
//! Security properties:
//! - Secret values wrapped in `secrecy::SecretString` — zeroized on drop
//! - Keyring access via `keyring` crate — cross-platform (macOS/Linux/Windows)
//! - Redaction via `aho-corasick` — single-pass, no quadratic behavior
//! - Recipes store *how* to resolve secrets, never the secret values themselves

mod audit;
mod guards;
mod recipes;
mod redact;
mod resolve;

pub use audit::AuditLog;
pub use guards::{GuardDecision, PathGuard};
pub use recipes::{Recipe, RecipeStore};
pub use redact::Redactor;
pub use resolve::{store_in_keyring, delete_from_keyring};

use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Central secrets manager — owns the redaction set, recipes, and guards.
pub struct SecretsManager {
    /// Resolved secret values for redaction (name → SecretString).
    /// Values are zeroized when dropped.
    redaction_set: Arc<RwLock<HashMap<String, SecretString>>>,
    /// Pre-compiled Aho-Corasick redactor (rebuilt when secrets change).
    redactor: Arc<RwLock<Redactor>>,
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
            redactor: Arc::new(RwLock::new(Redactor::build(&HashMap::new()))),
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
            .map(|s| s.expose_secret().to_string())
    }

    /// Redact all known secret values from a string.
    pub fn redact(&self, input: &str) -> String {
        let redactor = self.redactor.read().unwrap();
        redactor.redact(input)
    }

    /// Redact secrets from tool result content blocks.
    pub fn redact_content(&self, content: &mut Vec<omegon_traits::ContentBlock>) {
        let redactor = self.redactor.read().unwrap();
        redactor.redact_content_blocks(content);
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

    /// Re-resolve all secrets and rebuild the redaction automaton.
    fn refresh_redaction_set(&mut self) {
        let mut set = self.redaction_set.write().unwrap();
        set.clear();

        // Resolve from recipes
        for (name, recipe) in self.recipes.iter() {
            if let Some(value) = resolve::execute_recipe(name, recipe) {
                set.insert(name.clone(), value);
            }
        }

        // Also grab well-known env vars that might contain secrets
        for env_name in resolve::WELL_KNOWN_SECRET_ENVS {
            if let Ok(value) = std::env::var(env_name) {
                if !value.is_empty()
                    && !set.values().any(|v| v.expose_secret() == value)
                {
                    set.insert(env_name.to_string(), SecretString::from(value));
                }
            }
        }

        let count = set.len();

        // Rebuild the Aho-Corasick automaton
        let new_redactor = Redactor::build(&set);
        *self.redactor.write().unwrap() = new_redactor;

        tracing::info!(count = count, "redaction set refreshed (keyring + aho-corasick)");
    }
}
