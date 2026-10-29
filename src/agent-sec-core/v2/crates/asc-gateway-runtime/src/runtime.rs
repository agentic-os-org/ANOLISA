//! Files shared with the data plane under the runtime directory.
//!
//! The daemon writes plans, the secrets snapshot and `active.json`; the
//! gateway writes `status/health.json` and `status/ca-cert.pem`, which are
//! therefore read here as untrusted, size-limited input.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use asc_credential_store::{AgentName, CredentialId, CredentialStore, SecretsTarget};
use serde::{Deserialize, Serialize};

use crate::GatewayError;
use crate::files::{self, Owner, ensure_directory, read_limited};

pub(crate) const MANIFEST_NAME: &str = "active.json";
pub(crate) const STATUS_DIR: &str = "status";
pub(crate) const CA_BUNDLE_NAME: &str = "ca-bundle.pem";
const HEALTH_NAME: &str = "health.json";
const CA_NAME: &str = "ca-cert.pem";
const MANIFEST_VERSION: u32 = 1;
const SHARED_FILE_MODE: u32 = 0o640;
const MAX_HEALTH_BYTES: u64 = 64 * 1024;
const MAX_CA_BYTES: u64 = 64 * 1024;
const MAX_SYSTEM_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(100);
const MAX_REPORTED_ERROR: usize = 256;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    version: u32,
    generation: u64,
    secrets: String,
    listeners: Vec<Listener<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Listener<'a> {
    port: u16,
    agent: &'a AgentName,
    plan: String,
}

/// What the gateway reports in `status/health.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Health {
    #[serde(default)]
    pub(crate) loaded_generation: Option<u64>,
    #[serde(default)]
    pub(crate) last_attempt: Option<Attempt>,
    #[serde(default)]
    pub(crate) ca_error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Attempt {
    #[serde(default)]
    pub(crate) generation: Option<u64>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// One listener of a generation: the Agent domain, its port and its plan JSON.
pub(crate) struct PublishedAgent<'a> {
    pub(crate) agent: &'a AgentName,
    pub(crate) port: u16,
    pub(crate) plan: Vec<u8>,
}

/// The runtime directory and the account allowed to read it.
pub(crate) struct RuntimeDirectory {
    pub(crate) root: PathBuf,
    pub(crate) gateway: Owner,
}

impl RuntimeDirectory {
    fn status(&self) -> PathBuf {
        self.root.join(STATUS_DIR)
    }

    /// Creates the root (`root:gateway 0751`) and status (`gateway:gateway 0755`) directories.
    pub(crate) fn prepare(&self) -> Result<(), GatewayError> {
        let effective = Owner {
            uid: rustix::process::geteuid().as_raw(),
            gid: self.gateway.gid,
        };
        ensure_directory(&self.root, 0o751, Some(effective))?;
        ensure_directory(&self.status(), 0o755, Some(self.gateway))
    }

    /// Writes every file of `generation`, then points `active.json` at it.
    pub(crate) fn publish(
        &self,
        store: &CredentialStore,
        credentials: &[CredentialId],
        agents: &[PublishedAgent<'_>],
        generation: u64,
    ) -> Result<(), GatewayError> {
        let group = Owner {
            uid: rustix::process::geteuid().as_raw(),
            gid: self.gateway.gid,
        };
        let secrets = format!("secrets-{generation}.json");
        store.materialize(
            &SecretsTarget {
                directory: self.root.clone(),
                file_name: secrets.clone(),
                group: Some(self.gateway.gid),
            },
            credentials,
            generation,
        )?;
        let mut listeners = Vec::with_capacity(agents.len());
        for published in agents {
            let plan = format!("plan-{}-{generation}.json", published.agent);
            files::write_atomic(
                &self.root,
                &plan,
                &published.plan,
                SHARED_FILE_MODE,
                Some(group),
            )?;
            listeners.push(Listener {
                port: published.port,
                agent: published.agent,
                plan,
            });
        }
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            generation,
            secrets,
            listeners,
        };
        let bytes = serde_json::to_vec(&manifest)
            .map_err(|_| GatewayError::internal("cannot encode the gateway manifest"))?;
        files::write_atomic(
            &self.root,
            MANIFEST_NAME,
            &bytes,
            SHARED_FILE_MODE,
            Some(group),
        )
    }

    /// Reads the gateway's health report; absent or unreadable reports count as empty.
    pub(crate) fn health(&self) -> Health {
        read_limited(&self.status().join(HEALTH_NAME), MAX_HEALTH_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Waits until the gateway reports `generation` as loaded.
    pub(crate) fn await_generation(
        &self,
        generation: u64,
        timeout: Duration,
    ) -> Result<(), GatewayError> {
        let deadline = Instant::now() + timeout;
        loop {
            let health = self.health();
            if health.loaded_generation == Some(generation) {
                return Ok(());
            }
            if let Some(attempt) = health.last_attempt
                && attempt.generation == Some(generation)
                && let Some(error) = attempt.error
            {
                return Err(GatewayError::internal(format!(
                    "gateway rejected generation {generation}: {}",
                    truncate(&error)
                )));
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::unavailable(format!(
                    "gateway did not load generation {generation} within {}s",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Writes `ca-bundle.pem`: the system trust store plus the gateway CA.
    pub(crate) fn publish_ca_bundle(
        &self,
        system_bundles: &[PathBuf],
    ) -> Result<PathBuf, GatewayError> {
        let health = self.health();
        let certificate =
            files::read_owned(&self.status().join(CA_NAME), MAX_CA_BYTES, self.gateway.uid)?
                .filter(|bytes| bytes.starts_with(b"-----BEGIN CERTIFICATE-----"))
                .ok_or_else(|| {
                    GatewayError::unavailable(format!(
                        "gateway has not published its CA certificate{}",
                        health
                            .ca_error
                            .map(|error| format!(": {}", truncate(&error)))
                            .unwrap_or_default()
                    ))
                })?;
        let mut bundle = None;
        for candidate in system_bundles {
            if let Some(bytes) = files::read_system_file(candidate, MAX_SYSTEM_BUNDLE_BYTES)? {
                bundle = Some(bytes);
                break;
            }
        }
        let mut bundle = bundle.ok_or_else(|| {
            GatewayError::unavailable("no system CA bundle found to extend with the gateway CA")
        })?;
        if !bundle.ends_with(b"\n") {
            bundle.push(b'\n');
        }
        bundle.extend_from_slice(&certificate);
        let path = self.root.join(CA_BUNDLE_NAME);
        if read_limited(&path, MAX_SYSTEM_BUNDLE_BYTES + MAX_CA_BYTES + 1)?.as_deref()
            != Some(&bundle[..])
        {
            files::write_atomic(&self.root, CA_BUNDLE_NAME, &bundle, 0o644, None)?;
        }
        Ok(path)
    }

    /// Deletes plan and secrets files of every generation except `keep`.
    pub(crate) fn remove_stale(&self, keep: u64) -> Result<(), GatewayError> {
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| GatewayError::io("cannot list", &self.root, &error))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if generation_of(name).is_some_and(|generation| generation != keep) {
                files::remove_file(&entry.path())?;
            }
        }
        Ok(())
    }

    /// Retains public trust material so existing Agent CA settings remain usable after disable.
    pub(crate) fn remove_except_ca(&self) -> Result<(), GatewayError> {
        match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(GatewayError::internal("runtime root is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(GatewayError::io("cannot inspect", &self.root, &error)),
        }
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(GatewayError::io("cannot list", &self.root, &error)),
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| GatewayError::io("cannot list", &self.root, &error))?;
            if entry.file_name() == CA_BUNDLE_NAME {
                continue;
            }
            let kind = entry
                .file_type()
                .map_err(|error| GatewayError::io("cannot inspect", &entry.path(), &error))?;
            if kind.is_dir() {
                std::fs::remove_dir_all(entry.path())
                    .map_err(|error| GatewayError::io("cannot remove", &entry.path(), &error))?;
            } else {
                files::remove_file(&entry.path())?;
            }
        }
        if !self.ca_bundle_path().exists() {
            std::fs::remove_dir(&self.root)
                .map_err(|error| GatewayError::io("cannot remove", &self.root, &error))?;
        }
        Ok(())
    }

    /// Removes the whole runtime directory, secrets snapshot included.
    pub(crate) fn remove_all(&self) -> Result<(), GatewayError> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(GatewayError::io("cannot remove", &self.root, &error)),
        }
    }

    pub(crate) fn ca_bundle_path(&self) -> PathBuf {
        self.root.join(CA_BUNDLE_NAME)
    }
}

/// Generation number of a `plan-<agent>-<n>.json` or `secrets-<n>.json` file.
fn generation_of(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".json")?;
    if !(stem.starts_with("plan-") || stem.starts_with("secrets-")) {
        return None;
    }
    stem.rsplit_once('-')?.1.parse().ok()
}

fn truncate(text: &str) -> &str {
    let mut end = text.len().min(MAX_REPORTED_ERROR);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
