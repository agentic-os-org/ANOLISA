//! Stable cursor parsing and bounded session-summary page collection.

use super::{ProviderSessionId, SessionError, SessionSummary};

const LIST_CURSOR_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListCursor {
    pub(super) updated_at_ms: u64,
    pub(super) session_id: ProviderSessionId,
}

pub(super) struct ListEntry {
    pub(super) session_id: ProviderSessionId,
    pub(super) modified_at_ms: u64,
}

pub(super) fn parse_list_cursor(value: &str) -> Result<ListCursor, SessionError> {
    let mut parts = value.splitn(3, ':');
    let version = parts.next();
    let updated_at_ms = parts
        .next()
        .and_then(|value| u64::from_str_radix(value, 16).ok());
    let session_id = parts
        .next()
        .and_then(|value| ProviderSessionId::parse(value).ok());
    match (version, updated_at_ms, session_id) {
        (Some(LIST_CURSOR_VERSION), Some(updated_at_ms), Some(session_id)) => Ok(ListCursor {
            updated_at_ms,
            session_id,
        }),
        _ => Err(SessionError::InvalidCursor {
            cursor: value.to_string(),
        }),
    }
}

pub(super) fn collect_list_page(
    entries: &[ListEntry],
    start: usize,
    limit: usize,
    mut summarize: impl FnMut(&ListEntry) -> Option<SessionSummary>,
) -> (Vec<SessionSummary>, usize) {
    let mut page = Vec::with_capacity(limit);
    let mut examined_end = start.min(entries.len());
    for entry in entries.iter().skip(start) {
        examined_end = examined_end.saturating_add(1);
        if let Some(summary) = summarize(entry) {
            page.push(summary);
            if page.len() == limit {
                break;
            }
        }
    }
    (page, examined_end)
}

pub(super) fn format_list_cursor(entry: &ListEntry) -> String {
    format!(
        "{LIST_CURSOR_VERSION}:{:016x}:{}",
        entry.modified_at_ms, entry.session_id
    )
}

pub(super) fn entry_is_after_cursor(entry: &ListEntry, cursor: &ListCursor) -> bool {
    entry.modified_at_ms < cursor.updated_at_ms
        || (entry.modified_at_ms == cursor.updated_at_ms
            && entry.session_id.as_str() > cursor.session_id.as_str())
}
