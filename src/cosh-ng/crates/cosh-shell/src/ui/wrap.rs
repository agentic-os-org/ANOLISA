//! Text-width and line-wrapping helpers for terminal UI output.

use ratatui::text::{Line, Span};

pub(super) fn line_is_empty(line: &Line<'static>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

pub(super) fn line_to_string(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}

pub(super) fn ordered_list_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let marker_end = trimmed.find(". ")?;
    if marker_end == 0 || !trimmed[..marker_end].chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((&trimmed[..marker_end + 2], &trimmed[marker_end + 2..]))
}

pub(crate) fn wrap_plain_line(line: &str, width: usize) -> Vec<String> {
    if line.trim().is_empty() {
        return vec![String::new()];
    }

    let (first_prefix, rest_prefix, text) = split_prefix(line);
    wrap_with_prefix(text, &first_prefix, &rest_prefix, width)
}

pub(crate) fn wrap_plain_line_with_prefix(
    text: &str,
    first_prefix: &str,
    rest_prefix: &str,
    width: usize,
) -> Vec<String> {
    wrap_with_prefix(text, first_prefix, rest_prefix, width)
}

pub(super) fn compact_rendered_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn split_prefix(line: &str) -> (String, String, &str) {
    if let Some((indent, marker, rest)) = list_item_prefix(line) {
        let first_prefix = format!("{indent}{marker}");
        let rest_prefix = format!("{indent}{}", " ".repeat(marker.len()));
        (first_prefix, rest_prefix, rest)
    } else if let Some(rest) = line.strip_prefix("> ") {
        ("> ".to_string(), "  ".to_string(), rest)
    } else if line.starts_with("  ") {
        // Preserve arbitrary-width space indentation (2, 4, 6, ... columns)
        // instead of collapsing everything to two spaces, so nested panel
        // hierarchies survive wrapping with a matching hanging indent. Only
        // consecutive ASCII spaces count as indentation: control whitespace
        // (\r, \t, \n) after the spaces must never enter the hanging prefix,
        // and the body keeps the legacy full leading-whitespace cleanup.
        let indent_len = line.len() - line.trim_start_matches(' ').len();
        let indent = &line[..indent_len];
        (indent.to_string(), indent.to_string(), line.trim_start())
    } else {
        ("".to_string(), "".to_string(), line.trim_start())
    }
}

fn list_item_prefix(line: &str) -> Option<(&str, &str, &str)> {
    let indent_len = line
        .char_indices()
        .take_while(|(_, ch)| *ch == ' ')
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    if let Some(item) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
        return Some((indent, "- ", item));
    }
    let (marker, item) = ordered_list_item(rest)?;
    Some((indent, marker, item))
}

fn wrap_with_prefix(
    text: &str,
    first_prefix: &str,
    rest_prefix: &str,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut prefix = first_prefix;
    let mut current = String::new();
    let mut current_width = display_width(prefix);
    let max_width = width.max(current_width + 1);

    current.push_str(prefix);
    for mut token in split_wrap_tokens(text) {
        if token == "\n" {
            lines.push(current.trim_end().to_string());
            prefix = rest_prefix;
            current.clear();
            current.push_str(prefix);
            current_width = display_width(prefix);
            continue;
        }

        if current.trim() == prefix.trim() {
            token = token.trim_start().to_string();
        }

        let token_width = display_width(&token);
        if token_width > 0 && current_width + token_width > max_width {
            if current.trim() != prefix.trim() {
                lines.push(current.trim_end().to_string());
                prefix = rest_prefix;
                current.clear();
                current.push_str(prefix);
                current_width = display_width(prefix);
                token = token.trim_start().to_string();
            }

            if display_width(&token) + current_width > max_width {
                let wrapped =
                    wrap_long_token(&token, rest_prefix, max_width, current, current_width);
                lines.extend(wrapped.finished_lines);
                current = wrapped.current_line;
                current_width = wrapped.current_width;
                prefix = rest_prefix;
                continue;
            }
        }

        current.push_str(&token);
        current_width += display_width(&token);
    }

    if !current.trim().is_empty() || lines.is_empty() {
        lines.push(current.trim_end().to_string());
    }
    lines
}

fn split_wrap_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_kind = None;

    for ch in text.chars() {
        if ch == '\n' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push("\n".to_string());
            current_kind = None;
            continue;
        }

        let next_kind = if ch.is_whitespace() {
            Some(true)
        } else if is_cjk_breakable(ch) {
            None
        } else {
            Some(false)
        };
        if current_kind.is_some() && current_kind != next_kind && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }

        current.push(ch);
        current_kind = next_kind;

        if next_kind.is_none() {
            tokens.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    bind_line_punctuation(tokens)
}

fn bind_line_punctuation(tokens: Vec<String>) -> Vec<String> {
    let mut bound = Vec::<String>::with_capacity(tokens.len());

    for token in tokens {
        let is_text = token != "\n" && !token.chars().all(char::is_whitespace);
        if is_text
            && token
                .chars()
                .next()
                .is_some_and(is_line_closing_punctuation)
        {
            if let Some(previous) = bound.last_mut() {
                if previous != "\n" && !previous.chars().all(char::is_whitespace) {
                    previous.push_str(&token);
                    continue;
                }
            }
        }

        if is_text {
            if let Some(previous) = bound.last_mut() {
                if !previous.is_empty() && previous.chars().all(is_line_opening_punctuation) {
                    previous.push_str(&token);
                    continue;
                }
            }
        }

        bound.push(token);
    }
    bound
}

pub(super) fn is_cjk_breakable(ch: char) -> bool {
    matches!(
        ch,
        '\u{2e80}'..='\u{2fff}'
            | '\u{3000}'..='\u{303f}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{3100}'..='\u{31ff}'
            | '\u{3200}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{a960}'..='\u{a97f}'
            | '\u{ac00}'..='\u{d7ff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe30}'..='\u{fe4f}'
            | '\u{ff00}'..='\u{ffef}'
            | '\u{20000}'..='\u{2fa1f}'
    )
}

struct WrappedToken {
    finished_lines: Vec<String>,
    current_line: String,
    current_width: usize,
}

fn wrap_long_token(
    token: &str,
    rest_prefix: &str,
    max_width: usize,
    mut current: String,
    mut current_width: usize,
) -> WrappedToken {
    let mut finished_lines = Vec::new();

    for ch in token.chars() {
        let ch_width = char_width(ch);
        if ch_width > 0 && current_width + ch_width > max_width {
            finished_lines.push(current.trim_end().to_string());
            current.clear();
            current.push_str(rest_prefix);
            current_width = display_width(rest_prefix);
        }
        current.push(ch);
        current_width += ch_width;
    }

    WrappedToken {
        finished_lines,
        current_line: current,
        current_width,
    }
}

pub(super) fn strip_ansi_escape(text: &str) -> String {
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            stripped.push(ch);
            continue;
        }

        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    stripped
}

pub(crate) fn display_width(text: &str) -> usize {
    Span::raw(strip_ansi_escape(text)).width()
}

pub(super) fn char_width(ch: char) -> usize {
    let mut text = [0; 4];
    display_width(ch.encode_utf8(&mut text))
}

pub(super) fn should_buffer_word_char(ch: char) -> bool {
    ch.is_ascii() && !ch.is_whitespace() && !ch.is_control()
}

pub(super) fn is_line_closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '。' | '，'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | ')'
            | ']'
            | '}'
            | '）'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '”'
            | '’'
    )
}

fn is_line_opening_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '(' | '[' | '{' | '（' | '【' | '《' | '〈' | '「' | '『' | '“' | '‘'
    )
}
