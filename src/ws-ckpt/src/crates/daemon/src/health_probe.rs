//! Keep cancellable health probes independent of queries serving workspace mutations.

use std::future::Future;

use tokio::sync::{Mutex, MutexGuard};

tokio::task_local! {
    static HEALTH_QUERY: ();
}

static HEALTH_PROBE_SLOT: Mutex<()> = Mutex::const_new(());

/// Marks directly awaited backend queries as optional health work.
/// Spawned child owners retain their slot explicitly because task locals do not propagate.
pub(crate) async fn scope<F: Future>(future: F) -> F::Output {
    HEALTH_QUERY.scope((), future).await
}

/// Reserves cleanup capacity for health queries; business queries never wait here.
pub(crate) async fn acquire_slot() -> Option<MutexGuard<'static, ()>> {
    if HEALTH_QUERY.try_with(|_| ()).is_ok() {
        Some(HEALTH_PROBE_SLOT.lock().await)
    } else {
        None
    }
}

/// Verifies that production health entry points preserve context through backend awaits.
#[cfg(test)]
pub(crate) fn assert_scoped() {
    assert!(
        HEALTH_QUERY.try_with(|_| ()).is_ok(),
        "health query must retain its scope"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn scope_does_not_leak_into_joined_business_future() {
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let health = scope(async {
            let _slot = acquire_slot()
                .await
                .expect("health query must reserve a slot");
            held_tx.send(()).unwrap();
            release_rx.await.unwrap();
        });
        let business = async {
            held_rx.await.unwrap();
            let slot = tokio::time::timeout(Duration::from_secs(1), acquire_slot()).await;
            release_tx.send(()).unwrap();
            assert!(slot
                .expect("business query must bypass the held slot")
                .is_none());
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(health, business);
        })
        .await
        .expect("scope must restore the task local on every poll");
    }
}
