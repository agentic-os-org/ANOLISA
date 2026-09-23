//! Backend-neutral package *mutation* contract.
//!
//! [`PackageTransaction`] is the write-side counterpart to
//! [`PackageQuery`](crate::pkg_query::PackageQuery): where the query contract
//! only reads rpmdb / repo metadata, this one runs the package-manager
//! transactions ANOLISA delegates to dnf/rpm — `install`, `update`,
//! `reinstall`, and `remove`. The CLI uses them from `anolisa install`,
//! `update`, `repair`, and `uninstall`; `reinstall` is a backend repair
//! operation, not a standalone CLI command.
//!
//! The trait is object-safe so the CLI can hold a `&dyn PackageTransaction`
//! and inject a fake in tests instead of shelling out to a live `dnf`.
//! Privilege checks and post-transaction state refresh are the caller's
//! responsibility; this layer only spawns the transaction and classifies its
//! outcome.

use thiserror::Error;

/// Errors raised by [`PackageTransaction`] backends.
///
/// Mirrors [`PackageQueryError`](crate::pkg_query::PackageQueryError)'s
/// spawn-vs-exit split: a missing or non-executable binary is a spawn-phase
/// fault, while a backend that ran and exited non-zero surfaces as
/// [`TransactionFailed`](PackageTransactionError::TransactionFailed).
#[derive(Debug, Error)]
pub enum PackageTransactionError {
    /// The backend binary could not be found (spawn `NotFound`).
    #[error("command not found: {command}")]
    CommandMissing {
        /// Backend binary that could not be found.
        command: String,
    },
    /// The backend binary existed but could not be executed
    /// (`PermissionDenied`). For a privileged transaction this typically
    /// means the process is not running as root.
    #[error("permission denied running {command}")]
    PermissionDenied {
        /// Backend binary that could not be executed.
        command: String,
    },
    /// The transaction ran but the backend reported a hard failure
    /// (non-zero exit). `stderr` carries the captured diagnostics so the
    /// caller can surface why dnf refused.
    #[error("{command} {operation} failed (code {code:?}): {stderr}")]
    TransactionFailed {
        /// Backend binary that exited with a failure.
        command: String,
        /// Transaction verb that failed (e.g. `update`).
        operation: String,
        /// Exit code; `None` if the process was killed by a signal.
        code: Option<i32>,
        /// Captured diagnostics from the failed transaction.
        stderr: String,
    },
}

/// Backend-neutral package mutation contract.
///
/// All methods take `&self` and return concrete types, so the trait is
/// object-safe and any backend can be held as `Box<dyn PackageTransaction>`.
///
/// Every verb takes a `packages` slice and must run **one** native
/// transaction over the whole set: the package manager's solver sees all
/// packages together, and the transaction commits or fails as a unit. A
/// single-package call is the one-element slice; callers must not pass an
/// empty slice.
pub trait PackageTransaction {
    /// Explicit source constrained by this transaction, not historical installation provenance.
    /// Record it only after a successful install/reinstall and observation, or an
    /// update whose observation confirms the version changed. A no-op proves no origin.
    fn repository_source(&self) -> Option<&str> {
        None
    }

    /// Resolve an install using the same repositories and solver as apply,
    /// without downloading packages, running scriptlets, or changing rpmdb.
    /// Metadata caches may be refreshed. Pass the exact pinned specs, if any.
    ///
    /// # Errors
    /// Returns [`PackageTransactionError`] if solving fails or cannot run.
    fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError>;

    /// Install `packages` from the configured repos in one transaction.
    ///
    /// Delegates the whole file transaction (dependency solving, download,
    /// scriptlets, rpmdb write) to the package manager. ANOLISA records the
    /// result as an ANOLISA-delegated *managed* install — the package manager
    /// owns the files and a later uninstall delegates back to it. A package
    /// that is already installed is a success
    /// (the backend performs a no-op), not an error.
    ///
    /// # Errors
    /// See [`PackageTransactionError`]. A failure reports the transaction as
    /// a whole — the backend does not attribute it to one package. The caller
    /// owns the privilege precondition and records ANOLISA state from rpmdb
    /// afterwards.
    fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError>;

    /// Update `packages` to the latest candidates the configured repos offer,
    /// in one transaction.
    ///
    /// Delegates the whole file transaction (download, scriptlets, rpmdb
    /// write) to the package manager — ANOLISA never touches RPM-owned files
    /// directly. The update does **not** switch backends: it upgrades the
    /// packages in place. A package that is already at the latest version is a
    /// success (the backend performs a no-op), not an error.
    ///
    /// # Errors
    /// See [`PackageTransactionError`] for the failure conditions. The caller
    /// is responsible for the privilege precondition and for refreshing
    /// ANOLISA state from rpmdb after a successful update.
    fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError>;

    /// Reinstall `packages` at their currently installed versions, in one
    /// transaction.
    ///
    /// Delegates the whole file transaction to the package manager's
    /// reinstall operation. Unlike [`install`], which is a no-op
    /// success for an already-installed package, reinstall re-runs the file
    /// transaction so damaged or missing files are restored from the package
    /// payload. Versions do not change; a package that is absent is a
    /// backend hard failure, so the caller should confirm presence first.
    ///
    /// # Errors
    /// See [`PackageTransactionError`]. The caller owns the privilege
    /// precondition and refreshes ANOLISA state from rpmdb afterwards.
    ///
    /// [`install`]: PackageTransaction::install
    fn reinstall(&self, packages: &[&str]) -> Result<(), PackageTransactionError>;

    /// Remove `packages` in one transaction, delegating the file transaction
    /// (scriptlets, rpmdb write) to the package manager — ANOLISA never
    /// deletes RPM-owned files directly. A package that is already absent is
    /// reported by the backend as a hard failure (no match), so the caller
    /// should confirm presence first when it wants to treat "already gone" as
    /// success.
    ///
    /// This method is only the spawn/exit mechanism; **whether** a removal is
    /// authorized is the caller's decision. For an `rpm-observed` package
    /// without removal authority, the caller must require an
    /// explicit `--remove-system-package` override before invoking this, so a
    /// preinstalled system RPM is never dropped by a default uninstall.
    ///
    /// # Errors
    /// See [`PackageTransactionError`] for the failure conditions. The caller
    /// owns the privilege precondition and drops ANOLISA state after a
    /// successful removal.
    fn remove(&self, packages: &[&str]) -> Result<(), PackageTransactionError>;
}
