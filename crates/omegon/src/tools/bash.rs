//! Bash tool — execute shell commands with output capture.

use anyhow::Result;
use omegon_traits::{ContentBlock, ToolResult};
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_OUTPUT_LINES: usize = 2000;

pub async fn execute(
    command: &str,
    cwd: &Path,
    timeout_secs: Option<u64>,
    cancel: CancellationToken,
) -> Result<ToolResult> {
    let start = Instant::now();

    let mut cmd = Command::new("bash");
    cmd.args(["-c", command])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn()?;

    let output = tokio::select! {
        result = child.wait_with_output() => result?,
        _ = cancel.cancelled() => {
            anyhow::bail!("Command aborted");
        }
        _ = async {
            if let Some(secs) = timeout_secs {
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            anyhow::bail!("Command timed out after {} seconds", timeout_secs.unwrap());
        }
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    let exit_code = output.status.code().unwrap_or(-1);

    // Combine stdout + stderr
    let mut full_output = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !full_output.is_empty() {
            full_output.push('\n');
        }
        full_output.push_str(&stderr);
    }

    // Tail-truncate if needed
    let truncated = truncate_tail(&full_output);
    let mut text = truncated.content;

    if exit_code != 0 {
        text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
    }

    Ok(ToolResult {
        content: vec![ContentBlock::Text { text }],
        details: serde_json::json!({
            "exitCode": exit_code,
            "durationMs": duration_ms,
            "truncated": truncated.was_truncated,
            "totalLines": truncated.total_lines,
            "totalBytes": truncated.total_bytes,
        }),
    })
}

struct Truncated {
    content: String,
    was_truncated: bool,
    total_lines: usize,
    total_bytes: usize,
}

fn truncate_tail(output: &str) -> Truncated {
    let total_bytes = output.len();
    let lines: Vec<&str> = output.lines().collect();
    let total_lines = lines.len();

    if total_bytes <= MAX_OUTPUT_BYTES && total_lines <= MAX_OUTPUT_LINES {
        return Truncated {
            content: output.to_string(),
            was_truncated: false,
            total_lines,
            total_bytes,
        };
    }

    // Take the last N lines within byte budget
    let mut kept = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().rev() {
        let line_bytes = line.len() + 1; // +1 for newline
        if bytes + line_bytes > MAX_OUTPUT_BYTES || kept.len() >= MAX_OUTPUT_LINES {
            break;
        }
        kept.push(*line);
        bytes += line_bytes;
    }
    kept.reverse();

    let content = kept.join("\n");
    Truncated {
        content,
        was_truncated: true,
        total_lines,
        total_bytes,
    }
}
