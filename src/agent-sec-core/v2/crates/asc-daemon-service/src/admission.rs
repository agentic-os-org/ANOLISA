//! Connection quotas held through read, dispatch and response completion.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::ServiceConfig;

pub(crate) struct ConnectionAdmission {
    public_limit: usize,
    reserved_limit: usize,
    uid_limit: usize,
    state: Arc<Mutex<AdmissionState>>,
}

#[derive(Default)]
struct AdmissionState {
    public: usize,
    reserved: usize,
    ordinary_uids: BTreeMap<u32, usize>,
}

impl ConnectionAdmission {
    pub(crate) fn new(config: &ServiceConfig) -> Self {
        Self {
            public_limit: config.max_connections - config.reserved_administrator_connections,
            reserved_limit: config.reserved_administrator_connections,
            uid_limit: config.max_connections_per_uid,
            state: Arc::default(),
        }
    }

    pub(crate) fn try_acquire(&self, uid: u32, administrator: bool) -> Option<ConnectionPermit> {
        // Poisoned accounting fails closed rather than reusing uncertain capacity.
        let mut state = self.state.lock().ok()?;
        if !administrator && state.ordinary_uids.get(&uid).copied().unwrap_or(0) >= self.uid_limit {
            return None;
        }
        let reserved = state.public >= self.public_limit;
        if reserved && (!administrator || state.reserved >= self.reserved_limit) {
            return None;
        }
        let ordinary_uid = (!administrator).then_some(uid);
        if let Some(uid) = ordinary_uid {
            *state.ordinary_uids.entry(uid).or_default() += 1;
        }
        if reserved {
            state.reserved += 1;
        } else {
            state.public += 1;
        }
        Some(ConnectionPermit {
            state: Arc::clone(&self.state),
            reserved,
            ordinary_uid,
        })
    }
}

pub(crate) struct ConnectionPermit {
    state: Arc<Mutex<AdmissionState>>,
    reserved: bool,
    ordinary_uid: Option<u32>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            // New admissions also reject a poisoned lock, so no capacity can be reused.
            return;
        };
        if self.reserved {
            state.reserved -= 1;
        } else {
            state.public -= 1;
        }
        if let Some(uid) = self.ordinary_uid
            && let Some(count) = state.ordinary_uids.get_mut(&uid)
        {
            *count -= 1;
            if *count == 0 {
                state.ordinary_uids.remove(&uid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn uid_quotas_and_administrator_reserve_share_one_bounded_lifetime() {
        let admission = ConnectionAdmission::new(&ServiceConfig {
            max_request_frame_bytes: 64,
            max_response_frame_bytes: 64,
            max_connections: 4,
            reserved_administrator_connections: 1,
            max_connections_per_uid: 1,
            max_rejection_connections: 1,
            rejection_encode_timeout: Duration::from_secs(1),
            request_read_timeout: Duration::from_secs(1),
            dispatch_timeout: Duration::from_secs(1),
            max_dispatch_timeout: Duration::from_secs(1),
            response_write_timeout: Duration::from_secs(1),
            drain_timeout: Duration::from_secs(1),
            accept_error_backoff: Duration::from_secs(1),
        });
        let first = admission.try_acquire(1001, false).unwrap();
        assert!(admission.try_acquire(1001, false).is_none());
        let second = admission.try_acquire(1002, false).unwrap();
        // Administrators use public capacity before taking the reserve.
        let public_admin = admission.try_acquire(0, true).unwrap();
        assert!(admission.try_acquire(1003, false).is_none());
        let reserved_admin = admission.try_acquire(2001, true).unwrap();
        assert!(admission.try_acquire(0, true).is_none());

        drop(reserved_admin);
        assert!(admission.try_acquire(1003, false).is_none());
        let reserved_admin = admission.try_acquire(0, true).unwrap();
        drop(first);
        let replacement = admission.try_acquire(1001, false).unwrap();
        drop((second, public_admin, reserved_admin, replacement));
        let state = admission.state.lock().unwrap();
        assert_eq!((state.public, state.reserved), (0, 0));
        assert!(state.ordinary_uids.is_empty());
    }
}
