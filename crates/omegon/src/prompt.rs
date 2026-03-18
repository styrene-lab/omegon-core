//! System prompt assembly for the headless agent.
//!
//! Phase 0: static base prompt + tool definitions + project directives.
//! Phase 0+: ContextManager provides dynamic injection.

use omegon_traits::ToolDefinition;
use std::path::{Path, PathBuf};

/// Build the base system prompt for headless mode.
///
/// Loads project directives (AGENTS.md) from the working directory if present.
/// Build the base system prompt.
///
/// Loads: global AGENTS.md (~/.omegon/), project AGENTS.md, project conventions.
/// Includes rich tool guidelines that shape behavior, not just descriptions.
pub fn build_base_prompt(cwd: &Path, tools: &[ToolDefinition]) -> String {
    let date = utc_date();
    let tool_list = format_tool_list(tools);
    let tool_guidelines = build_tool_guidelines(tools);
    let global_directives = load_global_directives();
    let project_directives = load_project_directives(cwd);
    let project_conventions = detect_project_conventions(cwd);

    format!(
        r#"You are an expert coding assistant. You help by reading files, executing commands, editing code, and writing new files.

Available tools:
{tool_list}

# Tool Usage

{tool_guidelines}

# Behavior

- Be direct. Don't narrate what you're about to do — just do it. Show file paths clearly.
- When you disagree with the user's approach or see a better alternative, say so with your reasoning. Do not simply comply when you believe the request will produce a worse outcome.
- If the user's instructions are ambiguous, ask for clarification rather than guessing. If you're uncertain about a technical detail, say so rather than confabulating.
- Read files before editing. Edit requires exact text matches — if you guess at whitespace or formatting, the edit will fail.
- Edit runs automatic validation (type check, lint). Read the validation result. If it shows errors, fix them before moving on.
- Every non-trivial code change must include tests. Untested code is incomplete.
- Commit your work with descriptive messages when the task is complete. Do NOT push.
- When you complete the task, summarize what you did and what changed.
{global_directives}{project_directives}{project_conventions}
Current date: {date}
Current working directory: {cwd}"#,
        cwd = cwd.display()
    )
}

/// Rich tool guidelines — how to use each tool well, not just what it does.
fn build_tool_guidelines(tools: &[ToolDefinition]) -> String {
    let mut guidelines = Vec::new();

    // Only include guidelines for tools that are actually registered
    let tool_names: std::collections::HashSet<&str> =
        tools.iter().map(|t| t.name.as_str()).collect();

    if tool_names.contains("bash") {
        guidelines.push(
            "**bash**: Execute shell commands. Output is tail-truncated to 2000 lines / 50KB.\n\
             - Use for: running tests, git operations, installing dependencies, grepping across files\n\
             - Don't use for: reading files (use `read`), writing files (use `write`)\n\
             - Set timeout for potentially long commands: builds, test suites, network operations\n\
             - Check exit codes in the result — non-zero means the command failed"
        );
    }

    if tool_names.contains("read") {
        guidelines.push(
            "**read**: Read file contents. Use offset/limit for large files.\n\
             - Always read a file before editing it — you need the exact text to match\n\
             - For large files, use offset and limit to read specific sections\n\
             - When exploring a codebase, read entry points and type definitions first"
        );
    }

    if tool_names.contains("edit") {
        guidelines.push(
            "**edit**: Replace exact text in a file. The oldText must match exactly — every character, every whitespace.\n\
             - Read the file first to get the exact text. Don't guess at indentation or whitespace.\n\
             - If your edit fails with 'Could not find', read the file again — it may have changed.\n\
             - If it fails with 'multiple occurrences', include more surrounding context to make the match unique.\n\
             - Edit runs automatic validation (type check / lint) after every change — read the validation result.\n\
             - Multiple edits in the same turn are applied atomically with rollback on failure."
        );
    }

    if tool_names.contains("write") {
        guidelines.push(
            "**write**: Create or overwrite a file. Creates parent directories automatically.\n\
             - Use for new files only. For existing files, prefer edit (preserves content you don't need to change).\n\
             - Write runs automatic validation after creation."
        );
    }

    if tool_names.contains("change") {
        guidelines.push(
            "**change**: Atomic multi-file edit with validation. Use when editing multiple related files.\n\
             - All edits succeed or all roll back — no partial state.\n\
             - validate: 'standard' (default) runs type checker; 'full' also runs affected tests; 'none' skips."
        );
    }

    if tool_names.contains("speculate_start") {
        guidelines.push(
            "**speculate_start/check/commit/rollback**: Git checkpoint for exploratory changes.\n\
             - Use when trying a risky approach. Start → make changes → check → commit or rollback.\n\
             - Only one speculation can be active at a time."
        );
    }

    if tool_names.contains("memory_query") || tool_names.contains("memory_recall") {
        guidelines.push(
            "**memory**: Project memory persists across sessions.\n\
             - Use memory_recall(query) for targeted semantic search — more efficient than memory_query.\n\
             - Store conclusions, not investigation steps. Current state, not transitions.\n\
             - Before storing, check if an existing fact covers it — use memory_supersede to update."
        );
    }

    if tool_names.contains("web_search") {
        guidelines.push(
            "**web_search**: Search the web via Brave, Tavily, or Serper.\n\
             - Use 'compare' mode for research requiring cross-source verification.\n\
             - Use 'quick' mode for simple lookups."
        );
    }

    guidelines.join("\n\n")
}

/// Load global operator directives from ~/.omegon/AGENTS.md
fn load_global_directives() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let global_agents = home.join(".omegon/AGENTS.md");

    if let Ok(content) = std::fs::read_to_string(&global_agents) {
        let trimmed = truncate_directive(&content, 3000);
        format!("\n# Operator Directives\n\n{trimmed}\n")
    } else {
        String::new()
    }
}

/// Detect project conventions by scanning for config files.
fn detect_project_conventions(cwd: &Path) -> String {
    let mut conventions = Vec::new();
    let repo_root = find_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());

    // Rust
    if repo_root.join("Cargo.toml").exists() {
        conventions.push("- Rust project: use `cargo check` for type checking, `cargo clippy` for lints, `cargo test` for tests");
        if repo_root.join("Cargo.lock").exists() {
            conventions.push("- Cargo.lock is committed — this is an application, not a library");
        }
    }

    // TypeScript / JavaScript
    if repo_root.join("tsconfig.json").exists() {
        conventions.push("- TypeScript project: use `npx tsc --noEmit` for type checking");
    }
    if repo_root.join("package.json").exists() {
        // Check for test runner
        if repo_root.join("vitest.config.ts").exists()
            || repo_root.join("vitest.config.js").exists()
        {
            conventions.push("- Vitest for testing: `npx vitest run`");
        } else if repo_root.join("jest.config.ts").exists()
            || repo_root.join("jest.config.js").exists()
        {
            conventions.push("- Jest for testing: `npx jest`");
        }
    }

    // Python
    if repo_root.join("pyproject.toml").exists() {
        conventions.push("- Python project: use `ruff check` for linting, `pytest` for tests");
    }

    // Go
    if repo_root.join("go.mod").exists() {
        conventions.push("- Go project: use `go vet` for checking, `go test ./...` for tests");
    }

    // Git conventions
    if repo_root.join(".gitignore").exists() {
        conventions.push("- .gitignore present — respect it when creating files");
    }

    if conventions.is_empty() {
        String::new()
    } else {
        format!(
            "\n# Project Conventions\n\n{}\n",
            conventions.join("\n")
        )
    }
}

/// Truncate a directive string to a byte budget, breaking at a line boundary.
fn truncate_directive(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(nl) = content[..end].rfind('\n') {
        format!("{}\n[truncated]", &content[..nl])
    } else {
        format!("{}…", &content[..end])
    }
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
        if agents_file.exists()
            && let Ok(content) = std::fs::read_to_string(&agents_file) {
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
                if let Ok(content) = std::fs::read_to_string(&git_path)
                    && let Some(gitdir) = content.strip_prefix("gitdir: ") {
                        let gitdir = gitdir.trim();
                        // gitdir points to .git/worktrees/<name>, go up to .git, then up to repo root
                        let gitdir_path = if Path::new(gitdir).is_absolute() {
                            PathBuf::from(gitdir)
                        } else {
                            dir.join(gitdir)
                        };
                        // .git/worktrees/<name> → .git → repo root
                        // .git/worktrees/<name> → .git → repo root
                        if let Some(dot_git) = gitdir_path.parent().and_then(|p| p.parent())
                            && let Some(repo) = dot_git.parent() {
                                return Some(repo.to_path_buf());
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
        assert!(prompt.contains("Commit your work"), "should instruct to commit");
        assert!(prompt.contains("Do NOT push"), "should instruct not to push");
    }

    #[test]
    fn load_directives_returns_empty_for_missing() {
        let directives = load_project_directives(Path::new("/tmp/nonexistent"));
        assert!(directives.is_empty());
    }
}
