//! Doc-hidden binary facade for `cosh-ui-worker`.

/// Runs the standalone UI worker process.
#[doc(hidden)]
#[must_use]
pub fn main_entry() -> i32 {
    match crate::ui::worker_runtime::run_from_args(std::env::args_os()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
