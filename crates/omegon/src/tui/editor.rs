//! Terminal-style text editor with word operations and reverse search.
//!
//! Behaves like a standard terminal input:
//! - Meta+Backspace / Ctrl+W: delete word backward
//! - Meta+D: delete word forward
//! - Ctrl+A / Home: move to start
//! - Ctrl+E / End: move to end
//! - Ctrl+U: clear line
//! - Ctrl+K: kill to end of line
//! - Up/Down: history navigation
//! - Ctrl+R: reverse incremental search through history

/// Editor mode — normal input or reverse search.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorMode {
    Normal,
    /// Reverse incremental search: typing filters history matches.
    ReverseSearch {
        query: String,
        /// Index into history of the current match (None = no match).
        match_idx: Option<usize>,
    },
}

/// A terminal-style single-line text editor with history and reverse search.
pub struct Editor {
    buffer: String,
    cursor: usize,
    mode: EditorMode,
    /// Kill ring — last killed text (Ctrl+K, Ctrl+U).
    kill_ring: Option<String>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            mode: EditorMode::Normal,
            kill_ring: None,
        }
    }

    pub fn mode(&self) -> &EditorMode {
        &self.mode
    }

    // ─── Character insertion ────────────────────────────────────

    pub fn insert(&mut self, c: char) {
        if let EditorMode::ReverseSearch { ref mut query, .. } = self.mode {
            query.push(c);
            return; // search update handled by caller via search_update()
        }
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let EditorMode::ReverseSearch { ref mut query, .. } = self.mode {
            query.pop();
            return;
        }
        if self.cursor > 0 {
            let mut new_cursor = self.cursor - 1;
            while new_cursor > 0 && !self.buffer.is_char_boundary(new_cursor) {
                new_cursor -= 1;
            }
            self.buffer.drain(new_cursor..self.cursor);
            self.cursor = new_cursor;
        }
    }

    // ─── Word operations ────────────────────────────────────────

    /// Delete word backward (Meta+Backspace / Ctrl+W).
    pub fn delete_word_backward(&mut self) {
        if self.cursor == 0 { return; }
        let start = self.word_boundary_backward();
        let killed = self.buffer[start..self.cursor].to_string();
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
        self.kill_ring = Some(killed);
    }

    /// Delete word forward (Meta+D).
    pub fn delete_word_forward(&mut self) {
        if self.cursor >= self.buffer.len() { return; }
        let end = self.word_boundary_forward();
        let killed = self.buffer[self.cursor..end].to_string();
        self.buffer.drain(self.cursor..end);
        self.kill_ring = Some(killed);
    }

    /// Move cursor one word backward (Meta+B).
    pub fn move_word_backward(&mut self) {
        self.cursor = self.word_boundary_backward();
    }

    /// Move cursor one word forward (Meta+F).
    pub fn move_word_forward(&mut self) {
        self.cursor = self.word_boundary_forward();
    }

    /// Yank (paste) from kill ring (Ctrl+Y).
    pub fn yank(&mut self) {
        if let Some(ref text) = self.kill_ring.clone() {
            self.buffer.insert_str(self.cursor, text);
            self.cursor += text.len();
        }
    }

    // ─── Line operations ────────────────────────────────────────

    /// Kill to end of line (Ctrl+K).
    pub fn kill_to_end(&mut self) {
        if self.cursor < self.buffer.len() {
            let killed = self.buffer[self.cursor..].to_string();
            self.buffer.truncate(self.cursor);
            self.kill_ring = Some(killed);
        }
    }

    /// Clear entire line (Ctrl+U).
    pub fn clear_line(&mut self) {
        if !self.buffer.is_empty() {
            self.kill_ring = Some(std::mem::take(&mut self.buffer));
            self.cursor = 0;
        }
    }

    // ─── Movement ───────────────────────────────────────────────

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            while self.cursor > 0 && !self.buffer.is_char_boundary(self.cursor) {
                self.cursor -= 1;
            }
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
            while self.cursor < self.buffer.len() && !self.buffer.is_char_boundary(self.cursor) {
                self.cursor += 1;
            }
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    // ─── Reverse search ─────────────────────────────────────────

    /// Enter reverse search mode (Ctrl+R).
    pub fn start_reverse_search(&mut self) {
        self.mode = EditorMode::ReverseSearch {
            query: String::new(),
            match_idx: None,
        };
    }

    /// Update reverse search — finds the best match in history.
    /// Called after each keystroke in search mode.
    /// Returns the matched history entry (if any) for display.
    pub fn search_update(&mut self, history: &[String]) -> Option<String> {
        if let EditorMode::ReverseSearch { ref query, ref mut match_idx } = self.mode {
            if query.is_empty() {
                *match_idx = None;
                return None;
            }
            // Search backward through history for a match
            let start = match_idx.map(|i| i.saturating_sub(1)).unwrap_or(history.len().saturating_sub(1));
            for i in (0..=start).rev() {
                if history[i].contains(query.as_str()) {
                    *match_idx = Some(i);
                    return Some(history[i].clone());
                }
            }
            // No match — wrap around from end
            for i in (0..history.len()).rev() {
                if history[i].contains(query.as_str()) {
                    *match_idx = Some(i);
                    return Some(history[i].clone());
                }
            }
            *match_idx = None;
            None
        } else {
            None
        }
    }

    /// Search backward one more step (Ctrl+R pressed again during search).
    pub fn search_prev(&mut self, history: &[String]) -> Option<String> {
        if let EditorMode::ReverseSearch { ref query, ref mut match_idx } = self.mode {
            if query.is_empty() { return None; }
            let start = match_idx.map(|i| i.saturating_sub(1)).unwrap_or(0);
            for i in (0..=start).rev() {
                if history[i].contains(query.as_str()) && Some(i) != *match_idx {
                    *match_idx = Some(i);
                    return Some(history[i].clone());
                }
            }
            None
        } else {
            None
        }
    }

    /// Accept the current search result and return to normal mode.
    pub fn accept_search(&mut self, history: &[String]) {
        if let EditorMode::ReverseSearch { match_idx: Some(idx), .. } = &self.mode
            && let Some(entry) = history.get(*idx)
        {
            self.buffer = entry.clone();
            self.cursor = self.buffer.len();
        }
        self.mode = EditorMode::Normal;
    }

    /// Cancel search and restore previous buffer.
    pub fn cancel_search(&mut self) {
        self.mode = EditorMode::Normal;
    }

    /// Get the search query (for display in the prompt).
    pub fn search_query(&self) -> Option<&str> {
        if let EditorMode::ReverseSearch { ref query, .. } = self.mode {
            Some(query)
        } else {
            None
        }
    }

    // ─── Buffer access ──────────────────────────────────────────

    /// Take the current text and clear the editor.
    pub fn take_text(&mut self) -> String {
        self.mode = EditorMode::Normal;
        let text = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        text
    }

    pub fn cursor_position(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }

    /// Set the buffer text (for history navigation).
    pub fn set_text(&mut self, text: &str) {
        self.buffer = text.to_string();
        self.cursor = self.buffer.len();
    }

    pub fn render_text(&self) -> &str {
        &self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    // ─── Internal helpers ───────────────────────────────────────

    /// Find the start of the previous word (for word-backward operations).
    fn word_boundary_backward(&self) -> usize {
        let bytes = self.buffer.as_bytes();
        let mut pos = self.cursor;
        // Skip trailing whitespace/punctuation
        while pos > 0 && !bytes[pos - 1].is_ascii_alphanumeric() && bytes[pos - 1] != b'_' {
            pos -= 1;
        }
        // Skip the word itself
        while pos > 0 && (bytes[pos - 1].is_ascii_alphanumeric() || bytes[pos - 1] == b'_') {
            pos -= 1;
        }
        pos
    }

    /// Find the end of the next word (for word-forward operations).
    fn word_boundary_forward(&self) -> usize {
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        let mut pos = self.cursor;
        // Skip the current word
        while pos < len && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
            pos += 1;
        }
        // Skip trailing whitespace/punctuation
        while pos < len && !bytes[pos].is_ascii_alphanumeric() && bytes[pos] != b'_' {
            pos += 1;
        }
        pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_insert_and_take() {
        let mut e = Editor::new();
        e.insert('h');
        e.insert('i');
        assert_eq!(e.render_text(), "hi");
        assert_eq!(e.take_text(), "hi");
        assert_eq!(e.render_text(), "");
    }

    #[test]
    fn backspace() {
        let mut e = Editor::new();
        e.insert('a');
        e.insert('b');
        e.insert('c');
        e.backspace();
        assert_eq!(e.render_text(), "ab");
    }

    #[test]
    fn cursor_movement() {
        let mut e = Editor::new();
        e.insert('a');
        e.insert('b');
        e.insert('c');
        e.move_left();
        e.insert('x');
        assert_eq!(e.render_text(), "abxc");
    }

    #[test]
    fn home_end() {
        let mut e = Editor::new();
        e.set_text("abc");
        e.move_home();
        e.insert('0');
        assert_eq!(e.render_text(), "0abc");
        e.move_end();
        e.insert('9');
        assert_eq!(e.render_text(), "0abc9");
    }

    #[test]
    fn delete_word_backward() {
        let mut e = Editor::new();
        e.set_text("hello world foo");
        e.delete_word_backward();
        assert_eq!(e.render_text(), "hello world ");
        e.delete_word_backward();
        assert_eq!(e.render_text(), "hello ");
        e.delete_word_backward();
        assert_eq!(e.render_text(), "");
    }

    #[test]
    fn delete_word_forward() {
        let mut e = Editor::new();
        e.set_text("hello world foo");
        e.move_home();
        e.delete_word_forward();
        // Deletes "hello" (word) then " " (trailing non-word) = "hello "
        assert_eq!(e.render_text(), "world foo");
    }

    #[test]
    fn move_word_backward() {
        let mut e = Editor::new();
        e.set_text("hello world");
        e.move_word_backward();
        assert_eq!(e.cursor_position(), 6); // before "world"
    }

    #[test]
    fn move_word_forward() {
        let mut e = Editor::new();
        e.set_text("hello world");
        e.move_home();
        e.move_word_forward();
        assert_eq!(e.cursor_position(), 6); // after "hello "
    }

    #[test]
    fn kill_to_end() {
        let mut e = Editor::new();
        e.set_text("hello world");
        e.move_home();
        for _ in 0..5 { e.move_right(); }
        e.kill_to_end();
        assert_eq!(e.render_text(), "hello");
        assert_eq!(e.kill_ring.as_deref(), Some(" world"));
    }

    #[test]
    fn clear_line() {
        let mut e = Editor::new();
        e.set_text("hello");
        e.clear_line();
        assert_eq!(e.render_text(), "");
        assert_eq!(e.kill_ring.as_deref(), Some("hello"));
    }

    #[test]
    fn yank() {
        let mut e = Editor::new();
        e.set_text("hello world");
        e.delete_word_backward();
        assert_eq!(e.render_text(), "hello ");
        e.yank();
        assert_eq!(e.render_text(), "hello world");
    }

    #[test]
    fn reverse_search() {
        let history = vec![
            "cargo build".to_string(),
            "cargo test".to_string(),
            "git status".to_string(),
            "cargo clippy".to_string(),
        ];
        let mut e = Editor::new();
        e.start_reverse_search();
        assert!(matches!(e.mode(), EditorMode::ReverseSearch { .. }));

        // Type "test"
        e.insert('t');
        e.insert('e');
        e.insert('s');
        e.insert('t');
        let result = e.search_update(&history);
        assert_eq!(result.as_deref(), Some("cargo test"));

        // Accept
        e.accept_search(&history);
        assert_eq!(e.render_text(), "cargo test");
        assert!(matches!(e.mode(), EditorMode::Normal));
    }

    #[test]
    fn reverse_search_cancel() {
        let mut e = Editor::new();
        e.set_text("original");
        e.start_reverse_search();
        e.insert('x');
        e.cancel_search();
        assert_eq!(e.render_text(), "original");
        assert!(matches!(e.mode(), EditorMode::Normal));
    }

    #[test]
    fn reverse_search_backspace() {
        let history = vec!["cargo test".to_string()];
        let mut e = Editor::new();
        e.start_reverse_search();
        e.insert('t');
        e.insert('e');
        e.insert('s');
        assert_eq!(e.search_query(), Some("tes"));
        e.backspace();
        assert_eq!(e.search_query(), Some("te"));
    }

    #[test]
    fn unicode_handling() {
        let mut e = Editor::new();
        e.insert('é');
        e.insert('→');
        assert_eq!(e.render_text(), "é→");
        e.backspace();
        assert_eq!(e.render_text(), "é");
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut e = Editor::new();
        e.backspace();
        e.insert('a');
        e.move_home();
        e.backspace();
        assert_eq!(e.render_text(), "a");
    }

    #[test]
    fn word_boundaries_with_path() {
        let mut e = Editor::new();
        e.set_text("src/main.rs");
        e.delete_word_backward();
        // Should delete "rs" (stops at .)
        assert_eq!(e.render_text(), "src/main.");
    }
}
