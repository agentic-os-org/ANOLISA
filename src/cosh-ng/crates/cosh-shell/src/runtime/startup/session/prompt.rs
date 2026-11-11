use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub(super) fn clear_initial_prompt<W: Write>(prompt: &[u8], output: &mut W) -> io::Result<()> {
    let (columns, height) = ratatui::crossterm::terminal::size().unwrap_or((100, 40));
    let Some(rows) = linear_prompt_rows(prompt, columns, height) else {
        return write!(output, "\r\n");
    };
    write!(output, "\r\x1b[2K")?;
    for _ in 0..rows {
        write!(output, "\x1b[1A\r\x1b[2K")?;
    }
    Ok(())
}

fn linear_prompt_rows(prompt: &[u8], columns: u16, height: u16) -> Option<u16> {
    let mut chars = std::str::from_utf8(prompt).ok()?.chars().peekable();
    let mut row = 0;
    let mut column: usize = 0;
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.next()? {
                '[' => {
                    let mut sequence = String::new();
                    loop {
                        let ch = chars.next()?;
                        sequence.push(ch);
                        if ('\u{40}'..='\u{7e}').contains(&ch) {
                            break;
                        }
                        if sequence.len() > 32 {
                            return None;
                        }
                    }
                    if !sequence.ends_with('m') && !matches!(sequence.as_str(), "?2004h" | "?2004l")
                    {
                        return None;
                    }
                }
                ']' => loop {
                    match chars.next()? {
                        '\x07' => break,
                        '\x1b' if chars.next()? == '\\' => break,
                        _ => {}
                    }
                },
                _ => return None,
            },
            '\n' => {
                row += 1;
                column = 0;
            }
            '\r' => column = 0,
            '\t' => column += 8 - column % 8,
            '\x01' | '\x02' => {}
            ch if ch.is_control() => return None,
            ch => column += UnicodeWidthChar::width(ch).unwrap_or(0),
        }
        // Wrapped or cursor-addressed prompts have no reliable rewind origin.
        if column >= usize::from(columns) || row >= height {
            return None;
        }
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cleanup_counts_linear_rows_and_ignores_color_and_titles() {
        assert_eq!(
            linear_prompt_rows(b"\x1b]0;title\x07\x1b[32mrepo\x1b[0m\r\nnative$ ", 40, 24),
            Some(1)
        );
        assert_eq!(
            linear_prompt_rows("目录\r\n\x1b[?2004hnative$ ".as_bytes(), 40, 24),
            Some(1)
        );
    }

    #[test]
    fn prompt_cleanup_does_not_guess_wraps_cursor_addressing_or_scrollback() {
        assert_eq!(linear_prompt_rows(b"0123456789", 8, 24), None);
        assert_eq!(linear_prompt_rows(b"\x1b[2Anative$ ", 40, 24), None);
        assert_eq!(linear_prompt_rows(b"one\r\ntwo\r\nthree$ ", 40, 2), None);
        assert_eq!(linear_prompt_rows(b"\x1b[31", 40, 24), None);
    }
}
