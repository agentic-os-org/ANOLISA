//! Shared frontmatter fence handling for the hand-rolled readers in `tools/`.
//!
//! A memory file's frontmatter block opens with a leading `---` line and
//! closes at the next line that *is* exactly `---`. Readers that disagreed
//! about the closing form made the same file mean different things to
//! different tools (GH-3580): `memory_export` exported an empty frontmatter
//! map, dropping `id` / `category` / `source` from the AMA archive and
//! misfiling the entry under a category filter, and the body extractors
//! handed summaries and profiles the raw fence block as if it were body
//! text.

/// The marker that opens and closes a frontmatter block.
const FENCE: &str = "---";

/// Split a markdown document into `(frontmatter, body)`.
///
/// Returns `None` when the document carries no frontmatter block: either it
/// does not open with a `---` line, or that opening fence is never closed.
/// Callers pick their own fallback in that case.
///
/// The fence must be a whole line: `----` (a thematic break), `--- a/b` (a
/// unified-diff header) or `--- ` (trailing space) is body content, not a
/// fence — a prefix-only match treated those lines as the closing fence and
/// mangled the extracted body. A fence that immediately follows the opening
/// fence closes an empty frontmatter block, and a fence at the very end of
/// the file (no trailing newline) is still a fence. The line terminator
/// after the fence (`\r\n`, `\n` or a lone `\r`) is consumed with the
/// fence. The opening fence accepts both `---\n` and `---\r\n`
/// (`mem_write` stores agent content verbatim, so CRLF notes exist on
/// disk); the frontmatter slice keeps its per-line `\r`, which every
/// caller's `str::lines()` strips.
pub fn split(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let is_fence_line = |at: usize| -> bool {
        let tail = &rest[at..];
        if !tail.starts_with(FENCE) {
            return false;
        }
        matches!(
            tail[FENCE.len()..].chars().next(),
            None | Some('\n') | Some('\r')
        )
    };
    // A fence directly after the opening fence closes an empty frontmatter
    // block; the offset-of-the-preceding-newline contract could not even
    // represent this position.
    if is_fence_line(0) {
        let after = &rest[FENCE.len()..];
        let body = consume_line_terminator(after);
        return Some(("", body));
    }
    let mut from = 0;
    while let Some(rel) = rest[from..].find("\n---") {
        let fence_line_start = from + rel;
        if !is_fence_line(fence_line_start + 1) {
            from = fence_line_start + 1 + FENCE.len();
            continue;
        }
        let frontmatter = &rest[..fence_line_start];
        let after = &rest[fence_line_start + 1 + FENCE.len()..];
        let body = consume_line_terminator(after);
        return Some((frontmatter, body));
    }
    None
}

/// Skip the line terminator that follows a closing fence, so it cannot
/// leak a stray `\r` into the body. A fence at the very end of the file
/// has no terminator and yields an empty body.
fn consume_line_terminator(after: &str) -> &str {
    after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .or_else(|| after.strip_prefix('\r'))
        .unwrap_or(after)
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_a_well_formed_block() {
        let (fm, body) = split("---\nid: abc\n---\nbody text\n").unwrap();
        assert_eq!(fm, "id: abc");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn closing_fence_without_trailing_newline() {
        let (fm, body) = split("---\nid: abc\ncategory: lesson\n---").unwrap();
        assert_eq!(fm, "id: abc\ncategory: lesson");
        assert_eq!(body, "");
    }

    #[test]
    fn crlf_closing_fence_does_not_leak_into_the_body() {
        let (fm, body) = split("---\nid: abc\r\n---\r\nbody text\r\n").unwrap();
        assert_eq!(fm, "id: abc\r");
        assert_eq!(body, "body text\r\n");
    }

    #[test]
    fn crlf_opening_fence_is_accepted() {
        // `mem_write` stores agent content verbatim, so CRLF-authored notes
        // exist on disk and must parse like LF ones; the frontmatter slice
        // keeps its per-line `\r`, which callers' `str::lines()` strips.
        let (fm, body) = split("---\r\nid: abc\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(fm, "id: abc\r");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn empty_frontmatter_block() {
        // A fence directly after the opening fence closes an empty block;
        // this used to fall into the "unclosed" arm and the whole file was
        // treated as body.
        let (fm, body) = split("---\n---\nbody text\n").unwrap();
        assert_eq!(fm, "");
        assert_eq!(body, "body text\n");
        let (fm, body) = split("---\n---").unwrap();
        assert_eq!(fm, "");
        assert_eq!(body, "");
    }

    #[test]
    fn false_fence_lines_do_not_close_the_block() {
        // `----` (thematic break), `--- a/b` (diff header) and a
        // trailing-space `--- ` are body content, not fences: a
        // prefix-only match used to close on them and mangle the body.
        assert!(split("---\nid: abc\n----\nbody\n").is_none());
        assert!(split("---\nid: abc\n--- a/b\n+++ c/d\nbody\n").is_none());
        assert!(split("---\nid: abc\n--- \nbody\n").is_none());
        // With a real fence later, the false-fence lines stay in the
        // frontmatter and the real fence closes.
        let (fm, body) = split("---\nid: abc\n----\n---\nbody\n").unwrap();
        assert_eq!(fm, "id: abc\n----");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn body_may_start_with_a_rule_of_its_own() {
        // Only the first line starting with `---` closes the block.
        let (fm, body) = split("---\nid: abc\n---\n---\ntail\n").unwrap();
        assert_eq!(fm, "id: abc");
        assert_eq!(body, "---\ntail\n");
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert!(split("Just plain markdown content.").is_none());
        assert!(split("").is_none());
        // A leading rule is not an opening fence.
        assert!(split("--- not a fence\nbody\n").is_none());
    }

    #[test]
    fn unclosed_frontmatter_returns_none() {
        assert!(split("---\nid: abc\nbody without a fence\n").is_none());
    }
}
