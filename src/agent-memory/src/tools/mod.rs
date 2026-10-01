//! Tier A file tools — the "pen and notebook" given to the agent.
//!
//! All tools share the same shape:
//! - Take a `&MemoryService` plus path arguments
//! - Resolve every path through `ns::paths::resolve_path` (sandbox)
//! - Perform real IO under the namespace mount
//! - Emit one `AuditEntry` per call (success or failure)

/// Split `content` into `(frontmatter, body)` at a closing fence that is a
/// line consisting of exactly `---`. Returns `None` when there is no
/// opening fence (`---\n` at byte 0) or no closing fence.
///
/// The fence must be a whole line: `----` (a thematic break) or `--- a/b`
/// (a unified-diff header) is body content, not a fence — a prefix-only
/// match treated those lines as the closing fence and mangled the extracted
/// body. A fence that immediately follows the opening fence closes an empty
/// frontmatter block, and a fence at the very end of the file (no trailing
/// newline) is still a fence. The line terminator after the fence (`\r\n`,
/// `\n` or a lone `\r`) is consumed with the fence. The opening fence
/// accepts both `---\n` and `---\r\n` (`mem_write` stores agent content
/// verbatim, so CRLF notes exist on disk); the frontmatter slice keeps
/// its per-line `\r`, which every caller's `str::lines()` strips.
pub(crate) fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let is_fence_line = |at: usize| -> bool {
        let tail = &rest[at..];
        if !tail.starts_with("---") {
            return false;
        }
        matches!(tail[3..].chars().next(), None | Some('\n') | Some('\r'))
    };
    // A fence directly after the opening fence closes an empty frontmatter
    // block; the offset-of-the-preceding-newline contract could not even
    // represent this position.
    if is_fence_line(0) {
        let after = &rest[3..];
        let body = after
            .strip_prefix("\r\n")
            .or_else(|| after.strip_prefix('\n'))
            .or_else(|| after.strip_prefix('\r'))
            .unwrap_or(after);
        return Some(("", body));
    }
    let mut from = 0;
    while let Some(rel) = rest[from..].find("\n---") {
        let fence_line_start = from + rel;
        if !is_fence_line(fence_line_start + 1) {
            from = fence_line_start + 4;
            continue;
        }
        let frontmatter = &rest[..fence_line_start];
        let after = &rest[fence_line_start + 4..];
        let body = after
            .strip_prefix("\r\n")
            .or_else(|| after.strip_prefix('\n'))
            .or_else(|| after.strip_prefix('\r'))
            .unwrap_or(after);
        return Some((frontmatter, body));
    }
    None
}

pub mod append;
pub mod diff;
pub mod edit;
pub mod grep;
pub mod list;
pub mod mem_log;
pub mod mem_revert;
pub mod mem_snapshot;
pub mod mem_snapshot_list;
pub mod mem_snapshot_restore;
pub mod memory_export;
pub mod memory_get_context;
pub mod memory_import;
pub mod memory_observe;
pub mod memory_search;
pub mod memory_sovereignty;
pub mod memory_task;
pub mod mkdir;
pub mod promote;
pub mod read;
pub mod remove;
pub mod session_history;
pub mod session_log;
pub mod write;

pub use append::append;
pub use diff::diff;
pub use edit::edit;
pub use grep::{GrepHit, GrepOptions, grep};
pub use list::{ListEntry, ListOptions, list};
pub use mem_log::mem_log;
pub use mem_revert::mem_revert;
pub use mem_snapshot::snapshot;
pub use mem_snapshot_list::snapshot_list;
pub use mem_snapshot_restore::snapshot_restore;
pub use memory_get_context::memory_get_context;
pub use memory_observe::memory_observe;
pub use memory_search::memory_search;
pub use mkdir::mkdir;
pub use promote::promote;
pub use read::read;
pub use remove::remove;
pub use session_log::session_log;
pub use write::write;
pub mod memory_index;
pub mod memory_summary_tool;
pub mod session_context;
pub mod user_profile;
