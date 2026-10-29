//! Reclaim orphaned snapshot artefacts under `<mount>/.anolisa/snapshots/`.
//!
//! Artefact shapes that can accumulate without any code path reclaiming
//! them:
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
//!    valid archive. The archive is validated (full gzip stream, plus
//!    every tar entry passing the shared `EntryGate` that
//!    `restore_tarball` runs before extraction) before the sidecar is
//!    *repaired* (regenerated from the archive's own metadata); removing
//!    the sidecar would orphan a good backup and the next sweep would
//!    then destroy it under rule 2. An archive that fails that gate
//!    would also be refused by `restore_tarball`, so it is deleted
//!    instead and never advertised.
//! 5. `.<id>.staging` directories — `restore_tarball` extracts the full
//!    mount tree into a staging dir and cleans it on its error paths and
//!    on success (and clears a pre-existing one at entry), so only a
//!    hard kill (SIGKILL or power loss) mid-restore leaves one behind;
//!    `list_tarballs` never sees it, so it is the largest
//!    orphaned-artefact class byte-for-byte. A restore holds no
//!    reservation on the name (a re-run recreates staging from scratch),
//!    so past the age guard the directory can be removed wholesale —
//!    via `remove_restore_staging`, since a crashed extraction
//!    reproduces archived (possibly read-only) directory modes.
//!
//! The sweeper is age-guarded: an artefact younger than
//! `min_artifact_age_secs` is never touched, so an in-flight snapshot in
//! another process (fresh `*.partial`, archive awaiting its sidecar) is
//! always older than the guard when reclaimed. Within this process the
//! sweep holds `restore_lock`, so it cannot race a live restore's staging
//! directory; across processes the age guard is the only protection. The
//! sweep runs once per mount open, not continuously.

use std::os::fd::AsFd;
use std::path::Path;
use std::time::Duration;

use super::snapshots_dir;
use crate::error::MemoryError;
use crate::ns::MountPoint;

/// What the anchored sidecar read found for one archive.
enum SidecarState {
    /// Parsed successfully — a healthy pair.
    Healthy,
    /// Readable but unparseable — repairable.
    Corrupt,
    /// Resolvable but unreadable, or not there at all — not repairable.
    Absent,
    /// A symlink on the resolution path (or a mount crossing) — the name
    /// was not put there by `create_tarball`. Not repairable; the archive
    /// is discarded and the pair is reported as `tampered_pairs` so
    /// tampering is distinguishable from an ordinary crash leftover in
    /// the report and the summary log.
    Tampered,
}

/// Default age guard: 24 h. Any snapshot write — even of a multi-gigabyte
/// mount on slow storage — finishes far inside this window, so a younger
/// artefact may belong to a live `create_tarball` call in this or another
/// process. `SnapshotReclaimConfig`'s default defers to this constant so
/// the documented default cannot drift from the enforced one.
///
/// The floor itself — the smallest value any operator may configure — is
/// `config::MIN_ARTIFACT_AGE_FLOOR` (60 s): the age guard is the only
/// protection an in-flight snapshot in another process has, so values
/// below it are rejected at startup via TOML and ignored with a warning
/// via env. This constant is the default *above* that floor, not the
/// floor.
pub const DEFAULT_MIN_ARTIFACT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What the sweeper did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReclaimReport {
    /// `*.partial` files and atomic-write staging tempfiles removed.
    pub partials_removed: usize,
    /// `*.tar.gz` archives removed for having no sidecar.
    pub archives_removed: usize,
    /// `*.json` sidecars removed for having no archive.
    pub sidecars_removed: usize,
    /// Truncated sidecars whose archive was valid — repaired by
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
    /// (archive, sidecar) pairs discarded because the sidecar was a
    /// symlink or otherwise unresolvable through the anchored path —
    /// presumed tampering. Counted separately from `removal_failures` so
    /// the total stays true to its name: the archive removal here
    /// *succeeded*, the signal is the suspected tamper, not a failed
    /// removal.
    pub tampered_pairs: usize,
}

impl ReclaimReport {
    pub fn total_removed(&self) -> usize {
        self.partials_removed
            + self.archives_removed
            + self.sidecars_removed
            + self.staging_dirs_removed
    }
}

/// Anchored age probe: stat through the mount root fd instead of the
/// absolute path, so the age guard cannot be probed through a planted
/// symlink. Failure (including tampering) reads as "young": an
/// unstatable artefact is never removed on the basis of a missing mtime.
fn older_than_anchored(
    root: std::os::fd::BorrowedFd<'_>,
    rel: &Path,
    min_age: Duration,
    now: std::time::SystemTime,
) -> bool {
    match crate::safe_fs::metadata(root, rel).and_then(|m| m.modified().map_err(MemoryError::Io)) {
        Ok(modified) => match now.duration_since(modified) {
            Ok(age) => age >= min_age,
            // Future mtime: treat as young rather than ancient.
            Err(_) => false,
        },
        // Unreadable or unresolvable mtime: leave the file alone.
        Err(_) => false,
    }
}

/// Whether `archive` is a structurally valid snapshot: a full gzip stream
/// (headers, CRC, length) whose entries all pass the shared
/// `EntryGate` — the archive-intrinsic refusals `restore_tarball` applies
/// before extraction (entry type, `..` traversal, duplicate names,
/// file/dir prefix collisions). Runtime failures (I/O errors, ENOSPC,
/// staging permissions) are not visible to any pre-extraction check and
/// are not claimed. Because the gate is the same code restore runs, the
/// two sides cannot drift. A zero-entry archive is valid on purpose:
/// `create_tarball` legitimately produces one for an empty mount, and
/// its truncated sidecar is worth repairing. Fails closed: an unreadable
/// archive is treated as invalid. The fd is opened through the mount
/// root (`safe_fs::open_read`), both to honor the anchored-path rule and
/// because `File::open` on a FIFO blocks until a writer appears —
/// `open_read` refuses the traversal before that can matter.
fn archive_is_valid(root: std::os::fd::BorrowedFd<'_>, archive_rel: &Path) -> bool {
    use flate2::read::GzDecoder;

    let file = match crate::safe_fs::open_read(root, archive_rel) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut gz = GzDecoder::new(file);
    let mut tar = ::tar::Archive::new(&mut gz);
    let mut gate = super::tar::EntryGate::new();
    match tar.entries() {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => return false,
                };
                let ty = entry.header().entry_type();
                let path = match entry.path() {
                    Ok(path) => path.to_path_buf(),
                    Err(_) => return false,
                };
                if gate.admit(ty, &path).is_err() {
                    return false;
                }
            }
        }
        Err(_) => return false,
    }
    // Drain the rest so gzip's CRC/length check actually runs — tar stops
    // at its end marker before gzip validates the trailer.
    std::io::copy(&mut tar.into_inner(), &mut std::io::sink()).is_ok()
}

/// One bounded sweep over the snapshots directory. Best-effort: individual
/// removal failures are logged and counted, never propagated — reclaim
/// must not turn a mount open into a hard failure.
///
/// Holds `restore_lock` for the whole sweep: the staging-class removals
/// (rule 5) must not race an in-process `restore_tarball` that is between
/// creating its `.<id>.staging` and publishing it. The age guard remains
/// the only cross-process protection — this lock is per-process.
pub fn reclaim_artifacts(mount: &MountPoint, min_age: Duration) -> ReclaimReport {
    let _restore_guard = super::tar::restore_lock();
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
            // Atomic-write staging (`safe_fs::write_atomic`'s tempfiles,
            // created under the meta directory) — a hard kill between
            // staging and publication leaves one behind; no listing ever
            // shows it, so it is reclaimed purely on age like `*.partial`.
            Some("tmp") => {
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with(".memory-write-"))
                {
                    partials.push(path);
                }
            }
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

    // `<snapshots>/` relative to the mount root, for safe_fs anchored calls.
    let snapshots_rel = Path::new(mount.meta_dir_name()).join(super::SNAPSHOTS_DIR);
    let anchored = |path: &std::path::Path| -> Option<std::path::PathBuf> {
        path.strip_prefix(&dir).ok().map(|p| snapshots_rel.join(p))
    };

    // 1. `*.partial` and atomic-write `.tmp` staging — never referenced by
    //    list/restore; reclaim purely on age. Anchored like every other
    //    listed name.
    for path in partials {
        let rel = match anchored(&path) {
            Some(rel) => rel,
            None => {
                tracing::warn!(
                    "snapshot reclaim: staging path {} escapes snapshots dir, skipping",
                    path.display()
                );
                report.removal_failures += 1;
                continue;
            }
        };
        if !older_than_anchored(mount.root_fd.as_fd(), &rel, min_age, now) {
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
    //    deleted: the archive passed the same pre-extraction gate restore
    //    runs, and dropping the sidecar would hand the next sweep a
    //    "sidecar-less archive" to destroy.
    //
    //    SECURITY: `sidecar_for`/`archive_for` derive filenames from a
    //    directory listing, so unlike every other path builder in this
    //    crate they can be handed an attacker-chosen name. Every read,
    //    probe and write below therefore goes through `safe_fs` anchored
    //    at the mount root fd (openat2 RESOLVE_BENEATH | NO_SYMLINKS) —
    //    never the absolute path, which would follow a symlinked sidecar
    //    and turn the repair write into an arbitrary-file overwrite.
    //    Because the anchored probe refuses symlink traversal, a
    //    *symlinked* sidecar reads as tampered (`PathOutsideMount`): the
    //    archive itself is deleted while the link survives. That is the
    //    explicit decision here: both names are attacker-plantable,
    //    neither is trusted, and the honest outcome is to discard the
    //    pair and report it as `tampered_pairs` rather than repair
    //    through the link — so a tamper-induced destruction of a valid
    //    archive is distinguishable, in `ReclaimReport` and the summary
    //    log, from an ordinary crash leftover. A legitimate sidecar is a
    //    regular file written by `create_tarball`; the model has no
    //    symlink primitive, so losing the paired archive is not a
    //    real-data loss.
    let sidecar_for = |archive: &std::path::Path| -> std::path::PathBuf {
        let name = archive.file_name().unwrap_or_default().to_string_lossy();
        let stem = name.strip_suffix(".tar.gz").unwrap_or(&name);
        dir.join(format!("{stem}.json"))
    };

    for archive in &archives {
        let sidecar = sidecar_for(archive);
        let sidecar_rel = match anchored(&sidecar) {
            Some(rel) => rel,
            None => {
                tracing::warn!(
                    "snapshot reclaim: sidecar path {} escapes snapshots dir, skipping",
                    sidecar.display()
                );
                report.removal_failures += 1;
                continue;
            }
        };
        // The archive gets the same anchored treatment as the sidecar: the
        // listing filter keeps symlinks out of `archives`, but between
        // `read_dir` and these calls the name can be swapped, and a plain
        // `File::open` on a swapped-in FIFO blocks until a writer appears
        // — while the sweep holds `restore_lock`, hanging every restore in
        // the process. `open_read`/`metadata` resolve through openat2 with
        // NO_SYMLINKS, so a swap is refused instead of followed.
        let archive_rel = match anchored(archive) {
            Some(rel) => rel,
            None => {
                tracing::warn!(
                    "snapshot reclaim: archive path {} escapes snapshots dir, skipping",
                    archive.display()
                );
                report.removal_failures += 1;
                continue;
            }
        };
        // Read the sidecar anchored at the root fd. `read_to_string` fails
        // with ELOOP/NotFound on a symlinked or dangling sidecar (openat2
        // NO_SYMLINKS refuses the traversal), so a link never gets read;
        // `PathOutsideMount` (ELOOP/EXDEV) is reported as tampering rather
        // than folded into the ordinary absent case.
        let sidecar_state =
            match crate::safe_fs::read_to_string(mount.root_fd.as_fd(), &sidecar_rel) {
                Ok(body) => match serde_json::from_str::<super::SnapshotInfo>(&body) {
                    // Parseable sidecar: a healthy pair, nothing to do.
                    Ok(_) => SidecarState::Healthy,
                    Err(_) => SidecarState::Corrupt,
                },
                // A symlink or mount crossing on the resolution path is
                // tampering: both names are attacker-plantable, neither is
                // trusted, and the honest outcome is to discard the pair
                // and surface it as a failure rather than repair through
                // the link.
                Err(crate::error::MemoryError::PathOutsideMount(_)) => SidecarState::Tampered,
                // Unreadable or unresolvable (dangling symlink, permissions,
                // vanished inode): not repairable — treated like an absent
                // sidecar. The link itself is never followed.
                Err(_) => SidecarState::Absent,
            };
        match sidecar_state {
            SidecarState::Healthy => continue,
            // Truncated/corrupt sidecar on an intact archive: regenerate.
            // Only the id must match the filename for list/restore to work;
            // the remaining fields mirror what `create_tarball` writes.
            SidecarState::Corrupt => {
                if !older_than_anchored(mount.root_fd.as_fd(), &archive_rel, min_age, now) {
                    report.kept_young += 1;
                    continue;
                }
                // Only repair when the archive passes the pre-extraction
                // gate — an archive whose entries restore would refuse
                // must not gain a fabricated sidecar advertising it
                // forever (list shows it, restore fails, and neither sweep
                // rule ever fires again). Runtime restore failures (I/O,
                // ENOSPC) are outside any pre-extraction check and out of
                // scope here. An invalid archive is deleted under the
                // sidecar-less rule semantics below instead.
                if !archive_is_valid(mount.root_fd.as_fd(), &archive_rel) {
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
                    continue;
                }
                let id = archive
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.strip_suffix(".tar.gz").unwrap_or(n).to_string())
                    .unwrap_or_default();
                // One anchored metadata read for both repaired fields. If
                // the archive metadata has become unreadable (or
                // unresolvable — a planted link) since validation, fall
                // back to the epoch explicitly: `created_at` = 1970-01-01
                // sorts first in `list_tarballs`, so a fabricated timestamp
                // is at least visible and visibly wrong rather than
                // silently appearing recent.
                let meta = crate::safe_fs::metadata(mount.root_fd.as_fd(), &archive_rel);
                let created_at = chrono::DateTime::<chrono::Utc>::from(match meta.as_ref() {
                    Ok(meta) => meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    Err(_) => std::time::SystemTime::UNIX_EPOCH,
                })
                .to_rfc3339();
                let info = super::SnapshotInfo {
                    id: id.clone(),
                    // No label survives a truncated sidecar; fall back to the
                    // same id-as-name default `create` uses for unnamed
                    // snapshots.
                    name: id,
                    created_at,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
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
                match crate::safe_fs::write_atomic(
                    mount.root_fd.as_fd(),
                    // write_atomic stages its replacement in `staging_dir`;
                    // snapshots/ itself is the designated staging parent here,
                    // same filesystem as the destination sidecar.
                    &snapshots_rel,
                    &sidecar_rel,
                    body.as_bytes(),
                ) {
                    Ok(_) => {
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
            SidecarState::Absent => {
                // No readable sidecar (absent, dangling, or unreadable):
                // the archive is invisible to list/restore; past the age
                // guard it is dead weight. The link itself is never
                // followed and never repaired through.
                if !older_than_anchored(mount.root_fd.as_fd(), &archive_rel, min_age, now) {
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
            SidecarState::Tampered => {
                // A symlinked (or mount-crossing) sidecar: the same
                // discard-the-pair outcome as absent, but counted as
                // `tampered_pairs` — an operator must be able to tell a
                // crash leftover from someone planting links in the
                // snapshots directory. The link itself is never removed:
                // the sweep never deletes names it cannot classify as its
                // own regular artefacts.
                if !older_than_anchored(mount.root_fd.as_fd(), &archive_rel, min_age, now) {
                    report.kept_young += 1;
                    continue;
                }
                match std::fs::remove_file(archive) {
                    Ok(()) => {
                        report.archives_removed += 1;
                        report.tampered_pairs += 1;
                        tracing::warn!(
                            "snapshot reclaim: removed archive {} whose sidecar {} \
                             is a symlink or otherwise unresolvable through the \
                             anchored path; treating as tampering",
                            archive.display(),
                            sidecar.display()
                        );
                    }
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
        // Anchored existence probe, same reasoning as the archive loop: the
        // sidecar's name comes from a directory listing, so the archive
        // path must never be probed through the absolute path.
        let archive_rel = snapshots_rel.join(format!(
            "{}.tar.gz",
            sidecar
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.strip_suffix(".json").unwrap_or(n))
                .unwrap_or_default()
        ));
        if crate::safe_fs::exists(mount.root_fd.as_fd(), &archive_rel) {
            // An archive is present; this sidecar was either healthy (skipped
            // above) or repaired by the archive loop. Either way, done.
            continue;
        }
        // The sidecar's own age check is anchored too: its name is just as
        // attacker-chosen as the archive's, so the mtime must not be probed
        // through a planted link. An unresolvable sidecar reads as young
        // and is left for the next sweep.
        let sidecar_rel = match anchored(sidecar) {
            Some(rel) => rel,
            None => {
                tracing::warn!(
                    "snapshot reclaim: sidecar path {} escapes snapshots dir, skipping",
                    sidecar.display()
                );
                report.removal_failures += 1;
                continue;
            }
        };
        if !older_than_anchored(mount.root_fd.as_fd(), &sidecar_rel, min_age, now) {
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
    //    symlink-hardened, root-anchored removal can be used; a plain
    //    `std::fs::remove_dir_all` would follow symlinks planted inside a
    //    crashed extraction.
    //
    //    `remove_restore_staging` (not `remove_dir_all_safe`) because
    //    main's restore deliberately reproduces archived directory modes:
    //    a crashed extraction can contain read-only directories
    //    (0o500/0o444), whose files cannot be unlinked without temporarily
    //    granting owner write/search on the held inode. Staging is exactly
    //    the "disposable restore staging" that function's contract allows:
    //    the `.`-prefix + `.staging`-suffix matcher only matches names
    //    `restore_tarball` itself creates, and `.anolisa` is reserved, so
    //    no other writer can produce such a name under snapshots/.
    for staging in &staging_dirs {
        // The listing filter keeps symlinks out of `staging_dirs`, but the
        // mtime probe is anchored like every other listed name.
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
        if !older_than_anchored(mount.root_fd.as_fd(), &rel, min_age, now) {
            report.kept_young += 1;
            continue;
        }
        match crate::safe_fs::remove_restore_staging(mount.root_fd.as_fd(), &rel, staging) {
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

    if report.total_removed() > 0
        || report.sidecars_repaired > 0
        || report.removal_failures + report.tampered_pairs > 0
    {
        // Failures and tampering escalate to warn: neither resolves on its
        // own (a read-only staging dir, an unremovable orphan, planted
        // links) and would otherwise grow silently at info level. One
        // message body, two call sites — the macros need a literal level
        // for callsite registration, so the branch selects the macro
        // rather than duplicating the format string.
        macro_rules! summary {
            ($level:ident) => {
                tracing::$level!(
                    "snapshot reclaim: removed {} partial, {} orphaned archive, {} orphaned sidecar \
                     file(s), repaired {} truncated sidecar(s), removed {} stale staging dir(s) \
                     under {} ({} below the age guard, {} removal failures, {} tampered pair(s))",
                    report.partials_removed,
                    report.archives_removed,
                    report.sidecars_removed,
                    report.sidecars_repaired,
                    report.staging_dirs_removed,
                    dir.display(),
                    report.kept_young,
                    report.removal_failures,
                    report.tampered_pairs
                );
            };
        }
        if report.removal_failures + report.tampered_pairs > 0 {
            summary!(warn);
        } else {
            summary!(info);
        }
    }
    report
}
