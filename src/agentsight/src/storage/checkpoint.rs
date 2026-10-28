//! Checkpoint diagnostics shared by the managed capture databases.

use agentsight_sqlite_lifecycle::{CheckpointOutcome, LifecycleError, checkpoint_passive_stats};
use rusqlite::Connection;
use std::path::Path;
use std::time::Instant;

/// Count live pages from SQLite's current snapshot, excluding reusable WAL space.
pub(super) fn live_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    let (pages, free, page_size): (u64, u64, u64) = conn.query_row(
        "SELECT page_count, freelist_count, page_size
         FROM pragma_page_count(), pragma_freelist_count(), pragma_page_size()",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(pages.saturating_sub(free).saturating_mul(page_size))
}

/// Record WAL backlog and elapsed time without changing SQLite's durability.
pub(super) fn passive(
    conn: &Connection,
    store: &'static str,
) -> Result<CheckpointOutcome, LifecycleError> {
    if let Some(path) = conn.path() {
        let mut wal = Path::new(path).as_os_str().to_os_string();
        wal.push("-wal");
        if let Ok(metadata) = std::fs::metadata(wal) {
            metrics::gauge!("agentsight_sqlite_wal_bytes", "store" => store)
                .set(metadata.len() as f64);
        }
    }
    let started = Instant::now();
    let result = checkpoint_passive_stats(conn);
    metrics::histogram!("agentsight_sqlite_checkpoint_seconds", "store" => store, "mode" => "passive")
        .record(started.elapsed().as_secs_f64());
    let stats = result?;
    metrics::gauge!("agentsight_sqlite_wal_frames", "store" => store).set(stats.frames as f64);
    metrics::gauge!("agentsight_sqlite_wal_copied_frames", "store" => store)
        .set(stats.copied as f64);
    metrics::gauge!("agentsight_sqlite_wal_pending_frames", "store" => store)
        .set(stats.frames.saturating_sub(stats.copied) as f64);
    metrics::counter!("agentsight_sqlite_checkpoints_total", "store" => store,
        "outcome" => if stats.outcome == CheckpointOutcome::Busy { "busy" } else { "completed" })
    .increment(1);
    Ok(stats.outcome)
}

/// Record the non-waiting truncation separately from the preceding frame copy.
pub(super) fn truncate(
    conn: &Connection,
    store: &'static str,
) -> Result<CheckpointOutcome, LifecycleError> {
    let started = Instant::now();
    let result = agentsight_sqlite_lifecycle::checkpoint_truncate(conn);
    metrics::histogram!("agentsight_sqlite_checkpoint_seconds", "store" => store, "mode" => "truncate")
        .record(started.elapsed().as_secs_f64());
    result
}
