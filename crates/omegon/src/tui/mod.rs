//! Interactive TUI — ratatui-based terminal interface.
//!
//! Minimum viable interactive agent:
//! - Editor: single-line text input with line editing
//! - Conversation: scrollable message display with streaming
//! - Ctrl+C: cancel during execution, exit at editor
//!
//! The TUI runs in a separate tokio task from the agent loop.
//! They communicate via channels:
//!   - user_input_tx → agent loop receives prompts
//!   - AgentEvent broadcast → TUI receives streaming updates

pub mod conversation;
pub mod conv_widget;
pub mod dashboard;
pub mod editor;
pub mod effects;
pub mod footer;
pub mod segments;
pub mod selector;
pub mod spinner;
pub mod splash;
pub mod theme;
pub mod widgets;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use crossterm::event::{EnableMouseCapture, DisableMouseCapture};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use omegon_traits::AgentEvent;

use self::conversation::ConversationView;
use self::dashboard::DashboardState;
use self::footer::FooterData;
use self::editor::Editor;

/// Messages from TUI to the agent coordinator.
#[derive(Debug)]
pub enum TuiCommand {
    /// User submitted a prompt.
    UserPrompt(String),
    /// User wants to quit (double Ctrl+C, or /exit).
    Quit,
    /// Switch the model for the next turn.
    SetModel(String),
    /// Dispatch a bus command from a feature (name, args).
    BusCommand { name: String, args: String },
    /// Trigger manual compaction.
    Compact,
    /// List saved sessions.
    ListSessions,
}

/// Shared cancel token — the TUI writes it on Escape/Ctrl+C,
/// the agent loop checks it. Arc so both tasks can access it.
pub type SharedCancel = std::sync::Arc<std::sync::Mutex<Option<CancellationToken>>>;

/// Application state for the TUI.
pub struct App {
    editor: Editor,
    conversation: ConversationView,
    agent_active: bool,
    should_quit: bool,
    turn: u32,
    tool_calls: u32,
    history: Vec<String>,
    history_idx: Option<usize>,
    dashboard: DashboardState,
    footer_data: FooterData,
    theme: Box<dyn theme::Theme>,
    /// Shared settings — source of truth for model, thinking, etc.
    settings: crate::settings::SharedSettings,
    /// Shared cancel token — Escape/Ctrl+C cancels the active agent turn.
    cancel: SharedCancel,
    /// Timestamp of last Ctrl+C (for double-tap quit detection).
    last_ctrl_c: Option<std::time::Instant>,
    /// Session start time for /stats.
    session_start: std::time::Instant,
    /// Active selector popup (model picker, think level, etc.)
    selector: Option<selector::Selector>,
    /// What the selector is for — determines what happens on confirm.
    selector_kind: Option<SelectorKind>,
    /// Last tool name from ToolStart — used to track memory mutations.
    last_tool_name: Option<String>,
    /// Current spinner verb — rotates on each tool call.
    working_verb: &'static str,
    /// When true, replay the splash animation.
    replay_splash: bool,
    /// Visual effects manager (tachyonfx).
    effects: effects::Effects,
    /// Command definitions from bus features.
    bus_commands: Vec<omegon_traits::CommandDefinition>,
}

#[derive(Debug, Clone, Copy)]
enum SelectorKind {
    Model,
    ThinkingLevel,
}

/// Result of handling a slash command.
enum SlashResult {
    /// Display this text as a system message.
    Display(String),
    /// Command was handled silently (e.g. opened a popup).
    Handled,
    /// Not a recognized command — pass through as user prompt.
    NotACommand,
    /// Quit requested.
    Quit,
}

impl App {
    pub fn new(settings: crate::settings::SharedSettings) -> Self {
        let (model_id, model_provider) = {
            let s = settings.lock().unwrap();
            (s.model.clone(), s.provider().to_string())
        };
        Self {
            editor: Editor::new(),
            conversation: ConversationView::new(),
            agent_active: false,
            should_quit: false,
            turn: 0,
            tool_calls: 0,
            history: Vec::new(),
            history_idx: None,
            dashboard: DashboardState::default(),
            footer_data: FooterData {
                model_id,
                model_provider,
                ..Default::default()
            },
            theme: theme::default_theme(),
            settings,
            cancel: std::sync::Arc::new(std::sync::Mutex::new(None)),
            last_ctrl_c: None,
            session_start: std::time::Instant::now(),
            selector: None,
            selector_kind: None,
            last_tool_name: None,
            working_verb: "Working",
            replay_splash: false,
            effects: effects::Effects::new(),
            bus_commands: Vec::new(),
        }
    }

    fn open_model_selector(&mut self) {
        let current = self.settings().model.clone();
        let mut options: Vec<selector::SelectOption> = Vec::new();

        // Only show providers the user is actually authenticated with
        let anthropic_auth = crate::providers::resolve_api_key_sync("anthropic");
        let openai_auth = crate::providers::resolve_api_key_sync("openai");

        if let Some((_, is_oauth)) = anthropic_auth {
            let auth = if is_oauth { "oauth" } else { "key" };
            options.push(sel_opt("anthropic:claude-sonnet-4-6",          "Sonnet 4.6",  &format!("Anthropic · balanced · 200k · {auth}"), &current));
            options.push(sel_opt("anthropic:claude-opus-4-6",            "Opus 4.6",    &format!("Anthropic · strongest · 200k · {auth}"), &current));
            options.push(sel_opt("anthropic:claude-haiku-4-5-20251001",  "Haiku 4.5",   &format!("Anthropic · fast · cheap · 200k · {auth}"), &current));
        }

        if let Some((_, is_oauth)) = openai_auth {
            let auth = if is_oauth { "oauth" } else { "key" };
            options.push(sel_opt("openai:gpt-5.4",   "GPT-5.4",   &format!("OpenAI · frontier · 1M · {auth}"), &current));
            options.push(sel_opt("openai:o3",         "o3",        &format!("OpenAI · reasoning · 200k · {auth}"), &current));
            options.push(sel_opt("openai:o4-mini",    "o4-mini",   &format!("OpenAI · fast reasoning · 200k · {auth}"), &current));
            options.push(sel_opt("openai:gpt-4.1",    "GPT-4.1",  &format!("OpenAI · coding · 1M · {auth}"), &current));
        }

        if options.is_empty() {
            self.conversation.push_system(
                "No providers authenticated.\n\
                 Run: omegon-agent login anthropic  (Claude subscription)\n\
                 Run: omegon-agent login openai     (ChatGPT subscription)\n\
                 Or:  export ANTHROPIC_API_KEY=...   (API key)"
            );
            return;
        }

        self.selector = Some(selector::Selector::new("Select Model", options));
        self.selector_kind = Some(SelectorKind::Model);
    }

    fn open_thinking_selector(&mut self) {
        let current = self.settings().thinking;
        let options = crate::settings::ThinkingLevel::all().iter().map(|level| {
            selector::SelectOption {
                value: level.as_str().to_string(),
                label: format!("{} {}", level.icon(), level.as_str()),
                description: match level {
                    crate::settings::ThinkingLevel::Off => "no extended thinking".into(),
                    crate::settings::ThinkingLevel::Low => "~5k token budget".into(),
                    crate::settings::ThinkingLevel::Medium => "~10k token budget".into(),
                    crate::settings::ThinkingLevel::High => "~50k token budget".into(),
                },
                active: *level == current,
            }
        }).collect();
        self.selector = Some(selector::Selector::new("Thinking Level", options));
        self.selector_kind = Some(SelectorKind::ThinkingLevel);
    }

    fn confirm_selector(&mut self, tx: &mpsc::Sender<TuiCommand>) -> Option<String> {
        let sel = self.selector.take()?;
        let kind = self.selector_kind.take()?;
        let value = sel.selected_value().to_string();

        match kind {
            SelectorKind::Model => {
                self.update_settings(|s| {
                    s.model = value.clone();
                    s.context_window = crate::settings::Settings::new(&value).context_window;
                });
                let _ = tx.try_send(TuiCommand::SetModel(value.clone()));
                Some(format!("Model → {value}"))
            }
            SelectorKind::ThinkingLevel => {
                if let Some(level) = crate::settings::ThinkingLevel::parse(&value) {
                    self.update_settings(|s| s.thinking = level);
                    Some(format!("Thinking → {} {}", level.icon(), level.as_str()))
                } else {
                    Some(format!("Unknown level: {value}"))
                }
            }
        }
    }

    /// Read a snapshot of current settings (for display).
    fn settings(&self) -> crate::settings::Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Write a setting (for commands like /model, /think).
    fn update_settings<F: FnOnce(&mut crate::settings::Settings)>(&self, f: F) {
        if let Ok(mut s) = self.settings.lock() {
            f(&mut s);
        }
    }

    /// Try to cancel the active agent turn. Returns true if cancelled.
    fn interrupt(&self) -> bool {
        if let Ok(guard) = self.cancel.lock()
            && let Some(ref token) = *guard {
                token.cancel();
                return true;
            }
        false
    }

    /// Update the dashboard with lifecycle context.
    pub fn update_dashboard_from_lifecycle(
        &mut self,
        nodes: &std::collections::HashMap<String, crate::lifecycle::types::DesignNode>,
        changes: &[crate::lifecycle::types::ChangeInfo],
        focused_id: Option<&str>,
    ) {
        self.dashboard.focused_node = focused_id.and_then(|id| {
            nodes.get(id).map(|n| {
                let sections = crate::lifecycle::design::read_node_sections(n);
                dashboard::FocusedNodeSummary {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    status: n.status,
                    open_questions: n.open_questions.len(),
                    decisions: sections.map(|s| s.decisions.len()).unwrap_or(0),
                }
            })
        });
        self.dashboard.active_changes = changes
            .iter()
            .filter(|c| !matches!(c.stage, crate::lifecycle::types::ChangeStage::Archived))
            .map(|c| dashboard::ChangeSummary {
                name: c.name.clone(),
                stage: c.stage,
                done_tasks: c.done_tasks,
                total_tasks: c.total_tasks,
            })
            .collect();
    }

    fn draw(&mut self, frame: &mut Frame) {
        // Update dashboard stats
        self.dashboard.turns = self.turn;
        self.dashboard.tool_calls = self.tool_calls;

        let area = frame.area();

        // ── Horizontal split: main area | dashboard panel ───────────
        // Dashboard appears as a right-side panel when terminal is wide enough.
        let show_dashboard = area.width >= 120
            && (self.dashboard.focused_node.is_some()
                || !self.dashboard.active_changes.is_empty()
                || self.dashboard.cleave.as_ref().is_some_and(|c| c.active || c.total_children > 0));

        let (main_area, dash_area) = if show_dashboard {
            let h = Layout::horizontal([
                Constraint::Min(60),
                Constraint::Length(36),
            ]).split(area);
            (h[0], h[1])
        } else {
            (area, Rect::ZERO)
        };

        // ── Vertical layout in the main area ────────────────────────
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // [0] conversation (all remaining space)
                Constraint::Length(3), // [1] editor (input box)
                Constraint::Length(1), // [2] hint line
                Constraint::Length(5), // [3] footer cards (bordered, at the foot)
            ])
            .split(main_area);

        // Conversation view — segment-based widget.
        let t = &self.theme;
        let (segments, conv_state) = self.conversation.segments_and_state();
        let conv_widget = conv_widget::ConversationWidget::new(segments, t.as_ref());
        frame.render_stateful_widget(conv_widget, chunks[0], conv_state);

        // Dashboard panel (right side)
        if show_dashboard && dash_area.width > 0 {
            self.dashboard.render_themed(dash_area, frame, t.as_ref());
        }

        // Footer — sync from settings + session state (renders at the foot)
        {
            let s = self.settings();
            self.footer_data.model_id = s.model.clone();
            self.footer_data.model_provider = s.provider().to_string();
            self.footer_data.context_window = s.context_window;
            self.footer_data.context_mode = s.context_mode;
        }
        self.footer_data.turn = self.turn;
        self.footer_data.tool_calls = self.tool_calls;
        self.footer_data.compactions = self.dashboard.compactions;
        self.footer_data.render(chunks[3], frame, t.as_ref());

        // Hint line between footer and editor
        {
            let ctx_mode = self.footer_data.context_mode;
            let hint_spans = vec![
                Span::styled("Tab ", Style::default().fg(t.dim())),
                Span::styled("expand card", Style::default().fg(t.dim())),
                Span::styled("  ·  ", Style::default().fg(t.border_dim())),
                Span::styled(format!("{} {}", ctx_mode.icon(), ctx_mode.as_str()), Style::default().fg(t.dim())),
                Span::styled("  ·  ", Style::default().fg(t.border_dim())),
                Span::styled(
                    format!("{} {}", self.settings().thinking.icon(), self.settings().thinking.as_str()),
                    Style::default().fg(t.dim()),
                ),
                Span::styled("  ·  ", Style::default().fg(t.border_dim())),
                Span::styled("Shift+click ", Style::default().fg(t.dim())),
                Span::styled("select text", Style::default().fg(t.dim())),
            ];
            let hint = Paragraph::new(Line::from(hint_spans))
                .style(Style::default().bg(t.card_bg()));
            frame.render_widget(hint, chunks[2]);
        }

        // Editor — shows reverse search prompt when active
        let (editor_title, editor_content) = if let editor::EditorMode::ReverseSearch { ref query, ref match_idx } = *self.editor.mode() {
            let match_text = match_idx
                .and_then(|i| self.history.get(i))
                .map(|s| s.as_str())
                .unwrap_or("");
            (
                Span::styled(
                    format!(" (reverse-i-search)`{query}': "),
                    t.style_warning(),
                ),
                match_text.to_string(),
            )
        } else if self.agent_active {
            (
                Span::styled(
                    format!(" ⟳ {}... ", self.working_verb),
                    t.style_warning(),
                ),
                String::new(),
            )
        } else {
            (Span::styled(" ▸ ", t.style_accent()), String::new())
        };

        let editor_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(t.border()).bg(t.surface_bg()))
            .title(editor_title);

        let display_text = if editor_content.is_empty() {
            self.editor.render_text().to_string()
        } else {
            editor_content
        };
        let editor_widget = Paragraph::new(display_text)
            .style(Style::default().fg(t.fg()).bg(t.surface_bg()))
            .block(editor_block);
        frame.render_widget(editor_widget, chunks[1]);

        // Command palette popup (above editor when typing /)
        if !self.agent_active {
            let matches = self.matching_commands();
            if !matches.is_empty() {
                let palette_height = matches.len().min(8) as u16 + 2; // +2 for borders
                let editor_area = chunks[1];
                let palette_area = Rect {
                    x: editor_area.x,
                    y: editor_area.y.saturating_sub(palette_height),
                    width: editor_area.width.min(50),
                    height: palette_height,
                };

                let items: Vec<Line<'static>> = matches.iter().map(|(name, desc)| {
                    Line::from(vec![
                        Span::styled(format!(" /{name}"), t.style_accent()),
                        Span::styled(format!("  {desc}"), t.style_muted()),
                    ])
                }).collect();

                let palette = Paragraph::new(items)
                    .block(Block::default()
                        .borders(Borders::ALL)
                        .border_style(t.style_border())
                        .title(Span::styled(" commands ", t.style_dim())));

                // Clear the area first (prevents bleed-through)
                frame.render_widget(ratatui::widgets::Clear, palette_area);
                frame.render_widget(palette, palette_area);
            }

            // Position cursor in editor — no left border (Borders::TOP only),
            // so text starts at x=0 within the block's inner area (y+1 for top border).
            let editor_area = chunks[1];
            let cursor_x = editor_area.x + self.editor.cursor_position() as u16;
            let cursor_y = editor_area.y + 1; // +1 for top border
            frame.set_cursor_position(Position::new(
                cursor_x.min(editor_area.right().saturating_sub(1)),
                cursor_y,
            ));
        }

        // Selector popup (overlays everything when active)
        if let Some(ref sel) = self.selector {
            sel.render(area, frame, t.as_ref());
        }

        // ── Post-render effects (tachyonfx) — each zone processed separately ──
        self.effects.process(frame.buffer_mut(), chunks[0], chunks[3], chunks[1]);
    }

    /// Command registry: (name, description, subcommands).
    const COMMANDS: &'static [(&'static str, &'static str, &'static [&'static str])] = &[
        ("help",     "show available commands",              &[]),
        ("model",    "view or switch model",                 &["list"]),
        ("think",    "set thinking level",                   &["off", "low", "medium", "high"]),
        ("stats",    "session telemetry",                    &[]),
        ("compact",  "trigger context compaction",           &[]),
        ("clear",    "clear conversation display",           &[]),
        ("detail",   "toggle tool display (compact/detailed)", &["compact", "detailed"]),
        ("context",  "toggle context window (200k/1M)",       &["200k", "1m"]),
        ("sessions", "list saved sessions",                  &[]),
        ("memory",   "memory stats",                        &[]),
        ("chronos",  "date/time context",                      &["week", "month", "quarter", "relative", "iso", "epoch", "tz", "range", "all"]),
        ("migrate",  "import from other tools",               &["auto", "claude-code", "pi", "codex", "cursor", "aider"]),
        ("splash",   "replay splash animation",              &[]),
        ("exit",     "quit (or double Ctrl+C)",              &[]),
    ];

    /// Handle a slash command.
    fn handle_slash_command(&mut self, text: &str, tx: &mpsc::Sender<TuiCommand>) -> SlashResult {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') { return SlashResult::NotACommand; }
        let rest = &trimmed[1..];
        let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
        let args = args.trim();

        match cmd {
            "help" => {
                let lines: Vec<String> = Self::COMMANDS.iter()
                    .map(|(n, d, subs)| {
                        if subs.is_empty() {
                            format!("  /{n:<12} {d}")
                        } else {
                            format!("  /{n:<12} {d}  [{}]", subs.join("|"))
                        }
                    }).collect();
                SlashResult::Display(format!("Commands:\n{}\n\nType / to browse. Tab completes.", lines.join("\n")))
            }

            "model" => {
                if args.is_empty() {
                    // No args → open interactive selector
                    self.open_model_selector();
                    SlashResult::Handled
                } else {
                    // Direct switch: /model anthropic:claude-opus-4-6
                    self.update_settings(|s| {
                        s.model = args.to_string();
                        s.context_window = crate::settings::Settings::new(args).context_window;
                    });
                    let _ = tx.try_send(TuiCommand::SetModel(args.to_string()));
                    SlashResult::Display(format!("Model → {args}"))
                }
            }

            "think" => {
                if args.is_empty() {
                    // No args → open interactive selector
                    self.open_thinking_selector();
                    SlashResult::Handled
                } else if let Some(level) = crate::settings::ThinkingLevel::parse(args) {
                    self.update_settings(|s| s.thinking = level);
                    SlashResult::Display(format!("Thinking → {} {}", level.icon(), level.as_str()))
                } else {
                    SlashResult::Display(format!("Unknown level: {args}. Options: off, low, medium, high"))
                }
            }

            "stats" => {
                let s = self.settings();
                let elapsed = self.session_start.elapsed();
                let time = if elapsed.as_secs() >= 3600 {
                    format!("{}h{}m", elapsed.as_secs() / 3600, (elapsed.as_secs() % 3600) / 60)
                } else if elapsed.as_secs() >= 60 {
                    format!("{}m{}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
                } else {
                    format!("{}s", elapsed.as_secs())
                };
                SlashResult::Display(format!(
                    "Session:\n  Duration:    {time}\n  Turns:       {}\n  Tool calls:  {}\n  Compactions: {}\n\n\
                     Context:\n  Usage:       {:.0}%\n  Window:      {} tokens\n  Model:       {}\n  Thinking:    {} {}",
                    self.turn, self.tool_calls, self.dashboard.compactions,
                    self.footer_data.context_percent, s.context_window,
                    s.model_short(), s.thinking.icon(), s.thinking.as_str(),
                ))
            }

            "detail" => {
                if args.is_empty() {
                    // Toggle
                    let current = self.settings().tool_detail;
                    let next = match current {
                        crate::settings::ToolDetail::Compact => crate::settings::ToolDetail::Detailed,
                        crate::settings::ToolDetail::Detailed => crate::settings::ToolDetail::Compact,
                    };
                    self.update_settings(|s| s.tool_detail = next);
                    SlashResult::Display(format!("Tool display → {}", next.as_str()))
                } else if let Some(mode) = crate::settings::ToolDetail::parse(args) {
                    self.update_settings(|s| s.tool_detail = mode);
                    SlashResult::Display(format!("Tool display → {}", mode.as_str()))
                } else {
                    SlashResult::Display(format!("Unknown mode: {args}. Options: compact, detailed"))
                }
            }

            "context" => {
                if args.is_empty() {
                    // Toggle
                    let current = self.settings().context_mode;
                    let next = match current {
                        crate::settings::ContextMode::Standard => crate::settings::ContextMode::Extended,
                        crate::settings::ContextMode::Extended => crate::settings::ContextMode::Standard,
                    };
                    self.update_settings(|s| {
                        s.context_mode = next;
                        s.apply_context_mode();
                    });
                    let s = self.settings();
                    self.footer_data.context_window = s.context_window;
                    SlashResult::Display(format!(
                        "Context → {} {} ({})",
                        next.icon(), next.as_str(),
                        if next == crate::settings::ContextMode::Extended { "Anthropic 1M beta" } else { "standard" }
                    ))
                } else if let Some(mode) = crate::settings::ContextMode::parse(args) {
                    self.update_settings(|s| {
                        s.context_mode = mode;
                        s.apply_context_mode();
                    });
                    let s = self.settings();
                    self.footer_data.context_window = s.context_window;
                    SlashResult::Display(format!("Context → {} {}", mode.icon(), mode.as_str()))
                } else {
                    SlashResult::Display(format!("Unknown mode: {args}. Options: 200k, 1m"))
                }
            }

            "compact" => {
                let _ = tx.try_send(TuiCommand::Compact);
                SlashResult::Display("Compaction queued — runs before next turn.".into())
            }

            "clear" => {
                self.conversation = ConversationView::new();
                SlashResult::Display("Display cleared.".into())
            }

            "sessions" => {
                let _ = tx.try_send(TuiCommand::ListSessions);
                SlashResult::Handled
            }

            "memory" => {
                SlashResult::Display(format!(
                    "Memory:\n  Facts:          {}\n  Injected:       {}\n  Working memory: {}\n  ~{} tokens",
                    self.footer_data.total_facts, self.footer_data.injected_facts,
                    self.footer_data.working_memory, self.footer_data.memory_tokens_est,
                ))
            }

            "migrate" => {
                let source = if args.is_empty() { "auto" } else { args };
                let cwd = std::path::Path::new(&self.footer_data.cwd);
                let report = crate::migrate::run(source, cwd);
                SlashResult::Display(report.summary())
            }

            "chronos" => {
                let sub = if args.is_empty() { "week" } else { args };
                match crate::tools::chronos::execute(sub, None, None, None) {
                    Ok(text) => SlashResult::Display(text),
                    Err(e) => SlashResult::Display(format!("❌ {e}")),
                }
            }

            "splash" => {
                // Set flag to replay splash on next draw cycle
                self.replay_splash = true;
                SlashResult::Handled
            }

            "exit" | "quit" => SlashResult::Quit,
            _ => {
                // Check if a bus feature handles this command
                if self.bus_commands.iter().any(|c| c.name == cmd) {
                    let _ = tx.try_send(TuiCommand::BusCommand {
                        name: cmd.to_string(),
                        args: args.to_string(),
                    });
                    SlashResult::Handled
                } else {
                    SlashResult::NotACommand
                }
            }
        }
    }

    /// Palette: matching commands + subcommands for the current editor text.
    fn matching_commands(&self) -> Vec<(String, String)> {
        let text = self.editor.render_text();
        if !text.starts_with('/') { return vec![]; }
        let input = &text[1..];
        let parts: Vec<&str> = input.splitn(2, ' ').collect();

        if parts.len() <= 1 {
            let prefix = parts.first().copied().unwrap_or("");
            let mut matches: Vec<(String, String)> = if prefix.is_empty() {
                Self::COMMANDS.iter().map(|(n, d, _)| (n.to_string(), d.to_string())).collect()
            } else {
                Self::COMMANDS.iter()
                    .filter(|(name, _, _)| name.starts_with(prefix))
                    .map(|(n, d, _)| (n.to_string(), d.to_string()))
                    .collect()
            };
            // Append bus feature commands
            for cmd in &self.bus_commands {
                if prefix.is_empty() || cmd.name.starts_with(prefix) {
                    matches.push((cmd.name.clone(), cmd.description.clone()));
                }
            }
            matches
        } else {
            let cmd = parts[0];
            let sub_prefix = parts.get(1).copied().unwrap_or("");
            // Check built-in commands first, then bus commands
            if let Some((_, _, subs)) = Self::COMMANDS.iter().find(|(n, _, _)| *n == cmd) {
                subs.iter()
                    .filter(|s| s.starts_with(sub_prefix))
                    .map(|s| (format!("{cmd} {s}"), String::new()))
                    .collect()
            } else if let Some(bus_cmd) = self.bus_commands.iter().find(|c| c.name == cmd) {
                bus_cmd.subcommands.iter()
                    .filter(|s| s.starts_with(sub_prefix))
                    .map(|s| (format!("{cmd} {s}"), String::new()))
                    .collect()
            } else {
                vec![]
            }
        }
    }

    /// Load editor history from disk.
    fn load_history(cwd: &str) -> Vec<String> {
        let path = history_path(cwd);
        match std::fs::read_to_string(&path) {
            Ok(content) => content
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Save editor history to disk.
    fn save_history(&self) {
        let path = history_path(&self.footer_data.cwd);
        if self.history.is_empty() {
            return;
        }
        // Keep last 500 entries
        let start = self.history.len().saturating_sub(500);
        let content = self.history[start..].join("\n");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, content) {
            tracing::debug!("Failed to save history: {e}");
        }
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            None => self.history.len().saturating_sub(1),
            Some(i) => i.saturating_sub(1),
        };
        self.history_idx = Some(idx);
        self.editor.set_text(&self.history[idx]);
    }

    fn history_down(&mut self) {
        match self.history_idx {
            None => {}
            Some(i) => {
                if i + 1 < self.history.len() {
                    self.history_idx = Some(i + 1);
                    self.editor.set_text(&self.history[i + 1]);
                } else {
                    self.history_idx = None;
                    self.editor.set_text("");
                }
            }
        }
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart { turn } => {
                self.agent_active = true;
                self.turn = turn;
                self.working_verb = spinner::next_verb();
                self.effects.start_spinner_glow();
            }
            AgentEvent::TurnEnd { turn } => {
                self.turn = turn;
                // Estimate context usage from turn count + tool calls
                // (rough heuristic: ~2k tokens per turn average)
                let est_tokens = (turn as usize) * 2000 + (self.tool_calls as usize) * 500;
                let ctx_window = self.footer_data.context_window;
                if ctx_window > 0 {
                    self.footer_data.estimated_tokens = est_tokens;
                    self.footer_data.context_percent =
                        (est_tokens as f32 / ctx_window as f32 * 100.0).min(100.0);
                }
            }
            AgentEvent::MessageChunk { text } => {
                self.conversation.append_streaming(&text);
            }
            AgentEvent::ThinkingChunk { text } => {
                self.conversation.append_thinking(&text);
            }
            AgentEvent::ToolStart { id, name, args } => {
                self.working_verb = spinner::next_verb();
                let args_summary = crate::r#loop::summarize_tool_args(&name, &args);
                // Full args for detailed view
                let detail_args = match name.as_str() {
                    "bash" => args.get("command").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    "read" | "edit" | "write" | "view" => args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    _ => Some(serde_json::to_string_pretty(&args).unwrap_or_default()),
                };
                self.conversation.push_tool_start(&id, &name, args_summary.as_deref(), detail_args.as_deref());
                self.tool_calls += 1;
                self.last_tool_name = Some(name);
            }
            AgentEvent::ToolEnd { id, result, is_error } => {
                let summary = result.content.first().and_then(|c| match c {
                    omegon_traits::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                });
                self.conversation.push_tool_end(&id, is_error, summary);

                // Dynamic footer: memory tools update fact count
                if let Some(ref name) = self.last_tool_name {
                    let is_memory_mutation = matches!(name.as_str(),
                        "memory_store" | "memory_supersede" | "memory_archive");
                    if name == "memory_store" || name == "memory_supersede" {
                        self.footer_data.total_facts += 1;
                    } else if name == "memory_archive" {
                        self.footer_data.total_facts = self.footer_data.total_facts.saturating_sub(1);
                    }
                    if is_memory_mutation {
                        self.effects.ping_footer(self.theme.as_ref());
                    }
                }
                self.last_tool_name = None;
            }
            AgentEvent::AgentEnd => {
                self.agent_active = false;
                self.conversation.finalize_message();
                self.effects.stop_spinner_glow();
            }
            AgentEvent::PhaseChanged { phase } => {
                self.conversation.push_lifecycle("◈", &format!("Phase → {phase:?}"));
            }
            AgentEvent::DecompositionStarted { children } => {
                self.conversation.push_lifecycle(
                    "⚡",
                    &format!("Cleave: {} children dispatched", children.len()),
                );
            }
            AgentEvent::DecompositionChildCompleted { label, success } => {
                let icon = if success { "✓" } else { "✗" };
                self.conversation.push_lifecycle(icon, &format!("Child '{label}' completed"));
            }
            AgentEvent::DecompositionCompleted { merged } => {
                let status = if merged { "merged" } else { "completed (no merge)" };
                self.conversation.push_lifecycle("⚡", &format!("Cleave {status}"));
            }
            AgentEvent::SystemNotification { message } => {
                self.conversation.push_system(&message);
            }
            _ => {}
        }
    }
}

/// Run the interactive TUI. Returns when the user quits.
///
/// This spawns the ratatui event loop and communicates with the agent
/// coordinator through channels.
/// Configuration for the TUI — passed from main.
pub struct TuiConfig {
    pub cwd: String,
    pub is_oauth: bool,
    /// Pre-populated initial state so the first frame isn't empty.
    pub initial: TuiInitialState,
    /// Skip the splash animation on startup.
    pub no_splash: bool,
    /// Command definitions from bus features — shown in command palette.
    pub bus_commands: Vec<omegon_traits::CommandDefinition>,
}

/// Initial state snapshot gathered during setup, before the TUI event loop starts.
/// Populates footer cards and dashboard on the very first frame.
#[derive(Default)]
pub struct TuiInitialState {
    pub total_facts: usize,
    pub focused_node: Option<dashboard::FocusedNodeSummary>,
    pub active_changes: Vec<dashboard::ChangeSummary>,
}

/// Path to the editor history file — persists across sessions.
fn history_path(cwd: &str) -> std::path::PathBuf {
    let project_root = crate::setup::find_project_root(std::path::Path::new(cwd));
    project_root.join(".omegon").join("history")
}

fn sel_opt(value: &str, label: &str, desc: &str, current: &str) -> selector::SelectOption {
    selector::SelectOption {
        value: value.to_string(),
        label: label.to_string(),
        description: desc.to_string(),
        active: value == current,
    }
}

pub async fn run_tui(
    mut events_rx: broadcast::Receiver<AgentEvent>,
    command_tx: mpsc::Sender<TuiCommand>,
    config: TuiConfig,
    cancel: SharedCancel,
    settings: crate::settings::SharedSettings,
) -> io::Result<()> {
    // Set up terminal with mouse capture for scroll events
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // Install panic hook that restores terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    // Seed spinner from process start time for variety across sessions
    spinner::seed(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as usize)
        .unwrap_or(42));

    let mut app = App::new(settings);
    app.history = App::load_history(&config.cwd);
    app.footer_data.cwd = config.cwd;
    app.footer_data.is_oauth = config.is_oauth;
    app.bus_commands = config.bus_commands;
    app.cancel = cancel;

    // Pre-populate from initial state so first frame isn't empty
    app.footer_data.total_facts = config.initial.total_facts;
    app.dashboard.focused_node = config.initial.focused_node;
    app.dashboard.active_changes = config.initial.active_changes;

    // Build a contextual welcome message
    {
        let s = app.settings();
        let model_short = s.model_short();
        let project = app.footer_data.cwd.split('/').next_back().unwrap_or("project");
        let facts = app.footer_data.total_facts;
        let ctx = s.context_window / 1000;

        let mut welcome = format!("Ω Omegon — {project}");
        welcome.push_str(&format!("\n  ▸ {model_short}  ·  {ctx}k context"));
        if facts > 0 {
            welcome.push_str(&format!("  ·  {facts} facts loaded"));
        }
        welcome.push('\n');
        welcome.push_str("\n  /model  switch provider    /think  reasoning level");
        welcome.push_str("\n  /context  toggle 200k↔1M   /help   all commands");
        welcome.push_str("\n  Ctrl+R  search history      Ctrl+C  cancel/quit");

        app.conversation.push_system(&welcome);
    }

    // ── Splash screen ───────────────────────────────────────────────
    if !config.no_splash {
        let size = terminal.size()?;
        if let Some(mut splash) = splash::SplashScreen::new(size.width, size.height) {
            // Mark loading items done immediately — we load fast
            splash.set_load_state("providers", splash::LoadState::Active);
            splash.set_load_state("memory", splash::LoadState::Active);
            splash.set_load_state("tools", splash::LoadState::Active);

            // Run splash animation loop
            let splash_start = std::time::Instant::now();
            let safety_timeout = std::time::Duration::from_secs(5);

            loop {
                // Draw splash
                {
                    let t = &app.theme;
                    terminal.draw(|f| splash.draw(f, t.as_ref()))?;
                }

                // Poll for keypress at animation frame rate
                let interval = splash::SplashScreen::frame_interval();
                if event::poll(interval)?
                    && matches!(event::read()?, Event::Key(_))
                    && (splash.ready_to_dismiss()
                        || splash_start.elapsed() > std::time::Duration::from_millis(300))
                {
                    break;
                }

                splash.tick();

                // Cosmetic loading animation — the binary loads in ~50ms so
                // real subsystem tracking would be invisible. These frame
                // thresholds create a visual cascade for branding purposes.
                if splash.frame >= 8 {
                    splash.set_load_state("providers", splash::LoadState::Done);
                }
                if splash.frame >= 12 {
                    splash.set_load_state("memory", splash::LoadState::Done);
                }
                if splash.frame >= 16 {
                    splash.set_load_state("tools", splash::LoadState::Done);
                }

                // Drain agent events to prevent broadcast buffer overflow.
                // Events are silently discarded — the splash is a branding moment,
                // not functional UI.
                while events_rx.try_recv().is_ok() {}

                // Safety timeout
                if splash_start.elapsed() > safety_timeout {
                    splash.force_done();
                    break;
                }

                // Auto-dismiss after hold period
                if splash.ready_to_dismiss() && splash.hold_count > splash::HOLD_FRAMES + 30 {
                    break;
                }
            }
        }
    }

    // Queue startup reveal effects (footer sweep-in, conversation fade)
    {
        let t = &app.theme;
        app.effects.queue_startup(t.as_ref());
    }

    loop {
        // ── Splash replay (/splash command) ─────────────────────────
        if app.replay_splash {
            app.replay_splash = false;
            let size = terminal.size()?;
            if let Some(mut splash) = splash::SplashScreen::new(size.width, size.height) {
                splash.force_done(); // No loading checklist on replay
                loop {
                    {
                        let t = &app.theme;
                        terminal.draw(|f| splash.draw(f, t.as_ref()))?;
                    }
                    let interval = splash::SplashScreen::frame_interval();
                    if event::poll(interval)? {
                        let ev = event::read()?;
                        // Any key or mouse click dismisses the replay
                        if matches!(ev, Event::Key(_) | Event::Mouse(_)) {
                            break;
                        }
                    }
                    splash.tick();
                    // Auto-end after full animation + hold
                    if splash.frame > splash::TOTAL_FRAMES + splash::HOLD_FRAMES + 20 {
                        break;
                    }
                }
            }
        }

        // Draw
        terminal.draw(|f| app.draw(f))?;

        // Poll for events with timeout (16ms ≈ 60fps)
        let has_terminal_event = event::poll(Duration::from_millis(16))?;

        if has_terminal_event {
            match event::read()? {
                // ── Mouse scroll ────────────────────────────────────────
                // macOS natural scrolling inverts at the OS level BEFORE
                // the terminal sees it. crossterm's ScrollUp means "the OS
                // sent scroll-up" which, with natural scrolling, means the
                // user swiped fingers DOWN (wanting to see newer content).
                // So: ScrollUp → scroll toward bottom, ScrollDown → scroll toward top.
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.conversation.scroll_up(3);
                        }
                        MouseEventKind::ScrollDown => {
                            app.conversation.scroll_down(3);
                        }
                        _ => {}
                    }
                }
                // ── Paste — insert pasted text into editor ──────────
                Event::Paste(text) => {
                    if !app.agent_active {
                        for c in text.chars() {
                            // Skip control characters except newline
                            if c == '\n' || c == '\r' {
                                // For single-line editor, treat newline as submit
                                // (user pasted a command and hit enter)
                                continue;
                            }
                            if !c.is_control() {
                                app.editor.insert(c);
                            }
                        }
                    }
                }
                Event::Key(key) => {
                // ── Selector popup intercepts all keys when open ────
                if app.selector.is_some() {
                    match key.code {
                        KeyCode::Up => { if let Some(ref mut s) = app.selector { s.move_up(); } }
                        KeyCode::Down => { if let Some(ref mut s) = app.selector { s.move_down(); } }
                        KeyCode::Enter => {
                            if let Some(msg) = app.confirm_selector(&command_tx) {
                                app.conversation.push_system(&msg);
                            }
                        }
                        KeyCode::Esc => {
                            app.selector = None;
                            app.selector_kind = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                // ── Reverse search mode intercepts keys ─────────
                if matches!(app.editor.mode(), editor::EditorMode::ReverseSearch { .. }) {
                    match key.code {
                        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // Ctrl+R again: search further back
                            app.editor.search_prev(&app.history);
                        }
                        KeyCode::Char(c) => {
                            app.editor.insert(c);
                            app.editor.search_update(&app.history);
                        }
                        KeyCode::Backspace => {
                            app.editor.backspace();
                            app.editor.search_update(&app.history);
                        }
                        KeyCode::Enter => {
                            app.editor.accept_search(&app.history);
                        }
                        KeyCode::Esc => {
                            app.editor.cancel_search();
                        }
                        _ => {
                            // Any other key: accept search + process key normally
                            app.editor.accept_search(&app.history);
                        }
                    }
                    continue;
                }

                match (key.code, key.modifiers) {
                    // ── Interrupt: Escape or Ctrl+C ─────────────────
                    (KeyCode::Esc, _) => {
                        if app.agent_active {
                            app.interrupt();
                            app.agent_active = false; // Unblock editor immediately
                            app.conversation.finalize_message();
                            app.conversation.push_system("⎋ Interrupted");
                        }
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.agent_active {
                            app.interrupt();
                            app.agent_active = false; // Unblock editor immediately
                            app.conversation.finalize_message();
                            app.conversation.push_system("⎋ Interrupted (Ctrl+C)");
                        } else {
                            let now = std::time::Instant::now();
                            if let Some(last) = app.last_ctrl_c {
                                if now.duration_since(last).as_millis() < 1000 {
                                    app.should_quit = true;
                                    let _ = command_tx.send(TuiCommand::Quit).await;
                                } else {
                                    app.last_ctrl_c = Some(now);
                                    app.conversation.push_system("Press Ctrl+C again to quit");
                                }
                            } else {
                                app.last_ctrl_c = Some(now);
                                app.conversation.push_system("Press Ctrl+C again to quit");
                            }
                        }
                    }

                    // ── Editor: word/line operations (idle only) ────
                    (KeyCode::Char('w'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.delete_word_backward();
                    }
                    (KeyCode::Char('u'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.clear_line();
                    }
                    (KeyCode::Char('k'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.kill_to_end();
                    }
                    (KeyCode::Char('y'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.yank();
                    }
                    (KeyCode::Char('a'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.move_home();
                    }
                    (KeyCode::Char('e'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.move_end();
                    }
                    (KeyCode::Char('r'), KeyModifiers::CONTROL) if !app.agent_active => {
                        app.editor.start_reverse_search();
                    }

                    // Meta (Alt) key combos for word operations
                    (KeyCode::Backspace, KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.delete_word_backward();
                    }
                    (KeyCode::Char('d'), KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.delete_word_forward();
                    }
                    (KeyCode::Char('b'), KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.move_word_backward();
                    }
                    (KeyCode::Char('f'), KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.move_word_forward();
                    }

                    // Tab: command completion if typing, or toggle tool card expansion
                    (KeyCode::Tab, _) if !app.agent_active => {
                        let text = app.editor.render_text().to_string();
                        if text.starts_with('/') {
                            // Command completion
                            let matches = app.matching_commands();
                            if matches.len() == 1 {
                                let cmd = format!("/{}", matches[0].0);
                                app.editor.set_text(&cmd);
                            }
                        } else if text.is_empty() {
                            // Toggle nearest tool card expansion
                            if let Some(idx) = app.conversation.focused_tool_card() {
                                app.conversation.toggle_expand(idx);
                            }
                        }
                    }

                    // Submit
                    (KeyCode::Enter, _) if !app.agent_active => {
                        let text = app.editor.take_text();
                        if !text.is_empty() {
                            match app.handle_slash_command(&text, &command_tx) {
                                SlashResult::Display(response) => {
                                    app.conversation.push_system(&response);
                                }
                                SlashResult::Handled => {
                                    // Command handled silently (popup opened, etc.)
                                }
                                SlashResult::Quit => {
                                    app.should_quit = true;
                                    let _ = command_tx.send(TuiCommand::Quit).await;
                                }
                                SlashResult::NotACommand => {
                                    // Not a slash command — send as user prompt
                                    app.conversation.push_user(&text);
                                    app.history.push(text.clone());
                                    app.history_idx = None;
                                    app.agent_active = true;
                                    let _ = command_tx.send(TuiCommand::UserPrompt(text)).await;
                                }
                            }
                        }
                    }

                    // Basic editing
                    (KeyCode::Char(c), _) if !app.agent_active => {
                        app.editor.insert(c);
                    }
                    (KeyCode::Backspace, _) if !app.agent_active => {
                        app.editor.backspace();
                    }
                    (KeyCode::Left, KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.move_word_backward();
                    }
                    (KeyCode::Right, KeyModifiers::ALT) if !app.agent_active => {
                        app.editor.move_word_forward();
                    }
                    (KeyCode::Left, _) if !app.agent_active => {
                        app.editor.move_left();
                    }
                    (KeyCode::Right, _) if !app.agent_active => {
                        app.editor.move_right();
                    }
                    (KeyCode::Home, _) if !app.agent_active => {
                        app.editor.move_home();
                    }
                    (KeyCode::End, _) if !app.agent_active => {
                        app.editor.move_end();
                    }

                    // ── Scrolling ────────────────────────────────
                    (KeyCode::Up, KeyModifiers::SHIFT) => {
                        app.conversation.scroll_up(3);
                    }
                    (KeyCode::Down, KeyModifiers::SHIFT) => {
                        app.conversation.scroll_down(3);
                    }
                    (KeyCode::PageUp, _) => {
                        app.conversation.scroll_up(20);
                    }
                    (KeyCode::PageDown, _) => {
                        app.conversation.scroll_down(20);
                    }
                    (KeyCode::Up, _) if app.agent_active => {
                        app.conversation.scroll_up(3);
                    }
                    (KeyCode::Down, _) if app.agent_active => {
                        app.conversation.scroll_down(3);
                    }
                    (KeyCode::Up, _) if !app.agent_active => {
                        app.history_up();
                    }
                    (KeyCode::Down, _) if !app.agent_active => {
                        app.history_down();
                    }
                    _ => {}
                }
            } // Event::Key
            _ => {} // Other events (resize, etc.)
        } // match event::read()
        } // if has_terminal_event

        // Drain agent events
        while let Ok(agent_event) = events_rx.try_recv() {
            app.handle_agent_event(agent_event);
        }

        if app.should_quit {
            break;
        }
    }

    // Save history before restoring terminal
    app.save_history();

    // Restore terminal
    io::stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
