//! Project context discovery for cleave task file enrichment.
//!
//! Extracts dependency versions, test conventions, submodule info,
//! and file signatures from the project to give children the context
//! they need to write correct code on the first attempt.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Discovered project context for a child's scope.
#[derive(Debug, Default)]
pub struct ChildContext {
    /// Submodule paths detected in the repo.
    pub submodules: Vec<String>,
    /// Dependency version snippets relevant to the child's scope.
    pub dependency_snippets: Vec<String>,
    /// Example test code from the same crate/package.
    pub test_example: Option<String>,
    /// Finalization instructions (submodule-aware if applicable).
    pub finalization: String,
}

/// Build context for a child given its scope and the repo path.
pub fn discover_child_context(
    repo_path: &Path,
    scope: &[String],
) -> ChildContext {
    let submodules = detect_submodule_names(repo_path);
    let dependency_snippets = extract_dependency_versions(repo_path, scope);
    let test_example = sample_test_convention(repo_path, scope);
    let finalization = build_finalization_section(&submodules);

    ChildContext {
        submodules,
        dependency_snippets,
        test_example,
        finalization,
    }
}

/// Format the child context as markdown sections for the task file.
pub fn format_context_sections(ctx: &ChildContext) -> String {
    let mut sections = String::new();

    if !ctx.submodules.is_empty() {
        sections.push_str("## Repository Structure\n\n");
        sections.push_str("This repo uses git submodules. Your scope files live inside a submodule.\n");
        sections.push_str("Submodules: ");
        sections.push_str(&ctx.submodules.join(", "));
        sections.push_str("\n\n");
        sections.push_str("**Important**: When you modify files inside a submodule, you must commit\n");
        sections.push_str("inside the submodule first, then update the pointer in the parent repo.\n");
        sections.push_str("See the Finalization section below for exact steps.\n\n");
    }

    if !ctx.dependency_snippets.is_empty() {
        sections.push_str("## Dependency Versions\n\n");
        sections.push_str("Use these exact versions — do not rely on training data for API shapes:\n\n");
        for snippet in &ctx.dependency_snippets {
            sections.push_str("```toml\n");
            sections.push_str(snippet);
            sections.push_str("\n```\n\n");
        }
    }

    if let Some(ref example) = ctx.test_example {
        sections.push_str("## Test Convention\n\n");
        sections.push_str("Follow this pattern from an existing test in the same crate:\n\n");
        sections.push_str("```rust\n");
        sections.push_str(example);
        sections.push_str("\n```\n\n");
    }

    // Note: finalization is NOT included here — the orchestrator's
    // task file template places it after the Contract section.
    sections
}

/// Detect submodule names from `git submodule status`.
/// Delegates to worktree::detect_submodules to avoid duplication.
fn detect_submodule_names(repo_path: &Path) -> Vec<String> {
    super::worktree::detect_submodules(repo_path)
        .into_iter()
        .map(|(name, _path)| name)
        .collect()
}

/// Extract dependency version sections from Cargo.toml files relevant to scope.
fn extract_dependency_versions(repo_path: &Path, scope: &[String]) -> Vec<String> {
    let mut snippets = Vec::new();

    // Find unique Cargo.toml paths from scope entries
    let mut cargo_paths: Vec<PathBuf> = Vec::new();
    for s in scope {
        let full = repo_path.join(s);
        // Walk up from scope path to find nearest Cargo.toml
        let mut dir = if full.is_file() || !full.exists() {
            full.parent().map(|p| p.to_path_buf())
        } else {
            Some(full)
        };
        while let Some(d) = dir {
            let cargo = d.join("Cargo.toml");
            if cargo.exists() && !cargo_paths.contains(&cargo) {
                cargo_paths.push(cargo);
                break;
            }
            if d == repo_path {
                break;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    for cargo_path in &cargo_paths {
        if let Ok(content) = std::fs::read_to_string(cargo_path) {
            let relative = cargo_path.strip_prefix(repo_path).unwrap_or(cargo_path);
            let mut snippet = format!("# {}\n", relative.display());
            let mut in_deps = false;
            let mut lines_added = 0;

            for line in content.lines() {
                if line.starts_with("[dependencies]")
                    || line.starts_with("[dev-dependencies]")
                    || line.starts_with("[build-dependencies]")
                {
                    if lines_added > 0 {
                        snippet.push('\n');
                    }
                    snippet.push_str(line);
                    snippet.push('\n');
                    in_deps = true;
                    lines_added += 1;
                } else if line.starts_with('[') {
                    in_deps = false;
                } else if in_deps && !line.trim().is_empty() {
                    snippet.push_str(line);
                    snippet.push('\n');
                    lines_added += 1;
                }
            }

            if lines_added > 1 {
                // More than just the section header
                snippets.push(snippet);
            }
        }
    }

    // Also check package.json
    for s in scope {
        let full = repo_path.join(s);
        let mut dir = if full.is_file() || !full.exists() {
            full.parent().map(|p| p.to_path_buf())
        } else {
            Some(full)
        };
        while let Some(d) = dir {
            let pkg = d.join("package.json");
            if pkg.exists() {
                if let Ok(content) = std::fs::read_to_string(&pkg) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                        let relative = pkg.strip_prefix(repo_path).unwrap_or(&pkg);
                        let mut parts = vec![format!("# {}", relative.display())];
                        if let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) {
                            parts.push("[dependencies]".to_string());
                            for (k, ver) in deps {
                                parts.push(format!("{k} = {ver}"));
                            }
                        }
                        if let Some(deps) = v.get("devDependencies").and_then(|d| d.as_object()) {
                            parts.push("[devDependencies]".to_string());
                            for (k, ver) in deps {
                                parts.push(format!("{k} = {ver}"));
                            }
                        }
                        if parts.len() > 1 {
                            snippets.push(parts.join("\n"));
                        }
                    }
                }
                break;
            }
            if d == repo_path {
                break;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    snippets
}

/// Sample one existing test from the same crate to show the child the convention.
fn sample_test_convention(repo_path: &Path, scope: &[String]) -> Option<String> {
    // Find the crate/package root from scope
    for s in scope {
        let full = repo_path.join(s);
        let mut dir = if full.is_file() || !full.exists() {
            full.parent().map(|p| p.to_path_buf())
        } else {
            Some(full)
        };

        // Walk up to find src/ directory
        while let Some(d) = dir {
            let src_dir = if d.ends_with("src") {
                d.clone()
            } else if d.join("src").is_dir() {
                d.join("src")
            } else {
                dir = d.parent().map(|p| p.to_path_buf());
                if d == repo_path { break; }
                continue;
            };

            // Find a .rs file with #[test] or #[cfg(test)]
            if let Some(test_sample) = find_test_sample(&src_dir) {
                return Some(test_sample);
            }
            break;
        }
    }
    None
}

/// Find a single test function from a directory of Rust source files.
/// Searches recursively up to 3 levels deep.
fn find_test_sample(src_dir: &Path) -> Option<String> {
    find_test_sample_recursive(src_dir, src_dir, 0)
}

fn find_test_sample_recursive(src_dir: &Path, root: &Path, depth: usize) -> Option<String> {
    if depth > 3 { return None; }
    let entries = std::fs::read_dir(src_dir).ok()?;

    // First pass: check .rs files in this directory
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(sample) = extract_first_test(&content) {
                    let relative = path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy();
                    return Some(format!("// From {relative}\n{sample}"));
                }
            }
        }
    }

    // Second pass: recurse into subdirectories
    for subdir in subdirs {
        if let Some(sample) = find_test_sample_recursive(&subdir, root, depth + 1) {
            return Some(sample);
        }
    }
    None
}

/// Extract the first #[test] or #[tokio::test] function from source code.
/// Returns at most 30 lines to stay within token budget.
///
/// Uses brace counting on code outside string literals and comments
/// to find the function boundary.
fn extract_first_test(source: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut start = None;
    let mut brace_depth: i32 = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if start.is_none()
            && (trimmed == "#[test]" || trimmed == "#[tokio::test]")
        {
            start = Some(i);
            brace_depth = 0;
            continue;
        }

        if let Some(s) = start {
            // Count braces outside string literals and comments
            brace_depth += count_braces_outside_strings(trimmed);

            if brace_depth <= 0 && i > s {
                let end = (i + 1).min(s + 30); // cap at 30 lines
                let extracted: Vec<&str> = lines[s..end].to_vec();
                return Some(extracted.join("\n"));
            }
        }
    }
    None
}

/// Count net braces ({/}) in a line, ignoring those inside string literals
/// and after line comments.
fn count_braces_outside_strings(line: &str) -> i32 {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_raw_string = false;
    let mut prev = '\0';

    for ch in line.chars() {
        // Skip line comments
        if !in_string && !in_raw_string && prev == '/' && ch == '/' {
            // Undo the '/' we might have counted (we didn't — it's not a brace)
            break;
        }

        if !in_raw_string && ch == '"' && prev != '\\' {
            in_string = !in_string;
        }

        // Rough raw string detection: r#" ... "#
        if !in_string && prev == '#' && ch == '"' {
            in_raw_string = true;
        } else if in_raw_string && prev == '"' && ch == '#' {
            in_raw_string = false;
        }

        if !in_string && !in_raw_string {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        prev = ch;
    }
    depth
}

/// Build finalization section with submodule-aware instructions.
fn build_finalization_section(submodules: &[String]) -> String {
    let mut section = String::from("## Finalization (REQUIRED before completion)\n\n");

    section.push_str("You MUST complete these steps before finishing:\n\n");
    section.push_str("1. Run all guardrail checks listed above and fix failures\n");
    section.push_str("2. Ensure all new/modified files are staged with `git add`\n");

    if !submodules.is_empty() {
        section.push_str("3. **Submodule commits** (this repo has submodules):\n");
        for sub in submodules {
            section.push_str(&format!(
                "   - `cd {sub} && git add -A && git commit -m \"feat(<your-label>): <summary>\" && cd ..`\n"
            ));
        }
        section.push_str(&format!(
            "   - Then stage the pointer: `git add {} && git commit -m \"chore: update submodule\"`\n",
            submodules.join(" ")
        ));
        section.push_str("4. Verify clean state: `git status` should show nothing to commit\n");
        section.push_str("5. Update the Result section below with status=COMPLETED\n");
    } else {
        section.push_str("3. Commit with a clear message: `git commit -m \"feat(<label>): <summary>\"`\n");
        section.push_str("4. Verify clean state: `git status` should show nothing to commit\n");
        section.push_str("5. Update the Result section below with status=COMPLETED\n");
    }

    section.push_str("\n> ⚠️ Uncommitted work will be lost. The orchestrator merges from your branch's commits.\n");

    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_first_test_finds_sync_test() {
        let source = r#"
fn helper() -> u32 { 42 }

#[test]
fn test_helper() {
    assert_eq!(helper(), 42);
}

#[test]
fn test_other() {
    assert!(true);
}
"#;
        let result = extract_first_test(source).unwrap();
        assert!(result.contains("#[test]"));
        assert!(result.contains("test_helper"));
        assert!(!result.contains("test_other")); // Only first test
    }

    #[test]
    fn extract_first_test_finds_async_test() {
        let source = r#"
#[tokio::test]
async fn test_async() {
    let x = tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(true);
}
"#;
        let result = extract_first_test(source).unwrap();
        assert!(result.contains("#[tokio::test]"));
        assert!(result.contains("test_async"));
    }

    #[test]
    fn extract_first_test_caps_at_30_lines() {
        let mut source = String::from("#[test]\nfn long_test() {\n");
        for i in 0..50 {
            source.push_str(&format!("    let x{i} = {i};\n"));
        }
        source.push_str("}\n");
        let result = extract_first_test(&source).unwrap();
        assert!(result.lines().count() <= 30);
    }

    #[test]
    fn extract_first_test_none_when_no_tests() {
        let source = "fn main() { println!(\"hello\"); }";
        assert!(extract_first_test(source).is_none());
    }

    #[test]
    fn finalization_without_submodules() {
        let section = build_finalization_section(&[]);
        assert!(section.contains("Finalization"));
        assert!(!section.contains("Submodule"));
        assert!(section.contains("git commit"));
    }

    #[test]
    fn finalization_with_submodules() {
        let section = build_finalization_section(&["core".to_string()]);
        assert!(section.contains("Submodule commits"));
        assert!(section.contains("cd core"));
        assert!(section.contains("git add core"));
    }

    #[test]
    fn dependency_extraction_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(&cargo, r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = "1"
reqwest = { version = "0.12", features = ["json"] }

[dev-dependencies]
mockito = "1"
tokio-test = "0.4"

[build-dependencies]
cc = "1"
"#).unwrap();

        let snippets = extract_dependency_versions(
            dir.path(),
            &["src/lib.rs".to_string()],
        );
        assert!(!snippets.is_empty());
        let snippet = &snippets[0];
        assert!(snippet.contains("serde"));
        assert!(snippet.contains("mockito"));
        assert!(snippet.contains("[dev-dependencies]"));
    }
}
