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

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
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

/// Re-apply owner-only permissions through a held descriptor and verify
/// the result instead of trusting `Ok(())` — some FUSE and network mounts
/// accept a chmod without applying it, and the guarantees below would then
/// be silently false at runtime. `mode` is the exact target (0700 for
/// directories, where the owner-execute bit is what keeps them enterable;
/// 0600 for files). Shared by every metadata-path hardening call below
/// (same shape as the audit-log check; that one lives in `audit/mod.rs`
/// because it predates these).
pub(crate) fn tighten_owner_only(f: &File, path: &Path, mode: u32) -> Result<()> {
    f.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|e| {
            MemoryError::Other(format!(
                "cannot tighten {} to owner-only: {e}",
                path.display()
            ))
        })?;
    let actual = f
        .metadata()
        .map_err(|e| MemoryError::Other(format!("cannot stat {}: {e}", path.display())))?
        .permissions()
        .mode();
    if actual & 0o777 != mode {
        return Err(MemoryError::Other(format!(
            "{} is still mode {:o} after chmod; refusing it; move memory.paths.base_dir \
             onto a local filesystem",
            path.display(),
            actual & 0o777
        )));
    }
    Ok(())
}

/// Ensure a directory in the `.anolisa` metadata tree exists and stays
/// owner-only (0700).
///
/// The directory level is the *primary* co-tenant guarantee for everything
/// under `.anolisa`: sqlite creates the index's `-wal`/`-shm` siblings at
/// its own default mode and cannot be told otherwise, so an owner-only
/// directory is the only fence that covers them (along with snapshot
/// sidecars and anything added later). Files we create ourselves are
/// additionally kept 0600 by the callers — defense in depth for a
/// directory an operator later loosens.
///
/// Failing closed mirrors the audit-log precedent: a metadata tree that
/// cannot hold 0700 must not run, and the error names the remedy.
pub(crate) fn ensure_owner_only_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| {
            MemoryError::Other(format!("cannot create metadata dir {}: {e}", dir.display()))
        })?;
    // Open with O_NOFOLLOW so a symlinked directory is refused rather than
    // followed, and tighten through the descriptor so the verified mode is
    // the directory we will actually place files in.
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC)
        .open(dir)
        .map_err(|e| {
            MemoryError::Other(format!("cannot open metadata dir {}: {e}", dir.display()))
        })?;
    tighten_owner_only(&f, dir, 0o700)
}

/// Ensure a metadata file exists and stays owner-only (0600) — create it if
/// absent, otherwise repair permissions left by an older build. Opens
/// without truncation, so it is safe to run ahead of a real writer.
pub(crate) fn ensure_owner_only_file(path: &Path) -> Result<()> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            MemoryError::Other(format!("cannot open metadata file {}: {e}", path.display()))
        })?;
    tighten_owner_only(&f, path, 0o600)
}

/// Common helper: write the starter README + manifest into a freshly
/// created mount root. Used by both strategies after the path is decided.
pub(crate) fn populate_mount_dir(root: &Path, ns: &Namespace) -> Result<()> {
    let meta_dir = root.join(crate::ns::RESERVED_FIRST_SEGMENTS[0]);
    ensure_owner_only_dir(&meta_dir)?;

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
        // create_new keeps the existence check atomic and refuses a planted
        // symlink (O_NOFOLLOW); a pre-existing manifest from an older build
        // keeps its mode but is already fenced off by the 0700 dir above.
        let mut f = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&manifest)
        {
            Ok(f) => f,
            // A concurrent first start of the same fresh namespace won the
            // create race: the manifest it is writing is the one this
            // instance expects, so losing the race is success — but only
            // after re-opening through O_NOFOLLOW. O_CREAT|O_EXCL also
            // returns EEXIST for a planted symlink regardless of whether
            // its target exists, so accepting the error blindly (or
            // falling back to fs::write) would re-open the symlink hole
            // create_new exists to close.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                    .open(&manifest)
                    .map(|_| ())
                    .map_err(|e2| {
                        MemoryError::Other(format!(
                            "cannot create {}: concurrent initializer won but re-open refused the occupant: {e2}",
                            manifest.display()
                        ))
                    });
            }
            Err(e) => {
                return Err(MemoryError::Other(format!(
                    "cannot create {}: {e}",
                    manifest.display()
                )));
            }
        };
        use std::io::Write;
        f.write_all(body.as_bytes())
            .map_err(|e| MemoryError::Other(format!("cannot write {}: {e}", manifest.display())))?;
    }
    Ok(())
}
