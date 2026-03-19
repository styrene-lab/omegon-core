//! Agent setup — shared initialization for headless and interactive modes.
//!
//! Builds the EventBus with all features registered, plus the ContextManager
//! and ConversationState needed for the agent loop.

use std::path::{Path, PathBuf};

use omegon_memory::MemoryBackend as _; // bring trait methods into scope

use crate::bus::EventBus;
use crate::context::ContextManager;
use crate::conversation::ConversationState;
use crate::features;
use crate::lifecycle;
use crate::prompt;
use crate::session;
use crate::tools;

/// Everything needed to run an agent loop.
pub struct AgentSetup {
    /// The event bus — owns all features. The loop dispatches tools and
    /// emits events through the bus.
    pub bus: EventBus,
    pub context_manager: ContextManager,
    pub conversation: ConversationState,
    pub cwd: PathBuf,
    /// Snapshot of lifecycle + memory state at startup for TUI pre-population.
    pub(crate) startup_snapshot: StartupSnapshot,
    /// Shared handles for live dashboard updates.
    pub dashboard_handles: crate::tui::dashboard::DashboardHandles,
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
    fn from_lifecycle_feature(lf: &features::lifecycle::LifecycleFeature) -> Self {
        let lp = lf.provider();
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

        let active_changes: Vec<_> = lp
            .changes()
            .iter()
            .filter(|c| !matches!(c.stage, lifecycle::types::ChangeStage::Archived))
            .map(|c| crate::tui::dashboard::ChangeSummary {
                name: c.name.clone(),
                stage: c.stage,
                done_tasks: c.done_tasks,
                total_tasks: c.total_tasks,
            })
            .collect();

        Self {
            focused_node,
            active_changes,
        }
    }
}

impl AgentSetup {
    /// Initialize the event bus, tools, memory, lifecycle context, and conversation.
    pub async fn new(
        cwd: &Path,
        resume: Option<Option<&str>>,
        settings: Option<crate::settings::SharedSettings>,
    ) -> anyhow::Result<Self> {
        let cwd = std::fs::canonicalize(cwd)?;
        let is_child = std::env::var("OMEGON_CHILD").is_ok();

        let mut bus = EventBus::new();

        // ─── Core tools (bash, read, write, edit, change, speculate) ────
        let core_tools = tools::CoreTools::new(cwd.clone());
        bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
            "core-tools",
            Box::new(core_tools),
        )));

        // ─── Feature tool providers ─────────────────────────────────────
        bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
            "web-search",
            Box::new(tools::web_search::WebSearchProvider::new()),
        )));
        bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
            "local-inference",
            Box::new(tools::local_inference::LocalInferenceProvider::new()),
        )));
        bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
            "view",
            Box::new(tools::view::ViewProvider::new(cwd.clone())),
        )));
        bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
            "render",
            Box::new(tools::render::RenderProvider::new()),
        )));

        // ─── Memory ─────────────────────────────────────────────────────
        let mind = "default".to_string();
        let project_root = find_project_root(&cwd);
        let memory_dir = project_root.join(".pi").join("memory");
        let _ = std::fs::create_dir_all(&memory_dir);
        let db_path = memory_dir.join("facts.db");
        let jsonl_path = memory_dir.join("facts.jsonl");

        let mut initial_fact_count: usize = 0;

        if let Ok(backend) = omegon_memory::SqliteBackend::open(&db_path) {
            tracing::info!(mind = %mind, db = %db_path.display(), child = is_child, "memory backend loaded");

            if let Ok(stats) = backend.stats(&mind).await {
                initial_fact_count = stats.active_facts;
                tracing::info!(facts = initial_fact_count, "memory snapshot for TUI");
            }

            if !is_child {
                let stats = backend.stats(&mind).await.ok();
                if stats.as_ref().is_none_or(|s| s.active_facts == 0)
                    && jsonl_path.exists()
                    && let Ok(jsonl) = std::fs::read_to_string(&jsonl_path)
                {
                    match backend.import_jsonl(&jsonl).await {
                        Ok(import) => {
                            tracing::info!(imported = import.imported, "imported facts.jsonl")
                        }
                        Err(e) => tracing::warn!("JSONL import failed: {e}"),
                    }
                }

                let provider = omegon_memory::MemoryProvider::new(
                    backend,
                    omegon_memory::MarkdownRenderer,
                    mind.clone(),
                );
                bus.register(Box::new(features::legacy_bridge::LegacyToolFeature::new(
                    "memory",
                    Box::new(provider),
                )));
            }

            // Context injection: read-only handle for all processes
            if let Ok(ctx_backend) = omegon_memory::SqliteBackend::open(&db_path) {
                let ctx_provider = omegon_memory::MemoryProvider::new(
                    ctx_backend,
                    omegon_memory::MarkdownRenderer,
                    mind,
                );
                bus.register(Box::new(
                    features::legacy_bridge::LegacyContextFeature::new(
                        "memory-context",
                        Box::new(ctx_provider),
                    ),
                ));
            }
        }

        // ─── Lifecycle (design-tree + openspec) ──────────────────────────
        let lifecycle_feature = features::lifecycle::LifecycleFeature::new(&cwd);
        let lifecycle_snapshot = LifecycleSnapshot::from_lifecycle_feature(&lifecycle_feature);
        let lifecycle_handle = lifecycle_feature.shared_provider();
        bus.register(Box::new(lifecycle_feature));

        // ─── Cleave (decomposition + dispatch) ─────────────────────────
        let cleave_feature = features::cleave::CleaveFeature::new(&cwd);
        let cleave_handle = cleave_feature.shared_progress();
        bus.register(Box::new(cleave_feature));

        // ─── Session log (context injection) ────────────────────────────
        bus.register(Box::new(features::session_log::SessionLog::new(&cwd)));

        // ─── Model budget (tier switching + thinking) ───────────────────
        if let Some(ref settings) = settings {
            bus.register(Box::new(features::model_budget::ModelBudget::new(settings.clone())));
        }

        // ─── Native features ────────────────────────────────────────────
        bus.register(Box::new(features::auto_compact::AutoCompact::new()));
        bus.register(Box::new(features::terminal_title::TerminalTitle::new(
            &cwd.to_string_lossy(),
        )));
        bus.register(Box::new(features::version_check::VersionCheck::new(
            env!("CARGO_PKG_VERSION"),
        )));

        // ─── Finalize bus (caches tool/command definitions) ─────────────
        bus.finalize();

        // ─── System prompt + context ────────────────────────────────────
        // Build the base prompt from bus tool definitions (not the old tools vec)
        let tool_defs = bus.tool_definitions();
        let base_prompt = prompt::build_base_prompt(&cwd, &tool_defs);

        // Context providers: the bus collects context from features, but we
        // still need the ContextManager for the injection pipeline (TTL decay,
        // budget management, priority sorting). Pass no standalone providers —
        // the bus will provide context via collect_context().
        let context_manager = ContextManager::new(base_prompt, vec![]);

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
            bus,
            context_manager,
            conversation,
            cwd,
            startup_snapshot,
            dashboard_handles: crate::tui::dashboard::DashboardHandles {
                lifecycle: Some(lifecycle_handle),
                cleave: Some(cleave_handle),
                session: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::tui::dashboard::SharedSessionStats::default(),
                )),
            },
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
pub fn find_project_root(cwd: &Path) -> PathBuf {
    let mut dir = cwd.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.is_dir() {
            return dir;
        }
        if git_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_path)
                && let Some(gitdir) = content.strip_prefix("gitdir: ")
            {
                let gitdir = gitdir.trim();
                let gitdir_path = if Path::new(gitdir).is_absolute() {
                    PathBuf::from(gitdir)
                } else {
                    dir.join(gitdir)
                };
                if let Some(repo) = gitdir_path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                {
                    return repo.to_path_buf();
                }
            }
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    cwd.to_path_buf()
}
