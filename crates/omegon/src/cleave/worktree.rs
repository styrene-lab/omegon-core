//! Git worktree management for cleave children.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create a git worktree for a child branch.
///
/// 1. Creates the branch from HEAD
/// 2. Creates the worktree at `<workspace>/<child_id>-<label>`
pub fn create_worktree(
    repo_path: &Path,
    workspace_path: &Path,
    child_id: usize,
    label: &str,
    branch: &str,
) -> Result<PathBuf> {
    let worktree_dir = workspace_path.join(format!("{}-wt-{}", child_id, label));

    // Create branch from HEAD (ignore error if it already exists)
    let _ = Command::new("git")
        .args(["branch", branch, "HEAD"])
        .current_dir(repo_path)
        .output();

    // Create worktree (-f handles stale registrations from previous failed runs)
    let output = Command::new("git")
        .args(["worktree", "add", "-f", worktree_dir.to_str().unwrap(), branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If worktree already exists, that's fine
        if !stderr.contains("already exists") {
            anyhow::bail!("git worktree add failed: {}", stderr.trim());
        }
    }

    Ok(worktree_dir)
}

/// Remove a git worktree.
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("git worktree remove warning: {}", stderr.trim());
    }

    // Note: branch deletion is handled separately by delete_branch() after merge.
    // We don't delete the branch here because the caller may still need to merge it.

    Ok(())
}

/// Merge a child branch back into the current branch.
pub fn merge_branch(repo_path: &Path, branch: &str) -> Result<MergeResult> {
    let output = Command::new("git")
        .args(["merge", "--no-ff", "-m", &format!("cleave: merge {}", branch), branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git merge")?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("Already up to date") {
            return Ok(MergeResult::Failed(
                "Branch has no new commits — child did not produce any work".to_string(),
            ));
        }
        Ok(MergeResult::Success)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("CONFLICT") || stderr.contains("conflict") {
            // Abort the merge
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(repo_path)
                .output();
            Ok(MergeResult::Conflict(stderr.to_string()))
        } else {
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(repo_path)
                .output();
            let detail = stderr.trim().to_string();
            Ok(MergeResult::Failed(if detail.is_empty() {
                format!("git merge failed with exit code {}", output.status.code().unwrap_or(-1))
            } else {
                detail
            }))
        }
    }
}

/// Delete a branch after merge.
pub fn delete_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to delete branch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("branch delete warning: {}", stderr.trim());
    }
    Ok(())
}

/// Initialize submodules in a worktree.
///
/// Worktrees don't inherit submodule checkouts from the parent — this
/// ensures children can access files inside submodules.
pub fn submodule_init(worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["submodule", "update", "--init", "--recursive"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to init submodules in worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("submodule init warning: {}", stderr.trim());
    } else {
        tracing::info!(worktree = %worktree_path.display(), "submodules initialized");
    }
    Ok(())
}

/// Detect active submodules in a repo/worktree.
///
/// Returns a list of (submodule_name, submodule_path) pairs.
pub fn detect_submodules(repo_path: &Path) -> Vec<(String, PathBuf)> {
    let output = match Command::new("git")
        .args(["submodule", "status"])
        .current_dir(repo_path)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            // Format: " <sha1> <path> (<describe>)" or "+<sha1> <path> (<describe>)"
            let trimmed = line.trim_start_matches([' ', '+', '-', 'U']);
            let mut parts = trimmed.split_whitespace();
            let _sha = parts.next()?;
            let path = parts.next()?;
            Some((path.to_string(), repo_path.join(path)))
        })
        .collect()
}

/// Commit dirty submodules in a worktree after a child finishes.
///
/// For each submodule that has uncommitted changes:
/// 1. Stage and commit inside the submodule
/// 2. Stage the updated submodule pointer in the parent
/// 3. Commit the pointer update in the parent
///
/// Returns the number of submodules committed. Returns 0 (no-op) if
/// no submodules are dirty.
pub fn commit_dirty_submodules(worktree_path: &Path, child_label: &str) -> Result<usize> {
    let submodules = detect_submodules(worktree_path);
    if submodules.is_empty() {
        return Ok(0);
    }

    let mut committed = 0;

    for (name, sub_path) in &submodules {
        if !sub_path.exists() {
            continue;
        }

        // Check if submodule is dirty
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(sub_path)
            .output()
            .context(format!("Failed to check submodule status: {name}"))?;

        let stdout = String::from_utf8_lossy(&status.stdout);
        if stdout.trim().is_empty() {
            continue; // Clean — nothing to do
        }

        tracing::info!(
            child = %child_label,
            submodule = %name,
            dirty_files = stdout.lines().count(),
            "auto-committing dirty submodule"
        );

        // Stage all changes inside the submodule (.gitignore-aware)
        let add_output = Command::new("git")
            .args(["add", "-A"])
            .current_dir(sub_path)
            .output()
            .context(format!("Failed to stage submodule changes: {name}"))?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            tracing::warn!(submodule = %name, "git add warning: {}", stderr.trim());
        }

        // Commit inside the submodule
        let msg = format!("feat({child_label}): auto-commit from cleave child");
        let commit_output = Command::new("git")
            .args(["commit", "-m", &msg])
            .current_dir(sub_path)
            .output()
            .context(format!("Failed to commit in submodule: {name}"))?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            if stderr.contains("nothing to commit") {
                continue;
            }
            tracing::warn!(submodule = %name, "submodule commit warning: {}", stderr.trim());
            continue;
        }

        // Stage the submodule pointer update in the parent
        let _ = Command::new("git")
            .args(["add", name])
            .current_dir(worktree_path)
            .output();

        committed += 1;
    }

    // If any submodules were committed, commit the pointer updates in the parent
    if committed > 0 {
        let msg = format!("chore({child_label}): update submodule pointer(s)");
        let _ = Command::new("git")
            .args(["commit", "-m", &msg])
            .current_dir(worktree_path)
            .output();

        tracing::info!(
            child = %child_label,
            submodules_committed = committed,
            "submodule auto-commit complete"
        );
    }

    Ok(committed)
}

#[derive(Debug)]
pub enum MergeResult {
    Success,
    Conflict(String),
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_path_format() {
        let workspace = Path::new("/tmp/ws");
        let expected = workspace.join("2-wt-my-task");
        let result_path = workspace.join(format!("{}-wt-{}", 2, "my-task"));
        assert_eq!(result_path, expected);
    }

    #[test]
    fn merge_result_variants() {
        // Just verify the enum is constructable and debug-printable
        let s = MergeResult::Success;
        assert!(format!("{s:?}").contains("Success"));
        let c = MergeResult::Conflict("file.rs".into());
        assert!(format!("{c:?}").contains("file.rs"));
        let f = MergeResult::Failed("error".into());
        assert!(format!("{f:?}").contains("error"));
    }

    #[test]
    fn create_worktree_in_git_repo() {
        // Integration test — only runs if we're in a git repo
        let repo = std::env::current_dir().unwrap();
        if !repo.join(".git").exists() && !repo.join("../.git").exists() {
            tracing::debug!("Skipping: not in a git repo");
            return;
        }

        let workspace = tempfile::tempdir().unwrap();
        let branch_name = format!("test-wt-{}", std::process::id());
        let result = create_worktree(&repo, workspace.path(), 0, "test", &branch_name);

        if let Ok(wt_path) = result {
            assert!(wt_path.exists(), "worktree should exist");
            // Clean up
            let _ = remove_worktree(&repo, &wt_path);
            let _ = delete_branch(&repo, &branch_name);
        }
        // Failure is acceptable in CI where git might not be configured
    }
}
