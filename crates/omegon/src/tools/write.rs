//! Write tool — create or overwrite files, auto-creating parent directories.

use anyhow::Result;
use omegon_traits::{ContentBlock, ToolResult};
use std::path::Path;

pub async fn execute(path: &Path, content: &str) -> Result<ToolResult> {
    // Create parent directories if needed
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let created = !path.exists();
    tokio::fs::write(path, content).await?;

    let line_count = content.lines().count();
    let byte_count = content.len();
    let action = if created { "Created" } else { "Wrote" };

    Ok(ToolResult {
        content: vec![ContentBlock::Text {
            text: format!(
                "{action} {path} ({line_count} lines, {byte_count} bytes)",
                path = path.display()
            ),
        }],
        details: serde_json::json!({
            "path": path.display().to_string(),
            "created": created,
            "lines": line_count,
            "bytes": byte_count,
        }),
    })
}
