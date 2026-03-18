//! Agent setup — shared initialization for headless and interactive modes.
//!
//! Extracts common setup (tools, memory, lifecycle, context) into a reusable
//! struct to avoid duplication between run_agent_command and run_interactive_command.

use std::path::{Path, PathBuf};

use omegon_memory::MemoryBackend as _; // bring trait methods into scope

use crate::context::ContextManager;
use crate::conversation::ConversationState;
use crate::lifecycle;
use crate::prompt;
use crate::session;
use crate::tools;

/// Everything needed to run an agent loop — tools, context, conversation.
pub struct AgentSetup {
    pub tools: Vec<Box<dyn omegon_traits::ToolProvider>>,
    pub context_manager: ContextManager,
    pub conversation: ConversationState,
    pub cwd: PathBuf,
}

impl AgentSetup {
    /// Initialize tools, memory, lifecycle context, and conversation.
    pub async fn new(cwd: &Path, resume: Option<Option<&str>>) -> anyhow::Result<Self> {
        let cwd = std::fs::canonicalize(cwd)?;

        // ─── Tools ──────────────────────────────────────────────────────
        let core_tools = tools::CoreTools::new(cwd.clone());
        let mut tools: Vec<Box<dyn omegon_traits::ToolProvider>> = vec![Box::new(core_tools)];
        tools.push(Box::new(tools::web_search::WebSearchProvider::new()));
        tools.push(Box::new(tools::local_inference::LocalInferenceProvider::new()));
        tools.push(Box::new(tools::view::ViewProvider::new(cwd.clone())));
        tools.push(Box::new(tools::render::RenderProvider::new()));

        // ─── Memory ─────────────────────────────────────────────────────
        let mind = "default".to_string();
        let project_root = find_project_root(&cwd);
        let memory_dir = project_root.join(".pi").join("memory");
        let _ = std::fs::create_dir_all(&memory_dir);
        let db_path = memory_dir.join("facts.db");
        let jsonl_path = memory_dir.join("facts.jsonl");

        if let Ok(backend) = omegon_memory::SqliteBackend::open(&db_path) {
            tracing::info!(mind = %mind, db = %db_path.display(), "memory backend loaded");

            let stats = backend.stats(&mind).await.ok();
            if stats.as_ref().is_none_or(|s| s.active_facts == 0)
                && jsonl_path.exists()
                && let Ok(jsonl) = std::fs::read_to_string(&jsonl_path)
            {
                match backend.import_jsonl(&jsonl).await {
                    Ok(import) => tracing::info!(imported = import.imported, "imported facts.jsonl"),
                    Err(e) => tracing::warn!("JSONL import failed: {e}"),
                }
            }

            let provider = omegon_memory::MemoryProvider::new(
                backend,
                omegon_memory::MarkdownRenderer,
                mind,
            );
            tools.push(Box::new(provider));
        }

        // ─── System prompt + context ────────────────────────────────────
        let tool_defs: Vec<_> = tools.iter().flat_map(|p| p.tools()).collect();
        let base_prompt = prompt::build_base_prompt(&cwd, &tool_defs);
        let lifecycle_provider = lifecycle::context::LifecycleContextProvider::new(&cwd);
        let context_providers: Vec<Box<dyn omegon_traits::ContextProvider>> =
            vec![Box::new(lifecycle_provider)];
        let context_manager = ContextManager::new(base_prompt, context_providers);

        // ─── Conversation ───────────────────────────────────────────────
        let conversation = if let Some(resume_arg) = resume {
            let resume_id = resume_arg;
            match session::find_session(&cwd, resume_id) {
                Some(path) => {
                    tracing::info!(path = %path.display(), "Resuming session");
                    ConversationState::load_session(&path)?
                }
                None => {
                    if resume_id.is_some() {
                        tracing::warn!("No matching session found — starting fresh");
                    }
                    ConversationState::new()
                }
            }
        } else {
            ConversationState::new()
        };

        Ok(Self {
            tools,
            context_manager,
            conversation,
            cwd,
        })
    }
}

/// Find the project root by walking up from cwd looking for .git.
/// For cleave worktrees (.git is a file pointing to the main repo),
/// follows the gitdir to the real repo root.
/// Falls back to cwd if no .git found.
pub fn find_project_root(cwd: &Path) -> PathBuf {
    let mut dir = cwd.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.is_dir() {
            return dir;
        }
        if git_path.is_file() {
            // Worktree: .git file contains "gitdir: /main/repo/.git/worktrees/name"
            if let Ok(content) = std::fs::read_to_string(&git_path)
                && let Some(gitdir) = content.strip_prefix("gitdir: ")
            {
                let gitdir = gitdir.trim();
                let gitdir_path = if Path::new(gitdir).is_absolute() {
                    PathBuf::from(gitdir)
                } else {
                    dir.join(gitdir)
                };
                // .git/worktrees/<name> → .git → repo root
                if let Some(repo) = gitdir_path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                {
                    return repo.to_path_buf();
                }
            }
            return dir; // fallback
        }
        if !dir.pop() {
            break;
        }
    }
    cwd.to_path_buf()
}
