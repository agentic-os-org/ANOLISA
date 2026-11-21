//! Non-ECS Alibaba Cloud OAuth credential source for the SysOM provider.

pub mod client;
pub mod contract;
pub mod login;
pub mod logout;
pub mod loopback;
pub mod manager;
pub mod store;
#[cfg(test)]
pub mod test_support;

use std::path::Path;
use std::sync::Arc;

/// Effective OAuth source shared by every entry point. `NotSelected` keeps the
/// legacy `[ai]` resolution; an explicit provider change after login supersedes
/// the OAuth pointer because the snapshot no longer matches.
/// Effective OAuth source shared by every entry point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// A committed login under the current provider snapshot.
    Active { generation: u64, client_id: String },
    /// Selection sticky but credentials unavailable; never falls back to AK.
    NeedsLogin,
    /// Legacy `[ai]` resolution applies.
    NotSelected,
}

/// Resolves selection, snapshot supersede and deactivation from local state.
pub fn resolve_source(
    dir: &Path,
    active_provider: Option<&str>,
    backend: Arc<dyn store::OAuthSecretBackend>,
) -> Result<SourceState, contract::OAuthError> {
    let state = store::OAuthStore::new(dir, backend).read_state()?;
    if !state.selected {
        return Ok(SourceState::NotSelected);
    }
    match state.active {
        Some(login) if state.provider_snapshot.as_deref() == active_provider => {
            Ok(SourceState::Active {
                generation: login.generation,
                client_id: login.client_id,
            })
        }
        Some(_) => Ok(SourceState::NotSelected),
        None if state.provider_snapshot.as_deref() == active_provider => {
            Ok(SourceState::NeedsLogin)
        }
        None => Ok(SourceState::NotSelected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryBackend {
        slots: Mutex<std::collections::HashMap<String, String>>,
    }
    impl store::OAuthSecretBackend for MemoryBackend {
        fn set(&self, slot: &str, token: &str) -> Result<(), contract::OAuthError> {
            self.slots.lock().unwrap().insert(slot.into(), token.into());
            Ok(())
        }
        fn get(&self, slot: &str) -> Result<Option<String>, contract::OAuthError> {
            Ok(self.slots.lock().unwrap().get(slot).cloned())
        }
        fn delete(&self, slot: &str) -> Result<(), contract::OAuthError> {
            self.slots.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    fn backend() -> Arc<MemoryBackend> {
        Arc::new(MemoryBackend::default())
    }

    #[test]
    fn resolution_follows_selection_snapshot_and_deactivation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store::OAuthStore::new(dir.path(), backend());
        assert_eq!(
            resolve_source(dir.path(), None, backend()).unwrap(),
            SourceState::NotSelected
        );
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh", None)
            .unwrap();
        assert!(matches!(
            resolve_source(dir.path(), None, backend()).unwrap(),
            SourceState::Active { generation: 1, .. }
        ));
        assert_eq!(
            resolve_source(dir.path(), Some("other"), backend()).unwrap(),
            SourceState::NotSelected,
            "explicit provider change after login wins"
        );
        store.deactivate_generation(1).unwrap();
        assert_eq!(
            resolve_source(dir.path(), None, backend()).unwrap(),
            SourceState::NeedsLogin,
            "deactivation keeps the selection; never falls back to AK"
        );
        assert_eq!(
            resolve_source(dir.path(), Some("ecs-provider"), backend()).unwrap(),
            SourceState::NotSelected,
            "a deactivated OAuth login must not block a different explicit provider (ECS)"
        );
        store.commit_logout().unwrap();
        assert_eq!(
            resolve_source(dir.path(), None, backend()).unwrap(),
            SourceState::NotSelected
        );
    }

    #[test]
    fn snapshot_match_resumes_oauth_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = store::OAuthStore::new(dir.path(), backend());
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh", Some("picked".into()))
            .unwrap();
        assert!(matches!(
            resolve_source(dir.path(), Some("picked"), backend()).unwrap(),
            SourceState::Active { generation: 1, .. }
        ));
        assert_eq!(
            resolve_source(dir.path(), None, backend()).unwrap(),
            SourceState::NotSelected
        );
    }

    #[test]
    fn unreadable_state_is_an_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(contract::state_file_name()), b"{corrupt").unwrap();
        assert!(resolve_source(dir.path(), None, backend()).is_err());
    }
}
