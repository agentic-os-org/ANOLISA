//! Per-generation refresh slots and the local login-state commit protocol.
//!
//! The active selection lives in a dedicated local state file that older
//! versions ignore; `[ai]` and the original credential sources stay untouched.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::contract::OAuthError;

const LOCK_FILE: &str = "oauth-state.lock";

#[cfg(test)]
tokio::task_local! { pub(crate) static FAIL_STATE_WRITE: (); }

/// OS secret storage boundary for per-attempt refresh slots.
pub trait OAuthSecretBackend: Send + Sync {
    /// Stores a refresh token under a unique slot key.
    fn set(&self, slot: &str, refresh_token: &str) -> Result<(), OAuthError>;
    /// Reads a slot without exposing it to logs or registry views.
    fn get(&self, slot: &str) -> Result<Option<String>, OAuthError>;
    /// Removes a slot; missing slots are not an error.
    fn delete(&self, slot: &str) -> Result<(), OAuthError>;
}

/// Production backend in the environment-isolated `cosh-ng.oauth*` namespace.
pub struct KeyringOAuthBackend;

impl KeyringOAuthBackend {
    fn entry(slot: &str) -> Result<keyring::Entry, OAuthError> {
        keyring::Entry::new(
            super::contract::keyring_service(),
            &format!("refresh/{slot}"),
        )
        .map_err(|_| OAuthError::SecretStoreUnavailable)
    }
}

impl OAuthSecretBackend for KeyringOAuthBackend {
    fn set(&self, slot: &str, refresh_token: &str) -> Result<(), OAuthError> {
        Self::entry(slot)?
            .set_password(refresh_token)
            .map_err(|error| {
                tracing::warn!(error = ?error, "oauth keychain set failed");
                OAuthError::SecretStoreUnavailable
            })
    }

    fn get(&self, slot: &str) -> Result<Option<String>, OAuthError> {
        match Self::entry(slot)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                tracing::warn!(error = ?error, "oauth keychain get failed");
                Err(OAuthError::SecretStoreUnavailable)
            }
        }
    }

    fn delete(&self, slot: &str) -> Result<(), OAuthError> {
        match Self::entry(slot)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                tracing::warn!(error = ?error, "oauth keychain delete failed");
                Err(OAuthError::SecretStoreUnavailable)
            }
        }
    }
}

/// Currently committed login pointer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveLogin {
    /// Monotonic login generation bound to sessions and history.
    pub generation: u64,
    /// Unique secure-slot key holding this login's refresh token.
    pub slot: String,
    /// Native application id observed in the verified authorization URL.
    pub client_id: String,
    /// Commit timestamp (non-sensitive).
    pub committed_at: String,
}

/// Local selection state older versions ignore; `[ai]` stays untouched.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthLocalState {
    /// Envelope schema version.
    pub schema: u32,
    /// Monotonic commit counter fencing concurrent attempts.
    pub counter: u64,
    /// Sticky user selection: survives deactivation so requests never fall
    /// back to environment AK implicitly; cleared only by explicit logout.
    #[serde(default)]
    pub selected: bool,
    /// Committed login, absent after logout or deactivation.
    pub active: Option<ActiveLogin>,
    /// `[ai].active_provider` snapshot at commit time.
    pub provider_snapshot: Option<String>,
}

/// Atomic login-state commits plus secure-slot lifecycle.
pub struct OAuthStore {
    dir: PathBuf,
    backend: Arc<dyn OAuthSecretBackend>,
}

impl OAuthStore {
    /// Opens the store rooted at the configuration directory.
    pub fn new(dir: &Path, backend: Arc<dyn OAuthSecretBackend>) -> Self {
        Self {
            dir: dir.to_path_buf(),
            backend,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(super::contract::state_file_name())
    }

    /// Pure read: never triggers a refresh or writes anything.
    pub fn read_state(&self) -> Result<OAuthLocalState, OAuthError> {
        match fs::read(self.state_path()) {
            Ok(bytes) => {
                let state: OAuthLocalState =
                    serde_json::from_slice(&bytes).map_err(|_| OAuthError::StateUnavailable)?;
                if state.schema != 1 {
                    return Err(OAuthError::StateUnavailable);
                }
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OAuthLocalState {
                schema: 1,
                ..Default::default()
            }),
            Err(_) => Err(OAuthError::StateUnavailable),
        }
    }

    fn lock(&self) -> Result<fs::File, OAuthError> {
        fs::create_dir_all(&self.dir).map_err(|_| OAuthError::StateUnavailable)?;
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.dir.join(LOCK_FILE))
            .map_err(|_| OAuthError::StateUnavailable)?;
        file.lock_exclusive()
            .map_err(|_| OAuthError::StateUnavailable)?;
        Ok(file)
    }

    fn write_state(&self, state: &OAuthLocalState) -> Result<(), OAuthError> {
        #[cfg(test)]
        if FAIL_STATE_WRITE.try_with(|_| ()).is_ok() {
            return Err(OAuthError::StateUnavailable);
        }
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| OAuthError::StateUnavailable)?;
        let tmp = self.dir.join(format!(
            "{}.tmp-{}",
            super::contract::state_file_name(),
            uuid::Uuid::new_v4()
        ));
        let mut file = fs::File::create(&tmp).map_err(|_| OAuthError::StateUnavailable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
        }
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| OAuthError::StateUnavailable)?;
        drop(file);
        fs::rename(&tmp, self.state_path()).map_err(|_| {
            let _ = fs::remove_file(&tmp);
            OAuthError::StateUnavailable
        })
    }

    /// Write the candidate slot first, then atomically swap only the OAuth
    /// pointer under the config lock. Returns the replaced login, if any.
    pub fn commit_login(
        &self,
        starting: &OAuthLocalState,
        client_id: &str,
        refresh_token: &str,
        provider_snapshot: Option<String>,
    ) -> Result<(ActiveLogin, Option<ActiveLogin>), OAuthError> {
        let candidate = format!("candidate-{}", uuid::Uuid::new_v4());
        self.backend.set(&candidate, refresh_token)?;
        let result = (|| {
            let _guard = self.lock()?;
            let current = self.read_state()?;
            if current.counter != starting.counter || current.active != starting.active {
                return Err(OAuthError::GenerationChanged);
            }
            let generation = current.counter + 1;
            let login = ActiveLogin {
                generation,
                slot: candidate.clone(),
                client_id: client_id.to_owned(),
                committed_at: chrono::Utc::now().to_rfc3339(),
            };
            let next = OAuthLocalState {
                schema: 1,
                counter: generation,
                selected: true,
                active: Some(login.clone()),
                provider_snapshot,
            };
            self.write_state(&next)?;
            Ok((login, current.active))
        })();
        match result {
            Ok((login, replaced)) => {
                if let Some(previous) = &replaced {
                    let _ = self.backend.delete(&previous.slot);
                }
                Ok((login, replaced))
            }
            Err(error) => {
                let _ = self.backend.delete(&candidate);
                Err(error)
            }
        }
    }

    /// Commit the invalidation marker first, then clean the previously active
    /// slot. Returns the removed login so the caller can attempt remote revoke.
    pub fn commit_logout(&self) -> Result<Option<ActiveLogin>, OAuthError> {
        let _guard = self.lock()?;
        let current = self.read_state()?;
        let removed = current.active.clone();
        if removed.is_none() && !current.selected {
            return Ok(None);
        }
        let next = OAuthLocalState {
            schema: 1,
            counter: current.counter + 1,
            selected: false,
            active: None,
            provider_snapshot: current.provider_snapshot,
        };
        self.write_state(&next)?;
        if let Some(removed) = &removed {
            let _ = self.backend.delete(&removed.slot);
        }
        Ok(removed)
    }

    /// Deactivate a generation only while it is still the current one; late
    /// results after logout or account switch must not touch newer state.
    pub fn deactivate_generation(&self, generation: u64) -> Result<bool, OAuthError> {
        let _guard = self.lock()?;
        let current = self.read_state()?;
        if current.active.as_ref().map(|login| login.generation) != Some(generation) {
            return Ok(false);
        }
        let next = OAuthLocalState {
            schema: 1,
            counter: current.counter + 1,
            selected: current.selected,
            active: None,
            provider_snapshot: current.provider_snapshot,
        };
        self.write_state(&next)?;
        let _ = self.backend.delete(&current.active.unwrap().slot);
        Ok(true)
    }

    /// Reads the refresh token only while the generation is still current.
    pub fn load_refresh(&self, generation: u64) -> Result<Option<String>, OAuthError> {
        let state = self.read_state()?;
        match state.active {
            Some(login) if login.generation == generation => self.backend.get(&login.slot),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryBackend {
        slots: Mutex<std::collections::HashMap<String, String>>,
        log: Mutex<Vec<String>>,
        fail_set: std::sync::atomic::AtomicBool,
    }

    impl OAuthSecretBackend for MemoryBackend {
        fn set(&self, slot: &str, refresh_token: &str) -> Result<(), OAuthError> {
            self.log.lock().unwrap().push(format!("set:{slot}"));
            if self.fail_set.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(OAuthError::SecretStoreUnavailable);
            }
            self.slots
                .lock()
                .unwrap()
                .insert(slot.to_owned(), refresh_token.to_owned());
            Ok(())
        }
        fn get(&self, slot: &str) -> Result<Option<String>, OAuthError> {
            Ok(self.slots.lock().unwrap().get(slot).cloned())
        }
        fn delete(&self, slot: &str) -> Result<(), OAuthError> {
            self.log.lock().unwrap().push(format!("delete:{slot}"));
            self.slots.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    fn store() -> (tempfile::TempDir, Arc<MemoryBackend>, OAuthStore) {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryBackend::default());
        let store = OAuthStore::new(dir.path(), backend.clone());
        (dir, backend, store)
    }

    #[test]
    fn first_login_commits_slot_then_pointer() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        let (login, replaced) = store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        assert_eq!(login.generation, 1);
        assert!(replaced.is_none());
        assert_eq!(
            backend.get(&login.slot).unwrap().as_deref(),
            Some("refresh-1")
        );
        let state = store.read_state().unwrap();
        assert_eq!(state.active.as_ref().unwrap().generation, 1);
        assert_eq!(state.counter, 1);
        let log = backend.log.lock().unwrap();
        assert_eq!(log.len(), 1, "slot must exist before the pointer commit");
        assert!(log[0].starts_with("set:candidate-"));
    }

    #[test]
    fn account_switch_replaces_pointer_and_cleans_old_slot() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        let current = store.read_state().unwrap();
        let (login, replaced) = store
            .commit_login(&current, "client-2", "refresh-2", Some("other".into()))
            .unwrap();
        assert_eq!(login.generation, 2);
        assert_eq!(replaced.as_ref().unwrap().generation, 1);
        assert_eq!(replaced.as_ref().unwrap().client_id, "client-1");
        assert!(backend.get(&replaced.unwrap().slot).unwrap().is_none());
        assert_eq!(
            backend.get(&login.slot).unwrap().as_deref(),
            Some("refresh-2")
        );
        assert_eq!(
            store.read_state().unwrap().provider_snapshot.as_deref(),
            Some("other")
        );
    }

    #[test]
    fn stale_starting_state_cannot_commit_and_cleans_its_candidate() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        let error = store
            .commit_login(&starting, "client-2", "refresh-2", None)
            .unwrap_err();
        assert_eq!(error, OAuthError::GenerationChanged);
        assert_eq!(
            backend.slots.lock().unwrap().len(),
            1,
            "candidate slot leaked"
        );
        let state = store.read_state().unwrap();
        assert_eq!(state.active.as_ref().unwrap().client_id, "client-1");
        assert_eq!(state.counter, 1);
    }

    #[test]
    fn unavailable_secret_store_preserves_existing_state() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        backend
            .fail_set
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let current = store.read_state().unwrap();
        assert_eq!(
            store
                .commit_login(&current, "client-2", "refresh-2", None)
                .unwrap_err(),
            OAuthError::SecretStoreUnavailable
        );
        assert_eq!(
            store
                .read_state()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .client_id,
            "client-1"
        );
    }

    #[test]
    fn failed_state_write_cleans_the_candidate_slot() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        let current = store.read_state().unwrap();
        let result = FAIL_STATE_WRITE.sync_scope((), || {
            store.commit_login(&current, "client-2", "refresh-2", None)
        });
        assert!(matches!(result, Err(OAuthError::StateUnavailable)));
        assert_eq!(
            backend.slots.lock().unwrap().len(),
            1,
            "candidate slot leaked"
        );
        assert_eq!(
            store
                .read_state()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .client_id,
            "client-1"
        );
    }

    #[test]
    fn logout_invalidates_before_cleaning_the_slot() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        backend.log.lock().unwrap().clear();
        let removed = store.commit_logout().unwrap().unwrap();
        assert_eq!(removed.generation, 1);
        assert!(store.read_state().unwrap().active.is_none());
        assert!(backend.get(&removed.slot).unwrap().is_none());
        assert_eq!(
            backend.log.lock().unwrap().as_slice(),
            [format!("delete:{}", removed.slot)]
        );
        assert!(
            store.commit_logout().unwrap().is_none(),
            "logout must be idempotent"
        );
    }

    #[test]
    fn late_deactivation_cannot_touch_a_newer_generation() {
        let (_dir, backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        let current = store.read_state().unwrap();
        store
            .commit_login(&current, "client-2", "refresh-2", None)
            .unwrap();
        assert!(!store.deactivate_generation(1).unwrap());
        assert_eq!(
            store
                .read_state()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .generation,
            2
        );
        let current_slot = store.read_state().unwrap().active.unwrap().slot;
        assert_eq!(
            backend.get(&current_slot).unwrap().as_deref(),
            Some("refresh-2")
        );
        assert!(store.deactivate_generation(2).unwrap());
        assert!(store.read_state().unwrap().active.is_none());
        assert!(backend.get(&current_slot).unwrap().is_none());
    }

    #[test]
    fn corrupt_state_is_reported_not_silently_reset() {
        let (dir, _backend, store) = store();
        fs::write(
            dir.path().join(super::super::contract::state_file_name()),
            b"{corrupt",
        )
        .unwrap();
        assert_eq!(
            store.read_state().unwrap_err(),
            OAuthError::StateUnavailable
        );
        assert_eq!(
            store.commit_logout().unwrap_err(),
            OAuthError::StateUnavailable
        );
        assert!(
            dir.path()
                .join(super::super::contract::state_file_name())
                .exists(),
            "must not delete unreadable state"
        );
    }

    #[test]
    fn concurrent_commits_serialize_on_the_lock() {
        let (dir, backend, _store) = store();
        let starting = OAuthStore::new(dir.path(), backend.clone())
            .read_state()
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|index| {
                    let backend = backend.clone();
                    let barrier = barrier.clone();
                    let starting = starting.clone();
                    let path = dir.path().to_path_buf();
                    scope.spawn(move || {
                        let store = OAuthStore::new(&path, backend);
                        barrier.wait();
                        store.commit_login(&starting, &format!("client-{index}"), "refresh", None)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let successes = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(successes, 1, "exactly one commit may win: {results:?}");
        assert!(results
            .iter()
            .any(|result| matches!(result, Err(OAuthError::GenerationChanged))));
        let state = OAuthStore::new(dir.path(), backend.clone())
            .read_state()
            .unwrap();
        assert_eq!(state.counter, 1);
        assert_eq!(backend.slots.lock().unwrap().len(), 1);
    }

    #[test]
    fn state_file_is_dedicated_and_keyring_namespace_is_independent() {
        #[cfg(not(feature = "oauth-pre"))]
        {
            assert_eq!(
                super::super::contract::state_file_name(),
                "oauth-state.json"
            );
            assert_eq!(super::super::contract::keyring_service(), "cosh-ng.oauth");
        }
        #[cfg(feature = "oauth-pre")]
        {
            assert_eq!(
                super::super::contract::state_file_name(),
                "oauth-state.pre.json"
            );
            assert_eq!(
                super::super::contract::keyring_service(),
                "cosh-ng.oauth.pre"
            );
        }
        assert_ne!(
            super::super::contract::keyring_service(),
            "cosh-ng.extensions"
        );
        let (_dir, _backend, store) = store();
        let starting = store.read_state().unwrap();
        store
            .commit_login(&starting, "client-1", "refresh-1", None)
            .unwrap();
        assert!(store
            .dir
            .join(super::super::contract::state_file_name())
            .exists());
        assert!(!store.dir.join("config.toml").exists());
    }

    /// Real-machine macOS keychain handoff canary; run explicitly with --ignored.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_keychain_roundtrip_canary() {
        let backend = KeyringOAuthBackend;
        let slot = format!("canary-{}", uuid::Uuid::new_v4());
        backend.set(&slot, "canary-refresh-token").unwrap();
        assert_eq!(
            backend.get(&slot).unwrap().as_deref(),
            Some("canary-refresh-token")
        );
        backend.delete(&slot).unwrap();
        assert!(backend.get(&slot).unwrap().is_none());
        backend.delete(&slot).unwrap();
    }
}
