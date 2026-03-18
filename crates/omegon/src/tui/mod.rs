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
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // conversation (takes remaining space)
                Constraint::Length(1), // footer
                Constraint::Length(3), // editor
            ])
            .split(frame.area());

        // Conversation view
        let conv_block = Block::default()
            .borders(Borders::NONE);
        let conv_text = self.conversation.render_text();
        let conv_widget = Paragraph::new(conv_text)
            .block(conv_block)
            .wrap(Wrap { trim: false })
            .scroll((self.conversation.scroll_offset(), 0));
        frame.render_widget(conv_widget, chunks[0]);

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
                            if text == "/exit" || text == "/quit" {
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
