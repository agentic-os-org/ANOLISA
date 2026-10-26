//! Per-invocation RTK receipts, independent of optional compression statistics.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;

use super::path_policy;

/// Failure to persist or consume an RTK execution receipt.
#[derive(Debug, Error)]
pub enum OutcomeError {
    /// The state path violates the shared filesystem policy.
    #[error(transparent)]
    Path(#[from] path_policy::PathPolicyError),
    /// The private state directory could not be created.
    #[error("cannot create RTK outcome directory: {0}")]
    Directory(#[from] std::io::Error),
    /// The receipt database could not complete the operation.
    #[error("cannot access RTK outcomes: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Storage for complete, call-scoped RTK execution evidence.
pub struct OutcomeStore(Connection);

impl OutcomeStore {
    /// Returns the receipt database location without creating it.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("rtk-outcomes.db")
    }

    /// Opens the receipt store, optionally creating its private directory and schema.
    ///
    /// # Errors
    /// Returns path-policy, directory-creation, or SQLite errors.
    pub fn open(data_dir: &Path, create: bool) -> Result<Self, OutcomeError> {
        let data_dir = path_policy::validate_data_dir(data_dir)?;
        let path =
            path_policy::validate_database_path(&Self::path(&data_dir), &[data_dir.as_path()])?;
        if create {
            path_policy::ensure_state_dir(&data_dir)?;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | if create {
                OpenFlags::SQLITE_OPEN_CREATE
            } else {
                OpenFlags::empty()
            };
        let connection = Connection::open_with_flags(path, flags)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        if create {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS calls (
                    agent TEXT NOT NULL, session TEXT NOT NULL, tool TEXT NOT NULL,
                    run TEXT NOT NULL UNIQUE, expected INTEGER NOT NULL,
                    command TEXT NOT NULL, executed_command TEXT NOT NULL,
                    created INTEGER NOT NULL DEFAULT (unixepoch()),
                    PRIMARY KEY (agent, session, tool)
                );
                CREATE INDEX IF NOT EXISTS calls_created ON calls(created);
                CREATE TABLE IF NOT EXISTS outcomes (
                    run TEXT NOT NULL REFERENCES calls(run) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL, changed INTEGER NOT NULL,
                    PRIMARY KEY (run, ordinal)
                );",
            )?;
        }
        Ok(Self(connection))
    }

    /// Registers and returns a generation-bound command, expiring abandoned receipts after one day.
    ///
    /// The renderer embeds the supplied run ID in the command that the host executes.
    /// Registration stores that exact command atomically with its original provenance.
    /// Missing attribution disables receipt collection. Zero rewritten commands invalidate
    /// the previous generation without starting a new one.
    ///
    /// # Errors
    /// Returns SQLite errors without authorizing fallback.
    pub fn begin(
        &mut self,
        agent: &str,
        session: &str,
        tool: &str,
        expected: usize,
        command: &str,
        render: impl FnOnce(&str) -> String,
    ) -> Result<Option<String>, OutcomeError> {
        if agent.is_empty() || session.is_empty() || tool.is_empty() {
            return Ok(None);
        }
        let transaction = self.0.transaction()?;
        transaction.execute("DELETE FROM calls WHERE created < unixepoch() - 86400", [])?;
        transaction.execute(
            "DELETE FROM calls WHERE agent = ?1 AND session = ?2 AND tool = ?3",
            params![agent, session, tool],
        )?;
        if expected == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        let run: String =
            transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
        let executed_command = render(&run);
        transaction.execute(
            "INSERT INTO calls (agent, session, tool, run, expected, command, executed_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                agent,
                session,
                tool,
                run,
                expected,
                command,
                executed_command
            ],
        )?;
        transaction.commit()?;
        Ok(Some(executed_command))
    }

    /// Records one rewritten command; any observed output changes retain RTK ownership.
    ///
    /// Unknown generations and out-of-range ordinals cannot create receipts.
    ///
    /// # Errors
    /// Returns SQLite errors; callers must retain the conservative RTK guard.
    pub fn record(&self, run: &str, ordinal: usize, changed: bool) -> Result<(), OutcomeError> {
        self.0.execute(
            "INSERT INTO outcomes (run, ordinal, changed)
             SELECT run, ?2, ?3 FROM calls
             WHERE run = ?1 AND ?2 < expected AND created >= unixepoch() - 86400
             ON CONFLICT (run, ordinal) DO UPDATE SET changed = max(changed, excluded.changed)",
            params![run, ordinal, changed],
        )?;
        Ok(())
    }

    /// Consumes a call and returns its original command only if every RTK output was unchanged.
    ///
    /// The executed command must exactly match the registered generation. Missing,
    /// mismatched, partial, expired, or already-consumed evidence returns `None`.
    ///
    /// # Errors
    /// Returns SQLite errors without authorizing fallback.
    pub fn consume_unchanged(
        &mut self,
        agent: &str,
        session: &str,
        tool: &str,
        executed_command: &str,
    ) -> Result<Option<String>, OutcomeError> {
        // Legacy RTK and untracked calls have no row and need no write transaction.
        let exists: bool = self.0.query_row(
            "SELECT EXISTS (SELECT 1 FROM calls WHERE agent = ?1 AND session = ?2 AND tool = ?3 AND executed_command = ?4)",
            params![agent, session, tool, executed_command],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let transaction = self.0.transaction()?;
        let original_command = transaction
            .query_row(
                "SELECT command FROM calls
                 WHERE agent = ?1 AND session = ?2 AND tool = ?3 AND executed_command = ?4
                    AND created >= unixepoch() - 86400
                    AND expected = (SELECT count(*) FROM outcomes WHERE run = calls.run)
                    AND NOT EXISTS (SELECT 1 FROM outcomes WHERE run = calls.run AND changed != 0)",
                params![agent, session, tool, executed_command],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        transaction.execute(
            "DELETE FROM calls WHERE agent = ?1 AND session = ?2 AND tool = ?3 AND executed_command = ?4",
            params![agent, session, tool, executed_command],
        )?;
        transaction.commit()?;
        Ok(original_command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_must_report_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        for (results, expected) in [
            (vec![false, false], true),
            (vec![false, true], false),
            (vec![false], false),
            (vec![], false),
        ] {
            let run = store
                .begin(
                    "agent",
                    "session",
                    "tool",
                    2,
                    "git log && git show",
                    str::to_owned,
                )
                .unwrap()
                .unwrap();
            for (ordinal, changed) in results.into_iter().enumerate() {
                store.record(&run, ordinal, changed).unwrap();
            }
            assert_eq!(
                store
                    .consume_unchanged("agent", "session", "tool", &run)
                    .unwrap()
                    .as_deref(),
                expected.then_some("git log && git show")
            );
            assert!(
                store
                    .consume_unchanged("agent", "session", "tool", &run)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn stale_generations_and_duplicate_reports_cannot_authorize_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        let old = store
            .begin("agent", "session", "tool", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        let current = store
            .begin("agent", "session", "tool", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&old, 0, false).unwrap();
        store.record(&current, 1, false).unwrap();
        assert!(
            store
                .consume_unchanged("agent", "session", "tool", &current)
                .unwrap()
                .is_none()
        );

        let run = store
            .begin("agent", "session", "tool", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&run, 0, true).unwrap();
        store.record(&run, 0, false).unwrap();
        assert!(
            store
                .consume_unchanged("agent", "session", "tool", &run)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mismatched_commands_cannot_consume_the_current_generation() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        let run = store
            .begin("agent", "session", "tool", 1, "cat data", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&run, 0, false).unwrap();
        for command in ["", "cat data", "previous-generation"] {
            assert!(
                store
                    .consume_unchanged("agent", "session", "tool", command)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            store
                .consume_unchanged("agent", "session", "tool", &run)
                .unwrap()
                .as_deref(),
            Some("cat data")
        );
    }

    #[test]
    fn untracked_retry_invalidates_receipts_and_late_producers() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        let run = store
            .begin(
                "agent",
                "session",
                "tool",
                1,
                "cat clean.txt",
                str::to_owned,
            )
            .unwrap()
            .unwrap();
        store.record(&run, 0, false).unwrap();
        assert!(
            store
                .begin(
                    "agent",
                    "session",
                    "tool",
                    0,
                    "rtk read file && cat empty",
                    str::to_owned
                )
                .unwrap()
                .is_none()
        );
        store.record(&run, 0, false).unwrap();
        assert!(
            store
                .consume_unchanged("agent", "session", "tool", &run)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn session_isolation_expiry_and_missing_attribution_are_conservative() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = OutcomeStore::open(directory.path(), true).unwrap();
        assert!(
            store
                .begin("agent", "", "tool", 1, "git log", str::to_owned)
                .unwrap()
                .is_none()
        );
        let run = store
            .begin("agent", "session", "tool", 1, "git log", str::to_owned)
            .unwrap()
            .unwrap();
        store.record(&run, 0, false).unwrap();
        assert!(
            store
                .consume_unchanged("agent", "other", "tool", &run)
                .unwrap()
                .is_none()
        );
        store.0.execute("UPDATE calls SET created = 0", []).unwrap();
        assert!(
            store
                .consume_unchanged("agent", "session", "tool", &run)
                .unwrap()
                .is_none()
        );
    }
}
