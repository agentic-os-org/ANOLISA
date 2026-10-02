"""SQLite schema version metadata for security event storage."""

SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS = {
    1: "initial security_events table",
    2: "add run_id, call_id, and tool_call_id correlation columns",
    3: "add verdict column and backfill from event details",
}

SECURITY_EVENTS_VERDICT_SCHEMA_VERSION = 3
SECURITY_EVENTS_SQLITE_SCHEMA_VERSION = max(SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS)


__all__ = [
    "SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS",
    "SECURITY_EVENTS_SQLITE_SCHEMA_VERSION",
    "SECURITY_EVENTS_VERDICT_SCHEMA_VERSION",
]
