//! Secret resolution — env vars, keyring, shell commands.
//!
//! Uses the `keyring` crate for cross-platform credential store access
//! (macOS Keychain, Windows Credential Manager, Linux Secret Service).
//! Secret values are wrapped in `secrecy::SecretString` and zeroized on drop.

use crate::recipes::RecipeStore;
use secrecy::{ExposeSecret, SecretString};
use std::process::Command;

/// Well-known environment variables that commonly contain secrets.
pub const WELL_KNOWN_SECRET_ENVS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "BRAVE_API_KEY",
    "TAVILY_API_KEY",
    "SERPER_API_KEY",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "GH_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "NPM_TOKEN",
    "DOCKER_PASSWORD",
    "IGOR_API_KEY",
];

/// Omegon's keyring service name — used for cross-platform credential storage.
const KEYRING_SERVICE: &str = "omegon";

/// Resolve a secret by name. Priority: env var > recipe.
/// Returns a SecretString that auto-zeroizes on drop.
pub fn resolve_secret(name: &str, recipes: &RecipeStore) -> Option<SecretString> {
    // 1. Check environment variable
    if let Ok(val) = std::env::var(name) {
        if !val.is_empty() {
            return Some(SecretString::from(val));
        }
    }

    // 2. Check recipe store
    if let Some(recipe) = recipes.get(name) {
        return execute_recipe(name, recipe);
    }

    None
}

/// Execute a recipe string to resolve a secret value.
///
/// Recipe formats:
/// - `env:VAR_NAME` — read from environment variable
/// - `cmd:some command` — execute shell command, trim output
/// - `keyring:service_name` — cross-platform keyring (macOS Keychain, Linux Secret Service, Windows Credential Manager)
/// - `keychain:service_name` — alias for keyring (backward compat with macOS-only shell-out)
/// - `file:/path/to/file` — read first line of file
pub fn execute_recipe(name: &str, recipe: &str) -> Option<SecretString> {
    let (kind, payload) = recipe.split_once(':')?;

    match kind {
        "env" => std::env::var(payload)
            .ok()
            .filter(|v| !v.is_empty())
            .map(SecretString::from),

        "cmd" => {
            let output = Command::new("sh")
                .args(["-c", payload])
                .output()
                .ok()?;
            if output.status.success() {
                let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if val.is_empty() {
                    None
                } else {
                    Some(SecretString::from(val))
                }
            } else {
                tracing::warn!(recipe_kind = kind, "secret recipe command failed");
                None
            }
        }

        // Cross-platform keyring via the `keyring` crate
        "keyring" | "keychain" => {
            match keyring::Entry::new(KEYRING_SERVICE, payload) {
                Ok(entry) => match entry.get_password() {
                    Ok(val) if !val.is_empty() => Some(SecretString::from(val)),
                    Ok(_) => None,
                    Err(keyring::Error::NoEntry) => {
                        tracing::debug!(name = name, "no keyring entry found");
                        None
                    }
                    Err(e) => {
                        tracing::warn!(name = name, error = %e, "keyring access failed");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(name = name, error = %e, "keyring entry creation failed");
                    None
                }
            }
        }

        "file" => {
            let content = std::fs::read_to_string(payload).ok()?;
            let first_line = content.lines().next()?.trim().to_string();
            if first_line.is_empty() {
                None
            } else {
                Some(SecretString::from(first_line))
            }
        }

        _ => {
            tracing::warn!(kind = kind, "unknown secret recipe kind");
            None
        }
    }
}

/// Store a secret value in the cross-platform keyring.
pub fn store_in_keyring(name: &str, value: &str) -> anyhow::Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, name)?;
    entry.set_password(value)?;
    tracing::info!(name = name, "stored secret in keyring");
    Ok(())
}

/// Delete a secret from the cross-platform keyring.
pub fn delete_from_keyring(name: &str) -> anyhow::Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, name)?;
    entry.delete_credential()?;
    Ok(())
}

/// Expose a SecretString's value for operations that need it (e.g., redaction set building).
/// The caller is responsible for not leaking the exposed value.
pub fn expose(secret: &SecretString) -> &str {
    secret.expose_secret()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_from_env() {
        // SAFETY: test-only, single-threaded
        unsafe { std::env::set_var("TEST_SECRET_RESOLVE", "hunter2") };
        let recipes = RecipeStore::empty();
        let val = resolve_secret("TEST_SECRET_RESOLVE", &recipes);
        assert_eq!(val.map(|s| s.expose_secret().to_string()), Some("hunter2".to_string()));
        unsafe { std::env::remove_var("TEST_SECRET_RESOLVE") };
    }

    #[test]
    fn execute_env_recipe() {
        unsafe { std::env::set_var("TEST_RECIPE_ENV", "secret_val") };
        let val = execute_recipe("test", "env:TEST_RECIPE_ENV");
        assert_eq!(val.map(|s| s.expose_secret().to_string()), Some("secret_val".to_string()));
        unsafe { std::env::remove_var("TEST_RECIPE_ENV") };
    }

    #[test]
    fn execute_cmd_recipe() {
        let val = execute_recipe("test", "cmd:echo hello");
        assert_eq!(val.map(|s| s.expose_secret().to_string()), Some("hello".to_string()));
    }

    #[test]
    fn execute_file_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        std::fs::write(&path, "my_secret\nextra_line\n").unwrap();
        let val = execute_recipe("test", &format!("file:{}", path.display()));
        assert_eq!(val.map(|s| s.expose_secret().to_string()), Some("my_secret".to_string()));
    }

    #[test]
    fn unknown_recipe_kind() {
        let val = execute_recipe("test", "unknown:something");
        assert_eq!(val.map(|s| s.expose_secret().to_string()), None);
    }
}
