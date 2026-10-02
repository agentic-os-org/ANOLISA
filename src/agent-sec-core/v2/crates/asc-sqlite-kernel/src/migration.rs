//! The injectable schema migration hook.
//!
//! v1 declares
//! `SchemaMigration = Callable[[Connection, int, int, tuple[OrmModel, ...], str], None]`
//! and threads it through `ensure_schema`, `ensure_schema_if_needed` and
//! `SqliteStore` as a **parameter**. It is not an internal hard-wired call, and
//! v2 must keep that property: the observability stream passes `None`, and the
//! framework has to converge tables, add columns and stamp the version with no
//! migrator at all.
//!
//! Two mechanisms coexist in v1 and both must exist here:
//!
//! | Range | Mechanism |
//! |---|---|
//! | 1 → 2 | purely generic `extra_columns` convergence, no callback involved |
//! | 2 → 3 | callback does `ALTER` **and** a data backfill, while the column is *also* listed in `extra_columns` as a belt-and-braces path |
//!
//! Implementing only the callback path would leave rev-1 databases without the
//! three correlation columns.

use rusqlite::Connection;

use crate::error::KernelError;

/// A version-range migration applied before generic column convergence.
pub trait SchemaMigrator: Send + Sync {
    /// Migrates from `from` to `to`.
    ///
    /// Called on the phase-one connection, which is **not** inside an enclosing
    /// transaction, so implementations may commit in batches to avoid holding a
    /// long write lock. Implementations must guard their own version range and
    /// be idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the migration cannot complete. The caller
    /// aborts schema convergence.
    fn migrate(
        &self,
        conn: &Connection,
        from: u32,
        to: u32,
        log_prefix: &str,
    ) -> Result<(), KernelError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records its invocations so tests can assert the call contract.
    #[derive(Default)]
    struct SpyMigrator {
        calls: Mutex<Vec<(u32, u32)>>,
    }

    impl SchemaMigrator for SpyMigrator {
        fn migrate(
            &self,
            _conn: &Connection,
            from: u32,
            to: u32,
            _log_prefix: &str,
        ) -> Result<(), KernelError> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((from, to));
            Ok(())
        }
    }

    #[test]
    fn a_migrator_is_object_safe_and_shareable() {
        let spy = SpyMigrator::default();
        let as_dyn: &dyn SchemaMigrator = &spy;
        let conn = Connection::open_in_memory().expect("memory db");
        as_dyn.migrate(&conn, 1, 3, "[test]").expect("migrate");
        assert_eq!(
            *spy.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![(1, 3)]
        );
    }
}
