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
}

impl App {
    pub fn new() -> Self {
        Self {
            editor: Editor::new(),
            conversation: ConversationView::new(),
            agent_active: false,
            should_quit: false,
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // conversation (takes remaining space)
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

        // Editor
        let status = if self.agent_active { " (working...)" } else { "" };
        let editor_block = Block::default()
            .borders(Borders::TOP)
            .title(format!("Ω{status}"));
        let editor_text = self.editor.render_text();
        let editor_widget = Paragraph::new(editor_text).block(editor_block);
        frame.render_widget(editor_widget, chunks[1]);

        // Position cursor in editor
        let editor_area = chunks[1];
        let cursor_x = editor_area.x + 1 + self.editor.cursor_position() as u16;
        let cursor_y = editor_area.y + 1; // +1 for border
        frame.set_cursor_position(Position::new(
            cursor_x.min(editor_area.right().saturating_sub(1)),
            cursor_y,
        ));
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart { .. } => {
                self.agent_active = true;
            }
            AgentEvent::TurnEnd { .. } => {
                self.agent_active = false;
            }
            AgentEvent::MessageChunk { text } => {
                self.conversation.append_streaming(&text);
            }
            AgentEvent::ThinkingChunk { text } => {
                self.conversation.append_thinking(&text);
            }
            AgentEvent::ToolStart { id, name, .. } => {
                self.conversation.push_tool_start(&id, &name);
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

    let mut app = App::new();

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
                    (KeyCode::Up, _) => {
                        app.conversation.scroll_up(3);
                    }
                    (KeyCode::Down, _) => {
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
