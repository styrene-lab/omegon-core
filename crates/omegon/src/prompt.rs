//! System prompt assembly for the headless agent.
//!
//! Phase 0: static base prompt + tool definitions + project directives.
//! Phase 0+: ContextManager provides dynamic injection.

use omegon_traits::ToolDefinition;
use std::path::{Path, PathBuf};

/// Build the base system prompt for headless mode.
///
/// Loads project directives (AGENTS.md) from the working directory if present.
pub fn build_base_prompt(cwd: &Path, tools: &[ToolDefinition]) -> String {
    let date = utc_date();
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
- Every non-trivial code change must include tests. Untested code is incomplete.
- Write tests alongside implementation, not as a follow-up. Co-locate test files.
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
    // Resolve the repo root — handles both normal repos and worktrees.
    // In a worktree, .git is a file containing "gitdir: /path/to/main/.git/worktrees/name".
    // We need to find the main repo root where AGENTS.md lives.
    let repo_root = find_repo_root(cwd);

    // Search order: cwd, then walk up to repo root (if different)
    let search_dirs: Vec<&Path> = if let Some(ref root) = repo_root {
        if root != cwd {
            vec![cwd, root.as_path()]
        } else {
            vec![cwd]
        }
    } else {
        vec![cwd]
    };

    for dir in search_dirs {
        let agents_file = dir.join("AGENTS.md");
        if agents_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&agents_file) {
                let trimmed = if content.len() > 4000 {
                    let mut end = 4000;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...\n[truncated at ~4000 bytes]", &content[..end])
                } else {
                    content
                };
                return format!(
                    "\n# Project Directives\n\nFrom `{}`:\n\n{trimmed}\n",
                    agents_file.display()
                );
            }
        }
    }
    String::new()
}

/// Find the git repo root, handling worktrees.
/// In a worktree, `.git` is a file containing `gitdir: <path>`.
/// We follow that to find the main repo's `.git` directory.
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            if git_path.is_file() {
                // Worktree: .git is a file like "gitdir: /main/repo/.git/worktrees/name"
                if let Ok(content) = std::fs::read_to_string(&git_path) {
                    if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                        let gitdir = gitdir.trim();
                        // gitdir points to .git/worktrees/<name>, go up to .git, then up to repo root
                        let gitdir_path = if Path::new(gitdir).is_absolute() {
                            PathBuf::from(gitdir)
                        } else {
                            dir.join(gitdir)
                        };
                        // .git/worktrees/<name> → .git → repo root
                        // .git/worktrees/<name> → .git → repo root
                        if let Some(dot_git) = gitdir_path.parent().and_then(|p| p.parent()) {
                            if let Some(repo) = dot_git.parent() {
                                return Some(repo.to_path_buf());
                            }
                        }
                    }
                }
                // Fallback: treat as repo root
                return Some(dir);
            } else {
                // Normal repo: .git is a directory
                return Some(dir);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn format_tool_list(tools: &[ToolDefinition]) -> String {
    tools
        .iter()
        .map(|t| format!("- {}: {}", t.name, t.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// UTC date as YYYY-MM-DD from the system clock.
/// Hand-rolled to avoid pulling in chrono/time crates for one function.
fn utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    epoch_to_ymd(secs)
}

fn epoch_to_ymd(epoch_secs: u64) -> String {
    let mut days = (epoch_secs / 86400) as i64;
    let mut y = 1970i64;
    loop {
        let ydays = if is_leap(y) { 366 } else { 365 };
        if days < ydays { break; }
        days -= ydays;
        y += 1;
    }
    let leap = is_leap(y);
    let mdays: [i64; 12] = [31, if leap {29} else {28}, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    for (i, &md) in mdays.iter().enumerate() {
        if days < md { m = i; break; }
        days -= md;
    }
    format!("{y}-{:02}-{:02}", m + 1, days + 1)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format() {
        let date = utc_date();
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
