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

use omegon_traits::AgentEvent;

use self::conversation::ConversationView;
use self::dashboard::DashboardState;
use self::editor::Editor;

/// Messages from TUI to the agent coordinator.
#[derive(Debug)]
pub enum TuiCommand {
    /// User submitted a prompt.
    UserPrompt(String),
    /// User pressed Ctrl+C during execution (cancel current turn).
    Cancel,
    /// User wants to quit (Ctrl+C at idle editor, or /exit).
    Quit,
}

/// Application state for the TUI.
pub struct App {
    editor: Editor,
    conversation: ConversationView,
    /// True when the agent is running (waiting for response or executing tools).
    agent_active: bool,
    /// True when we should exit.
    should_quit: bool,
    /// Current model string for footer display.
    model: String,
    /// Turn counter.
    turn: u32,
    /// Tool calls this session.
    tool_calls: u32,
    /// Input history (most recent last).
    history: Vec<String>,
    /// History navigation index (None = not navigating).
    history_idx: Option<usize>,
    /// Dashboard state.
    dashboard: DashboardState,
}

impl App {
    pub fn new(model: String) -> Self {
        Self {
            editor: Editor::new(),
            conversation: ConversationView::new(),
            agent_active: false,
            should_quit: false,
            model,
            turn: 0,
            tool_calls: 0,
            history: Vec::new(),
            history_idx: None,
            dashboard: DashboardState::default(),
        }
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
                Constraint::Length(1), // footer
                Constraint::Length(3), // editor
            ])
            .split(main_area);

        // Conversation view
        let conv_block = Block::default()
            .borders(Borders::NONE);
        let conv_text = self.conversation.render_text();
        let conv_widget = Paragraph::new(conv_text)
            .block(conv_block)
            .wrap(Wrap { trim: false })
            .scroll((self.conversation.scroll_offset(), 0));
        frame.render_widget(conv_widget, chunks[0]);

        // Dashboard (right panel)
        if let Some(dash) = dash_area {
            self.dashboard.render(dash, frame);
        }

        // Footer
        let status_str = if self.agent_active { "working" } else { "idle" };
        let model_short = self.model.split(':').last().unwrap_or(&self.model);
        let footer_text = format!(
            " Ω {model_short} │ turn {} │ {} tools │ {status_str}",
            self.turn, self.tool_calls,
        );
        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::DarkGray).bg(Color::Black));
        frame.render_widget(footer, chunks[1]);

        // Editor
        let editor_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(if self.agent_active {
                Span::styled(" ⟳ ", Style::default().fg(Color::Yellow))
            } else {
                Span::styled(" ▸ ", Style::default().fg(Color::Cyan))
            });
        let editor_text = self.editor.render_text();
        let editor_widget = Paragraph::new(editor_text).block(editor_block);
        frame.render_widget(editor_widget, chunks[2]);

        // Position cursor in editor (only when not agent_active)
        if !self.agent_active {
            let editor_area = chunks[2];
            let cursor_x = editor_area.x + 1 + self.editor.cursor_position() as u16;
            let cursor_y = editor_area.y + 1; // +1 for border
            frame.set_cursor_position(Position::new(
                cursor_x.min(editor_area.right().saturating_sub(1)),
                cursor_y,
            ));
        }
    }

    /// Handle slash commands that are processed locally (not sent to the agent).
    /// Returns Some(response) if handled, None if not a recognized local command.
    fn handle_slash_command(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let (cmd, _args) = trimmed[1..].split_once(' ').unwrap_or((&trimmed[1..], ""));
        match cmd {
            "help" => Some(
                "Available commands:\n\
                 /help    — show this help\n\
                 /model   — show current model\n\
                 /stats   — show session statistics\n\
                 /exit    — quit the session\n\
                 \n\
                 All other input is sent as a prompt to the agent."
                    .into(),
            ),
            "model" => Some(format!("Current model: {}", self.model)),
            "stats" => Some(format!(
                "Session: {} turns, {} tool calls, {} compactions",
                self.turn, self.tool_calls, self.dashboard.compactions,
            )),
            "exit" | "quit" => None, // handled by caller
            _ => None,               // unknown slash command — send to agent
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
            AgentEvent::ToolEnd { id, is_error, .. } => {
                self.conversation.push_tool_end(&id, is_error);
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
pub async fn run_tui(
    mut events_rx: broadcast::Receiver<AgentEvent>,
    command_tx: mpsc::Sender<TuiCommand>,
    model: String,
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

    let mut app = App::new(model);
    app.conversation.push_system(
        "Ω Omegon interactive session\n\
         Type a message and press Enter. /help for commands. Ctrl+C to cancel/quit."
    );

    loop {
        // Draw
        terminal.draw(|f| app.draw(f))?;

        // Poll for events with timeout (16ms ≈ 60fps)
        let has_terminal_event = event::poll(Duration::from_millis(16))?;

        if has_terminal_event {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.agent_active {
                            let _ = command_tx.send(TuiCommand::Cancel).await;
                        } else {
                            app.should_quit = true;
                            let _ = command_tx.send(TuiCommand::Quit).await;
                        }
                    }
                    (KeyCode::Enter, _) if !app.agent_active => {
                        let text = app.editor.take_text();
                        if !text.is_empty() {
                            if let Some(response) = app.handle_slash_command(&text) {
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
