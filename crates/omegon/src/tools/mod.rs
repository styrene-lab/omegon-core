//! Core tools — the agent's primary capabilities.
//!
//! Phase 0: primitive tools (bash, read, write, edit).
//! Phase 0+: higher-level tools (understand, change, execute, remember, speculate)
//!           that compose the primitives.

pub mod bash;
pub mod change;
pub mod edit;
pub mod local_inference;
pub mod read;
pub mod render;
pub mod speculate;
pub mod validate;
pub mod view;
pub mod web_search;
pub mod write;

// Phase 0+ stubs:
// pub mod understand;  // tree-sitter + scope graph
// pub mod execute;     // bash with progressive disclosure
// pub mod remember;    // session scratchpad

use async_trait::async_trait;
use omegon_traits::{ToolDefinition, ToolProvider, ToolResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Core tool provider — registers the primitive tools.
pub struct CoreTools {
    cwd: PathBuf,
}

impl CoreTools {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    /// Resolve a user-provided path against cwd and verify it doesn't escape
    /// the workspace via `../` traversal. Returns the canonical path on success.
    fn resolve_path(&self, path_str: &str) -> anyhow::Result<PathBuf> {
        let joined = self.cwd.join(path_str);

        // Canonicalize to resolve symlinks and `..` — but the file may not
        // exist yet (write/edit creating new files). In that case, canonicalize
        // the parent directory and append the filename.
        let canonical = if joined.exists() {
            joined.canonicalize()?
        } else if let Some(parent) = joined.parent() {
            // Create parent dirs if needed (write tool does this), then canonicalize
            if parent.exists() {
                parent.canonicalize()?.join(joined.file_name().unwrap_or_default())
            } else {
                // Parent doesn't exist — resolve what we can. The write tool
                // will create parents. For now, use lexical normalization.
                lexical_normalize(&joined)
            }
        } else {
            joined.clone()
        };

        let cwd_canonical = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());

        if !canonical.starts_with(&cwd_canonical) {
            anyhow::bail!(
                "Path '{}' resolves to '{}' which is outside the workspace '{}'",
                path_str,
                canonical.display(),
                cwd_canonical.display()
            );
        }

        Ok(joined)
    }
}

/// Lexical path normalization — resolve `.` and `..` without filesystem access.
/// Used as a fallback when the path doesn't exist yet.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Only pop if there's a normal component to pop
                if components.last().is_some_and(|c| {
                    matches!(c, std::path::Component::Normal(_))
                }) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            std::path::Component::CurDir => {} // skip
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

#[async_trait]
impl ToolProvider for CoreTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "bash".into(),
                label: "bash".into(),
                description: "Execute a bash command in the current working directory. \
                    Returns stdout and stderr. Output is truncated to last 2000 lines \
                    or 50KB. Optionally provide a timeout in seconds."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Bash command to execute"
                        },
                        "timeout": {
                            "type": "number",
                            "description": "Timeout in seconds (optional)"
                        }
                    },
                    "required": ["command"]
                }),
            },
            ToolDefinition {
                name: "read".into(),
                label: "read".into(),
                description: "Read the contents of a file. Supports text files and \
                    images. Output is truncated to 2000 lines or 50KB. Use offset/limit \
                    for large files."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to read"
                        },
                        "offset": {
                            "type": "number",
                            "description": "Line number to start from (1-indexed)"
                        },
                        "limit": {
                            "type": "number",
                            "description": "Maximum number of lines to read"
                        }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "write".into(),
                label: "write".into(),
                description: "Write content to a file. Creates the file if it doesn't \
                    exist, overwrites if it does. Automatically creates parent directories."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolDefinition {
                name: "edit".into(),
                label: "edit".into(),
                description: "Edit a file by replacing exact text. The oldText must match \
                    exactly (including whitespace). Use this for precise, surgical edits."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to edit"
                        },
                        "oldText": {
                            "type": "string",
                            "description": "Exact text to find and replace"
                        },
                        "newText": {
                            "type": "string",
                            "description": "New text to replace the old text with"
                        }
                    },
                    "required": ["path", "oldText", "newText"]
                }),
            },
            ToolDefinition {
                name: "change".into(),
                label: "change".into(),
                description: "Atomic multi-file edit with automatic validation. Accepts an array \
                    of edits, applies all atomically (rollback on any failure), then runs type \
                    checking. One tool call replaces multiple edits + validation."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "edits": {
                            "type": "array",
                            "description": "Array of edits to apply atomically",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": { "type": "string", "description": "File path" },
                                    "oldText": { "type": "string", "description": "Exact text to find" },
                                    "newText": { "type": "string", "description": "Replacement text" }
                                },
                                "required": ["file", "oldText", "newText"]
                            }
                        },
                        "validate": {
                            "type": "string",
                            "description": "Validation mode: none, quick, standard (default), full (includes tests)",
                            "default": "standard"
                        }
                    },
                    "required": ["edits"]
                }),
            },
            ToolDefinition {
                name: "speculate_start".into(),
                label: "speculate".into(),
                description: "Create a git checkpoint for exploratory changes. Make changes freely, \
                    then use speculate_commit to keep them or speculate_rollback to undo everything."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "label": {
                            "type": "string",
                            "description": "Name for this speculation (e.g. 'try-approach-a')"
                        }
                    },
                    "required": ["label"]
                }),
            },
            ToolDefinition {
                name: "speculate_check".into(),
                label: "speculate".into(),
                description: "Check the current speculation state — shows modified files and \
                    runs validation against them."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDefinition {
                name: "speculate_commit".into(),
                label: "speculate".into(),
                description: "Keep all changes made during speculation and discard the checkpoint."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDefinition {
                name: "speculate_rollback".into(),
                label: "speculate".into(),
                description: "Revert all changes made during speculation back to the checkpoint."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        ]
    }

    async fn execute(
        &self,
        tool_name: &str,
        _call_id: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        match tool_name {
            "bash" => {
                let command = args["command"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'command' argument"))?;
                let timeout = args["timeout"].as_u64();
                bash::execute(command, &self.cwd, timeout, cancel).await
            }
            "read" => {
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;
                let path = self.resolve_path(path_str)?;
                let offset = args["offset"].as_u64().map(|n| n as usize);
                let limit = args["limit"].as_u64().map(|n| n as usize);
                read::execute(&path, offset, limit).await
            }
            "write" => {
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'content' argument"))?;
                let path = self.resolve_path(path_str)?;
                write::execute(&path, content, &self.cwd).await
            }
            "edit" => {
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;
                let old_text = args["oldText"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'oldText' argument"))?;
                let new_text = args["newText"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'newText' argument"))?;
                let path = self.resolve_path(path_str)?;
                edit::execute(&path, old_text, new_text, &self.cwd).await
            }
            "change" => {
                let edits_val = args.get("edits")
                    .ok_or_else(|| anyhow::anyhow!("missing 'edits' argument"))?;
                let edits: Vec<change::EditSpec> = serde_json::from_value(edits_val.clone())?;
                let validate_mode = args.get("validate")
                    .and_then(|v| v.as_str())
                    .map(change::ValidationMode::parse)
                    .unwrap_or(change::ValidationMode::Standard);
                let cwd = self.cwd.clone();
                let cwd2 = cwd.clone();
                change::execute(
                    &edits,
                    validate_mode,
                    &cwd,
                    move |p: &str| {
                        let tools = CoreTools::new(cwd2.clone());
                        tools.resolve_path(p)
                    },
                ).await
            }
            "speculate_start" => {
                let label = args["label"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'label' argument"))?;
                speculate::start(label, &self.cwd).await
            }
            "speculate_check" => {
                speculate::check(&self.cwd).await
            }
            "speculate_commit" => {
                speculate::commit(&self.cwd).await
            }
            "speculate_rollback" => {
                speculate::rollback(&self.cwd).await
            }
            _ => anyhow::bail!("Unknown core tool: {tool_name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_traversal_blocked() {
        let tools = CoreTools::new(PathBuf::from("/tmp/workspace"));
        // Attempting to escape the workspace via ../
        let result = tools.resolve_path("../../../etc/passwd");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("outside the workspace"), "error: {err}");
    }

    #[test]
    fn path_within_workspace_allowed() {
        let dir = tempfile::tempdir().unwrap();
        // Create the subdirectory so canonicalize works
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Use canonical path to match what main.rs does with fs::canonicalize(&cli.cwd)
        let cwd = dir.path().canonicalize().unwrap();
        let tools = CoreTools::new(cwd.clone());
        let result = tools.resolve_path("src/main.rs");
        assert!(result.is_ok(), "error: {:?}", result.unwrap_err());
        assert!(result.unwrap().starts_with(&cwd));
    }

    #[test]
    fn lexical_normalize_resolves_dotdot() {
        let result = lexical_normalize(Path::new("/a/b/../c"));
        assert_eq!(result, PathBuf::from("/a/c"));
    }

    #[test]
    fn lexical_normalize_resolves_dot() {
        let result = lexical_normalize(Path::new("/a/./b/./c"));
        assert_eq!(result, PathBuf::from("/a/b/c"));
    }
}
