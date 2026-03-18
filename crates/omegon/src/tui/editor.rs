//! Single-line text editor with basic line editing.

/// A minimal single-line text editor.
pub struct Editor {
    buffer: String,
    cursor: usize,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    pub fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find the previous char boundary
            let mut new_cursor = self.cursor - 1;
            while new_cursor > 0 && !self.buffer.is_char_boundary(new_cursor) {
                new_cursor -= 1;
            }
            self.buffer.drain(new_cursor..self.cursor);
            self.cursor = new_cursor;
        }
    }

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

    /// Take the current text and clear the editor.
    pub fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        text
    }

    pub fn cursor_position(&self) -> usize {
        // Count the display width (chars, not bytes)
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
        e.backspace();
        assert_eq!(e.render_text(), "a");
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
        e.insert('a');
        e.insert('b');
        e.insert('c');
        e.move_home();
        e.insert('0');
        assert_eq!(e.render_text(), "0abc");
        e.move_end();
        e.insert('9');
        assert_eq!(e.render_text(), "0abc9");
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut e = Editor::new();
        e.backspace(); // shouldn't panic
        e.insert('a');
        e.move_home();
        e.backspace(); // at position 0, noop
        assert_eq!(e.render_text(), "a");
    }

    #[test]
    fn unicode_handling() {
        let mut e = Editor::new();
        e.insert('é');
        e.insert('→');
        assert_eq!(e.render_text(), "é→");
        e.backspace();
        assert_eq!(e.render_text(), "é");
        assert_eq!(e.cursor_position(), 1);
    }
}
