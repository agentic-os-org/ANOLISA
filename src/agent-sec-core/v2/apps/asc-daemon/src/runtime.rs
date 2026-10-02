use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use tokio::runtime::Builder;

/// Runs the daemon future on an explicitly owned Tokio runtime.
///
/// Runtime shutdown waits only for `shutdown_timeout`, so an application bug in
/// an already-started blocking task cannot keep the foreground daemon process
/// alive forever after service drain has completed. Tokio cannot cancel that
/// blocking task; this is a process-exit safety net, not request cancellation.
///
/// # Errors
/// Returns an error when the multi-thread runtime cannot be constructed.
pub fn run_with_shutdown_timeout<F>(
    future: F,
    shutdown_timeout: Duration,
) -> Result<F::Output, RuntimeError>
where
    F: Future,
{
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    let outcome = catch_unwind(AssertUnwindSafe(|| runtime.block_on(future)));
    runtime.shutdown_timeout(shutdown_timeout);
    match outcome {
        Ok(output) => Ok(output),
        Err(panic) => resume_unwind(panic),
    }
}

/// Failure to construct the daemon's asynchronous runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Tokio could not create its worker or driver resources.
    #[error("daemon runtime could not be created")]
    Create(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Instant;

    use super::*;

    #[test]
    fn runtime_shutdown_is_bounded_when_a_blocking_task_does_not_finish() {
        let started = Arc::new(AtomicBool::new(false));
        let blocking_started = Arc::clone(&started);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let blocking_gate = Arc::clone(&gate);
        let (finished_sender, finished_receiver) = mpsc::channel();
        let before = Instant::now();

        run_with_shutdown_timeout(
            async move {
                tokio::task::spawn_blocking(move || {
                    blocking_started.store(true, Ordering::Release);
                    let (lock, ready) = &*blocking_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = ready.wait(released).unwrap();
                    }
                    finished_sender.send(()).unwrap();
                });
                while !started.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            },
            Duration::from_millis(25),
        )
        .unwrap();

        assert!(before.elapsed() < Duration::from_millis(500));
        let (lock, ready) = &*gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
}
