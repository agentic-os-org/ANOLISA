//! Parent-death watchdog for the headless backend: the shell spawns cosh-core
//! with setsid, so a terminal disconnect never reaches core as a signal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct ParentWatch {
    dead: AtomicBool,
    notify: Notify,
}

impl ParentWatch {
    /// Starts polling the current parent pid; reparenting means the shell died.
    pub(crate) fn spawn() -> Arc<Self> {
        let watch = Arc::new(Self {
            dead: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let original = nix::unistd::getppid();
        let weak = Arc::downgrade(&watch);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let Some(watch) = weak.upgrade() else { return };
                if nix::unistd::getppid() != original {
                    watch.dead.store(true, Ordering::SeqCst);
                    // notify_one keeps a permit so a waiter registering after
                    // the death still wakes; notify_waiters would lose it.
                    watch.notify.notify_one();
                    return;
                }
            }
        });
        watch
    }

    pub(crate) fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    pub(crate) async fn notified(&self) {
        loop {
            if self.is_dead() {
                return;
            }
            self.notify.notified().await;
        }
    }
}
