/// Hand-rolled, non-modal multi-line text buffer backing the Playground
/// tab's SQL input. `tui-textarea` (the standard ratatui multi-line editor
/// crate) is stuck on ratatui 0.29 and won't compile against this repo's
/// ratatui 0.30.2 pin; `edtui` does support 0.30 but is a Vim-modal editor
/// (normal/insert/visual modes), a paradigm nothing else in this app uses —
/// hand-rolling this small buffer keeps the UX non-modal and avoids both.
pub struct SqlEditor {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize, // char index, not byte index — see `byte_index`
}

impl Default for SqlEditor {
    fn default() -> Self {
        Self { lines: vec![String::new()], cursor_row: 0, cursor_col: 0 }
    }
}

impl SqlEditor {
    /// Bridges a char-index cursor column to the byte offset `String::insert`/
    /// `remove`/`split_off` need — char indices (not byte offsets) so a
    /// pasted non-ASCII literal can't panic a byte-slice op mid-character.
    fn byte_index(line: &str, char_idx: usize) -> usize {
        line.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(line.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = Self::byte_index(line, self.cursor_col);
        line.insert(byte_idx, c);
        self.cursor_col += 1;
    }

    /// Deletes the char before the cursor, or — at column 0 of a non-first
    /// line — merges the current line into the previous one.
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = Self::byte_index(line, self.cursor_col - 1);
            line.remove(byte_idx);
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev_len = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&current);
            self.cursor_col = prev_len;
        }
    }

    /// Splits the current line at the cursor into two lines.
    pub fn newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = Self::byte_index(line, self.cursor_col);
        let tail = line.split_off(byte_idx);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// No sticky-column memory (a vim/editor refinement where the cursor
    /// remembers its pre-clamp column across several short lines) — simplest
    /// correct behavior, clamps to whatever the target line can hold.
    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].chars().count());
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].chars().count());
        }
    }

    pub fn home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Column of the start of the word left of the cursor — skips any
    /// whitespace/punctuation run first, then the word itself. Readline's
    /// (and nvim insert-mode's) `Ctrl+W` word definition, not vim normal-mode's
    /// punctuation-is-its-own-word rule — simpler, good enough for SQL text.
    fn word_left_col(&self) -> usize {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let mut i = self.cursor_col;
        while i > 0 && !Self::is_word_char(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && Self::is_word_char(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    /// Ctrl+Left (nvim's `b`): jumps to the start of the previous word. At
    /// column 0, falls back to a plain `move_left` (onto the previous line)
    /// rather than special-casing line-crossing word jumps.
    pub fn move_word_left(&mut self) {
        if self.cursor_col == 0 {
            self.move_left();
            return;
        }
        self.cursor_col = self.word_left_col();
    }

    /// Ctrl+Right (nvim's `w`): jumps to the start of the next word.
    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let len = chars.len();
        if self.cursor_col >= len {
            self.move_right();
            return;
        }
        let mut i = self.cursor_col;
        while i < len && Self::is_word_char(chars[i]) {
            i += 1;
        }
        while i < len && !Self::is_word_char(chars[i]) {
            i += 1;
        }
        self.cursor_col = i;
    }

    /// Ctrl+Home (nvim's `gg`): jump to the very start of the buffer.
    pub fn move_to_buffer_start(&mut self) {
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Ctrl+End (nvim's `G`): jump to the very end of the buffer.
    pub fn move_to_buffer_end(&mut self) {
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    /// Ctrl+W (readline/nvim-insert-mode): deletes the word before the
    /// cursor. At column 0, merges into the previous line instead (same
    /// fallback as `move_word_left`).
    pub fn delete_word_backward(&mut self) {
        if self.cursor_col == 0 {
            self.backspace();
            return;
        }
        let start = self.word_left_col();
        let line = &mut self.lines[self.cursor_row];
        let from = Self::byte_index(line, start);
        let to = Self::byte_index(line, self.cursor_col);
        line.replace_range(from..to, "");
        self.cursor_col = start;
    }

    /// Ctrl+U (readline's unix-line-discard): deletes from the cursor back
    /// to the start of the current line.
    pub fn delete_to_line_start(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let to = Self::byte_index(line, self.cursor_col);
        line.replace_range(0..to, "");
        self.cursor_col = 0;
    }

    /// Ctrl+K (readline's kill-line): deletes from the cursor to the end of
    /// the current line. Doesn't cross into the next line, matching
    /// readline's own kill-line.
    pub fn delete_to_line_end(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let from = Self::byte_index(line, self.cursor_col);
        line.truncate(from);
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Replaces the char range `[start_col, cursor_col)` on the current line
    /// with `replacement` and moves the cursor to the end of the inserted
    /// text — used by Tab-completion (`main.rs::playground_autocomplete`) to
    /// swap a partially-typed word for a chosen candidate.
    pub fn replace_current_word(&mut self, start_col: usize, replacement: &str) {
        let start_col = start_col.min(self.cursor_col);
        let line = &mut self.lines[self.cursor_row];
        let from = Self::byte_index(line, start_col);
        let to = Self::byte_index(line, self.cursor_col);
        line.replace_range(from..to, replacement);
        self.cursor_col = start_col + replacement.chars().count();
    }

    /// Replaces the whole buffer with `text`, cursor at the end — used by
    /// history recall (Up/Down), which loads a past command wholesale
    /// rather than being typed in.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace_roundtrip() {
        let mut ed = SqlEditor::default();
        for c in "abc".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.text(), "abc");
        ed.backspace();
        ed.backspace();
        ed.backspace();
        assert_eq!(ed.text(), "");
        assert!(ed.is_empty());
    }

    #[test]
    fn newline_splits_line_and_repositions_cursor() {
        let mut ed = SqlEditor::default();
        for c in "SELECT 1".chars() {
            ed.insert_char(c);
        }
        // Cursor is after "SELECT" (6 chars), split there.
        ed.cursor_col = 6;
        ed.newline();
        assert_eq!(ed.lines(), ["SELECT", " 1"]);
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (1, 0));
    }

    #[test]
    fn backspace_at_column_zero_merges_into_previous_line() {
        let mut ed = SqlEditor::default();
        for c in "ab".chars() {
            ed.insert_char(c);
        }
        ed.newline();
        for c in "cd".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.lines(), ["ab", "cd"]);

        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.lines(), ["abcd"]);
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (0, 2));
    }

    #[test]
    fn vertical_move_clamps_column_to_target_line_length() {
        let mut ed = SqlEditor::default();
        for c in "ab".chars() {
            ed.insert_char(c);
        }
        ed.newline();
        for c in "abcdef".chars() {
            ed.insert_char(c);
        }
        // On row 1 ("abcdef"), column 4.
        ed.cursor_col = 4;
        ed.move_up();
        // Row 0 ("ab") only has 2 chars — column clamps to 2, not 4.
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (0, 2));
    }

    fn typed(s: &str) -> SqlEditor {
        let mut ed = SqlEditor::default();
        for c in s.chars() {
            ed.insert_char(c);
        }
        ed
    }

    #[test]
    fn move_word_left_and_right_skip_whitespace_runs() {
        let mut ed = typed("select  foo");
        ed.move_word_left();
        assert_eq!(ed.cursor_col(), 8); // start of "foo"
        ed.move_word_left();
        assert_eq!(ed.cursor_col(), 0); // start of "select"
        ed.move_word_right();
        assert_eq!(ed.cursor_col(), 8); // start of "foo" (vim `w`: next word start, not current word's end)
        ed.move_word_right();
        assert_eq!(ed.cursor_col(), 11); // no next word — lands at end of line
    }

    #[test]
    fn delete_word_backward_removes_just_the_preceding_word() {
        let mut ed = typed("select foo");
        ed.delete_word_backward();
        assert_eq!(ed.text(), "select ");
        assert_eq!(ed.cursor_col(), 7);
    }

    #[test]
    fn delete_to_line_start_and_end_split_on_cursor() {
        let mut ed = typed("select foo");
        ed.cursor_col = 7;
        ed.delete_to_line_start();
        assert_eq!(ed.text(), "foo");
        assert_eq!(ed.cursor_col(), 0);

        let mut ed = typed("select foo");
        ed.cursor_col = 6;
        ed.delete_to_line_end();
        assert_eq!(ed.text(), "select");
    }

    #[test]
    fn replace_current_word_swaps_partial_word_and_moves_cursor() {
        let mut ed = typed("select * from us");
        ed.replace_current_word(14, "users");
        assert_eq!(ed.text(), "select * from users");
        assert_eq!(ed.cursor_col(), 19);
    }

    #[test]
    fn set_text_replaces_buffer_and_moves_cursor_to_end() {
        let mut ed = typed("select 1");
        ed.cursor_col = 3;
        ed.set_text("select *\nfrom foo");
        assert_eq!(ed.lines(), ["select *", "from foo"]);
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (1, 8));

        ed.set_text("");
        assert!(ed.is_empty());
    }

    #[test]
    fn buffer_start_and_end_jump_across_lines() {
        let mut ed = typed("ab");
        ed.newline();
        for c in "cdef".chars() {
            ed.insert_char(c);
        }
        ed.move_to_buffer_start();
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (0, 0));
        ed.move_to_buffer_end();
        assert_eq!((ed.cursor_row(), ed.cursor_col()), (1, 4));
    }
}
