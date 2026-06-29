//! A pure text-editing buffer: a `String` + a byte cursor at a char boundary,
//! with the edit/navigation operations a text surface needs. Kept pure so the
//! editing logic is unit-tested independently of gpui focus/layout/IME.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBuffer {
    pub text: String,
    /// Cursor as a byte offset into `text` (always on a char boundary).
    pub cursor: usize,
}

impl TextBuffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        TextBuffer { text, cursor }
    }

    fn prev_boundary(&self, from: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let mut i = from - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_boundary(&self, from: usize) -> usize {
        if from >= self.text.len() {
            return self.text.len();
        }
        let mut i = from + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    /// Insert `s` at the cursor; cursor advances past it.
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the char before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.prev_boundary(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Delete the char at the cursor (forward delete).
    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let end = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn move_left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
    }

    /// Byte offset of the start of the line containing `pos`.
    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    /// Byte offset of the end of the line containing `pos` (before the newline).
    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..].find('\n').map(|i| pos + i).unwrap_or(self.text.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    /// The (0-based) column of the cursor measured in chars from the line start.
    fn column(&self) -> usize {
        let ls = self.line_start(self.cursor);
        self.text[ls..self.cursor].chars().count()
    }

    pub fn move_up(&mut self) {
        let ls = self.line_start(self.cursor);
        if ls == 0 {
            self.cursor = 0;
            return;
        }
        let col = self.column();
        let prev_end = ls - 1; // the '\n' ending the previous line
        let prev_start = self.line_start(prev_end);
        self.cursor = self.offset_at_col(prev_start, prev_end, col);
    }

    pub fn move_down(&mut self) {
        let le = self.line_end(self.cursor);
        if le == self.text.len() {
            self.cursor = self.text.len();
            return;
        }
        let col = self.column();
        let next_start = le + 1;
        let next_end = self.line_end(next_start);
        self.cursor = self.offset_at_col(next_start, next_end, col);
    }

    /// Byte offset within `[start, end]` at char column `col` (clamped to the line).
    fn offset_at_col(&self, start: usize, end: usize, col: usize) -> usize {
        let mut pos = start;
        let mut c = 0;
        while pos < end && c < col {
            pos = self.next_boundary(pos);
            c += 1;
        }
        pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_places_cursor_at_end() {
        let b = TextBuffer::new("hello");
        assert_eq!(b.cursor, 5);
    }

    #[test]
    fn insert_and_backspace() {
        let mut b = TextBuffer::new("");
        b.insert("ab");
        b.insert("c");
        assert_eq!(b.text, "abc");
        assert_eq!(b.cursor, 3);
        b.backspace();
        assert_eq!(b.text, "ab");
        assert_eq!(b.cursor, 2);
        b.move_left();
        b.backspace();
        assert_eq!(b.text, "b");
        assert_eq!(b.cursor, 0);
        b.backspace(); // no-op at start
        assert_eq!(b.text, "b");
    }

    #[test]
    fn delete_forward() {
        let mut b = TextBuffer::new("abc");
        b.cursor = 0;
        b.delete();
        assert_eq!(b.text, "bc");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn insert_newline_and_navigate() {
        let mut b = TextBuffer::new("");
        b.insert("line one\nline two");
        // cursor at end (line two)
        b.home();
        assert_eq!(&b.text[b.cursor..], "line two");
        b.end();
        assert_eq!(b.cursor, b.text.len());
        // up keeps column ~ end of "line two" (8) → clamps to end of "line one" (8)
        b.move_up();
        assert_eq!(&b.text[..b.cursor], "line one");
    }

    #[test]
    fn up_down_preserve_column() {
        let mut b = TextBuffer::new("abcd\nef\nghij");
        // place cursor at column 3 on line 0
        b.cursor = 0;
        b.move_right();
        b.move_right();
        b.move_right(); // col 3 → "abc|d"
        b.move_down(); // line 1 "ef" len 2 → clamps to end (col 2)
        assert_eq!(b.cursor, 4 + 1 + 2); // start of line1(5)=after "abcd\n"; +2 → "ef|"
        b.move_down(); // line 2 "ghij", col 2 → "gh|ij"
        let ls = b.text.rfind('\n').unwrap() + 1;
        assert_eq!(b.cursor, ls + 2);
    }

    #[test]
    fn unicode_boundaries() {
        let mut b = TextBuffer::new("a😀b");
        // cursor at end; backspace removes 'b', then '😀' (4 bytes) as one char.
        b.backspace();
        assert_eq!(b.text, "a😀");
        b.backspace();
        assert_eq!(b.text, "a");
        assert_eq!(b.cursor, 1);
    }
}
