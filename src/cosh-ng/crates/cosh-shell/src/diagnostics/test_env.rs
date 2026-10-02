//! Process-wide lock for tests that touch process environment variables.

use std::sync::{Mutex, MutexGuard};

/// Serializes tests that read or mutate shared env vars such as HOME.
///
/// Env mutators must hold the guard for their whole set-use-restore span;
/// readers whose assertions depend on a stable env (e.g. HOME redaction)
/// must hold it across the read and the assertion. Separate per-module
/// locks cannot prevent these tests from observing each other's mutations.
pub(crate) fn env_guard() -> MutexGuard<'static, ()> {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}
