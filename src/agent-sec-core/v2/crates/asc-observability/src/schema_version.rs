//! `SQLite` schema revision for the observability index.

/// Revision 2 adds trusted UID ownership and owner-scoped indexes.
///
/// Every new write requires a trusted UID. Revision 1 stores predate ownership:
/// the kernel converges them by adding a nullable `uid` column, so historical
/// rows keep an unknown owner (`NULL`) while new rows always carry one.
pub const OBSERVABILITY_SQLITE_SCHEMA_VERSION: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_includes_uid_ownership() {
        assert_eq!(OBSERVABILITY_SQLITE_SCHEMA_VERSION, 2);
    }
}
