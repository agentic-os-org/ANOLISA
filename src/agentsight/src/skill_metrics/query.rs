//! Skill metrics query helpers.
//!
//! Provides time-range based queries for skill metrics computation.

use crate::storage::sqlite::GenAISqliteStore;
use crate::storage::sqlite::genai::TraceEventDetail;

/// Query all events within a time range, optionally filtered by agent name.
pub fn get_events_in_time_range(
    store: &GenAISqliteStore,
    start_ns: i64,
    end_ns: i64,
    agent_name: Option<&str>,
) -> Result<Vec<TraceEventDetail>, Box<dyn std::error::Error>> {
    store.get_events_in_time_range(start_ns, end_ns, agent_name)
}
