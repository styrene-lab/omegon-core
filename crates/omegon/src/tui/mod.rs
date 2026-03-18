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
pub mod dashboard;
pub mod editor;
pub mod footer;
pub mod selector;
pub mod theme;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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
}

#[derive(Debug, Clone, Copy)]
enum SelectorKind {
    Model,
    ThinkingLevel,
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
        }
    }

    fn open_model_selector(&mut self) {
        let current = self.settings().model.clone();
        let mut options: Vec<selector::SelectOption> = Vec::new();

        // Only show providers the user is actually authenticated with
        let anthropic_auth = crate::providers::resolve_api_key_sync("anthropic");
        let openai_auth = crate::providers::resolve_api_key_sync("openai");

        if let Some((_, is_oauth)) = anthropic_auth {
            let auth = if is_oauth { "subscription" } else { "api-key" };
            options.push(sel_opt("anthropic:claude-sonnet-4-20250514", "Claude Sonnet 4", &format!("fast · 200k · {auth}"), &current));
            options.push(sel_opt("anthropic:claude-opus-4-20250514", "Claude Opus 4", &format!("strongest · 200k · {auth}"), &current));
            options.push(sel_opt("anthropic:claude-haiku-3-20250307", "Claude Haiku 3", &format!("cheapest · 200k · {auth}"), &current));
        }

        if let Some((_, is_oauth)) = openai_auth {
            let auth = if is_oauth { "subscription" } else { "api-key" };
            options.push(sel_opt("openai:gpt-4.1", "GPT-4.1", &format!("OpenAI · 128k · {auth}"), &current));
            options.push(sel_opt("openai:o3", "o3", &format!("OpenAI reasoning · 200k · {auth}"), &current));
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
        let show_dashboard = area.width >= 100;

        // Top-level horizontal split: main area | dashboard
        let (main_area, dash_area) = if show_dashboard {
            let h = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(60), Constraint::Length(30)])
                .split(area);
            (h[0], Some(h[1]))
        } else {
            (area, None)
        };

        // Vertical split within main area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // conversation
                Constraint::Length(4), // footer cards (header + 2 content lines + separator)
                Constraint::Length(3), // editor
            ])
            .split(main_area);

        // Conversation view
        let t = &self.theme;
        let conv_block = Block::default().borders(Borders::NONE);
        let conv_text = self.conversation.render_themed(t.as_ref());
        let conv_widget = Paragraph::new(conv_text)
            .block(conv_block)
            .wrap(Wrap { trim: false })
            .scroll((self.conversation.scroll_offset(), 0));
        frame.render_widget(conv_widget, chunks[0]);

        // Dashboard (right panel)
        if let Some(dash) = dash_area {
            self.dashboard.render_themed(dash, frame, t.as_ref());
        }

        // Footer — sync from settings + session state
        {
            let s = self.settings();
            self.footer_data.model_id = s.model.clone();
            self.footer_data.model_provider = s.provider().to_string();
            self.footer_data.context_window = s.context_window;
        }
        self.footer_data.turn = self.turn;
        self.footer_data.tool_calls = self.tool_calls;
        self.footer_data.compactions = self.dashboard.compactions;
        self.footer_data.render(chunks[1], frame, t.as_ref());

        // Editor
        let editor_block = Block::default()
            .borders(Borders::TOP)
            .border_style(t.style_border_dim())
            .title(if self.agent_active {
                Span::styled(" ⟳ ", t.style_warning())
            } else {
                Span::styled(" ▸ ", t.style_accent())
            });
        let editor_text = self.editor.render_text();
        let editor_widget = Paragraph::new(editor_text)
            .style(t.style_fg())
            .block(editor_block);
        frame.render_widget(editor_widget, chunks[2]);

        // Command palette popup (above editor when typing /)
        if !self.agent_active {
            let matches = self.matching_commands();
            if !matches.is_empty() {
                let palette_height = matches.len().min(8) as u16 + 2; // +2 for borders
                let editor_area = chunks[2];
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

            // Position cursor in editor
            let editor_area = chunks[2];
            let cursor_x = editor_area.x + 1 + self.editor.cursor_position() as u16;
            let cursor_y = editor_area.y + 1; // +1 for border
            frame.set_cursor_position(Position::new(
                cursor_x.min(editor_area.right().saturating_sub(1)),
                cursor_y,
            ));
        }

        // Selector popup (overlays everything when active)
        if let Some(ref sel) = self.selector {
            sel.render(area, frame, t.as_ref());
        }
    }

    /// Command registry: (name, description, subcommands).
    const COMMANDS: &'static [(&'static str, &'static str, &'static [&'static str])] = &[
        ("help",     "show available commands",              &[]),
        ("model",    "view or switch model",                 &["list"]),
        ("think",    "set thinking level",                   &["off", "low", "medium", "high"]),
        ("stats",    "session telemetry",                    &[]),
        ("compact",  "trigger context compaction",           &[]),
        ("clear",    "clear conversation display",           &[]),
        ("sessions", "list saved sessions",                  &[]),
        ("memory",   "memory stats",                        &[]),
        ("migrate",  "import from other tools",               &["auto", "claude-code", "pi", "codex", "cursor", "aider"]),
        ("exit",     "quit (or double Ctrl+C)",              &[]),
    ];

    /// Handle a slash command. Returns Some(text) for display, None for /exit.
    fn handle_slash_command(&mut self, text: &str, tx: &mpsc::Sender<TuiCommand>) -> Option<String> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') { return None; }
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
                Some(format!("Commands:\n{}\n\nType / to browse. Tab completes.", lines.join("\n")))
            }

            "model" => {
                if args.is_empty() {
                    // No args → open interactive selector
                    self.open_model_selector();
                    None // selector handles the rest
                } else {
                    // Direct switch: /model anthropic:claude-opus-4-20250514
                    self.update_settings(|s| {
                        s.model = args.to_string();
                        s.context_window = crate::settings::Settings::new(args).context_window;
                    });
                    let _ = tx.try_send(TuiCommand::SetModel(args.to_string()));
                    Some(format!("Model → {args}"))
                }
            }

            "think" => {
                if args.is_empty() {
                    // No args → open interactive selector
                    self.open_thinking_selector();
                    None
                } else if let Some(level) = crate::settings::ThinkingLevel::parse(args) {
                    self.update_settings(|s| s.thinking = level);
                    Some(format!("Thinking → {} {}", level.icon(), level.as_str()))
                } else {
                    Some(format!("Unknown level: {args}. Options: off, low, medium, high"))
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
                Some(format!(
                    "Session:\n  Duration:    {time}\n  Turns:       {}\n  Tool calls:  {}\n  Compactions: {}\n\n\
                     Context:\n  Usage:       {:.0}%\n  Window:      {} tokens\n  Model:       {}\n  Thinking:    {} {}",
                    self.turn, self.tool_calls, self.dashboard.compactions,
                    self.footer_data.context_percent, s.context_window,
                    s.model_short(), s.thinking.icon(), s.thinking.as_str(),
                ))
            }

            "compact" => {
                let _ = tx.try_send(TuiCommand::Compact);
                Some("Compaction queued — runs before next turn.".into())
            }

            "clear" => {
                self.conversation = ConversationView::new();
                Some("Display cleared.".into())
            }

            "sessions" => {
                let _ = tx.try_send(TuiCommand::ListSessions);
                None // coordinator handles this
            }

            "memory" => {
                Some(format!(
                    "Memory:\n  Facts:          {}\n  Injected:       {}\n  Working memory: {}\n  ~{} tokens",
                    self.footer_data.total_facts, self.footer_data.injected_facts,
                    self.footer_data.working_memory, self.footer_data.memory_tokens_est,
                ))
            }

            "migrate" => {
                let source = if args.is_empty() { "auto" } else { args };
                let cwd = std::path::Path::new(&self.footer_data.cwd);
                let report = crate::migrate::run(source, cwd);
                Some(report.summary())
            }

            "exit" | "quit" => None,
            _ => None,
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
            if prefix.is_empty() {
                return Self::COMMANDS.iter().map(|(n, d, _)| (n.to_string(), d.to_string())).collect();
            }
            Self::COMMANDS.iter()
                .filter(|(name, _, _)| name.starts_with(prefix))
                .map(|(n, d, _)| (n.to_string(), d.to_string()))
                .collect()
        } else {
            let cmd = parts[0];
            let sub_prefix = parts.get(1).copied().unwrap_or("");
            if let Some((_, _, subs)) = Self::COMMANDS.iter().find(|(n, _, _)| *n == cmd) {
                subs.iter()
                    .filter(|s| s.starts_with(sub_prefix))
                    .map(|s| (format!("{cmd} {s}"), String::new()))
                    .collect()
            } else {
                vec![]
            }
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
            }
            AgentEvent::TurnEnd { .. } => {
                // Don't set agent_active=false here — wait for AgentEnd
                // (multiple turns happen in sequence)
            }
            AgentEvent::MessageChunk { text } => {
                self.conversation.append_streaming(&text);
            }
            AgentEvent::ThinkingChunk { text } => {
                self.conversation.append_thinking(&text);
            }
            AgentEvent::ToolStart { id, name, .. } => {
                self.conversation.push_tool_start(&id, &name);
                self.tool_calls += 1;
            }
            AgentEvent::ToolEnd { id, result, is_error } => {
                // Extract first text content block for display
                let summary = result.content.first().and_then(|c| match c {
                    omegon_traits::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                });
                self.conversation.push_tool_end(&id, is_error, summary);
            }
            AgentEvent::AgentEnd => {
                self.agent_active = false;
                self.conversation.finalize_message();
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
    // Set up terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // Install panic hook that restores terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let mut app = App::new(settings);
    app.footer_data.cwd = config.cwd;
    app.footer_data.is_oauth = config.is_oauth;
    app.cancel = cancel;
    app.conversation.push_system(
        "Ω Omegon interactive session\n\
         Type a message and press Enter. /help for commands. Ctrl+C to cancel/quit."
    );

    loop {
        // Draw
        terminal.draw(|f| app.draw(f))?;

        // Poll for events with timeout (16ms ≈ 60fps)
        let has_terminal_event = event::poll(Duration::from_millis(16))?;

        if has_terminal_event
            && let Event::Key(key) = event::read()? {
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
                        _ => {} // ignore other keys when selector is open
                    }
                    continue; // skip normal key handling
                }

                match (key.code, key.modifiers) {
                    // ── Interrupt: Escape or Ctrl+C ─────────────────
                    (KeyCode::Esc, _) => {
                        if app.agent_active {
                            app.interrupt();
                            app.conversation.push_system("⎋ Interrupted");
                        }
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.agent_active {
                            // Single Ctrl+C: interrupt
                            app.interrupt();
                            app.conversation.push_system("⎋ Interrupted (Ctrl+C)");
                        } else {
                            // Double Ctrl+C within 1s: quit
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
                    (KeyCode::Tab, _) if !app.agent_active => {
                        // Tab completion for slash commands
                        let matches = app.matching_commands();
                        if matches.len() == 1 {
                            let cmd = format!("/{}", matches[0].0);
                            app.editor.set_text(&cmd);
                        }
                    }
                    (KeyCode::Enter, _) if !app.agent_active => {
                        let text = app.editor.take_text();
                        if !text.is_empty() {
                            if let Some(response) = app.handle_slash_command(&text, &command_tx) {
                                app.conversation.push_system(&response);
                            } else if text == "/exit" || text == "/quit" {
                                app.should_quit = true;
                                let _ = command_tx.send(TuiCommand::Quit).await;
                            } else {
                                app.conversation.push_user(&text);
                                app.history.push(text.clone());
                                app.history_idx = None;
                                app.agent_active = true;
                                let _ = command_tx.send(TuiCommand::UserPrompt(text)).await;
                            }
                        }
                    }
                    (KeyCode::Char(c), _) if !app.agent_active => {
                        app.editor.insert(c);
                    }
                    (KeyCode::Backspace, _) if !app.agent_active => {
                        app.editor.backspace();
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
                    (KeyCode::Up, _) if !app.agent_active => {
                        app.history_up();
                    }
                    (KeyCode::Down, _) if !app.agent_active => {
                        app.history_down();
                    }
                    (KeyCode::Up, KeyModifiers::SHIFT) | (KeyCode::Up, _) if app.agent_active => {
                        app.conversation.scroll_up(3);
                    }
                    (KeyCode::Down, KeyModifiers::SHIFT) | (KeyCode::Down, _) if app.agent_active => {
                        app.conversation.scroll_down(3);
                    }
                    (KeyCode::PageUp, _) => {
                        app.conversation.scroll_up(20);
                    }
                    (KeyCode::PageDown, _) => {
                        app.conversation.scroll_down(20);
                    }
                    _ => {}
                }
            }

        // Drain agent events
        while let Ok(agent_event) = events_rx.try_recv() {
            app.handle_agent_event(agent_event);
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
