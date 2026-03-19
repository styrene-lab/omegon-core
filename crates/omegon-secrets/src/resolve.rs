//! Secret resolution — env vars, keychain, shell commands.

use crate::recipes::RecipeStore;
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

/// Resolve a secret by name. Priority: env var > recipe.
pub fn resolve_secret(name: &str, recipes: &RecipeStore) -> Option<String> {
    // 1. Check environment variable
    if let Ok(val) = std::env::var(name) {
        if !val.is_empty() {
            return Some(val);
        }
    }

    // 2. Check recipe store
    if let Some(recipe) = recipes.get(name) {
        return execute_recipe(recipe);
    }

    None
}

/// Execute a recipe string to resolve a secret value.
///
/// Recipe formats:
/// - `env:VAR_NAME` — read from environment variable
/// - `cmd:some command` — execute shell command, trim output
/// - `keychain:service_name` — macOS Keychain (security find-generic-password)
/// - `file:/path/to/file` — read first line of file
pub fn execute_recipe(recipe: &str) -> Option<String> {
    let (kind, payload) = recipe.split_once(':')?;

    match kind {
        "env" => std::env::var(payload).ok().filter(|v| !v.is_empty()),

        "cmd" => {
            let output = Command::new("sh")
                .args(["-c", payload])
                .output()
                .ok()?;
            if output.status.success() {
                let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if val.is_empty() { None } else { Some(val) }
            } else {
                tracing::warn!(recipe = recipe, "secret recipe command failed");
                None
            }
        }

        "keychain" => {
            let output = Command::new("security")
                .args(["find-generic-password", "-s", payload, "-w"])
                .output()
                .ok()?;
            if output.status.success() {
                let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if val.is_empty() { None } else { Some(val) }
            } else {
                None
            }
        }

        "file" => {
            let content = std::fs::read_to_string(payload).ok()?;
            let first_line = content.lines().next()?.trim().to_string();
            if first_line.is_empty() { None } else { Some(first_line) }
        }

        _ => {
            tracing::warn!(kind = kind, "unknown secret recipe kind");
            None
        }
    }
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
        assert_eq!(val, Some("hunter2".to_string()));
        unsafe { std::env::remove_var("TEST_SECRET_RESOLVE") };
    }

    #[test]
    fn execute_env_recipe() {
        unsafe { std::env::set_var("TEST_RECIPE_ENV", "secret_val") };
        let val = execute_recipe("env:TEST_RECIPE_ENV");
        assert_eq!(val, Some("secret_val".to_string()));
        unsafe { std::env::remove_var("TEST_RECIPE_ENV") };
    }

    #[test]
    fn execute_cmd_recipe() {
        let val = execute_recipe("cmd:echo hello");
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn execute_file_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        std::fs::write(&path, "my_secret\nextra_line\n").unwrap();
        let val = execute_recipe(&format!("file:{}", path.display()));
        assert_eq!(val, Some("my_secret".to_string()));
    }

    #[test]
    fn unknown_recipe_kind() {
        let val = execute_recipe("unknown:something");
        assert_eq!(val, None);
    }
}
