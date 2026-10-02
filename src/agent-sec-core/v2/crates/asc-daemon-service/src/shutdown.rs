use tokio::sync::watch;

/// Cloneable cooperative shutdown signal owned by the process bootstrap.
#[derive(Debug, Clone)]
pub struct ShutdownToken {
    sender: watch::Sender<bool>,
}

impl ShutdownToken {
    /// Creates a shutdown signal in the running state.
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self { sender }
    }

    /// Requests idempotent service shutdown.
    pub fn request(&self) {
        self.sender.send_replace(true);
    }

    /// Returns whether shutdown has already been requested.
    pub fn is_requested(&self) -> bool {
        *self.sender.borrow()
    }

    pub(crate) async fn wait(&self) {
        let mut receiver = self.sender.subscribe();
        if *receiver.borrow_and_update() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                return;
            }
        }
    }
}

impl Default for ShutdownToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_wakes_existing_and_future_waiters() {
        let shutdown = ShutdownToken::new();
        let waiter = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { shutdown.wait().await }
        });

        shutdown.request();
        waiter.await.unwrap();
        shutdown.wait().await;
        assert!(shutdown.is_requested());
    }
}
