//! Debug-build-only deterministic storage fault controls.

use super::{SqliteTaskStore, StoreError};

impl SqliteTaskStore {
    /// Prevents the owned SQLite writer from allocating another database page.
    ///
    /// This narrow debug-only control exercises SQLite's real `SQLITE_FULL`
    /// transaction path without a release-build failpoint or host mutation.
    ///
    /// # Errors
    ///
    /// Returns a SQLite error when the page-count pragmas cannot be read or
    /// updated.
    #[doc(hidden)]
    pub fn freeze_database_growth_for_test(&mut self) -> Result<u64, StoreError> {
        let page_count = self
            .connection_mut()
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))?;
        let effective_limit = self.connection_mut().query_row(
            &format!("PRAGMA max_page_count = {page_count}"),
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(effective_limit).map_err(|_| StoreError::Corrupt {
            message: "SQLite returned a negative maximum page count".to_owned(),
        })
    }
}
