//! Multi-line prompt draft editor state (#1721 D13/D14).
//!
//! Pure data model for the V2 draft card: line/cursor bookkeeping with the
//! basic editing verbs (arrows with a desired column, Home/End, insert and
//! delete at the cursor, Ctrl+U to line start) and an 8-line viewport that
//! follows the cursor. Rendering and key decoding live elsewhere; every
//! method here is deterministic and unit-testable.

/// Maximum number of draft lines shown at once (D14); the viewport scrolls
/// with the cursor and the panel reports how many lines are hidden.
pub(crate) const DRAFT_VIEWPORT_ROWS: usize = 8;

/// Byte budget shared with the candidate line buffer (matrix #10).
pub(crate) const DRAFT_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PromptDraftEditor {
    lines: Vec<String>,
    row: usize,
    /// Cursor position in characters within `lines[row]`.
    col: usize,
    /// Column remembered across vertical moves (D14).
    desired_col: Option<usize>,
}

/// Viewport slice reported to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftViewport {
    pub(crate) first_row: usize,
    pub(crate) rows: Vec<String>,
    pub(crate) hidden_above: usize,
    pub(crate) hidden_below: usize,
    /// Cursor position relative to `rows` (row index, character column).
    pub(crate) cursor: (usize, usize),
}

impl PromptDraftEditor {
    pub(crate) fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self {
            lines,
            row,
            col,
            desired_col: None,
        }
    }

    pub(crate) fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.lines.iter().all(|line| line.trim().is_empty())
    }

    fn byte_len(&self) -> usize {
        self.lines.iter().map(String::len).sum::<usize>() + self.lines.len().saturating_sub(1)
    }

    fn byte_index(&self) -> usize {
        self.lines[self.row]
            .char_indices()
            .map(|(index, _)| index)
            .nth(self.col)
            .unwrap_or(self.lines[self.row].len())
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        if self.byte_len() + ch.len_utf8() > DRAFT_MAX_BYTES {
            return;
        }
        let index = self.byte_index();
        self.lines[self.row].insert(index, ch);
        self.col += 1;
        self.desired_col = None;
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                self.insert_newline();
            } else if !ch.is_control() || ch == '\t' {
                self.insert_char(ch);
            }
        }
    }

    pub(crate) fn replace_current_token(&mut self, replacement: &str) -> bool {
        if replacement.chars().any(|ch| ch == '\n' || ch == '\r') {
            return false;
        }
        let chars = self.lines[self.row].chars().collect::<Vec<_>>();
        let start_col = chars[..self.col]
            .iter()
            .rposition(|ch| ch.is_whitespace())
            .map_or(0, |index| index + 1);
        let end_col = chars[self.col..]
            .iter()
            .position(|ch| ch.is_whitespace())
            .map_or(chars.len(), |index| self.col + index);
        let start: usize = chars[..start_col].iter().map(|ch| ch.len_utf8()).sum();
        let end: usize = chars[..end_col].iter().map(|ch| ch.len_utf8()).sum();
        let new_len = self.byte_len() - (end - start) + replacement.len();
        if new_len > DRAFT_MAX_BYTES {
            return false;
        }
        self.lines[self.row].replace_range(start..end, replacement);
        self.col = start_col + replacement.chars().count();
        self.desired_col = None;
        true
    }

    pub(crate) fn insert_newline(&mut self) {
        if self.byte_len() + 1 > DRAFT_MAX_BYTES {
            return;
        }
        let index = self.byte_index();
        let rest = self.lines[self.row].split_off(index);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
        self.desired_col = None;
    }

    pub(crate) fn backspace(&mut self) {
        if self.col > 0 {
            let index = self.byte_index();
            let previous = self.lines[self.row][..index]
                .char_indices()
                .next_back()
                .map(|(start, _)| start)
                .unwrap_or(0);
            self.lines[self.row].replace_range(previous..index, "");
            self.col -= 1;
        } else if self.row > 0 {
            let current = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&current);
        }
        self.desired_col = None;
    }

    pub(crate) fn delete_forward(&mut self) {
        let line_chars = self.lines[self.row].chars().count();
        if self.col < line_chars {
            let index = self.byte_index();
            let end = self.lines[self.row][index..]
                .chars()
                .next()
                .map(|ch| index + ch.len_utf8())
                .unwrap_or(index);
            self.lines[self.row].replace_range(index..end, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
        self.desired_col = None;
    }

    /// Ctrl+U: delete from line start to the cursor (bash muscle memory).
    pub(crate) fn kill_to_line_start(&mut self) {
        let index = self.byte_index();
        self.lines[self.row].replace_range(..index, "");
        self.col = 0;
        self.desired_col = None;
    }

    pub(crate) fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
        self.desired_col = None;
    }

    pub(crate) fn move_right(&mut self) {
        if self.col < self.lines[self.row].chars().count() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
        self.desired_col = None;
    }

    pub(crate) fn move_up(&mut self) {
        if self.row == 0 {
            return;
        }
        let desired = *self.desired_col.get_or_insert(self.col);
        self.row -= 1;
        self.col = desired.min(self.lines[self.row].chars().count());
    }

    pub(crate) fn move_down(&mut self) {
        if self.row + 1 >= self.lines.len() {
            return;
        }
        let desired = *self.desired_col.get_or_insert(self.col);
        self.row += 1;
        self.col = desired.min(self.lines[self.row].chars().count());
    }

    pub(crate) fn move_line_start(&mut self) {
        self.col = 0;
        self.desired_col = None;
    }

    pub(crate) fn move_line_end(&mut self) {
        self.col = self.lines[self.row].chars().count();
        self.desired_col = None;
    }

    /// Cursor-following viewport capped at [`DRAFT_VIEWPORT_ROWS`].
    pub(crate) fn viewport(&self) -> DraftViewport {
        let total = self.lines.len();
        let rows = DRAFT_VIEWPORT_ROWS.min(total);
        let first_row = if self.row < rows {
            0
        } else {
            (self.row + 1 - rows).min(total - rows)
        };
        DraftViewport {
            first_row,
            rows: self.lines[first_row..first_row + rows].to_vec(),
            hidden_above: first_row,
            hidden_below: total - first_row - rows,
            cursor: (self.row - first_row, self.col),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> PromptDraftEditor {
        PromptDraftEditor::from_text(text)
    }

    #[test]
    fn from_text_places_cursor_at_end() {
        let editor = editor("第一行\n第二行");
        assert_eq!(editor.line_count(), 2);
        assert_eq!(editor.text(), "第一行\n第二行");
        assert_eq!(editor.viewport().cursor, (1, 3));
    }

    #[test]
    fn vertical_moves_remember_the_desired_column() {
        let mut editor = editor("很长的第一行内容\n短行\n另一个很长的行内容");
        editor.move_line_end();
        editor.move_up();
        assert_eq!(
            editor.viewport().cursor,
            (1, 2),
            "clamped to the short line"
        );
        editor.move_up();
        assert_eq!(
            editor.viewport().cursor,
            (0, 8),
            "desired column restored on the long line"
        );
    }

    #[test]
    fn backspace_at_line_start_joins_lines() {
        let mut editor = editor("前段\n后段");
        editor.move_line_start();
        editor.backspace();
        assert_eq!(editor.text(), "前段后段");
        assert_eq!(editor.viewport().cursor, (0, 2));
    }

    #[test]
    fn delete_forward_at_line_end_joins_next_line() {
        let mut editor = editor("前段\n后段");
        editor.move_up();
        editor.move_line_end();
        editor.delete_forward();
        assert_eq!(editor.text(), "前段后段");
    }

    #[test]
    fn kill_to_line_start_only_clears_before_cursor() {
        let mut editor = editor("保留删除");
        editor.move_line_start();
        editor.move_right();
        editor.move_right();
        editor.kill_to_line_start();
        assert_eq!(editor.text(), "删除");
    }

    #[test]
    fn insert_newline_splits_at_cursor() {
        let mut editor = editor("上下");
        editor.move_line_start();
        editor.move_right();
        editor.insert_newline();
        assert_eq!(editor.text(), "上\n下");
        assert_eq!(editor.viewport().cursor, (1, 0));
    }

    #[test]
    fn viewport_follows_the_cursor_beyond_eight_lines() {
        let text = (1..=12)
            .map(|n| format!("行{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = editor(&text);
        let view = editor.viewport();
        assert_eq!(view.rows.len(), DRAFT_VIEWPORT_ROWS);
        assert_eq!(view.hidden_above, 4);
        assert_eq!(view.hidden_below, 0);
        assert_eq!(view.cursor.0, DRAFT_VIEWPORT_ROWS - 1);

        for _ in 0..11 {
            editor.move_up();
        }
        let view = editor.viewport();
        assert_eq!(view.hidden_above, 0);
        assert_eq!(view.hidden_below, 4);
        // Desired column 3 clamps to the two-character first line.
        assert_eq!(view.cursor, (0, 2));
    }

    #[test]
    fn byte_budget_rejects_overflow_without_panicking() {
        let mut editor = editor(&"a".repeat(DRAFT_MAX_BYTES - 1));
        editor.insert_char('b');
        assert_eq!(editor.text().len(), DRAFT_MAX_BYTES);
        editor.insert_char('c');
        assert_eq!(editor.text().len(), DRAFT_MAX_BYTES, "budget must hold");
        editor.insert_newline();
        assert_eq!(editor.line_count(), 1, "newline also respects the budget");
    }

    #[test]
    fn blank_detection_covers_whitespace_only_drafts() {
        assert!(editor("").is_blank());
        assert!(editor(" \n\u{3000}").is_blank());
        assert!(!editor("正文").is_blank());
    }

    #[test]
    fn completion_replaces_the_token_at_the_cursor() {
        let mut editor = editor("review @sr now");
        editor.move_line_start();
        for _ in 0..10 {
            editor.move_right();
        }
        assert!(editor.replace_current_token("@src/"));
        assert_eq!(editor.text(), "review @src/ now");
        assert_eq!(editor.viewport().cursor, (0, 12));
    }
}
