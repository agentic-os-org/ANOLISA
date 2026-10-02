//! `SQLite` schema revision registry for security-event storage.
//!
//! The version is *derived* from the revision table rather than declared
//! separately, so adding a revision without bumping the version is impossible.

/// Ordered schema revisions with their descriptions.
pub const SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS: &[(u32, &str)] = &[
    (1, "initial security_events table"),
    (
        2,
        "add run_id, call_id, and tool_call_id correlation columns",
    ),
    (3, "add verdict column and backfill from event details"),
];

/// Revision that introduced the `verdict` column and its backfill.
pub const SECURITY_EVENTS_VERDICT_SCHEMA_VERSION: u32 = 3;

/// Highest known schema revision, derived from the registry.
pub const SECURITY_EVENTS_SQLITE_SCHEMA_VERSION: u32 = max_revision();

const fn max_revision() -> u32 {
    let mut max = 0;
    let mut index = 0;
    while index < SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS.len() {
        let (revision, _) = SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS[index];
        if revision > max {
            max = revision;
        }
        index += 1;
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard test mirroring v1 `test_schema_version_tracks_revision_history`.
    #[test]
    fn schema_version_tracks_revision_history() {
        let highest = SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS
            .iter()
            .map(|(revision, _)| *revision)
            .max()
            .expect("revision registry must not be empty");
        assert_eq!(SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, highest);
        assert_eq!(SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, 3);
    }

    #[test]
    fn verdict_revision_is_within_the_registry() {
        assert!(
            SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS
                .iter()
                .any(|(revision, _)| *revision == SECURITY_EVENTS_VERDICT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn revisions_are_contiguous_and_ascending() {
        for (index, (revision, description)) in
            SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS.iter().enumerate()
        {
            assert_eq!(
                *revision,
                u32::try_from(index).expect("registry stays small") + 1,
                "revision numbering must stay contiguous"
            );
            assert!(
                !description.is_empty(),
                "every revision needs a description"
            );
        }
    }
}
