//! `SQLite` schema version for the observability index.

/// Schema version of the `observability_events` table.
///
/// Migrated from v1 `observability/models.py`. Unlike security events there is
/// no revision registry and no migration callback: the table has only ever had
/// one shape, and the store is opened with no migrator at all.
pub const OBSERVABILITY_SQLITE_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_matches_v1() {
        assert_eq!(OBSERVABILITY_SQLITE_SCHEMA_VERSION, 1);
    }
}
