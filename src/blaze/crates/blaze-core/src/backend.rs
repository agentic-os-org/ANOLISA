// SPDX-License-Identifier: Apache-2.0
//! Sandbox backend kinds + selection / fallback.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{BlazeError, Result};
use crate::policy::{BackendConfigs, VmConfig};
use crate::storage::StorageSlot;

/// All backends that blaze v0.1 knows about. Each backend maps to a
/// binary path configured in the daemon `[backends]` section.
///
/// `LinuxSandbox` and `Landlock` are recognized for policy deserialization
/// but are not yet backed by a `BackendSpawner` implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Firecracker,
    Gvisor,
    GvisorSubstrate,
    Rund,
    KataFc,
    KataClh,
    KataQemu,
    Runc,
    Bubblewrap,
    LinuxSandbox,
    Landlock,
    Mock,
}

impl BackendKind {
    /// Stable string label used in policy files / metrics / config keys.
    pub const fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Firecracker => "firecracker",
            BackendKind::Gvisor => "gvisor",
            BackendKind::GvisorSubstrate => "gvisor-substrate",
            BackendKind::Rund => "rund",
            BackendKind::KataFc => "kata-fc",
            BackendKind::KataClh => "kata-clh",
            BackendKind::KataQemu => "kata-qemu",
            BackendKind::Runc => "runc",
            BackendKind::Bubblewrap => "bubblewrap",
            BackendKind::LinuxSandbox => "linux-sandbox",
            BackendKind::Landlock => "landlock",
            BackendKind::Mock => "mock",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackendKind {
    type Err = BlazeError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "firecracker" => Ok(BackendKind::Firecracker),
            "gvisor" => Ok(BackendKind::Gvisor),
            "gvisor-substrate" => Ok(BackendKind::GvisorSubstrate),
            "rund" => Ok(BackendKind::Rund),
            "kata-fc" => Ok(BackendKind::KataFc),
            "kata-clh" => Ok(BackendKind::KataClh),
            "kata-qemu" => Ok(BackendKind::KataQemu),
            "runc" => Ok(BackendKind::Runc),
            "bubblewrap" => Ok(BackendKind::Bubblewrap),
            "linux-sandbox" => Ok(BackendKind::LinuxSandbox),
            "landlock" => Ok(BackendKind::Landlock),
            "mock" => Ok(BackendKind::Mock),
            other => Err(BlazeError::PolicyEvalError {
                reason: format!("unknown backend kind: {other}"),
            }),
        }
    }
}

/// Portable parameters for starting a backend instance.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Stable sandbox identifier.
    pub instance_id: Uuid,
    /// Backend executable selected during daemon startup.
    pub binary_path: PathBuf,
    /// Storage resources owned by this sandbox.
    pub storage: StorageSlot,
    /// Backend-specific policy configuration.
    pub backend: BackendConfigs,
    /// Generic VM resource configuration.
    pub vm: Option<VmConfig>,
}

/// Backend identity and snapshot semantics accepted by a restore adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCapability {
    /// Concrete backend implementation that can consume the checkpoint.
    pub backend: BackendKind,
    /// Exact backend version required by versioned snapshot formats.
    pub version: Option<String>,
    /// Snapshot flavor accepted by the adapter.
    pub snapshot_kind: SnapshotKind,
}

/// Complete input for restoring an owned backend instance.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// Stable sandbox identifier.
    pub instance_id: Uuid,
    /// Backend executable selected from the current daemon configuration.
    pub binary_path: PathBuf,
    /// Storage resources reconstructed for this sandbox.
    pub storage: StorageSlot,
    /// Backend-owned payload subtree from a committed checkpoint. The
    /// layout inside is whatever the same backend wrote during capture.
    pub payload_dir: PathBuf,
    /// Backend identity frozen into the checkpoint metadata.
    pub checkpoint_backend: BackendKind,
    /// Backend version frozen into the checkpoint metadata.
    pub expected_version: Option<String>,
    /// Snapshot flavor frozen into the checkpoint metadata.
    pub snapshot_kind: SnapshotKind,
    /// Whether the captured runtime exposed the stable run-directory guest transport.
    pub expose_guest_socket: bool,
    /// Whether the captured runtime held a per-sandbox host network slot.
    ///
    /// The replacement must recreate the same shape, because a snapshot
    /// references its network device by host name and the previous owner's
    /// cleanup already removed that device.
    pub preserve_network: bool,
    /// Whether the captured runtime recorded guest console output.
    ///
    /// Carried so a restore does not silently stop recording console output for
    /// a sandbox whose operator asked for it.
    pub record_console_log: bool,
    /// Whether the snapshot was captured by a different sandbox.
    ///
    /// A checkpoint restore reloads this sandbox's own capture, so a recorded
    /// sandbox identity must still match. A template restore deliberately loads
    /// one published capture into many new sandboxes, so its recorded identity
    /// belongs to the source and cannot match. Adapters that bind a snapshot to
    /// a sandbox identity use this to tell the two apart instead of dropping the
    /// check for both.
    pub snapshot_from_other_sandbox: bool,
}

/// Snapshot flavor requested from a backend.
///
/// The file provider currently requires self-contained artifacts, so only
/// full snapshots are exposed until a restore-independent delta format exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotKind {
    /// Self-contained VM and memory snapshot.
    Full,
}

/// Paths and semantics for one snapshot operation.
#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    /// Payload subtree owned by the backend for this capture. The daemon
    /// guarantees it exists and is empty; the backend chooses the internal
    /// layout, so a VM backend can write two named files while a
    /// container-shaped backend can write a whole image directory.
    pub payload_dir: PathBuf,
    /// Snapshot flavor.
    pub kind: SnapshotKind,
}

/// Probed availability of a single backend on this host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendStatus {
    pub kind: BackendKind,
    pub available: bool,
    #[serde(default)]
    pub version: Option<String>,
}

/// Walk `priority` in order and return the first backend that is marked
/// available. Returns [`BlazeError::BackendUnavailable`] when no entry in
/// `priority` is available.
pub fn select_backend(
    priority: &[BackendKind],
    available: &[BackendStatus],
) -> Result<BackendKind> {
    for kind in priority {
        if available
            .iter()
            .any(|status| status.kind == *kind && status.available)
        {
            tracing::info!(backend = %kind, "selected backend");
            return Ok(*kind);
        }
        tracing::warn!(backend = %kind, "backend not available, falling back");
    }

    let requested = priority.iter().map(|b| b.as_str().to_string()).collect();
    let available = available
        .iter()
        .filter(|s| s.available)
        .map(|s| s.kind.as_str().to_string())
        .collect();
    Err(BlazeError::BackendUnavailable {
        requested,
        available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_str() {
        for kind in [
            BackendKind::Firecracker,
            BackendKind::Gvisor,
            BackendKind::GvisorSubstrate,
            BackendKind::Rund,
            BackendKind::KataFc,
            BackendKind::KataClh,
            BackendKind::KataQemu,
            BackendKind::Runc,
            BackendKind::Bubblewrap,
            BackendKind::LinuxSandbox,
            BackendKind::Landlock,
            BackendKind::Mock,
        ] {
            let s = kind.as_str();
            let parsed: BackendKind = s.parse().expect("round-trip");
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn select_picks_first_available() {
        let priority = vec![
            BackendKind::Firecracker,
            BackendKind::Gvisor,
            BackendKind::Bubblewrap,
        ];
        let available = vec![
            BackendStatus {
                kind: BackendKind::Firecracker,
                available: false,
                version: None,
            },
            BackendStatus {
                kind: BackendKind::Gvisor,
                available: true,
                version: Some("20260601".into()),
            },
            BackendStatus {
                kind: BackendKind::Bubblewrap,
                available: true,
                version: None,
            },
        ];
        let chosen = select_backend(&priority, &available).expect("selects");
        assert_eq!(chosen, BackendKind::Gvisor);
    }

    #[test]
    fn select_errors_when_none_available() {
        let priority = vec![BackendKind::Firecracker];
        let available = vec![BackendStatus {
            kind: BackendKind::Firecracker,
            available: false,
            version: None,
        }];
        let err = select_backend(&priority, &available).expect_err("must fail");
        assert!(matches!(err, BlazeError::BackendUnavailable { .. }));
    }

    #[test]
    fn snapshot_kind_serializes_as_a_stable_lowercase_value() {
        assert_eq!(
            serde_json::to_value(SnapshotKind::Full).expect("snapshot kind"),
            serde_json::json!("full")
        );
    }
}
