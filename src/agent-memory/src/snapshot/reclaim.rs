//! Reclaim orphaned snapshot artefacts under `<mount>/.anolisa/snapshots/`.
//!
//! Artefact shapes that can accumulate without any code path reclaiming
//! them (issue #9193):
//!
//! 1. `*.partial` — written by `create_tarball` before the atomic rename.
//!    A snapshot aborted mid-stream leaves one behind, and because each
//!    attempt uses a fresh `snap_<ULID>`, no later attempt ever touches it.
//! 2. sidecar-less `*.tar.gz` — the rename landed but the sidecar write
//!    failed or the process died first. `list_tarballs` enumerates
//!    `*.json` only, so the archive is invisible to every user-facing
//!    operation while still occupying disk.
//! 3. orphan `*.json` — a sidecar whose archive is gone. With no archive
//!    there is no restore path (`restore_tarball` checks the archive
//!    first), so the sidecar is dead weight.
//! 4. unparseable sidecar with an intact archive — a `write_sidecar`
//!    interrupted mid-`write_all` leaves truncated JSON next to a fully
//!    valid archive. The archive is restorable, so this is *repaired*
//!    (sidecar regenerated from the archive's own metadata) rather than
//!    deleted; removing the sidecar would orphan a good backup and the
//!    next sweep would then destroy it under rule 2.
//! 5. `.<id>.staging` directories — `restore_tarball` extracts the full
//!    mount tree into a staging dir and removes it only on its own error
//!    path. A SIGKILL (or power loss) mid-restore leaves the complete
//!    extraction behind; `list_tarballs` never sees it, so it is the
//!    largest orphaned-artefact class byte-for-byte. A restore holds no
//!    reservation on the name (a re-run recreates staging from scratch),
//!    so past the age guard the directory can be removed wholesale.
//!
//! The sweeper is age-guarded: an artefact younger than
//! `min_artifact_age_secs` is never touched, so an in-flight snapshot in
//! another process (fresh `*.partial`, archive awaiting its sidecar) is
//! always older than the guard when reclaimed. The sweep runs once per
//! mount open, not continuously.

use std::os::fd::AsFd;
use std::time::Duration;

use super::snapshots_dir;
use crate::ns::MountPoint;

/// Default age guard: 24 h. Any snapshot write — even of a multi-gigabyte
/// mount on slow storage — finishes far inside this window, so a younger
/// artefact may belong to a live `create_tarball` call in this or another
/// process. `SnapshotReclaimConfig`'s default defers to this constant so
/// the documented default cannot drift from the enforced one.
pub const DEFAULT_MIN_ARTIFACT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What the sweeper did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReclaimReport {
    /// `*.partial` files removed.
    pub partials_removed: usize,
    /// `*.tar.gz` archives removed for having no sidecar.
    pub archives_removed: usize,
    /// `*.json` sidecars removed for having no archive.
    pub sidecars_removed: usize,
    /// Truncated sidecars whose archive was intact — repaired by
    /// regenerating the sidecar from the archive's metadata.
    pub sidecars_repaired: usize,
    /// `.<id>.staging` directories from crashed restores, removed whole.
    pub staging_dirs_removed: usize,
    /// Artefacts seen but kept because they are younger than the age guard.
    pub kept_young: usize,
    /// Removal/regeneration attempts that failed (permissions, ...). Kept
    /// separate from `kept_young`: a persistently failing removal is what
    /// an operator needs surfaced, an age-guard hit is normal.
    pub removal_failures: usize,
}

impl ReclaimReport {
    pub fn total_removed(&self) -> usize {
        self.partials_removed
            + self.archives_removed
            + self.sidecars_removed
            + self.staging_dirs_removed
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
/// removal failures are logged and counted, never propagated — reclaim
/// must not turn a mount open into a hard failure.
pub fn reclaim_artifacts(mount: &MountPoint, min_age: Duration) -> ReclaimReport {
    let mut report = ReclaimReport::default();
    let dir = snapshots_dir(mount);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No snapshots directory (or unreadable): nothing to reclaim.
        Err(_) => return report,
    };
    let now = std::time::SystemTime::now();

    // Collect file paths first; the archive/sidecar cross-checks below then
    // call `.exists()` against the live filesystem rather than the collected
    // vectors — the live check is the safer one, because a sidecar that
    // appears between `read_dir` and the archive loop spares that archive.
    // Regular-file filtering keeps directories named `*.json`/`*.tar.gz`
    // out of the loops, where they would fail removal with EISDIR and pollute
    // `removal_failures`.
    let mut partials = Vec::new();
    let mut archives = Vec::new();
    let mut sidecars = Vec::new();
    let mut staging_dirs = Vec::new();
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let path = entry.path();
        if file_type.is_dir() {
            // A `.<id>.staging` directory is the extract-and-rename working
            // area of `restore_tarball`; the name is claimed fresh on every
            // restore, so a stale one belongs to no live writer once old
            // enough. Other directories are not snapshot artefacts — leave
            // them alone.
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with('.') && n.ends_with(".staging"))
            {
                staging_dirs.push(path);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
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
        if !older_than(&path, min_age, now) {
            report.kept_young += 1;
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => report.partials_removed += 1,
            Err(e) => {
                tracing::warn!("snapshot reclaim: could not remove {}: {e}", path.display());
                report.removal_failures += 1;
            }
        }
    }

    // 2/3/4. Archive ↔ sidecar reconciliation. The rename-then-sidecar order
    //    in `create_tarball` means an archive can legitimately lack its
    //    sidecar only inside that write window; past the age guard no live
    //    writer can still be in it. A sidecar without an archive has no
    //    restore path at all, so once guarded by age it is dead weight.
    //    An unparseable sidecar next to an intact archive is repaired, not
    //    deleted: the archive itself is fully restorable, and dropping the
    //    sidecar would hand the next sweep a "sidecar-less archive" to
    //    destroy.
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
        match std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|body| serde_json::from_str::<super::SnapshotInfo>(&body).ok())
        {
            // Parseable sidecar: a healthy pair, nothing to do.
            Some(_) => continue,
            // Truncated/corrupt sidecar on an intact archive: regenerate.
            // Only the id must match the filename for list/restore to work;
            // the remaining fields mirror what `create_tarball` writes.
            None if sidecar.exists() => {
                if !older_than(archive, min_age, now) {
                    report.kept_young += 1;
                    continue;
                }
                let id = archive
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.strip_suffix(".tar.gz").unwrap_or(n).to_string())
                    .unwrap_or_default();
                let info = super::SnapshotInfo {
                    id: id.clone(),
                    // No label survives a truncated sidecar; fall back to the
                    // same id-as-name default `create` uses for unnamed
                    // snapshots.
                    name: id,
                    created_at: chrono::DateTime::<chrono::Utc>::from(
                        std::fs::metadata(archive)
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    )
                    .to_rfc3339(),
                    size: std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0),
                    backend: "tar.gz".into(),
                };
                let body = match serde_json::to_string_pretty(&info) {
                    Ok(body) => body,
                    Err(e) => {
                        tracing::warn!(
                            "snapshot reclaim: could not serialize repaired sidecar for {}: {e}",
                            archive.display()
                        );
                        report.removal_failures += 1;
                        continue;
                    }
                };
                match std::fs::write(&sidecar, body.as_bytes()) {
                    Ok(()) => {
                        report.sidecars_repaired += 1;
                        tracing::info!(
                            "snapshot reclaim: repaired truncated sidecar {} from its archive",
                            sidecar.display()
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "snapshot reclaim: could not repair sidecar {}: {e}",
                            sidecar.display()
                        );
                        report.removal_failures += 1;
                    }
                }
            }
            // No sidecar at all: the archive is invisible to list/restore;
            // past the age guard it is dead weight.
            None => {
                if !older_than(archive, min_age, now) {
                    report.kept_young += 1;
                    continue;
                }
                match std::fs::remove_file(archive) {
                    Ok(()) => report.archives_removed += 1,
                    Err(e) => {
                        tracing::warn!(
                            "snapshot reclaim: could not remove {}: {e}",
                            archive.display()
                        );
                        report.removal_failures += 1;
                    }
                }
            }
        }
    }

    for sidecar in &sidecars {
        if archive_for(sidecar).exists() {
            // An archive is present; this sidecar was either healthy (skipped
            // above) or repaired by the archive loop. Either way, done.
            continue;
        }
        if !older_than(sidecar, min_age, now) {
            report.kept_young += 1;
            continue;
        }
        match std::fs::remove_file(sidecar) {
            Ok(()) => report.sidecars_removed += 1,
            Err(e) => {
                tracing::warn!(
                    "snapshot reclaim: could not remove {}: {e}",
                    sidecar.display()
                );
                report.removal_failures += 1;
            }
        }
    }

    // 5. `.<id>.staging` — the restore working area. The relative path is
    //    rebuilt here the same way `restore_tarball` builds it so the
    //    symlink-hardened `remove_dir_all_safe` (anchored at the mount
    //    root) can be used; a plain `std::fs::remove_dir_all` would follow
    //    symlinks planted inside a crashed extraction.
    for staging in &staging_dirs {
        if !older_than(staging, min_age, now) {
            report.kept_young += 1;
            continue;
        }
        let rel = match staging.strip_prefix(&mount.root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => {
                // Cannot happen for entries read from snapshots_dir, but a
                // failed strip must never reach the rooted removal call.
                tracing::warn!(
                    "snapshot reclaim: staging path {} escapes mount root, skipping",
                    staging.display()
                );
                report.removal_failures += 1;
                continue;
            }
        };
        match crate::safe_fs::remove_dir_all_safe(mount.root_fd.as_fd(), &rel, staging) {
            Ok(()) => {
                report.staging_dirs_removed += 1;
                tracing::info!(
                    "snapshot reclaim: removed stale staging dir {}",
                    staging.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "snapshot reclaim: could not remove staging dir {}: {e}",
                    staging.display()
                );
                report.removal_failures += 1;
            }
        }
    }

    if report.total_removed() > 0 || report.sidecars_repaired > 0 || report.removal_failures > 0 {
        tracing::info!(
            "snapshot reclaim: removed {} partial, {} orphaned archive, {} orphaned sidecar \
             file(s), repaired {} truncated sidecar(s), removed {} stale staging dir(s) \
             under {} ({} below the age guard, {} removal failures)",
            report.partials_removed,
            report.archives_removed,
            report.sidecars_removed,
            report.sidecars_repaired,
            report.staging_dirs_removed,
            dir.display(),
            report.kept_young,
            report.removal_failures
        );
    }
    report
}
