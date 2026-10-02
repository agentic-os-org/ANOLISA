//! Phase 2: pluggable mount strategies (Linux-only crate).
//!
//! Two strategies ship in this build:
//! - `UserlandMount` (default for tests / unprivileged runs): place data
//!   under `<base>/<ns>/` in the user's home — no syscall side effects.
//! - `LinuxUserNsMount`: enter a fresh `(user, mount)` namespace pair,
//!   overlay tmpfs on `/mnt`, bind-mount `<base>/<ns>/` at
//!   `/mnt/memory/<ns>/`. Callers see the standardized path; data still
//!   persists in the home directory.
//!
//! `pick_strategy()` decides at startup which one to use based on
//! `MemoryConfig::mount.strategy` (`auto` | `userland` | `userns`).

pub mod linux_userns;
pub mod userland;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, Result};
use crate::ns::Namespace;

/// Where to place the namespace mount root, and how strict to be about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountStrategyKind {
    /// Prefer user namespace; fall back to userland on failure.
    #[default]
    Auto,
    /// Always use the in-home directory layout.
    Userland,
    /// Force user-namespace mount; bail out if the kernel won't allow it.
    Userns,
}

impl MountStrategyKind {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "auto" | "default" => Self::Auto,
            "userland" | "home" => Self::Userland,
            "userns" | "user-ns" | "namespace" => Self::Userns,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Userland => "userland",
            Self::Userns => "userns",
        }
    }
}

/// What `pick_strategy` returns: a strategy plus a tag for diagnostics.
pub struct PickedStrategy {
    pub strategy: Box<dyn MountStrategy>,
    /// Whether the strategy actually entered a user namespace (for `info`).
    pub entered_userns: bool,
}

/// Side-effecting strategy: prepares (and possibly mounts) a directory tree
/// for `ns` under `base`. Returns the absolute path that subsequent code
/// should treat as the mount root.
pub trait MountStrategy: Send + Sync {
    fn ensure(&self, ns: &Namespace, base: &Path) -> Result<PathBuf>;
    fn name(&self) -> &'static str;
}

/// Resolve the configured strategy. May enter a user namespace as a side
/// effect — call once at process startup, before any other privileged work.
pub fn pick_strategy(kind: MountStrategyKind) -> Result<PickedStrategy> {
    match kind {
        MountStrategyKind::Userland => Ok(PickedStrategy {
            strategy: Box::new(userland::UserlandMount),
            entered_userns: false,
        }),

        MountStrategyKind::Userns => match linux_userns::LinuxUserNsMount::enter() {
            Ok(s) => Ok(PickedStrategy {
                strategy: Box::new(s),
                entered_userns: true,
            }),
            Err(e) => Err(MemoryError::Other(format!(
                "userns strategy requested but failed to enter namespace: {e}"
            ))),
        },

        MountStrategyKind::Auto => match linux_userns::LinuxUserNsMount::enter() {
            Ok(s) => Ok(PickedStrategy {
                strategy: Box::new(s),
                entered_userns: true,
            }),
            // Once the process is half-inside a fresh user namespace,
            // userland fallback is unsafe — every home-dir syscall would
            // run as `nobody`. Propagate so the binary fails hard.
            Err(e @ MemoryError::UserNsUnrecoverable(_)) => Err(e),
            Err(e) => {
                tracing::warn!("userns mount failed ({e}); falling back to userland");
                Ok(PickedStrategy {
                    strategy: Box::new(userland::UserlandMount),
                    entered_userns: false,
                })
            }
        },
    }
}

/// Common helper: write the starter README + manifest into a freshly
/// created mount root. Used by both strategies after the path is decided.
pub(crate) fn populate_mount_dir(root: &Path, ns: &Namespace) -> Result<()> {
    let meta_dir = root.join(crate::ns::RESERVED_FIRST_SEGMENTS[0]);
    std::fs::create_dir_all(&meta_dir)?;

    let readme = root.join("README.md");
    if !readme.exists() {
        std::fs::write(&readme, crate::ns::README_TEXT)?;
    }

    let manifest = meta_dir.join("manifest.toml");
    if !manifest.exists() {
        let body = format!(
            "schema_version = \"v2.0\"\ncreated_at = \"{}\"\nns_kind = \"{}\"\nns_id = \"{}\"\n",
            chrono::Utc::now().to_rfc3339(),
            ns.kind.as_str(),
            ns.id,
        );
        std::fs::write(&manifest, body)?;
    }
    Ok(())
}
