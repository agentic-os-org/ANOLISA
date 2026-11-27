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

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    minimum: usize,
    container_indent: usize,
}

#[derive(Clone, Copy)]
struct ListContainer {
    content_col: usize,
    empty: bool,
}

struct ParsedListMarker {
    next_idx: usize,
    next_col: usize,
    content_col: usize,
    has_content: bool,
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
    fence: Option<Fence>,
    list_containers: Vec<ListContainer>,
    in_paragraph: bool,
}

impl MultilineGuard {
    /// Protect entire regions because partial arguments cannot be judged safely.
    pub(super) fn can_normalize(&mut self, line: &str) -> bool {
        let (indent, trimmed) = split_leading_indent(line);
        if let Some(fence) = self.fence {
            if !trimmed.is_empty() && indent < fence.container_indent {
                // Leaving the list item closes its fenced block and ends any
                // shell state (including unterminated heredocs) inside it;
                // reprocess this dedented line in its enclosing scope.
                self.reset_shell_state();
                self.fence = None;
                self.in_paragraph = false;
            } else if is_closing_fence(indent, trimmed, fence) {
                // Shell state — including an unterminated heredoc body —
                // belongs to this Markdown example only.
                self.reset_shell_state();
                self.fence = None;
                self.in_paragraph = false;
                return false;
            }
        }

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

        let can_track_markdown = self.fence.is_none()
            && !self.uncertain_heredoc
            && !self.continued
            && self.contexts.is_empty();
        if can_track_markdown {
            if trimmed.is_empty() {
                self.in_paragraph = false;
                // CommonMark §5.2: an empty list item cannot continue across a blank line.
                while self.list_containers.last().is_some_and(|item| item.empty) {
                    self.list_containers.pop();
                }
            } else if self.track_markdown_line(line, indent, trimmed) {
                return false;
            }
        } else if self.fence.is_none() {
            let kept = self.kept_list_containers(indent);
            let container_indent = self.container_indent_at(kept);
            if indent <= container_indent.saturating_add(3) {
                if let Some((marker, minimum)) = fence_open(trimmed) {
                    self.list_containers.truncate(kept);
                    for item in &mut self.list_containers {
                        item.empty = false;
                    }
                    self.open_fence(marker, minimum, container_indent);
                    return false;
                }
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

    fn reset_shell_state(&mut self) {
        self.contexts.clear();
        self.heredocs.clear();
        self.heredoc_body = false;
        self.uncertain_heredoc = false;
        self.heredoc_line.clear();
        self.continued = false;
    }

    fn open_fence(&mut self, marker: u8, minimum: usize, container_indent: usize) {
        self.reset_shell_state();
        self.fence = Some(Fence {
            marker,
            minimum,
            container_indent,
        });
        self.in_paragraph = false;
    }

    fn container_indent(&self) -> usize {
        self.list_containers
            .last()
            .map_or(0, |item| item.content_col)
    }

    fn kept_list_containers(&self, indent: usize) -> usize {
        self.list_containers
            .iter()
            .take_while(|item| indent >= item.content_col)
            .count()
    }

    fn container_indent_at(&self, count: usize) -> usize {
        count
            .checked_sub(1)
            .and_then(|idx| self.list_containers.get(idx))
            .map_or(0, |item| item.content_col)
    }

    fn track_markdown_line(&mut self, line: &str, indent: usize, trimmed: &str) -> bool {
        let kept = self.kept_list_containers(indent);
        let exited_list = kept < self.list_containers.len();
        let matched_indent = self.container_indent_at(kept);
        let bytes = line.as_bytes();
        let leading_ws_len = bytes.len() - trimmed.len();

        // Exiting a list item closes its paragraph; any active paragraph
        // at the outer level was already closed when the popped item began.
        let marker_in_paragraph = if exited_list {
            false
        } else {
            self.in_paragraph
        };

        let first_marker = if indent <= matched_indent.saturating_add(3) {
            parse_list_marker(bytes, leading_ws_len, indent, marker_in_paragraph)
        } else {
            None
        };

        if let Some(first) = first_marker {
            self.list_containers.truncate(kept);
            for item in &mut self.list_containers {
                item.empty = false;
            }
            let mut idx = first.next_idx;
            let mut col = first.next_col;
            let mut has_content = first.has_content;
            self.list_containers.push(ListContainer {
                content_col: first.content_col,
                empty: !first.has_content,
            });
            while has_content && col <= self.container_indent().saturating_add(3) {
                let Some(parsed) = parse_list_marker(bytes, idx, col, false) else {
                    break;
                };
                if let Some(last) = self.list_containers.last_mut() {
                    last.empty = false;
                }
                self.list_containers.push(ListContainer {
                    content_col: parsed.content_col,
                    empty: !parsed.has_content,
                });
                idx = parsed.next_idx;
                col = parsed.next_col;
                has_content = parsed.has_content;
            }

            let container_indent = self.container_indent();
            if !has_content {
                self.in_paragraph = false;
                return false;
            }
            let rest = &line[idx..];
            if col <= container_indent.saturating_add(3) {
                if let Some((marker, minimum)) = fence_open(rest) {
                    self.open_fence(marker, minimum, container_indent);
                    return true;
                }
            }
            self.in_paragraph = col <= container_indent.saturating_add(3)
                && !is_atx_heading(rest)
                && !is_thematic_break_line(rest.as_bytes());
            return false;
        }

        let within_indent = indent <= matched_indent.saturating_add(3);
        if within_indent {
            if let Some((marker, minimum)) = fence_open(trimmed) {
                self.list_containers.truncate(kept);
                for item in &mut self.list_containers {
                    item.empty = false;
                }
                self.open_fence(marker, minimum, matched_indent);
                return true;
            }
            if is_atx_heading(trimmed) || is_thematic_break_line(trimmed.as_bytes()) {
                self.list_containers.truncate(kept);
                for item in &mut self.list_containers {
                    item.empty = false;
                }
                self.in_paragraph = false;
                return false;
            }
        }

        // CommonMark §5.2: a dedented non-block line while a list item's
        // paragraph is open lazily continues that paragraph and keeps the
        // list containers active.
        if exited_list && self.in_paragraph {
            return false;
        }

        self.list_containers.truncate(kept);
        for item in &mut self.list_containers {
            item.empty = false;
        }
        let prev_in_paragraph = if exited_list {
            false
        } else {
            self.in_paragraph
        };

        let is_indented_code = !prev_in_paragraph && !within_indent;
        let is_setext_heading =
            prev_in_paragraph && within_indent && is_setext_heading_underline(trimmed.as_bytes());
        self.in_paragraph = !(is_indented_code || is_setext_heading);
        false
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

fn split_leading_indent(line: &str) -> (usize, &str) {
    let mut col = 0usize;
    let mut idx = 0usize;
    for b in line.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col = (col / 4 + 1) * 4,
            _ => break,
        }
        idx += 1;
    }
    (col, &line[idx..])
}

fn fence_open(trimmed: &str) -> Option<(u8, usize)> {
    let marker = trimmed.as_bytes().first().copied()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = trimmed.bytes().take_while(|&b| b == marker).count();
    let info = &trimmed[length..];
    if length >= 3 && (marker == b'~' || !info.contains('`')) {
        Some((marker, length))
    } else {
        None
    }
}

fn is_atx_heading(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
    (1..=6).contains(&hashes) && matches!(bytes.get(hashes), None | Some(b' ' | b'\t'))
}

fn is_setext_heading_underline(bytes: &[u8]) -> bool {
    let Some(&marker @ (b'=' | b'-')) = bytes.first() else {
        return false;
    };
    let run = bytes.iter().take_while(|&&b| b == marker).count();
    run >= 1 && bytes[run..].iter().all(|&b| matches!(b, b' ' | b'\t'))
}

fn is_thematic_break(rest: &[u8], marker: u8) -> bool {
    let mut count = 0usize;
    for &b in rest {
        if b == marker {
            count += 1;
        } else if !matches!(b, b' ' | b'\t') {
            return false;
        }
    }
    count >= 3
}

fn is_thematic_break_line(bytes: &[u8]) -> bool {
    match bytes.first() {
        Some(&marker @ (b'-' | b'_' | b'*')) => is_thematic_break(bytes, marker),
        _ => false,
    }
}

fn parse_list_marker(
    bytes: &[u8],
    mut idx: usize,
    mut col: usize,
    in_paragraph: bool,
) -> Option<ParsedListMarker> {
    let rest = &bytes[idx..];
    let first = *rest.first()?;
    let (marker_len, start_num) = if matches!(first, b'-' | b'*' | b'+') {
        if matches!(first, b'-' | b'*') && is_thematic_break(rest, first) {
            return None;
        }
        (1, None)
    } else if first.is_ascii_digit() {
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let delim = *rest.get(digits)?;
        if !(1..=9).contains(&digits) || !matches!(delim, b'.' | b')') {
            return None;
        }
        let num: usize = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
        (digits + 1, Some(num))
    } else {
        return None;
    };

    idx += marker_len;
    col += marker_len;
    let has_content = if idx == bytes.len() {
        false
    } else if matches!(bytes[idx], b' ' | b'\t') {
        bytes[idx..].iter().any(|&b| !matches!(b, b' ' | b'\t'))
    } else {
        return None;
    };

    // Only non-empty markers (and ordered lists starting at 1) can interrupt
    // an active paragraph in the current container.
    if in_paragraph && (!has_content || matches!(start_num, Some(num) if num != 1)) {
        return None;
    }

    let after_col = col;
    while idx < bytes.len() {
        match bytes[idx] {
            b' ' => col += 1,
            b'\t' => col = (col / 4 + 1) * 4,
            _ => break,
        }
        idx += 1;
    }
    let ws_cols = col - after_col;
    let content_col = if !has_content || ws_cols >= 5 {
        after_col + 1
    } else {
        col
    };
    Some(ParsedListMarker {
        next_idx: idx,
        next_col: col,
        content_col,
        has_content,
    })
}

fn is_closing_fence(indent: usize, trimmed: &str, fence: Fence) -> bool {
    if indent > fence.container_indent.saturating_add(3) {
        return false;
    }
    let length = trimmed.bytes().take_while(|&b| b == fence.marker).count();
    length >= fence.minimum && trimmed[length..].bytes().all(|b| matches!(b, b' ' | b'\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_fence_ends_unterminated_heredoc() {
        let mut guard = MultilineGuard::default();
        for line in ["```bash", "cat <<EOF", "hello", "```"] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        // Prose after the example must be normalizable again.
        assert!(guard.can_normalize("pip install requests"));
    }

    #[test]
    fn closing_fence_ends_backslash_continued_heredoc_body() {
        let mut guard = MultilineGuard::default();
        for line in ["```bash", "cat <<EOF", "line1\\", "```"] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install requests"));
    }

    #[test]
    fn indented_fence_inside_heredoc_stays_data() {
        let mut guard = MultilineGuard::default();
        for line in [
            "```bash",
            "cat <<-EOF",
            "\t```",
            "\tpip install x",
            "    ```",
            "    pip install y",
            "\tEOF",
            "```",
        ] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install requests"));
    }

    #[test]
    fn opening_fence_indented_one_to_three_spaces_keeps_four_col_body_fences() {
        for open_indent in [" ", "  ", "   "] {
            for body_indent in ["    ", "\t"] {
                let mut guard = MultilineGuard::default();
                let open = format!("{open_indent}```sh");
                let body_fence = format!("{body_indent}```");
                let close = format!("{open_indent}```");
                for line in [
                    open.as_str(),
                    "cat <<'EOF'",
                    body_fence.as_str(),
                    "pip install literal",
                    "EOF",
                    close.as_str(),
                ] {
                    assert!(!guard.can_normalize(line), "{open:?} / {body_fence:?}");
                }
                assert!(
                    guard.can_normalize("pip install control"),
                    "{open:?} / {body_fence:?}"
                );
            }
        }
    }

    #[test]
    fn list_container_closing_fence_uses_container_indent_limit() {
        let mut guard = MultilineGuard::default();
        assert!(guard.can_normalize("10. Step"));
        for line in [
            "     ```sh",
            "    cat <<'EOF'",
            "        ```",
            "    pip install literal",
            "    ```",
        ] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install control"));
    }

    #[test]
    fn heredoc_fence_lines_with_other_marker_or_shorter_fence_stay_data() {
        let mut guard = MultilineGuard::default();
        for line in [
            "~~~bash",
            "cat <<EOF > README.md",
            "```",
            "pip install x",
            "EOF",
        ] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install requests"));

        let mut four = MultilineGuard::default();
        for line in ["````bash", "cat <<EOF", "```", "````"] {
            assert!(!four.can_normalize(line), "{line}");
        }
        assert!(four.can_normalize("pip install requests"));
    }

    #[test]
    fn heredoc_without_fence_keeps_backticks_as_data() {
        let mut guard = MultilineGuard::default();
        for line in ["cat <<EOF", "```", "pip install x", "EOF"] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install requests"));
    }

    #[test]
    fn paragraph_continuation_ordered_marker_does_not_open_list_container() {
        let mut guard = MultilineGuard::default();
        assert!(guard.can_normalize("Ordinary paragraph"));
        assert!(guard.can_normalize("2. Still the same paragraph"));
        for line in [
            "   ```sh",
            "cat <<'EOF'",
            "    ```",
            "pip install literal",
            "EOF",
            "   ```",
        ] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install control"));

        let mut interrupt = MultilineGuard::default();
        assert!(interrupt.can_normalize("Ordinary paragraph"));
        assert!(interrupt.can_normalize("1. Starts list"));
        for line in [
            "    ```sh",
            "   cat <<'EOF'",
            "       ```",
            "   pip install literal",
            "   EOF",
            "    ```",
        ] {
            assert!(!interrupt.can_normalize(line), "{line}");
        }
        assert!(interrupt.can_normalize("pip install control"));

        let mut empty_marker = MultilineGuard::default();
        assert!(empty_marker.can_normalize("Ordinary paragraph"));
        assert!(empty_marker.can_normalize("1."));
        for line in [
            "   ```sh",
            "cat <<'EOF'",
            "    ```",
            "pip install literal",
            "EOF",
            "   ```",
        ] {
            assert!(!empty_marker.can_normalize(line), "{line}");
        }
        assert!(empty_marker.can_normalize("pip install control"));
    }

    #[test]
    fn consecutive_sibling_list_items_keep_container_indent() {
        let mut sibling = MultilineGuard::default();
        assert!(sibling.can_normalize("1. First step"));
        assert!(sibling.can_normalize("2. Second step"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!sibling.can_normalize(line), "{line}");
        }
        assert!(sibling.can_normalize("pip install control"));

        let mut outer_after_nested = MultilineGuard::default();
        assert!(outer_after_nested.can_normalize("1. Outer"));
        assert!(outer_after_nested.can_normalize("   - Inner"));
        assert!(outer_after_nested.can_normalize("2. Outer 2"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!outer_after_nested.can_normalize(line), "{line}");
        }
        assert!(outer_after_nested.can_normalize("pip install control"));

        let mut chained = MultilineGuard::default();
        assert!(chained.can_normalize("Ordinary paragraph"));
        assert!(chained.can_normalize("- 1. Nested"));
        for line in ["       ```sh", "       echo \"", "       ```"] {
            assert!(!chained.can_normalize(line), "{line}");
        }
        assert!(chained.can_normalize("pip install control"));

        for (first, second) in [
            ("1. First step", "2) Second step"),
            ("- Bullet", "2. Ordered"),
        ] {
            let mut switched = MultilineGuard::default();
            assert!(switched.can_normalize(first), "{first}");
            assert!(switched.can_normalize(second), "{second}");
            for line in ["    ```sh", "    echo \"", "    ```"] {
                assert!(!switched.can_normalize(line), "{second} / {line}");
            }
            assert!(switched.can_normalize("pip install control"), "{second}");
        }

        let mut empty_after_exit = MultilineGuard::default();
        assert!(empty_after_exit.can_normalize("1. First"));
        assert!(empty_after_exit.can_normalize("-"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!empty_after_exit.can_normalize(line), "{line}");
        }
        assert!(empty_after_exit.can_normalize("pip install control"));

        let mut nested_switch = MultilineGuard::default();
        assert!(nested_switch.can_normalize("1. Outer"));
        assert!(nested_switch.can_normalize("   - Inner"));
        assert!(nested_switch.can_normalize("   2) Nested sibling"));
        for line in ["       ```sh", "       echo \"", "       ```"] {
            assert!(!nested_switch.can_normalize(line), "{line}");
        }
        assert!(nested_switch.can_normalize("pip install control"));
    }

    #[test]
    fn non_one_and_empty_markers_do_not_interrupt_same_container_paragraph() {
        let mut top_level_paren = MultilineGuard::default();
        assert!(top_level_paren.can_normalize("Ordinary paragraph"));
        assert!(top_level_paren.can_normalize("2) Still the same paragraph"));
        for line in [
            "   ```sh",
            "cat <<'EOF'",
            "    ```",
            "pip install literal",
            "EOF",
            "   ```",
        ] {
            assert!(!top_level_paren.can_normalize(line), "{line}");
        }
        assert!(top_level_paren.can_normalize("pip install control"));

        let mut empty_nested_bullet = MultilineGuard::default();
        assert!(empty_nested_bullet.can_normalize("1. Outer"));
        assert!(empty_nested_bullet.can_normalize("   +"));
        for line in [
            "      ```sh",
            "   cat <<'EOF'",
            "       ```",
            "   pip install literal",
            "   EOF",
            "      ```",
        ] {
            assert!(!empty_nested_bullet.can_normalize(line), "{line}");
        }
        assert!(empty_nested_bullet.can_normalize("pip install control"));

        for second in [
            "   2. Still outer paragraph",
            "   2) Continuation of item 1 paragraph",
        ] {
            let mut nested_two = MultilineGuard::default();
            assert!(nested_two.can_normalize("1. Outer"), "{second}");
            assert!(nested_two.can_normalize(second), "{second}");
            for line in [
                "      ```sh",
                "   cat <<'EOF'",
                "       ```",
                "   pip install literal",
                "   EOF",
                "      ```",
            ] {
                assert!(!nested_two.can_normalize(line), "{second} / {line}");
            }
            assert!(nested_two.can_normalize("pip install control"), "{second}");
        }
    }

    #[test]
    fn closing_fence_only_ignores_ascii_spaces_and_tabs() {
        let mut nbsp = MultilineGuard::default();
        for line in [
            "```sh",
            "cat <<'EOF'",
            "```\u{00a0}",
            "pip install literal",
            "EOF",
            "```",
        ] {
            assert!(!nbsp.can_normalize(line), "{line}");
        }
        assert!(nbsp.can_normalize("pip install control"));

        let mut em_space = MultilineGuard::default();
        for line in [
            "~~~sh",
            "cat <<'EOF'",
            "~~~\u{2003}",
            "pip install literal",
            "EOF",
            "~~~ \t ",
        ] {
            assert!(!em_space.can_normalize(line), "{line}");
        }
        assert!(em_space.can_normalize("pip install control"));
    }

    #[test]
    fn empty_list_item_ends_on_blank_line() {
        let mut guard = MultilineGuard::default();
        assert!(guard.can_normalize("-"));
        assert!(guard.can_normalize(""));
        for line in [
            "  ```sh",
            "  cat <<'EOF'",
            "    ```",
            "  pip install literal",
            "  EOF",
            "  ```",
        ] {
            assert!(!guard.can_normalize(line), "{line}");
        }
        assert!(guard.can_normalize("pip install control"));

        let mut without_blank = MultilineGuard::default();
        assert!(without_blank.can_normalize("-"));
        for line in [
            "  ```sh",
            "  cat <<'EOF'",
            "      ```",
            "  pip install literal",
            "  EOF",
            "  ```",
        ] {
            assert!(!without_blank.can_normalize(line), "{line}");
        }
        assert!(without_blank.can_normalize("pip install control"));

        let mut chained_empty = MultilineGuard::default();
        assert!(chained_empty.can_normalize("- -"));
        assert!(chained_empty.can_normalize(""));
        for line in [
            "  ```sh",
            "  cat <<'EOF'",
            "      ```",
            "  pip install literal",
            "  EOF",
            "  ```",
        ] {
            assert!(!chained_empty.can_normalize(line), "{line}");
        }
        assert!(chained_empty.can_normalize("pip install control"));
    }

    #[test]
    fn indented_code_setext_heading_and_five_space_marker_clear_paragraph_state() {
        for prefix in [
            &["    echo code", "2. Ordered item"][..],
            &["Heading", "===", "2. Ordered item"][..],
            &["Heading", "--", "2. Ordered item"][..],
        ] {
            let mut guard = MultilineGuard::default();
            for &line in prefix {
                assert!(guard.can_normalize(line), "{prefix:?} / {line}");
            }
            for line in ["    ```sh", "    echo \"", "    ```"] {
                assert!(!guard.can_normalize(line), "{prefix:?} / {line}");
            }
            assert!(guard.can_normalize("pip install control"), "{prefix:?}");
        }

        let mut five_space = MultilineGuard::default();
        assert!(five_space.can_normalize("-     code"));
        assert!(five_space.can_normalize("  2. Ordered item"));
        for line in ["      ```sh", "      echo \"", "      ```"] {
            assert!(!five_space.can_normalize(line), "{line}");
        }
        assert!(five_space.can_normalize("pip install control"));

        // Indented lines inside an active paragraph continue the paragraph.
        let mut indented_continuation = MultilineGuard::default();
        assert!(indented_continuation.can_normalize("Ordinary paragraph"));
        assert!(indented_continuation.can_normalize("    still paragraph"));
        assert!(indented_continuation.can_normalize("2. Still same paragraph"));
        for line in [
            "   ```sh",
            "cat <<'EOF'",
            "    ```",
            "pip install literal",
            "EOF",
            "   ```",
        ] {
            assert!(!indented_continuation.can_normalize(line), "{line}");
        }
        assert!(indented_continuation.can_normalize("pip install control"));
    }

    #[test]
    fn lazy_paragraph_continuation_preserves_list_exit_semantics() {
        let mut lazy_then_switch = MultilineGuard::default();
        assert!(lazy_then_switch.can_normalize("1. First item"));
        assert!(lazy_then_switch.can_normalize("Lazy continuation"));
        assert!(lazy_then_switch.can_normalize("2) Second item"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!lazy_then_switch.can_normalize(line), "{line}");
        }
        assert!(lazy_then_switch.can_normalize("pip install control"));

        let mut lazy_then_fence = MultilineGuard::default();
        assert!(lazy_then_fence.can_normalize("1. First item"));
        assert!(lazy_then_fence.can_normalize("Lazy continuation"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!lazy_then_fence.can_normalize(line), "{line}");
        }
        assert!(lazy_then_fence.can_normalize("pip install control"));
    }

    #[test]
    fn list_contained_fence_closes_at_list_boundary_and_reprocesses_line() {
        for lines in [
            &[
                "10. Step",
                "    ```sh",
                "    cat <<EOF",
                "    pip install inside",
                "",
            ][..],
            &[
                "10. Step",
                "    ```sh",
                "    cat <<EOF",
                "    pip install inside",
            ][..],
        ] {
            let mut guard = MultilineGuard::default();
            assert!(guard.can_normalize(lines[0]));
            for &line in &lines[1..] {
                assert!(!guard.can_normalize(line), "{lines:?} / {line}");
            }
            assert!(guard.can_normalize("pip install control"), "{lines:?}");
        }

        // Dedented fence closes the list fence and opens a new top-level fence.
        let mut dedented_fence = MultilineGuard::default();
        assert!(dedented_fence.can_normalize("10. A list example"));
        assert!(!dedented_fence.can_normalize("    ```sh"));
        assert!(dedented_fence.can_normalize("    echo hello"));
        for line in ["```", "cat <<EOF", "pip install inside_top_level", "```"] {
            assert!(!dedented_fence.can_normalize(line), "{line}");
        }
        assert!(dedented_fence.can_normalize("pip install control"));

        // Nested unclosed list fence ends when returning to parent item content.
        let mut nested = MultilineGuard::default();
        assert!(nested.can_normalize("- Outer"));
        assert!(nested.can_normalize("  1. Example"));
        for line in ["     ```sh", "     cat <<EOF", "     pip install inside"] {
            assert!(!nested.can_normalize(line), "{line}");
        }
        assert!(nested.can_normalize("  - Sibling"));
        for line in ["    ```sh", "    echo \"", "    ```"] {
            assert!(!nested.can_normalize(line), "{line}");
        }
        assert!(nested.can_normalize("  pip install in_outer"));
        assert!(nested.can_normalize("pip install control"));

        // A top-level fence after a list item containing an apostrophe must
        // open at top-level container_indent = 0, not inside the list item.
        let mut apostrophe_in_list = MultilineGuard::default();
        assert!(!apostrophe_in_list.can_normalize("1. Don't change the example."));
        assert!(!apostrophe_in_list.can_normalize("```sh"));
        assert!(apostrophe_in_list.can_normalize("pip install a"));
        assert!(!apostrophe_in_list.can_normalize("```"));
        assert!(apostrophe_in_list.can_normalize("pip install control"));
    }

    #[test]
    fn same_line_list_marker_fence_opens_in_list_container() {
        for (open, close) in [("- ```sh", "  ```"), ("1. ~~~sh", "   ~~~")] {
            let indent = if open.starts_with('-') { "  " } else { "   " };
            let mut guard = MultilineGuard::default();
            let heredoc = format!("{indent}cat <<EOF");
            let body = format!("{indent}pip install inside");
            for line in [open, heredoc.as_str(), body.as_str(), close] {
                assert!(!guard.can_normalize(line), "{open} / {line}");
            }
            assert!(guard.can_normalize("pip install control"), "{open}");
        }

        let mut dedented = MultilineGuard::default();
        for line in ["- ```sh", "  cat <<EOF", "  pip install inside"] {
            assert!(!dedented.can_normalize(line), "{line}");
        }
        assert!(dedented.can_normalize("pip install control"));

        // Five or more spaces after a list marker start indented code, not a fence.
        let mut five_space_code = MultilineGuard::default();
        assert!(five_space_code.can_normalize("-     ```sh```"));
        assert!(five_space_code.can_normalize("  2. Ordered item"));
        for line in ["      ```sh", "      echo \"", "      ```"] {
            assert!(!five_space_code.can_normalize(line), "{line}");
        }
        assert!(five_space_code.can_normalize("pip install control"));
    }
}
