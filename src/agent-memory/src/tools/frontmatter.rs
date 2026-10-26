//! Shared frontmatter fence handling for the hand-rolled readers in `tools/`.
//!
//! A memory file's frontmatter block opens with a leading `---` line and
//! closes at the next line that starts with `---` — whether or not a newline
//! follows that closing fence. Readers that disagreed about the closing form
//! made the same file mean different things to different tools (GH-3580):
//! `memory_export` exported an empty frontmatter map, dropping `id` /
//! `category` / `source` from the AMA archive and misfiling the entry under a
//! category filter, and the body extractors handed summaries and profiles the
//! raw fence block as if it were body text.

/// The marker that opens and closes a frontmatter block.
const FENCE: &str = "---";

/// Split a markdown document into `(frontmatter, body)`.
///
/// Returns `None` when the document carries no frontmatter block: either it
/// does not open with a `---` line, or that opening fence is never closed.
/// Callers pick their own fallback in that case.
///
/// The opening fence is exactly `---\n`, as every reader in this crate has
/// always required. The closing fence is any line starting with `---`; it may
/// end the file without a trailing newline and its terminator may be `\n` or
/// `\r\n`, and either way the body starts at the first line after it.
pub fn split(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    // `end` points at the newline that terminates the last frontmatter line;
    // the fence starts right after it. Skip the whole fence line so a CRLF
    // terminator cannot leak a stray `\r` into the body.
    let after_fence = &rest[end + 1 + FENCE.len()..];
    let body = match after_fence.find('\n') {
        Some(i) => &after_fence[i + 1..],
        None => "",
    };
    Some((frontmatter, body))
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
    fn crlf_opening_fence_is_not_a_block() {
        // Every reader in the crate opens on "---\n"; keep that unchanged.
        assert!(split("---\r\nid: abc\r\n---\r\nbody\r\n").is_none());
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
