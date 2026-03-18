//! System prompt assembly for the headless agent.
//!
//! Phase 0: static base prompt + tool definitions + project directives.
//! Phase 0+: ContextManager provides dynamic injection.

use omegon_traits::ToolDefinition;
use std::path::Path;

/// Build the base system prompt for headless mode.
///
/// Loads project directives (AGENTS.md) from the working directory if present.
pub fn build_base_prompt(cwd: &Path, tools: &[ToolDefinition]) -> String {
    let date = chrono_date();
    let tool_list = format_tool_list(tools);
    let project_directives = load_project_directives(cwd);

    format!(
        r#"You are an expert coding assistant operating as a headless agent. You help by reading files, executing commands, editing code, and writing new files.

Available tools:
{tool_list}

Guidelines:
- Use bash for file operations like ls, grep, find
- Use read to examine files before editing
- Use edit for precise changes (old text must match exactly)
- Use write only for new files or complete rewrites
- Be concise in your responses
- Show file paths clearly when working with files
- Always commit your work with clear, descriptive commit messages before finishing
- Do NOT push — only commit locally
- When you complete the task, update any task/result sections in the prompt, then summarize what you did
{project_directives}
Current date: {date}
Current working directory: {cwd}"#,
        cwd = cwd.display()
    )
}

/// Load project directives from AGENTS.md files.
///
/// Checks (in order):
/// 1. `<cwd>/AGENTS.md` — project-level directives
/// 2. Walks up to repo root looking for AGENTS.md
///
/// Returns a formatted section or empty string if no directives found.
fn load_project_directives(cwd: &Path) -> String {
    // Try cwd first, then walk up to find repo root
    let mut dir = cwd.to_path_buf();
    loop {
        let agents_file = dir.join("AGENTS.md");
        if agents_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&agents_file) {
                // Trim to reasonable size — don't blow up the system prompt
                let trimmed = if content.len() > 4000 {
                    format!("{}...\n[truncated at 4000 chars]", &content[..4000])
                } else {
                    content
                };
                return format!(
                    "\n# Project Directives\n\nFrom `{}`:\n\n{trimmed}\n",
                    agents_file.display()
                );
            }
        }
        // Stop at git root or filesystem root
        if dir.join(".git").exists() || !dir.pop() {
            break;
        }
    }
    String::new()
}

fn format_tool_list(tools: &[ToolDefinition]) -> String {
    tools
        .iter()
        .map(|t| format!("- {}: {}", t.name, t.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn chrono_date() -> String {
    // Simple date without chrono dependency — use system time
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Convert epoch seconds to YYYY-MM-DD
    // Using a simple algorithm — correct for 2000-2099
    let days = (now / 86400) as i64;
    let mut y = 1970i64;
    let mut remaining = days;

    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut m = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            m = i;
            break;
        }
        remaining -= md;
    }

    format!("{y}-{:02}-{:02}", m + 1, remaining + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format() {
        let date = chrono_date();
        assert!(date.len() == 10, "date should be YYYY-MM-DD: {date}");
        assert!(date.starts_with("202"), "date should be in 202x: {date}");
    }

    #[test]
    fn base_prompt_includes_tools() {
        let tools = vec![omegon_traits::ToolDefinition {
            name: "test_tool".into(),
            label: "test".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({}),
        }];
        let prompt = build_base_prompt(Path::new("/tmp"), &tools);
        assert!(prompt.contains("test_tool"));
        assert!(prompt.contains("A test tool"));
        assert!(prompt.contains("/tmp"));
    }

    #[test]
    fn base_prompt_includes_commit_instructions() {
        let tools = vec![];
        let prompt = build_base_prompt(Path::new("/tmp"), &tools);
        assert!(prompt.contains("commit your work"), "should instruct to commit");
        assert!(prompt.contains("Do NOT push"), "should instruct not to push");
    }

    #[test]
    fn load_directives_returns_empty_for_missing() {
        let directives = load_project_directives(Path::new("/tmp/nonexistent"));
        assert!(directives.is_empty());
    }
}
