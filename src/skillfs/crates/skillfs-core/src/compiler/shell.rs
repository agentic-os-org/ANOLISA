//! Conservative cross-line exclusions for heuristic command normalization.

use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    SingleQuote,
    DoubleQuote,
    Backtick,
    Parenthesis,
    Arithmetic,
}

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
}

/// Exclude physical lines whose shell arguments or data span multiple lines.
#[derive(Default)]
pub(super) struct MultilineGuard {
    contexts: Vec<Context>,
    heredocs: VecDeque<Heredoc>,
    heredoc_body: bool,
    uncertain_heredoc: bool,
    heredoc_line: String,
    continued: bool,
    fence: Option<(u8, usize)>,
}

impl MultilineGuard {
    /// Protect entire regions because partial arguments cannot be judged safely.
    pub(super) fn can_normalize(&mut self, line: &str) -> bool {
        if self.heredoc_body {
            if let Some(heredoc) = self.heredocs.front() {
                let body = if heredoc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                // Unquoted heredocs remove backslash-newline before comparing
                // delimiters. A physical delimiter after an escaped newline
                // can still be data; a delimiter can also span physical lines.
                let continued = !heredoc.quoted
                    && body.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1;
                self.heredoc_line.push_str(if continued {
                    &body[..body.len() - 1]
                } else {
                    body
                });
                if continued {
                    return false;
                }
                if self.heredoc_line == heredoc.delimiter {
                    self.heredocs.pop_front();
                }
                self.heredoc_line.clear();
            }
            self.heredoc_body = !self.heredocs.is_empty();
            return false;
        }

        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied();
        if let Some(marker @ (b'`' | b'~')) = marker {
            let length = trimmed.bytes().take_while(|&b| b == marker).count();
            if let Some((open, minimum)) = self.fence {
                if marker == open && length >= minimum && trimmed[length..].trim().is_empty() {
                    // Shell state belongs to this Markdown example only.
                    *self = Self::default();
                    return false;
                }
            } else if length >= 3 {
                *self = Self {
                    fence: Some((marker, length)),
                    ..Self::default()
                };
                return false;
            }
        }

        if self.uncertain_heredoc {
            return false;
        }
        let was_continued = self.continued || !self.contexts.is_empty();
        self.continued = false;
        self.scan(line);
        if self.uncertain_heredoc {
            return false;
        }
        self.heredoc_body = !self.heredocs.is_empty() && !self.continued;
        !was_continued && !self.continued && self.contexts.is_empty() && self.heredocs.is_empty()
    }

    fn scan(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let ch = bytes[i];
            let context = self.contexts.last().copied();
            if context == Some(Context::SingleQuote) {
                if ch == b'\'' {
                    self.contexts.pop();
                }
            } else if ch == b'\\' {
                if i + 1 == bytes.len() {
                    self.continued = true;
                }
                i += 1;
            } else if context == Some(Context::DoubleQuote) && ch == b'"'
                || context == Some(Context::Backtick) && ch == b'`'
            {
                self.contexts.pop();
            } else if ch == b'`' {
                // Balanced Markdown spans must not leak shell quotes into
                // neighboring lines of prose. Fenced examples use shell ticks.
                let length = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                if self.fence.is_none() && context.is_none() {
                    let delimiter = "`".repeat(length);
                    if let Some(end) = line[i + length..].find(&delimiter) {
                        i += length + end + length;
                        continue;
                    }
                }
                self.contexts.push(Context::Backtick);
            } else if ch == b'$' && bytes.get(i + 1) == Some(&b'(') {
                if bytes.get(i + 2) == Some(&b'(') {
                    self.contexts.extend([Context::Arithmetic; 2]);
                    i += 1;
                } else {
                    self.contexts.push(Context::Parenthesis);
                }
                i += 1;
            } else if context != Some(Context::DoubleQuote) {
                match ch {
                    b'#' if i == 0 || bytes[i - 1].is_ascii_whitespace() => break,
                    b'\'' => self.contexts.push(Context::SingleQuote),
                    b'"' => self.contexts.push(Context::DoubleQuote),
                    b'(' => {
                        if context == Some(Context::Arithmetic) {
                            self.contexts.push(Context::Arithmetic);
                        } else if bytes.get(i + 1) == Some(&b'(') {
                            self.contexts.extend([Context::Arithmetic; 2]);
                            i += 1;
                        } else {
                            self.contexts.push(Context::Parenthesis);
                        }
                    }
                    b')' if matches!(context, Some(Context::Parenthesis | Context::Arithmetic)) => {
                        self.contexts.pop();
                    }
                    b'<' if context != Some(Context::Arithmetic)
                        && bytes.get(i + 1) == Some(&b'<') =>
                    {
                        // Here-strings (<<<) have no following body.
                        if bytes.get(i + 2) == Some(&b'<') {
                            i += 3;
                            continue;
                        }
                        if let Some((heredoc, end)) = parse_heredoc(line, i + 2) {
                            self.heredocs.push_back(heredoc);
                            i = end;
                            continue;
                        }
                        // A continued or incomplete delimiter may still start
                        // a body. Preserve this example until its fence ends.
                        self.uncertain_heredoc = true;
                        return;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }
}

// Shell quote removal applies to the delimiter word, including mixed quoted
// fragments. Keep the exact resulting delimiter: spaces cannot be trimmed.
fn parse_heredoc(line: &str, mut i: usize) -> Option<(Heredoc, usize)> {
    let bytes = line.as_bytes();
    let strip_tabs = bytes.get(i) == Some(&b'-');
    i += usize::from(strip_tabs);
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    let start = i;
    let mut delimiter = Vec::new();
    let mut quote = None;
    let mut quoted = false;
    while let Some(&ch) = bytes.get(i) {
        if ch == b'\\' && quote != Some(b'\'') {
            quoted = true;
            i += 1;
            let &escaped = bytes.get(i)?;
            if quote == Some(b'"') && !matches!(escaped, b'$' | b'`' | b'"' | b'\\') {
                delimiter.push(b'\\');
            }
            delimiter.push(escaped);
        } else if let Some(delim) = quote {
            if ch == delim {
                quote = None;
            } else {
                delimiter.push(ch);
            }
        } else if matches!(ch, b'\'' | b'"') {
            quoted = true;
            quote = Some(ch);
        } else if ch.is_ascii_whitespace()
            || matches!(ch, b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')')
        {
            break;
        } else {
            delimiter.push(ch);
        }
        i += 1;
    }
    if i == start || quote.is_some() {
        return None;
    }
    Some((
        Heredoc {
            delimiter: String::from_utf8(delimiter).ok()?,
            strip_tabs,
            quoted,
        },
        i,
    ))
}
