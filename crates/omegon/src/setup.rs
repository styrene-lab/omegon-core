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
    /// Snapshot of lifecycle + memory state at startup for TUI pre-population.
    pub(crate) startup_snapshot: StartupSnapshot,
}

/// Pre-computed state gathered during setup for TUI initial display.
pub(crate) struct StartupSnapshot {
    pub total_facts: usize,
    pub lifecycle: LifecycleSnapshot,
}

/// Snapshot of design-tree + openspec state, extracted before boxing the provider.
pub(crate) struct LifecycleSnapshot {
    pub focused_node: Option<crate::tui::dashboard::FocusedNodeSummary>,
    pub active_changes: Vec<crate::tui::dashboard::ChangeSummary>,
}

impl LifecycleSnapshot {
    fn from_provider(lp: &lifecycle::context::LifecycleContextProvider) -> Self {
        let focused_node = lp.focused_node_id().and_then(|id| {
            lp.get_node(id).map(|n| {
                let sections = lifecycle::design::read_node_sections(n);
                crate::tui::dashboard::FocusedNodeSummary {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    status: n.status,
                    open_questions: n.open_questions.len(),
                    decisions: sections.map(|s| s.decisions.len()).unwrap_or(0),
                }
            })
        });

        let active_changes: Vec<_> = lp.changes().iter()
            .filter(|c| !matches!(c.stage, lifecycle::types::ChangeStage::Archived))
            .map(|c| crate::tui::dashboard::ChangeSummary {
                name: c.name.clone(),
                stage: c.stage,
                done_tasks: c.done_tasks,
                total_tasks: c.total_tasks,
            })
            .collect();

        Self { focused_node, active_changes }
    }
}

impl AgentSetup {
    /// Initialize tools, memory, lifecycle context, and conversation.
    pub async fn new(cwd: &Path, resume: Option<Option<&str>>) -> anyhow::Result<Self> {
        let cwd = std::fs::canonicalize(cwd)?;
        let is_child = std::env::var("OMEGON_CHILD").is_ok();

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

        // Memory: two access patterns with separate handles.
        //
        // Tool provider (read+write): registered for parent processes only.
        //   Cleave children (OMEGON_CHILD=1) don't get memory_store/archive/supersede
        //   to avoid SQLITE_BUSY contention across concurrent child processes.
        //
        // Context provider (read-only): registered for ALL processes.
        //   WAL mode supports unlimited concurrent readers safely.
        //   Children still get project facts injected into their system prompt.
        let mut memory_context: Option<Box<dyn omegon_traits::ContextProvider>> = None;
        let mut initial_fact_count: usize = 0;

        if let Ok(backend) = omegon_memory::SqliteBackend::open(&db_path) {
            tracing::info!(mind = %mind, db = %db_path.display(), child = is_child, "memory backend loaded");

            // Snapshot fact count for TUI initial display
            if let Ok(stats) = backend.stats(&mind).await {
                initial_fact_count = stats.active_facts;
                tracing::info!(facts = initial_fact_count, "memory snapshot for TUI");
            }

            if !is_child {
                // Parent: import JSONL if DB is empty, register tool provider
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
                    mind.clone(),
                );
                tools.push(Box::new(provider));
            }

            // Context injection: read-only handle for all processes.
            // SQLite WAL supports concurrent readers across processes safely.
            if let Ok(ctx_backend) = omegon_memory::SqliteBackend::open(&db_path) {
                let ctx_provider = omegon_memory::MemoryProvider::new(
                    ctx_backend,
                    omegon_memory::MarkdownRenderer,
                    mind,
                );
                memory_context = Some(Box::new(ctx_provider));
                tracing::info!("Memory context injection enabled");
            }
        }

        // ─── System prompt + context ────────────────────────────────────
        let tool_defs: Vec<_> = tools.iter().flat_map(|p| p.tools()).collect();
        let base_prompt = prompt::build_base_prompt(&cwd, &tool_defs);
        let lifecycle_provider = lifecycle::context::LifecycleContextProvider::new(&cwd);

        // Snapshot lifecycle state for TUI initial display (before boxing)
        let lifecycle_snapshot = LifecycleSnapshot::from_provider(&lifecycle_provider);

        let mut context_providers: Vec<Box<dyn omegon_traits::ContextProvider>> =
            vec![Box::new(lifecycle_provider)];
        if let Some(mc) = memory_context {
            context_providers.push(mc);
        }
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

        let startup_snapshot = StartupSnapshot {
            total_facts: initial_fact_count,
            lifecycle: lifecycle_snapshot,
        };

        Ok(Self {
            tools,
            context_manager,
            conversation,
            cwd,
            startup_snapshot,
        })
    }

    /// Gather initial state for the TUI so the first frame has real data.
    pub fn initial_tui_state(&self) -> crate::tui::TuiInitialState {
        crate::tui::TuiInitialState {
            total_facts: self.startup_snapshot.total_facts,
            focused_node: self.startup_snapshot.lifecycle.focused_node.clone(),
            active_changes: self.startup_snapshot.lifecycle.active_changes.clone(),
        }
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
