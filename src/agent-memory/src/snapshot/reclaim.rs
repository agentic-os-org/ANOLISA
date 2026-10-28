//! Reclaim orphaned snapshot artefacts under `<mount>/.anolisa/snapshots/`.
//!
//! Three artefact shapes can accumulate without any code path reclaiming
//! them (issue #9193):
//!
//! 1. `*.partial` — written by `create_tarball` before the atomic rename.
//!    A snapshot aborted mid-stream leaves one behind, and because each
//!    attempt uses a fresh `snap_<ULID>`, no later attempt ever touches it.
//! 2. sidecar-less `*.tar.gz` — the rename landed but the sidecar write
//!    failed or the process died first. `list_tarballs` enumerates
//!    `*.json` only, so the archive is invisible to every user-facing
//!    operation while still occupying quota.
//! 3. orphan `*.json` — a sidecar whose archive is gone, or a truncated
//!    sidecar left by a failed write. `read_sidecar` fails and the entry
//!    is skipped, but the file stays.
//!
//! The sweeper is age-guarded: an artefact younger than
//! `min_artifact_age_secs` is never touched, so an in-flight snapshot in
//! another process (fresh `*.partial`, archive awaiting its sidecar) is
//! always older than the guard when reclaimed.

use std::time::Duration;

use super::{SnapshotInfo, snapshots_dir};
use crate::ns::MountPoint;

/// Default age guard: 24 h. Any snapshot write — even of a multi-gigabyte
/// mount on slow storage — finishes far inside this window, so a younger
/// artefact may belong to a live `create_tarball` call in this or another
/// process.
pub const DEFAULT_MIN_ARTIFACT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What the sweeper removed and what it left behind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReclaimReport {
    /// `*.partial` files removed.
    pub partials_removed: usize,
    /// `*.tar.gz` archives removed for having no sidecar.
    pub archives_removed: usize,
    /// `*.json` sidecars removed for having no archive (or being unparseable
    /// with no archive present).
    pub sidecars_removed: usize,
    /// Artefacts seen but kept because they are younger than the age guard.
    pub kept_young: usize,
}

impl ReclaimReport {
    pub fn total_removed(&self) -> usize {
        self.partials_removed + self.archives_removed + self.sidecars_removed
    }
}

fn older_than(path: &std::path::Path, min_age: Duration, now: std::time::SystemTime) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(modified) => match now.duration_since(modified) {
            Ok(age) => age >= min_age,
            // Future mtime: treat as young rather than ancient.
            Err(_) => false,
        },
        // Unreadable mtime: leave the file alone.
        Err(_) => false,
    }
}

/// One bounded sweep over the snapshots directory. Best-effort: individual
/// removal failures are logged and counted as kept, never propagated —
/// reclaim must not turn a mount open into a hard failure.
pub fn reclaim_artifacts(mount: &MountPoint, min_age: Duration) -> ReclaimReport {
    let mut report = ReclaimReport::default();
    let dir = snapshots_dir(mount);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No snapshots directory (or unreadable): nothing to reclaim.
        Err(_) => return report,
    };
    let now = std::time::SystemTime::now();

    // Collect first so the archive/sidecar cross-checks below see a stable
    // snapshot of the directory rather than interleaving with our own
    // removals.
    let mut partials = Vec::new();
    let mut archives = Vec::new();
    let mut sidecars = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|s| s.to_str()) {
            Some("partial") => partials.push(path),
            Some("gz") => {
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.ends_with(".tar.gz") && !n.ends_with(".tar.gz.partial"))
                {
                    archives.push(path);
                }
            }
            Some("json") => sidecars.push(path),
            _ => {}
        }
    }

    // 1. `*.partial` — never referenced by list/restore; reclaim purely on age.
    for path in partials {
        if older_than(&path, min_age, now) {
            match std::fs::remove_file(&path) {
                Ok(()) => report.partials_removed += 1,
                Err(e) => {
                    tracing::warn!("snapshot reclaim: could not remove {}: {e}", path.display());
                    report.kept_young += 1;
                }
            }
        } else {
            report.kept_young += 1;
        }
    }

    // 2. Sidecar-less archives and 3. orphan sidecars. The rename-then-sidecar
    //    order in `create_tarball` means an archive can legitimately lack its
    //    sidecar only inside that write window; past the age guard no live
    //    writer can still be in it. A sidecar without an archive has no
    //    restore path at all (`restore_tarball` checks the archive first), so
    //    once guarded by age it is dead weight.
    let sidecar_for = |archive: &std::path::Path| -> std::path::PathBuf {
        let name = archive.file_name().unwrap_or_default().to_string_lossy();
        let stem = name.strip_suffix(".tar.gz").unwrap_or(&name);
        dir.join(format!("{stem}.json"))
    };
    let archive_for = |sidecar: &std::path::Path| -> std::path::PathBuf {
        let name = sidecar.file_name().unwrap_or_default().to_string_lossy();
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        dir.join(format!("{stem}.tar.gz"))
    };

    for archive in &archives {
        let sidecar = sidecar_for(archive);
        if sidecar.exists() {
            continue;
        }
        if older_than(archive, min_age, now) {
            match std::fs::remove_file(archive) {
                Ok(()) => report.archives_removed += 1,
                Err(e) => {
                    tracing::warn!(
                        "snapshot reclaim: could not remove {}: {e}",
                        archive.display()
                    );
                    report.kept_young += 1;
                }
            }
        } else {
            report.kept_young += 1;
        }
    }

    for sidecar in &sidecars {
        if archive_for(sidecar).exists() {
            continue;
        }
        // A parseable sidecar whose archive is gone and an unparseable
        // truncated one are both unrecoverable; age-guard applies equally.
        if older_than(sidecar, min_age, now) {
            match std::fs::remove_file(sidecar) {
                Ok(()) => report.sidecars_removed += 1,
                Err(e) => {
                    tracing::warn!(
                        "snapshot reclaim: could not remove {}: {e}",
                        sidecar.display()
                    );
                    report.kept_young += 1;
                }
            }
        } else {
            report.kept_young += 1;
        }
    }

    if report.total_removed() > 0 {
        tracing::info!(
            "snapshot reclaim: removed {} partial, {} orphaned archive, {} orphaned sidecar \
             file(s) under {} (kept {} below the age guard)",
            report.partials_removed,
            report.archives_removed,
            report.sidecars_removed,
            dir.display(),
            report.kept_young
        );
    }
    report
}

/// Build a `SnapshotInfo` for tests that need a parseable sidecar body.
/// Hidden from the public API docs; integration tests use it to construct
/// valid archive/sidecar pairs.
#[doc(hidden)]
pub fn test_sidecar_info(id: &str) -> SnapshotInfo {
    SnapshotInfo {
        id: id.to_string(),
        name: id.to_string(),
        created_at: "2026-01-01T00:00:00+00:00".into(),
        size: 1,
        backend: "tar.gz".into(),
    }
}
