//! Core tools — the agent's primary capabilities.
//!
//! Phase 0: primitive tools (bash, read, write, edit).
//! Phase 0+: higher-level tools (understand, change, execute, remember, speculate)
//!           that compose the primitives.

pub mod bash;
pub mod edit;
pub mod read;
pub mod validate;
pub mod write;

// Phase 0+ stubs:
// pub mod understand;  // tree-sitter + scope graph
// pub mod change;      // atomic edits + validation pipeline
// pub mod execute;     // bash with progressive disclosure
// pub mod remember;    // session scratchpad
// pub mod speculate;   // git checkpoint/rollback

use async_trait::async_trait;
use omegon_traits::{ToolDefinition, ToolProvider, ToolResult};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Core tool provider — registers the primitive tools.
pub struct CoreTools {
    cwd: PathBuf,
}

impl CoreTools {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
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
                let path = self.cwd.join(path_str);
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
                let path = self.cwd.join(path_str);
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
                let path = self.cwd.join(path_str);
                edit::execute(&path, old_text, new_text, &self.cwd).await
            }
            _ => anyhow::bail!("Unknown core tool: {tool_name}"),
        }
    }
}
