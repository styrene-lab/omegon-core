//! Conversation view — scrollable message display with streaming support.

use ratatui::text::{Line, Span};
use ratatui::style::{Color, Modifier, Style};

/// A message in the conversation view.
#[derive(Debug, Clone)]
enum Message {
    User(String),
    /// System/info message (from slash commands, etc.)
    System(String),
    /// Assistant text (possibly still streaming).
    Assistant {
        text: String,
        thinking: String,
        complete: bool,
    },
    /// Tool call summary.
    Tool {
        id: String,
        name: String,
        is_error: bool,
        complete: bool,
    },
}

pub struct ConversationView {
    messages: Vec<Message>,
    /// Scroll offset from the bottom (0 = at bottom, showing latest).
    scroll: u16,
    /// Whether we're currently receiving streaming text.
    streaming: bool,
}

impl ConversationView {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll: 0,
            streaming: false,
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.messages.push(Message::User(text.to_string()));
        self.scroll = 0;
    }

    pub fn push_system(&mut self, text: &str) {
        self.messages.push(Message::System(text.to_string()));
        self.scroll = 0;
    }

    pub fn append_streaming(&mut self, delta: &str) {
        if !self.streaming {
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Message::Assistant { text, .. }) = self.messages.last_mut() {
            text.push_str(delta);
        }
        self.scroll = 0;
    }

    pub fn append_thinking(&mut self, delta: &str) {
        if !self.streaming {
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Message::Assistant { thinking, .. }) = self.messages.last_mut() {
            thinking.push_str(delta);
        }
    }

    pub fn push_tool_start(&mut self, id: &str, name: &str) {
        self.messages.push(Message::Tool {
            id: id.to_string(),
            name: name.to_string(),
            is_error: false,
            complete: false,
        });
        self.scroll = 0;
    }

    pub fn push_tool_end(&mut self, id: &str, is_error: bool) {
        // Find the matching tool start by id and mark it complete
        for msg in self.messages.iter_mut().rev() {
            if let Message::Tool {
                id: tool_id,
                complete: c,
                is_error: e,
                ..
            } = msg
                && tool_id == id && !*c {
                    *c = true;
                    *e = is_error;
                    break;
                }
        }
    }

    pub fn finalize_message(&mut self) {
        if let Some(Message::Assistant { complete, .. }) = self.messages.last_mut() {
            *complete = true;
        }
        self.streaming = false;
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn scroll_offset(&self) -> u16 {
        self.scroll
    }

    /// Render conversation to ratatui Lines.
    pub fn render_text(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for msg in &self.messages {
            match msg {
                Message::User(text) => {
                    lines.push(Line::from(vec![
                        Span::styled("▸ ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            text.clone(),
                            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    lines.push(Line::from(""));
                }
                Message::System(text) => {
                    for line in text.lines() {
                        lines.push(Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                        )));
                    }
                    lines.push(Line::from(""));
                }
                Message::Assistant {
                    text,
                    thinking,
                    complete,
                } => {
                    if !thinking.is_empty() {
                        // Show thinking in dim
                        for line in thinking.lines() {
                            lines.push(Line::from(Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                    }
                    for line in text.lines() {
                        lines.push(Line::from(Span::raw(line.to_string())));
                    }
                    if !*complete && text.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "...",
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                }
                Message::Tool {
                    name,
                    is_error,
                    complete,
                    ..
                } => {
                    let (icon, color) = if *complete {
                        if *is_error {
                            ("✗", Color::Red)
                        } else {
                            ("✓", Color::Green)
                        }
                    } else {
                        ("→", Color::Yellow)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{icon} "), Style::default().fg(color)),
                        Span::styled(name.clone(), Style::default().fg(color)),
                    ]));
                }
            }
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_renders() {
        let mut cv = ConversationView::new();
        cv.push_user("hello");
        let lines = cv.render_text();
        assert!(!lines.is_empty());
    }

    #[test]
    fn streaming_appends_to_same_message() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Hello ");
        cv.append_streaming("world");
        assert_eq!(cv.messages.len(), 1);
        if let Message::Assistant { text, .. } = &cv.messages[0] {
            assert_eq!(text, "Hello world");
        }
    }

    #[test]
    fn finalize_marks_complete() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Done");
        cv.finalize_message();
        if let Message::Assistant { complete, .. } = &cv.messages[0] {
            assert!(complete);
        }
    }

    #[test]
    fn tool_lifecycle() {
        let mut cv = ConversationView::new();
        cv.push_tool_start("tc1", "read");
        cv.push_tool_end("tc1", false);
        if let Message::Tool {
            complete, is_error, ..
        } = &cv.messages[0]
        {
            assert!(complete);
            assert!(!is_error);
        }
    }

    #[test]
    fn scroll_operations() {
        let mut cv = ConversationView::new();
        assert_eq!(cv.scroll_offset(), 0);
        cv.scroll_up(5);
        assert_eq!(cv.scroll_offset(), 5);
        cv.scroll_down(3);
        assert_eq!(cv.scroll_offset(), 2);
        cv.scroll_down(10);
        assert_eq!(cv.scroll_offset(), 0); // saturates at 0
    }
}
