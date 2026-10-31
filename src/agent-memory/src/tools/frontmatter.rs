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

/// Drop the blank line that separates the closing fence from the body.
///
/// [`split`] starts the body at the first line after the closing fence, and
/// every writer in this crate emits that fence as `---\n\n`
/// (`memory_task::to_markdown`, `memory_import::reconstruct_markdown`,
/// `memory_observe`, `consolidation::fact::to_markdown`), so for anything they
/// wrote the first body line is the separator rather than content.
///
/// Readers that hand the body back to a writer have to consume it. Otherwise
/// the round trip is not the inverse of the write and every cycle puts one
/// more blank line in front of the text: `memory_export` -> `mem_import` grew
/// each archived memory, and `parse_task` -> `memory_task_save` grew each task
/// context. Readers that only display or search the body may keep [`split`]'s
/// output as it is — a leading blank line is invisible to them.
///
/// A body that does not start with a line break (a hand-written `---\nBody`,
/// or a file that ends at its fence) is returned unchanged, so this never
/// eats real content.
pub fn strip_separator(body: &str) -> &str {
    body.strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::{split, strip_separator};

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

    #[test]
    fn strip_separator_drops_only_the_line_below_the_fence() {
        // What every writer in this crate produces: "---\n\n" + body.
        assert_eq!(
            strip_separator("\nThe user prefers tabs.\n"),
            "The user prefers tabs.\n"
        );
        // An empty context is stored as the separator and nothing else.
        assert_eq!(strip_separator("\n"), "");
        assert_eq!(strip_separator(""), "");
        // A hand-written file with no blank line keeps its body.
        assert_eq!(strip_separator("Body"), "Body");
        // Only one line break goes: a deliberate blank line is content.
        assert_eq!(strip_separator("\n\nBody"), "\nBody");
        // CRLF files separate with "\r\n"; no stray "\r" may survive.
        assert_eq!(strip_separator("\r\nbody\r\n"), "body\r\n");
    }

    #[test]
    fn split_then_strip_separator_round_trips_a_written_file() {
        let file = "---\nid: abc\n---\n\nBody text\n";
        let (fm, body) = split(file).unwrap();
        assert_eq!(fm, "id: abc");
        assert_eq!(strip_separator(body), "Body text\n");
        // Writing the pair back the way the writers do reproduces the file.
        assert_eq!(format!("---\n{fm}\n---\n\n{}", strip_separator(body)), file);
    }
}
