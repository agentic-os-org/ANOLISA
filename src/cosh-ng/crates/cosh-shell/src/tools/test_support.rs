//! Shared test infrastructure for the `tools` modules.
//!
//! The readonly executors' tests mutate the process-global `PATH`
//! environment to prove shadow-binary resistance, and several other
//! tests (the guarded diagnostics) spawn their programs by bare name,
//! which resolves through the same process `PATH`. Two independent
//! tests doing either race each other, and a mid-test panic used to
//! leak a mutated `PATH` into the rest of the suite. Both problems
//! are fixed by ONE lock at the `tools` module root that every
//! PATH-mutating AND every bare-name-spawning test in this crate must
//! hold, plus an RAII guard that restores the original value on scope
//! exit — including the panic path. The lock lives here so all of
//! them share the same instance; a test-local `static` would be a
//! private lock serializing nothing but itself.
//!
//! Scope note: the contract is crate-local by design. Tests that need
//! to spawn children with absolute, pre-resolved paths (the readonly
//! executors after trusted-dirs pinning) do not resolve through `PATH`
//! and do not need the lock; only bare-name spawns do. If a new test
//! in this crate spawns a bare program name, it must acquire
//! [`path_env_guard`] first.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The single, project-level lock serializing every test that mutates
/// the process-global `PATH`. All such tests must acquire this lock —
/// through [`path_env_guard`] — before touching the variable.
pub(crate) fn path_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Acquire the shared `PATH` lock and capture the current `PATH`.
///
/// Dropping the returned guard restores the captured value — on normal
/// scope exit AND on panic — so a failing test can never leak its
/// shadowed `PATH` into the rest of the suite.
pub(crate) struct PathEnvGuard {
    _lock: MutexGuard<'static, ()>,
    original: Option<OsString>,
}

impl Drop for PathEnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// Take the shared lock and snapshot `PATH`. Hold the guard for the
/// whole body of a `PATH`-mutating test.
pub(crate) fn path_env_guard() -> PathEnvGuard {
    PathEnvGuard {
        _lock: path_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        original: std::env::var_os("PATH"),
    }
}
