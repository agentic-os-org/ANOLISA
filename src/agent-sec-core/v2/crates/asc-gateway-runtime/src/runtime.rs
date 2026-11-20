//! Files shared with the data plane under the runtime directory.
//!
//! The daemon writes plans, the secrets snapshot, `active.json` and the public
//! trust bundles; the gateway writes `status/health.json`, `status/ca-cert.pem`
//! and `status/ca-bundle.p12`, which are therefore read here as untrusted,
//! size-limited input.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use asc_credential_store::{CredentialId, CredentialStore, ScopeId, SecretsTarget};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::GatewayError;
use crate::files::{self, Owner, ensure_directory};

pub(crate) const MANIFEST_NAME: &str = "active.json";
pub(crate) const STATUS_DIR: &str = "status";
pub(crate) const CA_BUNDLE_NAME: &str = "ca-bundle.pem";
pub(crate) const CA_PKCS12_NAME: &str = "ca-bundle.p12";
const HEALTH_NAME: &str = "health.json";
const CA_NAME: &str = "ca-cert.pem";
const MANIFEST_VERSION: u32 = 1;
const SHARED_FILE_MODE: u32 = 0o640;
const MAX_HEALTH_BYTES: u64 = 64 * 1024;
const MAX_CA_BYTES: u64 = 64 * 1024;
const MAX_SYSTEM_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = MAX_SYSTEM_BUNDLE_BYTES + MAX_CA_BYTES + 1;
// Certificate bags, aliases, encryption and MAC need independent overhead headroom.
const MAX_PKCS12_BYTES: u64 = 16 * 1024 * 1024;
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
    scope_id: &'a ScopeId,
    plan: String,
}

/// What the gateway reports in `status/health.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Health {
    #[serde(default)]
    pub(crate) pid: Option<u32>,
    #[serde(default)]
    pub(crate) loaded_generation: Option<u64>,
    #[serde(default)]
    pub(crate) last_attempt: Option<Attempt>,
    #[serde(default)]
    pub(crate) ca_error: Option<String>,
    #[serde(default)]
    pub(crate) ca_pkcs12: Option<Pkcs12Report>,
}

/// The gateway's latest conversion of `ca-bundle.pem` into `status/ca-bundle.p12`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pkcs12Report {
    /// SHA-256 of the PEM bundle the conversion read.
    #[serde(default)]
    pub(crate) pem_sha256: Option<String>,
    /// SHA-256 of the produced keystore; absent when the conversion failed.
    #[serde(default)]
    pub(crate) pkcs12_sha256: Option<String>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// Absolute paths of the shared public trust bundles, one per format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaBundlePaths {
    /// System trust roots plus the gateway CA as PEM.
    pub pem: PathBuf,
    /// The same certificates as a PKCS#12 truststore under the public password `changeit`.
    pub pkcs12: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Attempt {
    #[serde(default)]
    pub(crate) generation: Option<u64>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// One listener of a generation: the Scope, its port and its plan JSON.
pub(crate) struct PublishedScope<'a> {
    pub(crate) scope_id: &'a ScopeId,
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
        scope_ids: &[PublishedScope<'_>],
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
        let mut listeners = Vec::with_capacity(scope_ids.len());
        for published in scope_ids {
            let plan = format!("plan-{}-{generation}.json", published.scope_id);
            files::write_atomic(
                &self.root,
                &plan,
                &published.plan,
                SHARED_FILE_MODE,
                Some(group),
            )?;
            listeners.push(Listener {
                port: published.port,
                scope_id: published.scope_id,
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

    /// Reads only health from the current systemd main process; unsafe reports count as empty.
    pub(crate) fn health(&self, pid: u32) -> Health {
        files::read_owned(
            &self.status().join(HEALTH_NAME),
            MAX_HEALTH_BYTES,
            self.gateway.uid,
        )
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Health>(&bytes).ok())
        .filter(|health| pid != 0 && health.pid == Some(pid))
        .unwrap_or_default()
    }

    /// Waits for plan loading only; PEM publication must precede the PKCS#12 wait.
    pub(crate) fn await_generation(
        &self,
        generation: u64,
        pid: u32,
        timeout: Duration,
    ) -> Result<(), GatewayError> {
        let deadline = Instant::now() + timeout;
        loop {
            let health = self.health(pid);
            check_attempt(&health, generation)?;
            if health.loaded_generation == Some(generation) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::unavailable(format!(
                    "gateway did not load generation {generation} within {}s",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn ca_certificate(&self) -> Result<Vec<u8>, GatewayError> {
        files::read_owned(&self.status().join(CA_NAME), MAX_CA_BYTES, self.gateway.uid)?
            .filter(|bytes| {
                bytes.starts_with(b"-----BEGIN CERTIFICATE-----")
                    && bytes
                        .trim_ascii_end()
                        .ends_with(b"-----END CERTIFICATE-----")
                    && reject_private_key(bytes).is_ok()
            })
            .ok_or_else(|| GatewayError::unavailable("gateway CA certificate is absent or invalid"))
    }

    fn published_pem(&self) -> Result<Vec<u8>, GatewayError> {
        let certificate = self.ca_certificate()?;
        files::read_owned(
            &self.ca_bundle_path(),
            MAX_BUNDLE_BYTES,
            rustix::process::geteuid().as_raw(),
        )?
        .filter(|bytes| {
            // System stores may start with comments; the appended CA must match this proxy.
            bytes.ends_with(&certificate)
        })
        .ok_or_else(|| {
            GatewayError::unavailable("published gateway PEM bundle is absent or invalid")
        })
    }

    /// Checks both root-published files against current health without repairing either file.
    pub(crate) fn published_ca_bundle(
        &self,
        generation: u64,
        pid: u32,
    ) -> Result<CaBundlePaths, GatewayError> {
        let health = self.health(pid);
        let pem = self.published_pem()?;
        reject_private_key(&pem)?;
        let report = current_report(&health, generation, &pem)?
            .ok_or_else(|| GatewayError::unavailable("gateway trust bundles are not current"))?;
        let pkcs12 = self.root.join(CA_PKCS12_NAME);
        files::read_owned(
            &pkcs12,
            MAX_PKCS12_BYTES,
            rustix::process::geteuid().as_raw(),
        )?
        .filter(|bytes| {
            !bytes.is_empty() && report.pkcs12_sha256.as_deref() == Some(&sha256(bytes))
        })
        .ok_or_else(|| {
            GatewayError::unavailable("published gateway PKCS#12 bundle is absent or invalid")
        })?;
        Ok(CaBundlePaths {
            pem: self.ca_bundle_path(),
            pkcs12,
        })
    }

    /// Waits for conversion of the published PEM, then publishes the matching public PKCS#12.
    pub(crate) fn await_pkcs12(
        &self,
        generation: u64,
        pid: u32,
        timeout: Duration,
    ) -> Result<(), GatewayError> {
        let deadline = Instant::now() + timeout;
        loop {
            let health = self.health(pid);
            let pem = self.published_pem()?;
            reject_private_key(&pem)?;
            if let Some(report) = current_report(&health, generation, &pem)?
                && let Some(bytes) = files::read_owned(
                    &self.status().join(CA_PKCS12_NAME),
                    MAX_PKCS12_BYTES,
                    self.gateway.uid,
                )?
                && !bytes.is_empty()
                && report.pkcs12_sha256.as_deref() == Some(&sha256(&bytes))
            {
                let path = self.root.join(CA_PKCS12_NAME);
                if files::read_owned(&path, MAX_PKCS12_BYTES, rustix::process::geteuid().as_raw())?
                    .as_deref()
                    != Some(&bytes)
                {
                    files::write_atomic(&self.root, CA_PKCS12_NAME, &bytes, 0o644, None)?;
                }
                self.published_ca_bundle(generation, pid)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::unavailable(
                    "gateway PKCS#12 conversion did not become current before the deadline",
                ));
            }
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// Writes `ca-bundle.pem`: the system trust store plus the gateway CA.
    pub(crate) fn publish_ca_bundle(
        &self,
        system_bundles: &[PathBuf],
        timeout: Duration,
    ) -> Result<PathBuf, GatewayError> {
        let deadline = Instant::now() + timeout;
        // Plan acknowledgement may precede public CA publication during addon startup.
        let certificate = loop {
            match self.ca_certificate() {
                Err(error)
                    if error.kind() == crate::GatewayErrorKind::Unavailable
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(
                        POLL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                result => break result?,
            }
        };
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
        reject_private_key(&bundle)?;
        let path = self.root.join(CA_BUNDLE_NAME);
        if files::read_owned(&path, MAX_BUNDLE_BYTES, rustix::process::geteuid().as_raw())?
            .as_deref()
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
            if entry.file_name() == CA_BUNDLE_NAME || entry.file_name() == CA_PKCS12_NAME {
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
        if !self.ca_bundle_path().exists() && !self.root.join(CA_PKCS12_NAME).exists() {
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

/// Generation number of a `plan-<scope_id>-<n>.json` or `secrets-<n>.json` file.
fn generation_of(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".json")?;
    if !(stem.starts_with("plan-") || stem.starts_with("secrets-")) {
        return None;
    }
    stem.rsplit_once('-')?.1.parse().ok()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn reject_private_key(bytes: &[u8]) -> Result<(), GatewayError> {
    const MARKERS: [&[u8]; 6] = [
        b"-----BEGIN PRIVATE KEY-----",
        b"-----BEGIN ENCRYPTED PRIVATE KEY-----",
        b"-----BEGIN RSA PRIVATE KEY-----",
        b"-----BEGIN EC PRIVATE KEY-----",
        b"-----BEGIN DSA PRIVATE KEY-----",
        b"-----BEGIN OPENSSH PRIVATE KEY-----",
    ];
    if MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|part| part == *marker))
    {
        return Err(GatewayError::unavailable(
            "published gateway trust bundle contains private key material",
        ));
    }
    Ok(())
}

fn check_attempt(health: &Health, generation: u64) -> Result<(), GatewayError> {
    if let Some(attempt) = &health.last_attempt
        && attempt.generation == Some(generation)
        && let Some(error) = &attempt.error
    {
        return Err(GatewayError::internal(format!(
            "gateway rejected generation {generation}: {}",
            truncate(error)
        )));
    }
    Ok(())
}

fn current_report<'a>(
    health: &'a Health,
    generation: u64,
    pem: &[u8],
) -> Result<Option<&'a Pkcs12Report>, GatewayError> {
    check_attempt(health, generation)?;
    if health.loaded_generation != Some(generation) {
        return Ok(None);
    }
    if health.ca_error.is_some() {
        return Err(GatewayError::unavailable(
            "gateway could not publish its CA certificate",
        ));
    }
    let Some(report) = &health.ca_pkcs12 else {
        return Ok(None);
    };
    // A cold start may report the not-yet-published PEM; wait for the new input first.
    if report.pem_sha256.as_deref() != Some(&sha256(pem)) {
        return Ok(None);
    }
    // Failure invalidates old success digests, including a failed conversion of the same PEM.
    if report.error.is_some() {
        return Err(GatewayError::unavailable(
            "gateway PKCS#12 conversion failed",
        ));
    }
    Ok(Some(report))
}

fn truncate(text: &str) -> &str {
    let mut end = text.len().min(MAX_REPORTED_ERROR);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
