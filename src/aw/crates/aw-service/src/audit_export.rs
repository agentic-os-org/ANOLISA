//! Public metadata export from retained journals without an execution control socket.

mod wire;

use aw_contracts::{canonical, Registry};
use aw_core::journal::reader::{self, JournalReader};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use wire::{Anchor, ChainCursor, InventoryCursor, Operation, Request};

/// Version of the standalone read request/response, distinct from control RPCs.
pub const VERSION: &str = "audit-export.aw/v1alpha1";
/// Maximum encoded query size, including bounded replay cursors.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Maximum encoded response size, including public objects and cursor metadata.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Reads one bounded public page as the current effective user.
///
/// `directory` is an existing service journal directory. It must be owned by the
/// caller with mode 0700; source metadata and chains require mode 0600. The result
/// always uses the export response schema and never includes private source records
/// or OS error strings. Reads create no files, service, claim or execution handle.
///
/// Queries are bounded by the supplied deadline and cancellation flag. Filesystem
/// calls are cooperative and require a local filesystem; they have no hard real-time
/// guarantee. Rescan periodically, retain chain cursors, and
/// revisit every nonterminal chain even after catching up to its current tip.
pub fn query(directory: &Path, bytes: &[u8], deadline: Instant, cancelled: &AtomicBool) -> Value {
    let mut source_id = None;
    let result = (|| -> Result<Value, &'static str> {
        let cancelled = || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline;
        check_cancel(&cancelled)?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("invalid_request");
        }
        let value = canonical::parse(bytes).map_err(|_| "invalid_request")?;
        let registry = Registry::new().map_err(|_| "export_unavailable")?;
        registry
            .validate("audit-export-request-v1alpha1", &value)
            .map_err(|_| "invalid_request")?;
        let request: Request = serde_json::from_value(value).map_err(|_| "invalid_request")?;
        // SAFETY: geteuid takes no arguments and does not retain pointers.
        let reader =
            JournalReader::open(directory, unsafe { libc::geteuid() }).map_err(read_error)?;
        let source =
            crate::public_projection::source_identity(&reader.pinned_path()).map_err(|error| {
                match error {
                    crate::Error::Rejected(code) if code == "audit_source_missing" => {
                        "source_missing"
                    }
                    _ => "source_invalid",
                }
            })?;
        let source = source.as_str();
        source_id = Some(source.to_owned());
        let value = match request.operation {
            Operation::Enumerate { cursor, limit } => {
                if cursor.as_ref().is_some_and(|c| c.source_id != source) {
                    return Err("source_changed");
                }
                let page = reader
                    .enumerate(
                        cursor.as_ref().map(|c| c.after.as_str()),
                        cursor.as_ref().map(|c| c.inventory_digest.as_str()),
                        cursor.as_ref().map(|c| c.through.as_str()),
                        limit,
                        &cancelled,
                    )
                    .map_err(read_error)?;
                let next = match page.next {
                    Some(after) => Some(InventoryCursor {
                        source_id: source.into(),
                        inventory_digest: page.digest.clone(),
                        after,
                        through: page.through.ok_or("history_changed")?,
                    }),
                    None => None,
                };
                json!({"type":"enumerate", "keys":page.keys, "inventory_digest":page.digest,
                    "changed":page.changed, "next_cursor":next, "limits":limits()})
            }
            Operation::Read {
                chain_id,
                cursor,
                limit,
            } => {
                if let Some(cursor) = &cursor {
                    if cursor.source_id != source {
                        return Err("source_changed");
                    }
                    if cursor.chain_id != chain_id
                        || cursor.consumed.sequence > cursor.observed_tip.sequence
                    {
                        return Err("invalid_request");
                    }
                }
                let after = cursor.as_ref().map(|c| (&c.consumed).into());
                let observed = cursor.as_ref().map(|c| (&c.observed_tip).into());
                let page = reader
                    .read(
                        &chain_id,
                        after.as_ref(),
                        observed.as_ref(),
                        limit,
                        &cancelled,
                    )
                    .map_err(read_error)?;
                let event = match crate::public_projection::execution_event(&page.initial)
                    .map_err(|_| "projection_invalid")?
                {
                    Some(event) => event,
                    None => {
                        return Err(
                            match page.initial["record"]["plan"].get("publicProjectionGap") {
                                Some(Value::String(reason))
                                    if matches!(
                                        reason.as_str(),
                                        "aw.native_metadata_unavailable"
                                            | "aw.observation_callback_limit"
                                    ) =>
                                {
                                    "projection_unavailable"
                                }
                                Some(_) => "projection_invalid",
                                None => "unsupported_history",
                            },
                        )
                    }
                };
                if event["metadata"]["producer"]["scopeId"] != source {
                    return Err("source_changed");
                }
                let mut records = Vec::new();
                for envelope in &page.records {
                    check_cancel(&cancelled)?;
                    if let Some(record) = crate::public_projection::project_record(envelope, &event)
                        .map_err(|_| "projection_invalid")?
                    {
                        records.push(record);
                    }
                }
                let consumed = page.next.as_ref().ok_or("history_incomplete")?;
                let cursor = ChainCursor {
                    source_id: source.into(),
                    chain_id: chain_id.clone(),
                    consumed: Anchor::from(consumed),
                    observed_tip: Anchor::from(&page.tip),
                };
                json!({"type":"read", "chain_id":chain_id, "event":event, "records":records,
                    "cursor":cursor, "tip":Anchor::from(&page.tip), "caught_up":page.caught_up,
                    "terminal":page.terminal, "limits":limits()})
            }
        };
        check_cancel(&cancelled)?;
        let response = response(source_id.as_deref(), Some(value.clone()), None);
        registry
            .validate("audit-export-response-v1alpha1", &response)
            .map_err(|_| "projection_invalid")?;
        if canonical::bytes(&response)
            .map_err(|_| "response_limit")?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err("response_limit");
        }
        Ok(value)
    })();
    match result {
        Ok(value) => response(source_id.as_deref(), Some(value), None),
        Err(code) => response(source_id.as_deref(), None, Some(code)),
    }
}

fn response(source: Option<&str>, result: Option<Value>, error: Option<&str>) -> Value {
    json!({"api_version":VERSION,"source_id":source,"result":result,"error":error})
}

fn limits() -> Value {
    json!({"page":reader::MAX_PAGE,"inventory_entries":reader::MAX_ENTRIES,
        "chain_bytes":reader::MAX_CHAIN_BYTES,"chain_records":reader::MAX_CHAIN_RECORDS,
        "response_bytes":MAX_RESPONSE_BYTES})
}
fn check_cancel(cancelled: &impl Fn() -> bool) -> Result<(), &'static str> {
    if cancelled() {
        return Err("cancelled");
    }
    Ok(())
}
fn read_error(error: reader::Error) -> &'static str {
    match error {
        reader::Error::Io(_) => "storage_unavailable",
        reader::Error::Access => "access_denied",
        reader::Error::Missing => "history_missing",
        reader::Error::Corrupt => "history_corrupt",
        reader::Error::Incomplete => "history_incomplete",
        reader::Error::Request => "invalid_request",
        reader::Error::Limit => "read_limit",
        reader::Error::Cancelled => "cancelled",
        reader::Error::Changed => "history_changed",
    }
}
