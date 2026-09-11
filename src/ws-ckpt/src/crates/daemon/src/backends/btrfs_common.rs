use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, error, info, warn};
use ws_ckpt_common::backend::SnapshotDeleteOutcome;
use ws_ckpt_common::{ChangeType, DiffEntry};

use crate::util::unescape_proc_mount;

/// init_workspace backup path (#673).
pub fn backup_path_for(original_path: &str) -> String {
    format!("{}.pre-init-bak", original_path.trim_end_matches('/'))
}

/// Recover an orphan `.pre-init-bak` left by an interrupted prior init.
///
/// Called by `do_init_storage` in `btrfs_base.rs` and `btrfs_loop.rs` BEFORE
/// the Step 3 `rename(original_path -> backup_path)`. Two cases handled:
///
/// 1. Backup exists + subvol_path does NOT exist:
///    Prior init was interrupted AFTER Step 3 (rename) but BEFORE Step 4
///    (data migration). User's original data is sitting in the backup. The
///    right thing is to rename the backup back to `original_path`, restoring
///    the user's data, and let the caller proceed with a fresh normal init.
///    A stale empty directory at `original_path` (e.g., from a fixture's
///    `rm -rf + mkdir -p`) is removed first; a non-empty dir is refused to
///    avoid destroying user data.
///
/// 2. Backup exists + subvol_path DOES exist:
///    Prior init completed data migration (Step 4) and may have created the
///    symlink (Step 5). State is ambiguous (subvol might be valid, symlink
///    might be dangling, original_path might be a stale dir, etc.). Auto-
///    recovery would risk data loss, so we bail with an actionable error
///    pointing at `ws-ckpt recover -w <path> --force` (which we also patch
///    in this PR to clean orphan backups at the end of recovery).
///
/// 3. No backup exists: noop.
pub async fn recover_orphan_backup(original_path: &str, subvol_path: &Path) -> Result<()> {
    let backup_path = backup_path_for(original_path);
    if tokio::fs::symlink_metadata(&backup_path).await.is_err() {
        return Ok(());
    }

    let subvol_exists = tokio::fs::metadata(subvol_path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false);

    if !subvol_exists {
        // Case 1: prior init crashed between Step 3 (rename) and Step 4
        // (data migration). User data is in backup. Restore it back to
        // original_path so the caller can re-run a clean init.
        warn!(
            "init: orphan backup {:?} detected (prior init interrupted before data migration); \
             restoring user data from backup",
            backup_path
        );
        // Remove stale state at original_path before restoring backup into it.
        match tokio::fs::symlink_metadata(original_path).await {
            Ok(m) if m.file_type().is_symlink() => {
                // dangling or stale symlink — safe to remove
                let _ = tokio::fs::remove_file(original_path).await;
            }
            Ok(m) if m.is_dir() => {
                let mut entries = tokio::fs::read_dir(original_path).await?;
                let is_empty = entries.next_entry().await?.is_none();
                if !is_empty {
                    bail!(
                        "found orphan backup {:?} but {:?} is a non-empty directory; \
                         refusing to overwrite user data. Inspect {:?} (likely contains \
                         data from interrupted init) and {:?}, move data out of {:?} or \
                         remove {:?} manually, then re-run init",
                        backup_path,
                        original_path,
                        backup_path,
                        original_path,
                        original_path,
                        backup_path
                    );
                }
                let _ = tokio::fs::remove_dir(original_path).await;
            }
            Ok(_) => {
                bail!(
                    "found orphan backup {:?} but {:?} is an unexpected file type; \
                     remove {:?} manually before retrying",
                    backup_path,
                    original_path,
                    original_path
                );
            }
            Err(_) => { /* original_path missing — fine, rename will create it */ }
        }
        tokio::fs::rename(&backup_path, original_path)
            .await
            .with_context(|| {
                format!(
                    "failed to restore orphan backup {:?} -> {:?}",
                    backup_path, original_path
                )
            })?;
        info!(
            "init: restored user data from orphan backup {:?} to {:?}; proceeding with fresh init",
            backup_path, original_path
        );
        return Ok(());
    }

    // Case 2: subvol_path exists. Ambiguous state — bail with actionable error.
    bail!(
        "found orphan backup {:?} and existing subvolume {:?} from an interrupted prior init. \
         Run `ws-ckpt recover -w {} --force` to restore user data from the subvolume and clean \
         up the orphan backup, then re-run init. If `ws-ckpt recover` does not list this workspace, \
         manually inspect {:?} (likely contains pre-migration user data) and {:?}, move data out \
         as needed, and remove the backup before retrying",
        backup_path, subvol_path, original_path, backup_path, subvol_path
    );
}

/// Roll back a failed init_workspace; `backup_owned=true` only when this init created the backup (#673).
pub async fn cleanup_init_storage(
    original_path: &str,
    subvol_path: &Path,
    snap_dir: &Path,
    backup_owned: bool,
    fs_root: &Path,
) {
    if backup_owned {
        restore_original_from_backup(original_path).await;
    } else if let Ok(meta) = tokio::fs::symlink_metadata(original_path).await {
        if meta.file_type().is_symlink() {
            let _ = tokio::fs::remove_file(original_path).await;
        }
    }
    let _ = tokio::fs::remove_dir_all(snap_dir).await;
    // Space-aware (#3053): the half-migrated subvolume can hold substantial
    // rsync'd data, and the trigger chain for this cleanup is often "backend
    // full → init fails with ENOSPC" — i.e. exactly the regime where a plain
    // async delete strands a cleaner-stalled zombie.
    if let Err(e) = delete_subvolume_space_aware(subvol_path, fs_root).await {
        error!("cleanup: failed to delete subvolume: {}", e);
    }
}

/// Rename our own `.pre-init-bak` back over original_path; foreign data at original is preserved.
async fn restore_original_from_backup(original_path: &str) {
    let backup_path = backup_path_for(original_path);
    match tokio::fs::symlink_metadata(&backup_path).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!(
                "cleanup: backup {:?} unexpectedly missing; dropping leftover symlink at {}",
                backup_path, original_path
            );
            if let Ok(meta) = tokio::fs::symlink_metadata(original_path).await {
                if meta.file_type().is_symlink() {
                    let _ = tokio::fs::remove_file(original_path).await;
                }
            }
            return;
        }
        Err(e) => {
            error!(
                "cleanup: cannot stat backup {:?}: {}; aborting restore (manual recovery required)",
                backup_path, e
            );
            return;
        }
    }

    match tokio::fs::symlink_metadata(original_path).await {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = tokio::fs::remove_file(original_path).await;
        }
        Ok(meta) if meta.is_dir() => {
            let _ = tokio::fs::remove_dir(original_path).await;
        }
        _ => {}
    }

    match tokio::fs::rename(&backup_path, original_path).await {
        Ok(()) => info!("cleanup: restored {} from backup", original_path),
        Err(e) => error!(
            "cleanup: failed to restore {:?} -> {:?}: {}; backup retained for manual recovery",
            backup_path, original_path, e
        ),
    }
}

/// Ensure the current kernel can mount btrfs.
///
/// Checks `/proc/filesystems`; if absent, tries `modprobe btrfs` once and rechecks.
/// Fails with an actionable message pointing at kernel-modules-extra / CONFIG_BTRFS_FS.
pub async fn ensure_btrfs_support() -> Result<()> {
    if proc_filesystems_has_btrfs().await? {
        return Ok(());
    }

    // Best-effort modprobe; exit code is ignored, the recheck is authoritative.
    let _ = Command::new("modprobe").arg("btrfs").status().await;

    if proc_filesystems_has_btrfs().await? {
        info!("Loaded btrfs kernel module");
        return Ok(());
    }

    bail!(
        "Kernel does not support btrfs (no entry in /proc/filesystems and \
         `modprobe btrfs` did not register the module). Install the matching \
         kernel-modules-extra package or rebuild the kernel with CONFIG_BTRFS_FS, \
         then restart the systemd service (`systemctl restart ws-ckpt`) or the \
         ws-ckpt daemon container."
    );
}

/// True if `btrfs` is listed in `/proc/filesystems`.
async fn proc_filesystems_has_btrfs() -> Result<bool> {
    let file = File::open("/proc/filesystems")
        .await
        .context("Failed to open /proc/filesystems")?;
    let mut reader = BufReader::new(file).lines();
    while let Some(line) = reader.next_line().await? {
        // Line format: "<fstype>" or "nodev <fstype>"; fs name is always the last token.
        if line.split_whitespace().last() == Some("btrfs") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Resolve a path that may be a symlink to its real (canonical) path.
/// If the path is a symlink, it is resolved via `canonicalize`.
/// If the path does not exist or is not a symlink, it is returned as-is.
pub async fn resolve_symlink_path(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    match tokio::fs::symlink_metadata(p).await {
        Ok(meta) if meta.file_type().is_symlink() => {
            let resolved = tokio::fs::canonicalize(p)
                .await
                .with_context(|| format!("failed to resolve workspace symlink: {}", path))?;
            info!(
                "resolved workspace symlink: {} -> {}",
                path,
                resolved.display()
            );
            Ok(resolved)
        }
        _ => Ok(PathBuf::from(path)),
    }
}

/// Create a new btrfs subvolume at the given path
pub async fn create_subvolume(path: &Path) -> Result<()> {
    info!("creating btrfs subvolume: {}", path.display());
    let output = Command::new("btrfs")
        .args(["subvolume", "create"])
        .arg(path)
        .output()
        .await
        .context("failed to execute btrfs subvolume create")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("btrfs subvolume create failed: {}", stderr);
        bail!("btrfs subvolume create failed: {}", stderr.trim());
    }
    info!("subvolume created: {}", path.display());
    Ok(())
}

/// Create a btrfs snapshot
/// If readonly=true, creates a readonly snapshot (-r flag)
pub async fn create_snapshot(src: &Path, dst: &Path, readonly: bool) -> Result<()> {
    info!(
        "creating snapshot: {} -> {} (readonly={})",
        src.display(),
        dst.display(),
        readonly
    );
    let mut cmd = Command::new("btrfs");
    cmd.arg("subvolume").arg("snapshot");
    if readonly {
        cmd.arg("-r");
    }
    cmd.arg(src).arg(dst);

    let output = cmd
        .output()
        .await
        .context("failed to execute btrfs subvolume snapshot")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("btrfs snapshot failed: {}", stderr);
        bail!("btrfs snapshot failed: {}", stderr.trim());
    }
    info!("snapshot created: {}", dst.display());
    Ok(())
}

/// Delete a btrfs subvolume
pub async fn delete_subvolume(path: &Path) -> Result<()> {
    info!("deleting btrfs subvolume: {}", path.display());
    let output = Command::new("btrfs")
        .args(["subvolume", "delete"])
        .arg(path)
        .output()
        .await
        .context("failed to execute btrfs subvolume delete")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("btrfs subvolume delete failed: {}", stderr);
        bail!("btrfs subvolume delete failed: {}", stderr.trim());
    }
    info!("subvolume deleted: {}", path.display());
    Ok(())
}

/// Check path existence without hiding filesystem errors as absence.
async fn path_exists_fallible(path: &Path) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("failed to inspect path {}", path.display())),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Space-aware deletion & zombie subvolume handling (issue #3053)
//
// `btrfs subvolume delete` is asynchronous: it only queues the subvolume for
// removal, and the kernel cleaner thread frees the extents later. Under
// ENOSPC the cleaner cannot make progress, so deleted subvolumes become
// zombies (`btrfs subvolume list -d` shows `top level 0 path DELETED`) that
// pin all backend space. Because the daemon reuses the existing mount across
// restarts (by design, #2809), no mount cycle ever
// kicks the cleaner, and the space stays pinned until an operator manually
// umounts.
//
// Crucially, `btrfs subvolume sync` does NOT kick the cleaner: in
// btrfs-progs 6.1.2 it is a pure poll loop (`wait_for_subvolume_cleaning`
// in cmds/subvolume.c — a subvolume-info ioctl plus `sleep(1)` per round,
// no commit, no ioctl that would wake the cleaner thread). Waking the
// cleaner is a side effect of committing a transaction, i.e. of
// `btrfs filesystem sync`. Waiting before kicking would just burn the
// timeout while a stalled cleaner stays asleep.
//
// The helpers below (a) gate deletions on backend fullness and, when the
// backend is nearly full, kick the cleaner with a commit BEFORE waiting on a
// bounded `btrfs subvolume sync`, then commit again so reclaimed space
// becomes visible, and (b) detect and drain pre-existing zombies at
// bootstrap.
// ────────────────────────────────────────────────────────────────────────────

/// Backend usage percentage at which subvolume deletion switches to the
/// guarded path (delete + commit to kick the cleaner + bounded `btrfs
/// subvolume sync` wait + commit to publish the freed space).
pub const FS_DELETE_GUARD_THRESHOLD_PERCENT: f64 = 95.0;

/// Timeout for the post-delete `btrfs subvolume sync` on the guarded path.
/// Bounds the extra latency a rollback/cleanup pays when the backend is full.
pub const DELETE_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for a BATCH guarded delete's single multi-id `btrfs subvolume
/// sync`. The whole batch shares ONE kick + ONE wait + ONE visibility commit,
/// so the worst-case added latency stays constant in batch size (#3053
/// review P1-c); the larger budget accounts for the cleaner dropping several
/// subvolumes sequentially.
pub const BATCH_DELETE_SYNC_TIMEOUT: Duration = Duration::from_secs(120);

/// Timeout for the bootstrap zombie sweep's `btrfs subvolume sync`. Longer
/// than the delete-path timeout: draining happens once at startup and the
/// backend is already in a degraded state when zombies exist.
pub const BOOTSTRAP_ZOMBIE_SWEEP_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeout for each transaction commit (`btrfs filesystem sync`) on the
/// guarded paths — both the pre-wait cleaner kick and the post-reclaim
/// visibility commit.
pub const COMMIT_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Commit the current transaction (`btrfs filesystem sync`), best-effort.
///
/// This plays two distinct roles on the guarded paths (#3053):
///
/// 1. **Cleaner kick, BEFORE waiting.** Committing a transaction is what
///    wakes the kernel cleaner thread. `btrfs subvolume sync` does not: in
///    btrfs-progs 6.1.2 it only polls root ids in a sleep loop
///    (`wait_for_subvolume_cleaning`, cmds/subvolume.c). A commit-first
///    ordering is what turns the bounded sync wait into a wait for a cleaner
///    that has actually been started.
/// 2. **Visibility commit, AFTER reclaim.** Freed extents only become
///    visible — and usable — once the transaction commits. Probed
///    empirically on btrfs-progs 6.1/kernel 6.6: between drop and commit,
///    reported free space can even DROP (extents sit pinned pre-commit).
///    Committing after the sync makes reclaimed space immediately available
///    and reflected by `get_usage`/health reporting, without waiting for the
///    next natural commit cycle.
pub async fn commit_filesystem(fs_root: &Path) {
    let spawned = Command::new("btrfs")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["filesystem", "sync"])
        .arg(fs_root)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    match spawned {
        Ok(child) => {
            match tokio::time::timeout(COMMIT_SYNC_TIMEOUT, child.wait_with_output()).await {
                Ok(Ok(out)) if out.status.success() => {}
                Ok(Ok(out)) => warn!(
                    "btrfs filesystem sync on {} failed: {}",
                    fs_root.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Ok(Err(e)) => warn!(
                    "btrfs filesystem sync on {} wait failed: {:#}",
                    fs_root.display(),
                    e
                ),
                Err(_) => warn!(
                    "btrfs filesystem sync on {} timed out after {}s",
                    fs_root.display(),
                    COMMIT_SYNC_TIMEOUT.as_secs()
                ),
            }
        }
        Err(e) => warn!(
            "failed to execute btrfs filesystem sync on {}: {:#}",
            fs_root.display(),
            e
        ),
    }
}

/// Whether the backend filesystem is full enough that async subvolume
/// deletion risks producing cleaner-stalled zombie subvolumes (#3053).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceRisk {
    Low,
    High,
}

/// How far the bootstrap zombie sweep may go on a backend filesystem
/// (#3053 review P1-a). The wait budget lives in the only variant that
/// actually waits, so no caller ever passes a parameter the callee ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZombieSweepPolicy {
    /// Dedicated loop image: every `list -d` entry is ws-ckpt's own, so the
    /// sweep may block startup on a kick + bounded `subvolume sync` wait +
    /// re-check.
    KickThenWait { wait_timeout: Duration },
    /// Shared host partition (btrfs-base, possibly the root fs): `list -d`
    /// is FILESYSTEM-WIDE and may contain other tools' subvolumes. The sweep
    /// kicks the cleaner (a commit is fs-global and cheap) but never waits —
    /// blocking ws-ckpt startup on entries it does not own is not acceptable,
    /// and all diagnostics must say the listing is fs-wide.
    KickOnly,
}

/// Whether a zombie sweep authoritatively confirmed that no deleted roots remain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZombieSweepOutcome {
    /// The dead-root list was successfully observed empty.
    Clear,
    /// Deleted roots remain, or their state could not be verified.
    Unresolved,
}

/// Pure decision: usage percentage at/above the guard threshold ⇒ High.
/// An unreadable capacity (`total == 0`, e.g. `btrfs filesystem usage`
/// output that failed to parse) is UNKNOWN, and unknown must fail CLOSED —
/// see [`assess_space_risk`].
fn space_risk_from_usage(total: u64, used: u64) -> SpaceRisk {
    if total == 0 {
        return SpaceRisk::High;
    }
    let pct = used as f64 / total as f64 * 100.0;
    if pct >= FS_DELETE_GUARD_THRESHOLD_PERCENT {
        SpaceRisk::High
    } else {
        SpaceRisk::Low
    }
}

/// Internal usage details needed for conservative deletion decisions.
#[derive(Debug, PartialEq, Eq)]
struct FilesystemUsage {
    total: u64,
    /// Nominal usage retained for the public health tuple.
    used: u64,
    /// Usage derived from the `Free (estimated)` minimum when available.
    deletion_used: u64,
    metadata_profiles: Vec<(u64, u64)>,
    malformed_usage: bool,
    malformed_metadata_profile: bool,
}

impl FilesystemUsage {
    fn deletion_risk(&self) -> SpaceRisk {
        if self.malformed_usage
            || self.malformed_metadata_profile
            || self.metadata_profiles.is_empty()
            || self
                .metadata_profiles
                .iter()
                .any(|&(size, used)| space_risk_from_usage(size, used) == SpaceRisk::High)
        {
            SpaceRisk::High
        } else {
            space_risk_from_usage(self.total, self.deletion_used)
        }
    }
}

/// Classify the backend's current space risk.
///
/// FAIL-CLOSED: when usage cannot be read (command failure, or output that
/// parses to `total == 0` — e.g. a localized `btrfs filesystem usage` whose
/// field names defeated the parser), the backend is treated as High. The
/// degraded-probe regime is exactly where ENOSPC zombies breed (#3053);
/// silently downgrading to the unguarded path there would re-create the
/// original bug. Fail-closed costs nothing in correctness: the guarded
/// path's sync/commit/re-check steps are all best-effort and can never turn
/// a delete that would have succeeded into a failure — only bounded extra
/// latency and louder logs.
pub async fn assess_space_risk(fs_root: &Path) -> SpaceRisk {
    match get_filesystem_usage_details(fs_root).await {
        Ok(usage) => {
            let risk = usage.deletion_risk();
            if risk == SpaceRisk::High {
                if usage.total == 0 {
                    warn!(
                        "cannot determine backend capacity at {:?} (no usable 'Device size' in \
                         `btrfs filesystem usage` output) — entering guarded delete path \
                         conservatively (fail-closed, #3053)",
                        fs_root
                    );
                } else {
                    warn!(
                        "backend filesystem deletion risk is high at {:?} (conservative usage {} / \
                         {} bytes, or metadata profile full/unreadable at the {:.1}% threshold) — \
                         entering guarded delete path: subvolume deletions will wait on the btrfs \
                         cleaner (ENOSPC zombie risk, #3053)",
                        fs_root,
                        usage.deletion_used,
                        usage.total,
                        FS_DELETE_GUARD_THRESHOLD_PERCENT
                    );
                }
            }
            risk
        }
        Err(e) => {
            warn!(
                "cannot assess backend usage at {:?} before deletion ({:#}) — entering guarded \
                 delete path conservatively (fail-closed, #3053)",
                fs_root, e
            );
            SpaceRisk::High
        }
    }
}

/// Resolve the btrfs subvolume (root) id of `path` via
/// `btrfs inspect-internal rootid`. Must be called BEFORE deletion — the id
/// is not resolvable once the subvolume is gone.
pub async fn get_subvolume_id(path: &Path) -> Result<u64> {
    let output = Command::new("btrfs")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["inspect-internal", "rootid"])
        .arg(path)
        .output()
        .await
        .context("failed to execute btrfs inspect-internal rootid")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "btrfs inspect-internal rootid failed for {}: {}",
            path.display(),
            stderr.trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .with_context(|| format!("failed to parse rootid output for {}", path.display()))
}

/// List ids of subvolumes that were deleted but not yet reclaimed by the
/// kernel cleaner ("zombie" subvolumes) on the filesystem containing
/// `fs_root`. These keep pinning backend space until drained (#3053).
pub async fn list_deleted_subvolumes(fs_root: &Path) -> Result<Vec<u64>> {
    let output = Command::new("btrfs")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["subvolume", "list", "-d"])
        .arg(fs_root)
        .output()
        .await
        .context("failed to execute btrfs subvolume list -d")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("btrfs subvolume list -d failed: {}", stderr.trim());
    }
    parse_deleted_subvolume_ids(&String::from_utf8_lossy(&output.stdout))
        .context("failed to parse btrfs subvolume list -d output")
}

/// Parse `btrfs subvolume list -d` output and return ids of true zombie
/// subvolumes that are awaiting cleaner reclaim.
///
/// In btrfs-progs, a deleted root makes `resolve_root` fail before assigning
/// `top_id`, so it prints `top level 0 path DELETED`. A live subvolume whose
/// ancestor was deleted can also print `path DELETED`, but retains a non-zero
/// top level. Only `top level 0` therefore identifies a root that can disappear
/// from a `btrfs subvolume sync` wait.
///
/// The `<FS_TREE>/` prefix is added by `filter_full_path`, which `subvolume
/// list` installs only with `-a`; this daemon invokes `list -d` without `-a`.
///
/// ```text
/// ID 259 gen 40 top level 0 path DELETED      ← zombie (included)
/// ID 262 gen 43 top level 256 path DELETED    ← live orphan (excluded)
/// ```
fn parse_deleted_subvolume_ids(output: &str) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first() != Some(&"ID") {
            bail!("unexpected record at line {}: {}", line_number + 1, line);
        }
        let id = tokens
            .get(1)
            .context("missing subvolume id")?
            .parse::<u64>()
            .with_context(|| format!("invalid subvolume id at line {}", line_number + 1))?;
        let top_level = tokens
            .windows(3)
            .find(|w| w[0] == "top" && w[1] == "level")
            .and_then(|w| w[2].parse::<u64>().ok())
            .with_context(|| format!("invalid top level at line {}", line_number + 1))?;
        if top_level == 0 {
            ids.push(id);
        } else if tokens
            .last()
            .is_some_and(|path| path.eq_ignore_ascii_case("DELETED") || path.ends_with("/DELETED"))
        {
            debug!(id, line, "skipping live orphan from deleted-subvolume list");
        }
    }
    Ok(ids)
}

/// Wait (bounded by `timeout`) for the kernel cleaner to finish reclaiming
/// the given deleted subvolumes via `btrfs subvolume sync`.
///
/// NOTE: this only WAITS. btrfs-progs implements `subvolume sync` as a poll
/// loop over subvolume-info ioctls (`wait_for_subvolume_cleaning`); it never
/// wakes the cleaner thread. Callers must kick the cleaner first via
/// [`commit_filesystem`] — waiting before kicking just burns the timeout
/// while a stalled cleaner stays asleep (#3053).
///
/// The child is killed on timeout (`kill_on_drop`) so a stalled cleaner
/// never leaves a lingering process behind. A timeout is not a hard error
/// for the filesystem itself — the cleaner keeps working in the background —
/// callers decide how to report it.
pub async fn sync_subvolume_deletes(fs_root: &Path, ids: &[u64], timeout: Duration) -> Result<()> {
    let mut cmd = Command::new("btrfs");
    cmd.env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("subvolume")
        .arg("sync")
        .arg(fs_root)
        .kill_on_drop(true);
    for id in ids {
        cmd.arg(id.to_string());
    }
    let child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to execute btrfs subvolume sync")?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => bail!(
            "btrfs subvolume sync failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Ok(Err(e)) => Err(e).context("failed to wait for btrfs subvolume sync"),
        Err(_) => bail!(
            "btrfs subvolume sync timed out after {}s waiting for the cleaner to reclaim {:?}",
            timeout.as_secs(),
            ids
        ),
    }
}

/// Parse `/proc/mounts` content and return EVERY mount point of the
/// filesystem containing `path`: longest-prefix match locates the mount
/// holding `path`, then all mounts sharing that line's device field are
/// collected (octal escapes decoded, result sorted for deterministic
/// messages).
///
/// Collecting the whole device matters because the btrfs cleaner only resets
/// when the LAST mount of the filesystem goes away: umounting one of
/// several points — another subvolume of the same device, a bind mount, a
/// propagated container mount — leaves the superblock alive, and guidance
/// naming only that one point would silently accomplish nothing (#3053
/// review P2). Mounts in other namespaces are invisible from here; this is
/// best-effort by nature.
fn mount_points_for_device_in(content: &str, path: &Path) -> Vec<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let mp = PathBuf::from(unescape_proc_mount(parts[1]));
        if path == mp || path.starts_with(&mp) {
            let better = match &best {
                Some((_, b)) => mp.as_os_str().len() > b.as_os_str().len(),
                None => true,
            };
            if better {
                best = Some((parts[0].to_string(), mp));
            }
        }
    }
    let Some((device, _)) = best else {
        return Vec::new();
    };
    let mut all: Vec<PathBuf> = Vec::new();
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 || parts[0] != device {
            continue;
        }
        let mp = PathBuf::from(unescape_proc_mount(parts[1]));
        if !all.contains(&mp) {
            all.push(mp);
        }
    }
    all.sort();
    all
}

/// Best-effort resolution of ALL mount points of the filesystem containing
/// `fs_root`, for umount-cycle recovery guidance.
///
/// The btrfs-base backend's `fs_root` is `<btrfs_mount>/ws-ckpt-data` — a
/// SUBDIRECTORY of the host partition — so guidance printing "umount
/// <fs_root>" would hand operators a command that fails with "not mounted"
/// at the exact moment the backend is pinned and they are most stressed.
/// Falls back to `fs_root` itself (correct for btrfs-loop, whose fs_root IS
/// the mount point) when /proc/mounts is unreadable or unmatched.
pub async fn mount_points_for(fs_root: &Path) -> Vec<PathBuf> {
    match tokio::fs::read_to_string("/proc/mounts").await {
        Ok(content) => {
            let mounts = mount_points_for_device_in(&content, fs_root);
            if mounts.is_empty() {
                vec![fs_root.to_path_buf()]
            } else {
                mounts
            }
        }
        Err(e) => {
            warn!(
                "cannot resolve mount points for {:?} ({}); recovery guidance falls back to the path itself",
                fs_root, e
            );
            vec![fs_root.to_path_buf()]
        }
    }
}

/// Pure phrasing of the umount-cycle recovery guidance (split out for tests).
/// `mount_points` are ALL mount points of the filesystem containing
/// `fs_root` (see [`mount_points_for_device_in`]):
///
/// * any mount is `/` → a full live umount is impossible; recommend a reboot
///   instead of the rejected (or forced-catastrophic) `umount /` (#3053
///   review P1-a);
/// * several mounts → ALL must go, because the cleaner resets only when the
///   LAST mount of the device disappears (#3053 review P2);
/// * exactly one → the plain umount cycle.
fn recovery_guidance_text(mount_points: &[PathBuf], fs_root: &Path) -> String {
    if mount_points.iter().any(|m| m == Path::new("/")) {
        return format!(
            "reboot the host — the btrfs filesystem containing {:?} is mounted at / (mount \
             points: {:?}) and cannot be fully umounted while the system is running",
            fs_root, mount_points
        );
    }
    if mount_points.len() > 1 {
        return format!(
            "stop ws-ckpt, umount ALL of {:?} (every mount point of the same btrfs filesystem, \
             which contains {:?} — the cleaner only resets when the LAST mount goes away), \
             start ws-ckpt",
            mount_points, fs_root
        );
    }
    let target = mount_points
        .first()
        .map(PathBuf::as_path)
        .unwrap_or(fs_root);
    format!(
        "stop ws-ckpt, umount {:?} (the btrfs filesystem containing {:?}), start ws-ckpt",
        target, fs_root
    )
}

/// Operator-facing recovery guidance for pinned backend space: resolves ALL
/// real mount points of the filesystem containing `fs_root` and phrases the
/// umount cycle — a reboot when the root mount is involved, an umount-all
/// list when the device has several mount points.
pub async fn recovery_guidance(fs_root: &Path) -> String {
    let mount_points = mount_points_for(fs_root).await;
    recovery_guidance_text(&mount_points, fs_root)
}

/// Delete a subvolume, kicking the kernel cleaner synchronously when the
/// backend is nearly full (`risk == High`).
///
/// High-risk path: resolve the subvolume id BEFORE deletion (the path is
/// unresolvable afterwards), delete, commit the transaction to KICK the
/// cleaner, wait for the drop via a bounded `btrfs subvolume sync`, then
/// commit again to publish the freed space. The kick-before-wait ordering is
/// load-bearing: `subvolume sync` only polls (see [`sync_subvolume_deletes`]),
/// so waiting first would burn the entire timeout while a stalled cleaner —
/// exactly the regime this guard exists for — stays asleep (#3053).
///
/// A cleaner that still cannot reclaim (true ENOSPC) keeps the delete's own
/// success semantics — the subvolume IS deleted from the namespace — but
/// logs an explicit WARN with the umount-cycle recovery guidance instead of
/// failing silently, because the space may stay pinned.
pub async fn delete_subvolume_with_risk(
    path: &Path,
    fs_root: &Path,
    risk: SpaceRisk,
) -> Result<()> {
    if risk == SpaceRisk::Low {
        return delete_subvolume(path).await;
    }

    // Best-effort id capture; deletion proceeds even if rootid fails.
    let id = match get_subvolume_id(path).await {
        Ok(id) => Some(id),
        Err(e) => {
            warn!(
                "guarded delete: cannot resolve subvolume id of {} ({:#}); \
                 will not be able to verify cleaner reclaim",
                path.display(),
                e
            );
            None
        }
    };

    delete_subvolume(path).await?;

    let Some(id) = id else {
        // No id → no bounded wait possible, but the KICK does not depend on
        // the id: a commit is what wakes the cleaner. Skipping it here would
        // reproduce exactly the #3053 failure — delete queued, cleaner never
        // woken — while still returning Ok(()) (review P1-b).
        commit_filesystem(fs_root).await;
        warn!(
            "guarded delete: {} deleted under high backend usage but its subvolume id is unknown; \
             kicked the cleaner with a transaction commit but cannot verify reclaim — watch \
             `btrfs subvolume list -d {}` for zombie subvolumes (#3053)",
            path.display(),
            fs_root.display()
        );
        return Ok(());
    };

    // Kick the cleaner BEFORE waiting: a transaction commit is what wakes it,
    // while `btrfs subvolume sync` only polls for the root id to disappear.
    // Waiting first would burn the whole timeout when the cleaner is stalled
    // — exactly the regime this guard exists for (#3053).
    commit_filesystem(fs_root).await;

    let sync_outcome = sync_subvolume_deletes(fs_root, &[id], DELETE_SYNC_TIMEOUT).await;

    // Commit again so extents the cleaner freed stop sitting pinned: this
    // publishes the reclaimed space to get_usage before we classify the
    // outcome — see commit_filesystem.
    commit_filesystem(fs_root).await;

    // Classify on the authoritative dead list rather than on sync's exit
    // status: sync can time out or fail after the cleaner already drained the
    // subvolume, and (pathologically) report success while the id lingers.
    let still_pending = list_deleted_subvolumes(fs_root)
        .await
        .map(|ids| ids.contains(&id))
        .unwrap_or(true);
    if still_pending {
        let sync_note = match &sync_outcome {
            Ok(()) => "sync reported success but the id is still on the dead list".to_string(),
            Err(e) => format!("sync did not complete: {:#}", e),
        };
        warn!(
            "guarded delete: subvolume {} ({}) deleted but the cleaner could not reclaim it \
             within {}s ({}). It is now a DELETED zombie pinning backend space; daemon restarts \
             will NOT free it (mount is reused by design). Manual recovery: {} — the cleaner \
             drains zombies within minutes of a fresh mount cycle (#3053)",
            id,
            path.display(),
            DELETE_SYNC_TIMEOUT.as_secs(),
            sync_note,
            recovery_guidance(fs_root).await
        );
    } else {
        match &sync_outcome {
            Ok(()) => info!(
                "guarded delete: cleaner reclaimed subvolume {} ({}) synchronously",
                id,
                path.display()
            ),
            Err(e) => info!(
                "guarded delete: cleaner reclaimed subvolume {} ({}) after sync reported: {:#}",
                id,
                path.display(),
                e
            ),
        }
    }
    Ok(())
}

/// Convenience wrapper for single-delete callers: assess space risk, then
/// delete with the guard. Batch callers must use [`delete_subvolumes_guarded`]
/// instead — per-item guarding would pay the bounded cleaner wait once per
/// item (#3053 review P1-c).
pub async fn delete_subvolume_space_aware(path: &Path, fs_root: &Path) -> Result<()> {
    let risk = assess_space_risk(fs_root).await;
    delete_subvolume_with_risk(path, fs_root, risk).await
}

/// Unguarded (Low-risk) single delete mapped onto the batch outcome type:
/// a missing path is [`SnapshotDeleteOutcome::NotFound`], a failed delete is
/// `Failed`, success is `Removed`.
pub async fn delete_subvolume_outcome(path: &Path) -> SnapshotDeleteOutcome {
    match path_exists_fallible(path).await {
        Ok(false) => return SnapshotDeleteOutcome::NotFound,
        Ok(true) => {}
        Err(e) => return SnapshotDeleteOutcome::Failed(format!("{:#}", e)),
    }
    match delete_subvolume(path).await {
        Ok(()) => SnapshotDeleteOutcome::Removed,
        Err(e) => SnapshotDeleteOutcome::Failed(format!("{:#}", e)),
    }
}

/// Batch guarded delete for High-risk callers (#3053 review P1-c): per-item
/// rootid capture + delete, then ONE kick commit, ONE multi-id bounded
/// `btrfs subvolume sync`, and ONE visibility commit for the WHOLE batch.
///
/// Why batch: the production cleanup chain hands the backend a batch of
/// snapshots to delete; guarding each item individually would serialize
/// (2 commit timeouts + 1 sync timeout) PER snapshot — ~90s worst case each,
/// i.e. ~30 minutes for a 20-snapshot pass on a stalled cleaner. `btrfs
/// subvolume sync` natively waits on multiple ids, so one bounded wait
/// covers the batch and the worst-case added latency becomes constant in
/// batch size.
///
/// Returns one outcome per input path, aligned by index. A subvolume the
/// cleaner could not drain in time is still `Removed` (it IS deleted from
/// the namespace) — the stall is surfaced as a single summary zombie WARN
/// with recovery guidance, matching the per-item guarded delete's
/// semantics.
///
/// Trade-off vs per-item guarding: at TRUE ENOSPC (100%, not merely past
/// the guard threshold) a later delete in the batch can fail its metadata
/// reservation because earlier drops have not freed space yet — the old
/// per-item sync+commit sequence would have made room first. Such items are
/// reported per-item as `Failed` (never silently), callers roll back their
/// index detaches, and the next cleanup pass retries once the shared wait
/// has freed space. Bounded latency in the common >=95% regime outweighs
/// this edge.
pub async fn delete_subvolumes_guarded(
    paths: &[PathBuf],
    fs_root: &Path,
) -> Vec<SnapshotDeleteOutcome> {
    enum ItemState {
        Done(SnapshotDeleteOutcome),
        /// Deleted with a known id; the shared wait below classifies it.
        Pending,
    }

    let mut states: Vec<ItemState> = Vec::with_capacity(paths.len());
    let mut pending: Vec<u64> = Vec::new();
    let mut deleted_without_id = false;

    // Phase 1: per-item rootid capture (before delete — unresolvable
    // afterwards) + delete. Deleting is metadata-cheap; the expensive part
    // (cleaner drop) is deferred to the single shared wait below.
    for path in paths {
        match path_exists_fallible(path).await {
            Ok(false) => {
                states.push(ItemState::Done(SnapshotDeleteOutcome::NotFound));
                continue;
            }
            Ok(true) => {}
            Err(e) => {
                states.push(ItemState::Done(SnapshotDeleteOutcome::Failed(format!(
                    "{:#}",
                    e
                ))));
                continue;
            }
        }
        let id = match get_subvolume_id(path).await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!(
                    "guarded batch delete: cannot resolve subvolume id of {} ({:#}); \
                     will kick the cleaner but cannot verify its reclaim",
                    path.display(),
                    e
                );
                None
            }
        };
        if let Err(e) = delete_subvolume(path).await {
            states.push(ItemState::Done(SnapshotDeleteOutcome::Failed(format!(
                "{:#}",
                e
            ))));
            continue;
        }
        match id {
            Some(id) => {
                states.push(ItemState::Pending);
                pending.push(id);
            }
            None => {
                deleted_without_id = true;
                states.push(ItemState::Done(SnapshotDeleteOutcome::Removed));
            }
        }
    }

    // Phase 2: ONE kick for the whole batch — and the kick runs even when
    // every rootid capture failed (pending empty): a queued delete must
    // never be left without a woken cleaner, the same rule as the
    // single-delete guarded path (review P1-b). Kick-before-wait:
    // `subvolume sync` only polls; the commit is what wakes the cleaner
    // (see module header, #3053).
    if !pending.is_empty() || deleted_without_id {
        commit_filesystem(fs_root).await;
    }

    if !pending.is_empty() {
        // ONE bounded multi-id wait + ONE visibility commit for the batch.
        let sync_outcome =
            sync_subvolume_deletes(fs_root, &pending, BATCH_DELETE_SYNC_TIMEOUT).await;
        commit_filesystem(fs_root).await;

        // Phase 3: classify per id on the authoritative dead list — sync's
        // exit status is batch-wide, individual ids may have drained even
        // when it timed out. A failed re-check assumes the worst (still
        // pending) so a zombie is never reported as reclaimed.
        let still_dead = match list_deleted_subvolumes(fs_root).await {
            Ok(ids) => ids,
            Err(e) => {
                warn!(
                    "guarded batch delete: cannot re-check the dead list ({:#}); \
                     assuming the batch is still pending",
                    e
                );
                pending.clone()
            }
        };
        let zombies: Vec<u64> = pending
            .iter()
            .copied()
            .filter(|id| still_dead.contains(id))
            .collect();
        if zombies.is_empty() {
            info!(
                "guarded batch delete: cleaner reclaimed all {} subvolume(s) synchronously",
                pending.len()
            );
        } else {
            let sync_note = match &sync_outcome {
                Ok(()) => "sync reported success but ids remain on the dead list".to_string(),
                Err(e) => format!("sync did not complete: {:#}", e),
            };
            warn!(
                "guarded batch delete: {} of {} subvolume(s) {:?} deleted but NOT reclaimed by \
                 the cleaner within {}s ({}). They are DELETED zombies pinning backend space; \
                 daemon restarts will NOT free them (mount is reused by design). Manual \
                 recovery: {} — the cleaner drains zombies within minutes of a fresh mount \
                 cycle (#3053)",
                zombies.len(),
                pending.len(),
                zombies,
                BATCH_DELETE_SYNC_TIMEOUT.as_secs(),
                sync_note,
                recovery_guidance(fs_root).await
            );
        }
    }

    states
        .into_iter()
        .map(|s| match s {
            ItemState::Done(outcome) => outcome,
            ItemState::Pending => SnapshotDeleteOutcome::Removed,
        })
        .collect()
}

async fn delete_subvolumes_space_aware(
    paths: &[PathBuf],
    fs_root: &Path,
) -> Vec<SnapshotDeleteOutcome> {
    if assess_space_risk(fs_root).await == SpaceRisk::High {
        delete_subvolumes_guarded(paths, fs_root).await
    } else {
        let mut outcomes = Vec::with_capacity(paths.len());
        for path in paths {
            outcomes.push(delete_subvolume_outcome(path).await);
        }
        outcomes
    }
}

async fn delete_recovery_batch(paths: &[PathBuf], fs_root: &Path) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }

    let outcomes = delete_subvolumes_space_aware(paths, fs_root).await;
    let retry: Vec<(PathBuf, String)> = paths
        .iter()
        .zip(outcomes)
        .filter_map(|(path, outcome)| match outcome {
            SnapshotDeleteOutcome::Failed(error) => Some((path.clone(), error)),
            SnapshotDeleteOutcome::Removed | SnapshotDeleteOutcome::NotFound => None,
        })
        .collect();
    if retry.is_empty() {
        return Vec::new();
    }

    for (path, error) in &retry {
        warn!(
            "failed to delete subvolume {:?} during workspace recovery: {}; retrying",
            path, error
        );
    }
    let retry_paths: Vec<PathBuf> = retry.iter().map(|(path, _)| path.clone()).collect();
    retry
        .into_iter()
        .zip(delete_subvolumes_space_aware(&retry_paths, fs_root).await)
        .filter_map(|((path, initial_error), outcome)| match outcome {
            SnapshotDeleteOutcome::Failed(retry_error) => Some(format!(
                "{}: initial error: {}; retry error: {}",
                path.display(),
                initial_error,
                retry_error
            )),
            SnapshotDeleteOutcome::Removed | SnapshotDeleteOutcome::NotFound => None,
        })
        .collect()
}

async fn recovery_snapshot_paths(snapshot_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match tokio::fs::read_dir(snapshot_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to open snapshot directory {}",
                    snapshot_dir.display()
                )
            });
        }
    };

    let mut snapshots = Vec::new();
    while let Some(entry) = entries.next_entry().await.with_context(|| {
        format!(
            "failed to scan snapshot directory {}",
            snapshot_dir.display()
        )
    })? {
        let path = entry.path();
        if entry
            .file_type()
            .await
            .with_context(|| format!("failed to inspect snapshot entry {}", path.display()))?
            .is_dir()
        {
            snapshots.push(path);
        }
    }
    Ok(snapshots)
}

pub(super) async fn delete_recovery_subvolumes(
    snapshot_dir: &Path,
    workspace_subvolume: &Path,
    fs_root: &Path,
    original_path: &str,
) -> Result<()> {
    let snapshots = recovery_snapshot_paths(snapshot_dir)
        .await
        .with_context(|| {
            format!(
                "workspace data was restored to {}, but backend teardown could not start; move the \
                 restored directory aside and retry recovery by workspace ID",
                original_path
            )
        })?;

    let snapshot_failures = delete_recovery_batch(&snapshots, fs_root).await;
    if !snapshot_failures.is_empty() {
        bail!(
            "workspace data was restored to {}, but snapshot teardown remains incomplete and the \
             workspace subvolume was preserved: {}; move the restored directory aside and retry \
             recovery by workspace ID",
            original_path,
            snapshot_failures.join("; ")
        );
    }

    let workspace = [workspace_subvolume.to_path_buf()];
    let workspace_failures = delete_recovery_batch(&workspace, fs_root).await;
    if !workspace_failures.is_empty() {
        bail!(
            "workspace data was restored to {}, but backend teardown remains incomplete: {}; \
             move the restored directory aside and retry recovery by workspace ID",
            original_path,
            workspace_failures.join("; ")
        );
    }

    Ok(())
}

/// Shared `cleanup_snapshots` implementation for both btrfs backends:
/// assess backend fullness ONCE, then either batch-guard the whole list
/// (High risk → one kick + one multi-id sync + one commit, #3053 review
/// P1-c) or plain-delete per item (Low risk), reporting one outcome per
/// requested id.
pub async fn cleanup_snapshots_batch(
    snap_dir: &Path,
    fs_root: &Path,
    snapshot_ids: &[String],
) -> Vec<(String, SnapshotDeleteOutcome)> {
    let paths: Vec<PathBuf> = snapshot_ids.iter().map(|id| snap_dir.join(id)).collect();
    let outcomes = delete_subvolumes_space_aware(&paths, fs_root).await;
    snapshot_ids
        .iter()
        .cloned()
        .zip(outcomes)
        .inspect(|(id, outcome)| match outcome {
            SnapshotDeleteOutcome::Removed => info!("cleanup: removed snapshot {}", id),
            SnapshotDeleteOutcome::NotFound => warn!(
                "cleanup: snapshot {} already gone (index/filesystem mismatch)",
                id
            ),
            SnapshotDeleteOutcome::Failed(e) => {
                warn!("cleanup: failed to delete snapshot {}: {}", id, e)
            }
        })
        .collect()
}

/// Bootstrap-time zombie sweep (#3053): detect DELETED subvolumes left by a
/// previous run's cleaner-stalled deletions and kick the cleaner with a
/// transaction commit — the only chance a plain restart gets at waking it,
/// since the daemon reuses the existing mount by design (#2809).
///
/// Never fails. How far the sweep goes is decided by [`ZombieSweepPolicy`]
/// (review P1-a):
///
/// * [`ZombieSweepPolicy::KickThenWait`] (dedicated loop image): every
///   dead-list entry is ws-ckpt's own, so the sweep follows the kick with a
///   bounded `subvolume sync` wait + visibility commit + re-check. Blocks
///   startup at most `wait_timeout` + two commit timeouts.
/// * [`ZombieSweepPolicy::KickOnly`] (shared host partition, possibly `/`):
///   `list -d` is FILESYSTEM-WIDE — entries may be other tools' subvolumes.
///   The sweep still kicks (a commit is fs-global, cheap, and starts the
///   cleaner for everyone's benefit) but does NOT wait: blocking ws-ckpt
///   startup on subvolumes it does not own is not acceptable, and an
///   immediate re-check would race the just-started cleaner anyway. The WARN
///   is explicitly labeled fs-wide, and the periodic health check tracks the
///   count afterwards.
pub async fn sweep_zombie_subvolumes(
    fs_root: &Path,
    policy: ZombieSweepPolicy,
) -> ZombieSweepOutcome {
    let ids = match list_deleted_subvolumes(fs_root).await {
        Ok(ids) if ids.is_empty() => return ZombieSweepOutcome::Clear,
        Ok(ids) => ids,
        Err(e) => {
            warn!(
                "zombie sweep: cannot list deleted subvolumes on {:?}: {:#}",
                fs_root, e
            );
            return ZombieSweepOutcome::Unresolved;
        }
    };

    let ZombieSweepPolicy::KickThenWait { wait_timeout } = policy else {
        warn!(
            "zombie sweep: `subvolume list -d` on {:?} reports {} deleted subvolume(s) {:?} \
             FILESYSTEM-WIDE — this is a SHARED backend, so entries may belong to other tools, \
             not necessarily ws-ckpt. They pin backend space until the kernel cleaner drains \
             them. Kicking the cleaner with a transaction commit; it continues in the \
             background and the periodic health check tracks the count (startup is NOT blocked \
             waiting on subvolumes ws-ckpt does not own). If the count never shrinks, manual \
             recovery: {} (#3053)",
            fs_root,
            ids.len(),
            ids,
            recovery_guidance(fs_root).await
        );
        commit_filesystem(fs_root).await;
        return ZombieSweepOutcome::Unresolved;
    };

    warn!(
        "zombie sweep: {} deleted subvolume(s) {:?} on {:?} are still awaiting cleaner reclaim \
         (they pin backend space); kicking the cleaner with a transaction commit, then waiting \
         via `btrfs subvolume sync` (timeout {}s)",
        ids.len(),
        ids,
        fs_root,
        wait_timeout.as_secs()
    );
    // Kick BEFORE waiting — `subvolume sync` only polls; a commit is what
    // wakes the cleaner (same ordering rationale as delete_subvolume_with_risk,
    // #3053). Without this the wait below races a cleaner that was never
    // started and the post-check almost always cries "STILL not reclaimed"
    // even when draining was seconds away.
    commit_filesystem(fs_root).await;
    if let Err(e) = sync_subvolume_deletes(fs_root, &ids, wait_timeout).await {
        warn!("zombie sweep: subvolume sync did not complete: {:#}", e);
    }
    // Commit the drop so reclaimed space is actually released before the
    // post-check (extents sit pinned until the transaction commits).
    commit_filesystem(fs_root).await;

    match list_deleted_subvolumes(fs_root).await {
        Ok(remaining) if remaining.is_empty() => {
            info!("zombie sweep: cleaner reclaimed all deleted subvolumes; backend space released");
            ZombieSweepOutcome::Clear
        }
        Ok(remaining) => {
            warn!(
                "zombie sweep: {} deleted subvolume(s) {:?} STILL not reclaimed — backend space \
                 stays pinned and restarting the daemon alone will not free it (mount is reused \
                 by design). Manual recovery: {}; the cleaner drains zombies within minutes of a \
                 fresh mount cycle (#3053)",
                remaining.len(),
                remaining,
                recovery_guidance(fs_root).await
            );
            ZombieSweepOutcome::Unresolved
        }
        Err(e) => {
            warn!(
                "zombie sweep: cannot re-check deleted subvolumes on {:?}: {:#}",
                fs_root, e
            );
            ZombieSweepOutcome::Unresolved
        }
    }
}

/// Compute the diff between two btrfs snapshots using `btrfs send --no-data -p`.
///
/// Requires root privileges and a btrfs filesystem.
///
/// Uses `std::process::Command` (blocking) inside `spawn_blocking` to avoid
/// tokio setting the pipe fd to O_NONBLOCK, which causes `btrfs receive --dump`
/// to fail with EAGAIN ("Resource temporarily unavailable").
pub async fn diff_between_snapshots(snap_from: &Path, snap_to: &Path) -> Result<Vec<DiffEntry>> {
    info!(
        "computing diff between {} and {}",
        snap_from.display(),
        snap_to.display()
    );

    let snap_from = snap_from.to_path_buf();
    let snap_to = snap_to.to_path_buf();

    tokio::task::spawn_blocking(move || diff_between_snapshots_blocking(&snap_from, &snap_to))
        .await
        .context("diff task panicked")?
}

/// Diff a snapshot against the live (writable) workspace subvolume.
///
/// Creates a temporary read-only snapshot of `live_subvol` inside `snap_dir`,
/// runs the diff, then removes the temporary snapshot regardless of outcome.
///
/// `fs_root` is the backend filesystem root used for space-risk assessment:
/// the temp snapshot is read-only and shares extents (little space to free),
/// but its drop still rides the kernel cleaner — on a full backend an
/// unguarded delete leaves a `list -d` zombie entry that the bootstrap sweep
/// and health check then report (#3053).
pub async fn diff_against_live(
    snap_from: &Path,
    live_subvol: &Path,
    snap_dir: &Path,
    fs_root: &Path,
) -> Result<Vec<DiffEntry>> {
    use std::hash::{BuildHasher, Hasher, RandomState};

    let h = RandomState::new().build_hasher().finish();
    let tmp_snap = snap_dir.join(format!(".diff-tmp-{:06x}", h & 0xFFFFFF));

    // Clean up stale temp snapshot from a prior crash before creating a new one.
    if path_exists_fallible(&tmp_snap).await? {
        let _ = delete_subvolume_space_aware(&tmp_snap, fs_root).await;
    }

    create_snapshot(live_subvol, &tmp_snap, true)
        .await
        .context("failed to create temporary snapshot of live workspace for diff")?;

    let result = diff_between_snapshots(snap_from, &tmp_snap).await;

    // Reassess after the diff because send/receive activity can materially
    // change free data or metadata space. Cleanup still runs when diff fails.
    if let Err(e) = delete_subvolume_space_aware(&tmp_snap, fs_root).await {
        warn!(error = %e, path = %tmp_snap.display(), "failed to remove temp diff snapshot");
    }

    result
}

/// Blocking implementation of snapshot diff using `btrfs send | btrfs receive --dump`.
fn diff_between_snapshots_blocking(snap_from: &Path, snap_to: &Path) -> Result<Vec<DiffEntry>> {
    use std::process::{Command as StdCommand, Stdio};

    let mut sender = StdCommand::new("btrfs")
        .args(["send", "--no-data", "-p"])
        .arg(snap_from)
        .arg(snap_to)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn btrfs send")?;

    let sender_stdout = sender
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture btrfs send stdout"))?;

    // Take sender's stderr before passing stdout to receiver, so we can
    // read the correct error stream when btrfs send fails.
    let sender_stderr = sender
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture btrfs send stderr"))?;

    // std::process::ChildStdout implements Into<Stdio>, keeping the fd in blocking mode
    let receiver_output = StdCommand::new("btrfs")
        .args(["receive", "--dump"])
        .stdin(sender_stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run btrfs receive --dump")?;

    let sender_status = sender.wait().context("failed to wait for btrfs send")?;

    if !sender_status.success() {
        let mut err_msg = String::new();
        use std::io::Read;
        let _ = std::io::BufReader::new(sender_stderr).read_to_string(&mut err_msg);
        error!("btrfs send failed (exit={}): {}", sender_status, err_msg);
        bail!("btrfs send failed: {}", err_msg.trim());
    }

    if !receiver_output.status.success() {
        let stderr = String::from_utf8_lossy(&receiver_output.stderr);
        error!("btrfs receive --dump failed: {}", stderr);
        bail!("btrfs receive --dump failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&receiver_output.stdout);
    let entries = parse_btrfs_diff_output(&stdout);
    Ok(entries)
}

/// Parse `btrfs receive --dump` output into deduplicated DiffEntry items.
///
/// Phase 1 collects: snapshot prefix, temp→real rename map, link pairs,
/// unlinks. A `link new dest=old` paired with `unlink old` encodes an `mv`
/// (btrfs send emits no `rename` line for cross-snapshot mv).
/// Phase 2 emits entries with precedence dedup (Renamed > Added > Deleted > Modified).
fn parse_btrfs_diff_output(output: &str) -> Vec<DiffEntry> {
    let mut snapshot_prefix = String::new();
    let mut rename_map: HashMap<String, String> = HashMap::new();
    let mut link_pairs: Vec<(String, String)> = Vec::new();
    let mut unlinked: HashSet<String> = HashSet::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("snapshot") {
            if let Some(name) = rest.split_whitespace().next() {
                snapshot_prefix = format!("{}/", name);
            }
        } else if let Some(rest) = line.strip_prefix("rename") {
            if let Some((src, dst)) = parse_dest_pair(rest, &snapshot_prefix) {
                rename_map.insert(src, dst);
            }
        } else if let Some(rest) = line.strip_prefix("link") {
            if let Some((new_real, dest_path)) = parse_dest_pair(rest, &snapshot_prefix) {
                link_pairs.push((new_real, dest_path));
            }
        } else if let Some(rest) = line.strip_prefix("unlink") {
            unlinked.insert(strip_snap_prefix(&first_token(rest), &snapshot_prefix));
        }
    }

    // mv detection: a `link new dest=old` paired with `unlink old` folds into
    // a single Renamed and the matching Deleted is suppressed. Each old path
    // can pair with at most one link — additional links to the same old path
    // fall through to real-hardlink (Added) handling in Phase 2.
    let mut mv_renames: HashMap<String, String> = HashMap::new();
    let mut suppressed_unlinks: HashSet<String> = HashSet::new();
    for (new_real, dest_path) in &link_pairs {
        if unlinked.contains(dest_path) && !suppressed_unlinks.contains(dest_path) {
            mv_renames.insert(new_real.clone(), dest_path.clone());
            suppressed_unlinks.insert(dest_path.clone());
        }
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut entries: Vec<DiffEntry> = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("mkfile") {
            let path = resolve_path(rest, &snapshot_prefix, &rename_map);
            insert_dedup(&mut seen, &mut entries, path, ChangeType::Added, None);
        } else if let Some(rest) = line.strip_prefix("mkdir") {
            let path = resolve_path(rest, &snapshot_prefix, &rename_map);
            insert_dedup(
                &mut seen,
                &mut entries,
                path,
                ChangeType::Added,
                Some("directory".to_string()),
            );
        } else if let Some(rest) = line.strip_prefix("symlink") {
            // First token is the new symlink path (often a temp inode renamed
            // later); `dest=` is the link target string and isn't used.
            let path = resolve_path(rest, &snapshot_prefix, &rename_map);
            insert_dedup(
                &mut seen,
                &mut entries,
                path,
                ChangeType::Added,
                Some("symlink".to_string()),
            );
        } else if let Some(rest) = line.strip_prefix("link") {
            if let Some((new_real, _)) = parse_dest_pair(rest, &snapshot_prefix) {
                if let Some(old) = mv_renames.get(&new_real).cloned() {
                    insert_dedup(
                        &mut seen,
                        &mut entries,
                        new_real.clone(),
                        ChangeType::Renamed,
                        Some(format!("{} → {}", old, new_real)),
                    );
                } else {
                    insert_dedup(
                        &mut seen,
                        &mut entries,
                        new_real,
                        ChangeType::Added,
                        Some("hardlink".to_string()),
                    );
                }
            }
        } else if let Some(rest) = line.strip_prefix("unlink") {
            let path = strip_snap_prefix(&first_token(rest), &snapshot_prefix);
            if !suppressed_unlinks.contains(&path) {
                insert_dedup(&mut seen, &mut entries, path, ChangeType::Deleted, None);
            }
        } else if let Some(rest) = line.strip_prefix("rmdir") {
            let path = strip_snap_prefix(&first_token(rest), &snapshot_prefix);
            insert_dedup(
                &mut seen,
                &mut entries,
                path,
                ChangeType::Deleted,
                Some("directory".to_string()),
            );
        } else if let Some(rest) = line.strip_prefix("rename") {
            // temp→real renames are folded via rename_map; only emit the rest.
            if let Some((src, dst)) = parse_dest_pair(rest, &snapshot_prefix) {
                if !is_btrfs_temp_ref(&src) {
                    insert_dedup(
                        &mut seen,
                        &mut entries,
                        dst.clone(),
                        ChangeType::Renamed,
                        Some(format!("{} → {}", src, dst)),
                    );
                }
            }
        } else if let Some(rest) = line.strip_prefix("update_extent") {
            // `btrfs send --no-data` emits update_extent instead of write.
            let path = resolve_path(rest, &snapshot_prefix, &rename_map);
            insert_dedup(&mut seen, &mut entries, path, ChangeType::Modified, None);
        } else if let Some(rest) = line.strip_prefix("write") {
            let path = strip_snap_prefix(&first_token(rest), &snapshot_prefix);
            insert_dedup(&mut seen, &mut entries, path, ChangeType::Modified, None);
        } else if let Some(rest) = line.strip_prefix("truncate") {
            let path = strip_snap_prefix(&first_token(rest), &snapshot_prefix);
            insert_dedup(&mut seen, &mut entries, path, ChangeType::Modified, None);
        }
        // Skip metadata-only ops: utimes, chown, chmod, set_xattr, remove_xattr, clone.
    }

    entries
}

/// Strip the snapshot prefix from the first token of `rest`, then resolve
/// through `rename_map` (temp → real) when applicable.
fn resolve_path(rest: &str, snapshot_prefix: &str, rename_map: &HashMap<String, String>) -> String {
    let path = strip_snap_prefix(&first_token(rest), snapshot_prefix);
    rename_map.get(&path).cloned().unwrap_or(path)
}

/// Parse a `<src>  dest=<dst>` line tail into `(src, dst)`, both with the
/// snapshot prefix stripped. `dest=` for `link`/mvs may carry a bare relative
/// path (no prefix), which `strip_snap_prefix` no-ops cleanly.
fn parse_dest_pair(rest: &str, snapshot_prefix: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    let dest_pos = rest.find("dest=")?;
    let src = strip_snap_prefix(&first_token(&rest[..dest_pos]), snapshot_prefix);
    let dst = strip_snap_prefix(&first_token(&rest[dest_pos + 5..]), snapshot_prefix);
    Some((src, dst))
}

/// Insert a DiffEntry, dedup'd by path. Higher-precedence change_type wins
/// on conflict (see `change_precedence`).
fn insert_dedup(
    seen: &mut HashMap<String, usize>,
    entries: &mut Vec<DiffEntry>,
    path: String,
    change_type: ChangeType,
    detail: Option<String>,
) {
    if path.is_empty() {
        return;
    }
    if let Some(&idx) = seen.get(&path) {
        if change_precedence(&change_type) > change_precedence(&entries[idx].change_type) {
            // Replace both fields together: keeping the old `detail` (e.g.
            // `"directory"` from a prior `rmdir`) when a `mkfile` reuses the
            // path leaks misleading metadata into the new entry.
            entries[idx].change_type = change_type;
            entries[idx].detail = detail;
        }
    } else {
        seen.insert(path.clone(), entries.len());
        entries.push(DiffEntry {
            path,
            change_type,
            detail,
        });
    }
}

/// Renamed > Added > Deleted > Modified.
fn change_precedence(c: &ChangeType) -> u8 {
    match c {
        ChangeType::Renamed => 4,
        ChangeType::Added => 3,
        ChangeType::Deleted => 2,
        ChangeType::Modified => 1,
    }
}

/// Extract the first whitespace-delimited token from a string.
fn first_token(s: &str) -> String {
    s.split_whitespace().next().unwrap_or("").to_string()
}

/// Strip the snapshot name prefix (e.g. `./msg1-step1/`) from a path.
fn strip_snap_prefix(path: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return path.to_string();
    }
    path.strip_prefix(prefix).unwrap_or(path).to_string()
}

/// Check whether a path's filename is a btrfs internal temporary inode
/// reference (e.g. `o261-118-0` from the `btrfs send` stream).
fn is_btrfs_temp_ref(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if !name.starts_with('o') || name.len() < 4 {
        return false;
    }
    let rest = &name[1..];
    let parts: Vec<&str> = rest.splitn(3, '-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Get filesystem usage for the given btrfs mount path.
///
/// Returns (total_bytes, used_bytes). Requires root privileges and a btrfs filesystem.
pub async fn get_filesystem_usage(mount_path: &Path) -> Result<(u64, u64)> {
    let usage = get_filesystem_usage_details(mount_path).await?;
    Ok((usage.total, usage.used))
}

async fn get_filesystem_usage_details(mount_path: &Path) -> Result<FilesystemUsage> {
    // LC_ALL/LANG pinned to C: parsing matches English field names; a
    // localized btrfs-progs output must fail closed into guarded deletion.
    let output = Command::new("btrfs")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["filesystem", "usage", "-b"])
        .arg(mount_path)
        .output()
        .await
        .context("failed to execute btrfs filesystem usage")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("btrfs filesystem usage failed: {}", stderr.trim());
    }

    parse_filesystem_usage_details(&String::from_utf8_lossy(&output.stdout))
}

/// Parse btrfs filesystem usage -b output to extract total and used bytes.
///
/// The public tuple retains nominal `Free (estimated)` behavior for health
/// reporting. Deletion risk separately uses its optional minimum and metadata
/// profile usage from [`parse_filesystem_usage_details`].
#[cfg(test)]
fn parse_filesystem_usage(output: &str) -> Result<(u64, u64)> {
    let usage = parse_filesystem_usage_details(output)?;
    Ok((usage.total, usage.used))
}

fn parse_filesystem_usage_details(output: &str) -> Result<FilesystemUsage> {
    let mut total: Option<u64> = None;
    let mut used: Option<u64> = None;
    let mut free_estimated: Option<u64> = None;
    let mut free_estimated_min: Option<u64> = None;
    let mut metadata_profiles = Vec::new();
    let mut malformed_usage = false;
    let mut malformed_metadata_profile = false;

    for line in output.lines() {
        let line = line.trim();
        // Metadata and mixed profiles can exhaust independently of overall
        // data space, preventing the metadata reservation needed for delete.
        if line.starts_with("Metadata,") || line.starts_with("Data+Metadata,") {
            match parse_profile_usage(line) {
                Some((size, profile_used)) if size > 0 => {
                    metadata_profiles.push((size, profile_used));
                }
                _ => malformed_metadata_profile = true,
            }
        } else if line.starts_with("Device size") {
            // Handle both "Device size:" and "Device size (approx):"
            // variants across different btrfs-progs versions.
            if let Some(val) = extract_last_numeric(line) {
                total = Some(val);
            }
        } else if line.starts_with("Used:") || line.starts_with("Used (approx):") {
            if let Some(val) = extract_last_numeric(line) {
                used = Some(val);
            }
        } else if line.starts_with("Free (estimated):") {
            free_estimated = extract_numeric_after_marker(line, "Free (estimated):");
            free_estimated_min = extract_numeric_after_marker(line, "(min:");
            if free_estimated.is_none() || (line.contains("(min:") && free_estimated_min.is_none())
            {
                malformed_usage = true;
            }
        }
    }

    let (total, used) = match (total, free_estimated, used) {
        (Some(t), Some(f), _) => (t, t.saturating_sub(f)),
        (Some(t), None, Some(u)) => (t, u),
        (None, _, _) => {
            warn!("parse_filesystem_usage: 'Device size' field not found in btrfs output");
            malformed_usage = true;
            (0, used.unwrap_or(0))
        }
        (Some(t), None, None) => {
            warn!("parse_filesystem_usage: neither 'Free (estimated)' nor 'Used' field found in btrfs output");
            malformed_usage = true;
            (t, 0)
        }
    };
    let deletion_used = free_estimated_min
        .map(|free| total.saturating_sub(free))
        .unwrap_or(used)
        .max(used);

    Ok(FilesystemUsage {
        total,
        used,
        deletion_used,
        metadata_profiles,
        malformed_usage,
        malformed_metadata_profile,
    })
}

fn parse_profile_usage(line: &str) -> Option<(u64, u64)> {
    Some((
        extract_numeric_after_marker(line, "Size:")?,
        extract_numeric_after_marker(line, "Used:")?,
    ))
}

fn extract_numeric_after_marker(line: &str, marker: &str) -> Option<u64> {
    let token = line.split_once(marker)?.1.split_whitespace().next()?;
    let numeric = token.trim_end_matches(|c: char| !c.is_ascii_digit());
    if numeric.is_empty() || !numeric.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    numeric.parse().ok()
}

/// Extract the last numeric value from a line, stripping any non-numeric suffix.
fn extract_last_numeric(line: &str) -> Option<u64> {
    line.split_whitespace().last().and_then(|val| {
        val.trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .ok()
    })
}

/// Check whether the given path resides on a btrfs filesystem.
pub async fn is_on_btrfs(path: &Path) -> bool {
    let output = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(path)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => {
            let fs_type = String::from_utf8_lossy(&o.stdout).trim().to_string();
            fs_type == "btrfs"
        }
        _ => false,
    }
}

/// Information about a mounted btrfs partition.
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub device: String,
    pub mount_point: String,
}

/// Find the first available btrfs partition by scanning /proc/mounts.
/// Skips read-only mounts and subvolume mounts (prefers physical /dev/ devices).
/// Returns an error if no writable physical btrfs partition is found.
pub async fn find_available_btrfs_partition() -> Result<MountInfo> {
    let file = File::open("/proc/mounts")
        .await
        .context("Failed to open /proc/mounts")?;
    let mut lines = BufReader::new(file).lines();

    let mut found_ro = false;

    while let Some(line) = lines.next_line().await? {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == "btrfs" {
            // Skip read-only mounts
            if parts.len() >= 4 && parts[3].split(',').any(|opt| opt == "ro") {
                found_ro = true;
                continue;
            }
            // Skip subvolume mounts: prefer physical device partitions (/dev/xxx)
            if !parts[0].starts_with("/dev/") {
                continue;
            }
            // Skip loop devices (created by BtrfsLoop backend)
            if parts[0].starts_with("/dev/loop") {
                continue;
            }
            return Ok(MountInfo {
                device: unescape_proc_mount(parts[0]),
                mount_point: unescape_proc_mount(parts[1]),
            });
        }
    }

    if found_ro {
        bail!("Found btrfs partition(s), but all are read-only")
    } else {
        bail!("No available btrfs partition found in /proc/mounts")
    }
}

/// Warmup snapshot metadata cache to speed up subsequent btrfs operations.
///
/// Traverses the snapshot directory to trigger the kernel to load btrfs metadata
/// into page cache, significantly reducing cold-start latency for rollback
/// (up to 60-70% improvement for large file scenarios).
/// This is a read-only operation; failure does not affect the main flow.
pub async fn warmup_snapshot_metadata(snap_path: &Path) {
    use tokio::process::Command as TokioCommand;
    info!(
        "warming up snapshot metadata cache for: {}",
        snap_path.display()
    );
    let _ = TokioCommand::new("find")
        .arg(snap_path)
        .arg("-type")
        .arg("f")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    static REAL_BTRFS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn lock_real_btrfs_test() -> tokio::sync::MutexGuard<'static, ()> {
        REAL_BTRFS_TEST_LOCK.lock().await
    }

    // NOTE: Real-btrfs tests require root, btrfs-progs, and the same mounted
    // test filesystem. The shared lock also covers the complete lifetime of
    // process-wide PATH shims, so `cargo test -- --ignored` is concurrency-safe.

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn create_and_delete_subvolume() {
        let _test_guard = lock_real_btrfs_test().await;
        let path = PathBuf::from("/mnt/btrfs-workspace/test-subvol-unit");
        // Clean up from prior runs
        let _ = delete_subvolume(&path).await;

        create_subvolume(&path)
            .await
            .expect("create_subvolume failed");
        assert!(path.exists());

        delete_subvolume(&path)
            .await
            .expect("delete_subvolume failed");
        assert!(!path.exists());
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn create_readonly_snapshot() {
        let _test_guard = lock_real_btrfs_test().await;
        let src = PathBuf::from("/mnt/btrfs-workspace/test-snap-src");
        let dst = PathBuf::from("/mnt/btrfs-workspace/test-snap-dst-ro");
        let _ = delete_subvolume(&dst).await;
        let _ = delete_subvolume(&src).await;

        create_subvolume(&src).await.expect("create src subvolume");
        create_snapshot(&src, &dst, true)
            .await
            .expect("create readonly snapshot");
        assert!(dst.exists());

        // Cleanup
        let _ = delete_subvolume(&dst).await;
        let _ = delete_subvolume(&src).await;
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn create_writable_snapshot() {
        let _test_guard = lock_real_btrfs_test().await;
        let src = PathBuf::from("/mnt/btrfs-workspace/test-snap-src-w");
        let dst = PathBuf::from("/mnt/btrfs-workspace/test-snap-dst-rw");
        let _ = delete_subvolume(&dst).await;
        let _ = delete_subvolume(&src).await;

        create_subvolume(&src).await.expect("create src subvolume");
        create_snapshot(&src, &dst, false)
            .await
            .expect("create writable snapshot");
        assert!(dst.exists());

        // Cleanup
        let _ = delete_subvolume(&dst).await;
        let _ = delete_subvolume(&src).await;
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn diff_between_two_snapshots() {
        let _test_guard = lock_real_btrfs_test().await;
        let src = PathBuf::from("/mnt/btrfs-workspace/test-diff-src");
        let snap1 = PathBuf::from("/mnt/btrfs-workspace/test-diff-snap1");
        let snap2 = PathBuf::from("/mnt/btrfs-workspace/test-diff-snap2");
        // Cleanup prior
        let _ = delete_subvolume(&snap2).await;
        let _ = delete_subvolume(&snap1).await;
        let _ = delete_subvolume(&src).await;

        create_subvolume(&src).await.unwrap();
        create_snapshot(&src, &snap1, true).await.unwrap();
        // Modify src
        tokio::fs::write(src.join("newfile.txt"), "hello")
            .await
            .unwrap();
        create_snapshot(&src, &snap2, true).await.unwrap();

        let entries = diff_between_snapshots(&snap1, &snap2).await.unwrap();
        assert!(!entries.is_empty());

        // Cleanup
        let _ = delete_subvolume(&snap2).await;
        let _ = delete_subvolume(&snap1).await;
        let _ = delete_subvolume(&src).await;
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn diff_against_live_workspace() {
        let _test_guard = lock_real_btrfs_test().await;
        let base = PathBuf::from("/mnt/btrfs-workspace");
        let src = base.join("test-diff-live-src");
        let snap1 = base.join("test-diff-live-snap1");
        // Cleanup prior
        let _ = delete_subvolume(&snap1).await;
        let _ = delete_subvolume(&src).await;

        create_subvolume(&src).await.unwrap();
        create_snapshot(&src, &snap1, true).await.unwrap();
        // Modify the live subvolume after snapshot
        tokio::fs::write(src.join("live-change.txt"), "world")
            .await
            .unwrap();

        let entries = diff_against_live(&snap1, &src, &base, &base).await.unwrap();
        assert!(!entries.is_empty());

        // Cleanup
        let _ = delete_subvolume(&snap1).await;
        let _ = delete_subvolume(&src).await;
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn get_fs_usage() {
        let _test_guard = lock_real_btrfs_test().await;
        let (total, used) = get_filesystem_usage(Path::new("/mnt/btrfs-workspace"))
            .await
            .unwrap();
        assert!(total > 0);
        assert!(used <= total);
    }

    #[test]
    fn parse_btrfs_diff_output_handles_common_ops() {
        // Use real `btrfs receive --dump` format: rename uses "dest=" syntax
        let output = "snapshot  ./snap  uuid=abc transid=42\nmkfile  ./snap/src/main.rs\nunlink  ./snap/old.txt\nrename  ./snap/old_name  dest=./snap/new_name\nwrite   ./snap/src/lib.rs\nmkdir   ./snap/new_dir\nrmdir   ./snap/old_dir\ntruncate  ./snap/data.bin\nupdate_extent  ./snap/src/config.rs  offset=0 len=128\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 8);
        assert_eq!(entries[0].change_type, ChangeType::Added); // mkfile
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[1].change_type, ChangeType::Deleted); // unlink
        assert_eq!(entries[2].change_type, ChangeType::Renamed); // rename (real rename, not temp)
        assert_eq!(entries[3].change_type, ChangeType::Modified); // write
        assert_eq!(entries[4].change_type, ChangeType::Added); // mkdir
        assert_eq!(entries[5].change_type, ChangeType::Deleted); // rmdir
        assert_eq!(entries[6].change_type, ChangeType::Modified); // truncate
        assert_eq!(entries[7].change_type, ChangeType::Modified); // update_extent
    }

    #[test]
    fn parse_btrfs_diff_output_mapper_resolves_temp_inodes() {
        let output = "snapshot  ./msg1-step1  uuid=abc transid=42\n\
                       mkfile    ./msg1-step1/o261-118-0\n\
                       rename    ./msg1-step1/o261-118-0  dest=./msg1-step1/src/lib.rs\n\
                       update_extent  ./msg1-step1/src/lib.rs  offset=0 len=84\n\
                       utimes    ./msg1-step1/src/lib.rs\n\
                       update_extent  ./msg1-step1/src/main.rs  offset=0 len=50\n\
                       mkfile    ./msg1-step1/o262-119-0\n\
                       rename    ./msg1-step1/o262-119-0  dest=./msg1-step1/.gitignore\n\
                       utimes    ./msg1-step1/\n";
        let entries = parse_btrfs_diff_output(output);

        assert_eq!(entries.len(), 3, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[0].change_type, ChangeType::Added);
        assert_eq!(entries[1].path, "src/main.rs");
        assert_eq!(entries[1].change_type, ChangeType::Modified);
        assert_eq!(entries[2].path, ".gitignore");
        assert_eq!(entries[2].change_type, ChangeType::Added);
    }

    #[test]
    fn parse_btrfs_diff_output_empty() {
        let entries = parse_btrfs_diff_output("");
        assert!(entries.is_empty());
    }

    #[test]
    fn backup_path_for_appends_suffix() {
        assert_eq!(backup_path_for("/tmp/ws"), "/tmp/ws.pre-init-bak");
        assert_eq!(backup_path_for("/tmp/ws/"), "/tmp/ws.pre-init-bak");
    }

    /// Backup restores user data when symlink already replaced original (#673).
    #[tokio::test]
    async fn restore_swaps_symlink_back_to_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let target = tmp.path().join("subvol");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"important")
            .await
            .unwrap();
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::symlink(&target, &orig).await.unwrap();

        restore_original_from_backup(orig.to_str().unwrap()).await;

        assert!(!bak.exists(), "backup should be renamed away");
        assert!(orig.is_dir(), "original must be a real dir again");
        let payload = tokio::fs::read_to_string(orig.join("foo.txt"))
            .await
            .unwrap();
        assert_eq!(payload, "important");
    }

    /// TOCTOU racer: an empty foreign dir appears at original between rename
    /// and symlink. Backup must still restore (#673).
    #[tokio::test]
    async fn restore_clears_empty_racer_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::create_dir(&orig).await.unwrap();

        restore_original_from_backup(orig.to_str().unwrap()).await;

        assert!(!bak.exists());
        assert!(orig.join("foo.txt").exists(), "user data must be back");
    }

    /// Non-empty foreign dir at original must NOT be deleted; backup stays put.
    #[tokio::test]
    async fn restore_preserves_non_empty_foreign_dir_and_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::create_dir(&orig).await.unwrap();
        tokio::fs::write(orig.join("racer.txt"), b"foreign")
            .await
            .unwrap();

        restore_original_from_backup(orig.to_str().unwrap()).await;

        assert!(bak.exists(), "backup must be retained for manual recovery");
        assert!(orig.join("racer.txt").exists());
        assert!(bak.join("foo.txt").exists());
    }

    /// No backup -> noop, must not touch anything else.
    #[tokio::test]
    async fn restore_is_noop_when_backup_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        tokio::fs::create_dir(&orig).await.unwrap();
        tokio::fs::write(orig.join("x"), b"y").await.unwrap();

        restore_original_from_backup(orig.to_str().unwrap()).await;

        assert!(orig.join("x").exists());
    }

    /// Foreign .pre-init-bak must not be restored when backup_owned=false (#673).
    #[tokio::test]
    async fn cleanup_does_not_restore_unowned_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");
        let snap = tmp.path().join("snap");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("attacker.txt"), b"foreign")
            .await
            .unwrap();
        tokio::fs::create_dir(&orig).await.unwrap();
        tokio::fs::write(orig.join("user.txt"), b"real")
            .await
            .unwrap();
        tokio::fs::create_dir(&snap).await.unwrap();

        cleanup_init_storage(orig.to_str().unwrap(), &subvol, &snap, false, tmp.path()).await;

        assert!(orig.join("user.txt").exists(), "user data must remain");
        assert!(
            bak.join("attacker.txt").exists(),
            "foreign backup not restored"
        );
        assert!(!snap.exists(), "snap dir cleaned");
    }

    /// cleanup with backup_owned=false drops a leftover symlink we created in step 6.
    #[tokio::test]
    async fn cleanup_drops_leftover_symlink_when_unowned() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let target = tmp.path().join("subvol");
        let snap = tmp.path().join("snap");

        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::symlink(&target, &orig).await.unwrap();
        tokio::fs::create_dir(&snap).await.unwrap();

        cleanup_init_storage(orig.to_str().unwrap(), &target, &snap, false, tmp.path()).await;

        assert!(!orig.exists(), "leftover symlink dropped");
    }

    /// backup_owned=true restores the backup over original (legit happy path).
    #[tokio::test]
    async fn cleanup_restores_owned_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let target = tmp.path().join("subvol");
        let snap = tmp.path().join("snap");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("user.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::symlink(&target, &orig).await.unwrap();
        tokio::fs::create_dir(&snap).await.unwrap();

        cleanup_init_storage(orig.to_str().unwrap(), &target, &snap, true, tmp.path()).await;

        assert!(orig.is_dir(), "original restored as real dir");
        assert!(orig.join("user.txt").exists(), "user data back at original");
        assert!(!bak.exists(), "backup consumed");
    }

    #[tokio::test]
    async fn delete_outcome_reports_uninspectable_path_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let regular_file = tmp.path().join("regular-file");
        tokio::fs::write(&regular_file, b"not a directory")
            .await
            .unwrap();
        let child = regular_file.join("child");

        let outcome = delete_subvolume_outcome(&child).await;
        assert!(
            matches!(&outcome, SnapshotDeleteOutcome::Failed(_)),
            "ENOTDIR must not be reported as NotFound: {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn guarded_batch_reports_uninspectable_path_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let regular_file = tmp.path().join("regular-file");
        tokio::fs::write(&regular_file, b"not a directory")
            .await
            .unwrap();
        let child = regular_file.join("child");

        let outcomes = delete_subvolumes_guarded(&[child], tmp.path()).await;
        assert!(
            matches!(outcomes.as_slice(), [SnapshotDeleteOutcome::Failed(_)]),
            "ENOTDIR must not be reported as NotFound: {:?}",
            outcomes
        );
    }

    #[tokio::test]
    async fn recovery_snapshot_scan_collects_directories_only() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshot = tmp.path().join("snapshot");
        tokio::fs::create_dir(&snapshot).await.unwrap();
        tokio::fs::write(tmp.path().join("unexpected-file"), b"data")
            .await
            .unwrap();

        assert_eq!(
            recovery_snapshot_paths(tmp.path()).await.unwrap(),
            vec![snapshot]
        );
        assert!(recovery_snapshot_paths(&tmp.path().join("missing"))
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parse_filesystem_usage_parses_output() {
        let output = r#"Overall:
    Device size:                 107374182400
    Device allocated:             10737418240
    Device unallocated:           96636764160
    Used:                          5368709120
"#;
        let (total, used) = parse_filesystem_usage(output).unwrap();
        assert_eq!(total, 107374182400);
        assert_eq!(used, 5368709120);
    }

    #[test]
    fn parse_filesystem_usage_with_free_estimated() {
        let output = r#"Overall:
    Device size:                  53686042624
    Device allocated:              2164260864
    Device unallocated:           51521781760
    Used:                             2121728
    Free (estimated):             52593926144      (min: 26833035264)
    Free (statfs, df):            52592877568
"#;
        let (total, used) = parse_filesystem_usage(output).unwrap();
        assert_eq!(total, 53686042624);
        // used should be total - free_estimated, NOT the raw Used field
        assert_eq!(used, 53686042624 - 52593926144);
        assert_eq!(used, 1092116480);
    }

    #[test]
    fn parse_filesystem_usage_free_estimated_without_min() {
        let output = r#"Overall:
    Device size:                  53686042624
    Device allocated:              2164260864
    Used:                             2121728
    Free (estimated):             52593926144
"#;
        let (total, used) = parse_filesystem_usage(output).unwrap();
        assert_eq!(total, 53686042624);
        assert_eq!(used, 53686042624 - 52593926144);
    }

    #[test]
    fn parse_filesystem_usage_missing_fields() {
        let output = "some random output\n";
        let (total, used) = parse_filesystem_usage(output).unwrap();
        assert_eq!(total, 0);
        assert_eq!(used, 0);
    }

    #[test]
    fn healthy_metadata_profile_keeps_deletion_risk_low() {
        let output = r#"Overall:
    Device size:                 1000
    Used:                         500
Metadata,DUP: Size: 1000, Used: 949
"#;
        let usage = parse_filesystem_usage_details(output).unwrap();
        assert_eq!(usage.metadata_profiles, vec![(1000, 949)]);
        assert_eq!(usage.deletion_risk(), SpaceRisk::Low);
    }

    #[test]
    fn missing_metadata_profile_fails_closed() {
        let output = r#"Overall:
    Device size:                 1000
    Used:                         500
"#;
        let usage = parse_filesystem_usage_details(output).unwrap();
        assert!(usage.metadata_profiles.is_empty());
        assert_eq!(usage.deletion_risk(), SpaceRisk::High);
    }

    #[test]
    fn metadata_profile_at_threshold_is_high_risk() {
        let output = r#"Overall:
    Device size:                 1000
    Used:                         500
Metadata,DUP: Size: 1000, Used: 950
"#;
        let usage = parse_filesystem_usage_details(output).unwrap();
        assert_eq!(usage.deletion_risk(), SpaceRisk::High);
    }

    #[test]
    fn mixed_data_metadata_profile_is_included_in_risk() {
        let output = r#"Overall:
    Device size:                      1000
    Used:                              500
Data+Metadata,single: Size: 2000, Used: 1900
"#;
        let usage = parse_filesystem_usage_details(output).unwrap();
        assert_eq!(usage.metadata_profiles, vec![(2000, 1900)]);
        assert_eq!(usage.deletion_risk(), SpaceRisk::High);
    }

    #[test]
    fn malformed_or_zero_size_metadata_fails_closed() {
        for profile in [
            "Metadata,DUP: Size: invalid, Used: 10",
            "Metadata,DUP: Size: 0, Used: 0",
        ] {
            let output = format!(
                "Overall:\n    Device size: 1000\n    Used: 500\n{}\n",
                profile
            );
            let usage = parse_filesystem_usage_details(&output).unwrap();
            assert!(usage.malformed_metadata_profile, "profile: {}", profile);
            assert_eq!(
                usage.deletion_risk(),
                SpaceRisk::High,
                "profile: {}",
                profile
            );
        }
    }

    #[test]
    fn malformed_overall_usage_fails_closed() {
        for output in [
            "Overall:\n    Device size: 1000\n",
            "Overall:\n    Device size: 1000\n    Used: 100\n    Free (estimated): invalid\n",
            "Overall:\n    Device size: 1000\n    Used: 100\n    Free (estimated): 900 (min: invalid)\n",
        ] {
            let usage = parse_filesystem_usage_details(output).unwrap();
            assert!(usage.malformed_usage, "output: {}", output);
            assert_eq!(usage.deletion_risk(), SpaceRisk::High, "output: {}", output);
        }
    }

    #[test]
    fn deletion_risk_uses_conservative_free_estimated_minimum() {
        let output = r#"Overall:
    Device size:                 1000
    Used:                         100
    Free (estimated):             100      (min: 40)
"#;
        let usage = parse_filesystem_usage_details(output).unwrap();
        assert_eq!((usage.total, usage.used), (1000, 900));
        assert_eq!(usage.deletion_used, 960);
        assert_eq!(usage.deletion_risk(), SpaceRisk::High);
    }

    #[test]
    fn parse_btrfs_diff_output_unknown_ops_are_skipped() {
        let output = "mkfile  new.txt\nchown  foo.txt\nxattr  bar.txt\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].change_type, ChangeType::Added);
    }

    // mkfile temp + rename temp→foo.txt + update_extent foo.txt → Added wins.
    #[test]
    fn parse_btrfs_diff_output_added_file_with_temp_rename() {
        let output = "snapshot  ./snap_a_ro  uuid=abc transid=1\n\
                      mkfile          ./snap_a_ro/o257-34321-0\n\
                      rename          ./snap_a_ro/o257-34321-0  dest=./snap_a_ro/foo.txt\n\
                      update_extent   ./snap_a_ro/foo.txt  offset=0 len=6\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "foo.txt");
        assert_eq!(entries[0].change_type, ChangeType::Added);
    }

    // symlink temp + rename temp→mylink → Added(mylink, "symlink").
    #[test]
    fn parse_btrfs_diff_output_symlink_with_temp_rename() {
        let output = "snapshot  ./snap_a_ro  uuid=abc transid=1\n\
                      symlink         ./snap_a_ro/o258-34321-0  dest=/etc/passwd\n\
                      rename          ./snap_a_ro/o258-34321-0  dest=./snap_a_ro/mylink\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "mylink");
        assert_eq!(entries[0].change_type, ChangeType::Added);
        assert_eq!(entries[0].detail.as_deref(), Some("symlink"));
    }

    // link new dest=existing where existing is NOT unlinked → real hardlink.
    #[test]
    fn parse_btrfs_diff_output_real_hardlink_emits_added() {
        let output = "snapshot  ./snap_a_ro  uuid=abc transid=1\n\
                      mkfile          ./snap_a_ro/o259-34321-0\n\
                      rename          ./snap_a_ro/o259-34321-0  dest=./snap_a_ro/target.txt\n\
                      link            ./snap_a_ro/hardlink_to_target  dest=target.txt\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 2, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "target.txt");
        assert_eq!(entries[0].change_type, ChangeType::Added);
        assert_eq!(entries[1].path, "hardlink_to_target");
        assert_eq!(entries[1].change_type, ChangeType::Added);
        assert_eq!(entries[1].detail.as_deref(), Some("hardlink"));
    }

    // mv foo.txt → bar.txt: link bar dest=foo + unlink foo → single Renamed,
    // Deleted(foo) suppressed.
    #[test]
    fn parse_btrfs_diff_output_mv_emits_renamed_and_drops_deleted() {
        let output = "snapshot  ./snap_b_ro  uuid=abc transid=2\n\
                      link            ./snap_b_ro/bar.txt  dest=foo.txt\n\
                      unlink          ./snap_b_ro/foo.txt\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "bar.txt");
        assert_eq!(entries[0].change_type, ChangeType::Renamed);
        assert_eq!(entries[0].detail.as_deref(), Some("foo.txt → bar.txt"));
    }

    // rmdir foo + mkfile foo: Added wins over Deleted, and the old "directory"
    // detail must NOT leak into the new file entry.
    #[test]
    fn parse_btrfs_diff_output_replace_clears_stale_detail() {
        let output = "snapshot  ./snap  uuid=abc transid=1\n\
                      rmdir   ./snap/foo\n\
                      mkfile  ./snap/o100-1-0\n\
                      rename  ./snap/o100-1-0  dest=./snap/foo\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "foo");
        assert_eq!(entries[0].change_type, ChangeType::Added);
        assert_eq!(entries[0].detail, None, "stale 'directory' detail leaked");
    }

    // Two `link X dest=foo` plus one `unlink foo`: only the first link is
    // treated as the mv rename; the second is a real hardlink Added.
    #[test]
    fn parse_btrfs_diff_output_multi_link_to_same_old_path() {
        let output = "snapshot  ./snap  uuid=abc transid=1\n\
                      link    ./snap/bar  dest=foo\n\
                      link    ./snap/baz  dest=foo\n\
                      unlink  ./snap/foo\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 2, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "bar");
        assert_eq!(entries[0].change_type, ChangeType::Renamed);
        assert_eq!(entries[0].detail.as_deref(), Some("foo → bar"));
        assert_eq!(entries[1].path, "baz");
        assert_eq!(entries[1].change_type, ChangeType::Added);
        assert_eq!(entries[1].detail.as_deref(), Some("hardlink"));
    }

    // PB-004: update_extent before mkfile (both resolve to same real path);
    // Added must win over the earlier-seen Modified via precedence dedup.
    #[test]
    fn parse_btrfs_diff_output_added_wins_over_modified_when_extent_first() {
        let output = "snapshot  ./snap  uuid=abc transid=1\n\
                      update_extent   ./snap/foo.txt  offset=0 len=6\n\
                      mkfile          ./snap/o100-1-0\n\
                      rename          ./snap/o100-1-0  dest=./snap/foo.txt\n";
        let entries = parse_btrfs_diff_output(output);
        assert_eq!(entries.len(), 1, "entries: {:?}", entries);
        assert_eq!(entries[0].path, "foo.txt");
        assert_eq!(entries[0].change_type, ChangeType::Added);
    }

    #[test]
    fn parse_filesystem_usage_approx_variant() {
        let output = r#"Overall:
    Device size (approx):        107374182400
    Device allocated:             10737418240
    Device unallocated:           96636764160
    Used (approx):                 5368709120
"#;
        let (total, used) = parse_filesystem_usage(output).unwrap();
        assert_eq!(total, 107374182400);
        assert_eq!(used, 5368709120);
    }

    #[test]
    fn extract_numeric_after_marker_picks_correct_value() {
        assert_eq!(
            extract_numeric_after_marker(
                "Free (estimated):  52593926144      (min: 26833035264)",
                "Free (estimated):"
            ),
            Some(52593926144)
        );
        assert_eq!(
            extract_numeric_after_marker("Free (estimated):  12345", "Free (estimated):"),
            Some(12345)
        );
        assert_eq!(
            extract_numeric_after_marker("no matching field here", "Free (estimated):"),
            None
        );
    }

    // -------------------------------------------------------------------------
    // Tests for recover_orphan_backup
    // -------------------------------------------------------------------------

    /// No orphan backup → noop. Does not touch original_path or subvol_path.
    #[tokio::test]
    async fn recover_orphan_backup_noop_when_no_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let subvol = tmp.path().join("subvol");
        tokio::fs::create_dir(&orig).await.unwrap();
        tokio::fs::write(orig.join("user.txt"), b"keep")
            .await
            .unwrap();

        recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap();

        assert!(orig.join("user.txt").exists(), "original untouched");
        assert!(!subvol.exists(), "subvol untouched");
        assert!(
            !tmp.path().join("ws.pre-init-bak").exists(),
            "no backup created"
        );
    }

    /// Orphan backup + no subvol → restores backup to original_path. (Case 1)
    #[tokio::test]
    async fn recover_orphan_backup_restores_user_data_when_no_subvol() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"important")
            .await
            .unwrap();

        recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap();

        assert!(!bak.exists(), "backup consumed by rename");
        assert!(orig.is_dir(), "original restored as real dir");
        assert!(orig.join("foo.txt").exists(), "user data restored");
    }

    /// Orphan backup + no subvol + stale empty dir at original → removes the
    /// stale dir and restores backup. (Case 1 with fixture-like state.)
    #[tokio::test]
    async fn recover_orphan_backup_clears_stale_empty_dir_at_original() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"keep")
            .await
            .unwrap();
        // Simulate a test fixture's `rm -rf + mkdir -p` leaving an empty dir.
        tokio::fs::create_dir(&orig).await.unwrap();

        recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap();

        assert!(!bak.exists());
        assert!(
            orig.join("foo.txt").exists(),
            "user data restored over empty dir"
        );
    }

    /// Orphan backup + no subvol + stale dangling symlink at original → removes
    /// the symlink and restores backup. (Case 1 with broken symlink.)
    #[tokio::test]
    async fn recover_orphan_backup_clears_dangling_symlink_at_original() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");
        let ghost = tmp.path().join("nonexistent-target");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("foo.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::symlink(&ghost, &orig).await.unwrap(); // dangling symlink

        recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap();

        assert!(!bak.exists());
        assert!(orig.is_dir(), "original is real dir, not symlink");
        assert!(orig.join("foo.txt").exists());
    }

    /// Orphan backup + non-empty dir at original → refuses and preserves user
    /// data in both locations. (Case 1 safety bail.)
    #[tokio::test]
    async fn recover_orphan_backup_refuses_non_empty_dir_at_original() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("from_backup.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::create_dir(&orig).await.unwrap();
        tokio::fs::write(orig.join("racer.txt"), b"foreign")
            .await
            .unwrap();

        let err = recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("non-empty directory"), "got: {}", msg);
        assert!(msg.contains("remove"), "actionable error: {}", msg);

        // Both must be preserved — no data loss.
        assert!(bak.join("from_backup.txt").exists());
        assert!(orig.join("racer.txt").exists());
    }

    /// Orphan backup + subvol exists → bails with actionable error pointing
    /// at `ws-ckpt recover`. Does not touch either path. (Case 2.)
    #[tokio::test]
    async fn recover_orphan_backup_bails_when_subvol_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("ws");
        let bak = tmp.path().join("ws.pre-init-bak");
        let subvol = tmp.path().join("subvol");

        tokio::fs::create_dir(&bak).await.unwrap();
        tokio::fs::write(bak.join("user.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::create_dir(&subvol).await.unwrap();
        tokio::fs::write(subvol.join("migrated.txt"), b"partial")
            .await
            .unwrap();

        let err = recover_orphan_backup(orig.to_str().unwrap(), &subvol)
            .await
            .unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("ws-ckpt recover"), "actionable error: {}", msg);
        assert!(msg.contains("interrupted prior init"), "context: {}", msg);

        // Nothing should be destroyed on ambiguous state.
        assert!(bak.join("user.txt").exists());
        assert!(subvol.join("migrated.txt").exists());
    }

    // ── Zombie subvolume handling on a real filesystem (#3053) ──
    // Same environment requirements as the tests above (root + mounted btrfs
    // at /mnt/btrfs-workspace). Smoke-level: a clean small filesystem cannot
    // reliably reproduce the ENOSPC cleaner stall, so these verify the new
    // code paths execute correctly against real btrfs-progs output.

    /// Poll until the cleaner drains all deleted subvolumes (bounded).
    ///
    /// A plain async delete leaves a TRANSIENT `list -d` entry even on a
    /// healthy fs — the idle cleaner wakes on a ~30s cycle, so draining can
    /// take one or two cycles. Tests must not assert emptiness immediately
    /// (or even within a few seconds) after an unguarded delete. This is
    /// exactly the latency the guarded High-risk path removes by polling with
    /// `btrfs subvolume sync`; its caller's preceding commit kicks the cleaner.
    async fn wait_no_deleted_subvolumes(mount: &Path, attempts: u32) -> bool {
        for _ in 0..attempts {
            match list_deleted_subvolumes(mount).await {
                Ok(ids) if ids.is_empty() => return true,
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Common baseline for the real-fs tests: dead list drained AND usage
    /// back below the guard threshold. Makes the suite order-independent —
    /// a failing test cannot poison the next one's risk assessment.
    async fn wait_clean_baseline(mount: &Path) -> bool {
        if !wait_no_deleted_subvolumes(mount, 600).await {
            return false;
        }
        for _ in 0..600 {
            if assess_space_risk(mount).await == SpaceRisk::Low {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn space_aware_delete_on_real_btrfs() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let path = mount.join("test-guarded-delete");
        let _ = delete_subvolume(&path).await;
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");

        create_subvolume(&path).await.expect("create_subvolume");
        // Fresh small fs → Low risk → plain delete path.
        assert_eq!(assess_space_risk(&mount).await, SpaceRisk::Low);
        delete_subvolume_space_aware(&path, &mount)
            .await
            .expect("space-aware delete failed");
        assert!(!path.exists());
        // Unguarded delete is async and the idle cleaner wakes on a ~30s
        // cycle: allow two cycles for the transient entry to drain. A healthy
        // fs must not retain zombies beyond that.
        assert!(
            wait_no_deleted_subvolumes(&mount, 600).await,
            "cleaner did not drain deleted subvolume within 60s"
        );
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn high_risk_delete_syncs_on_real_btrfs() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let path = mount.join("test-highrisk-delete");
        let _ = delete_subvolume(&path).await;
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");

        create_subvolume(&path).await.expect("create_subvolume");
        // Force the guarded branch: rootid capture + delete + commit kick +
        // bounded subvolume sync + visibility commit.
        delete_subvolume_with_risk(&path, &mount, SpaceRisk::High)
            .await
            .expect("guarded delete failed");
        assert!(!path.exists());
        assert!(list_deleted_subvolumes(&mount).await.unwrap().is_empty());
    }

    /// Everything the full-fs scenario measured, collected before teardown so
    /// asserts run after the dedicated filesystem is destroyed (a failing
    /// assert must never leave mounts/loops behind).
    struct FullFsOutcome {
        pct: f64,
        risk: SpaceRisk,
        victim_id: u64,
        deleted: Result<()>,
        used_before: u64,
        used_after: u64,
        zombies_at_check: Vec<u64>,
        late_drained: bool,
        used_late: u64,
        /// A fill-round dd actually failed — the fs reached the true ENOSPC
        /// edge, where a stalled cleaner is legitimate (contract branch b).
        hit_enospc: bool,
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs-progs + a free loop device"]
    async fn guarded_delete_reclaims_space_on_full_fs() {
        let _test_guard = lock_real_btrfs_test().await;
        // The core #3053 guarantee, mirroring the incident sequence: with the
        // backend at the >=95% guard threshold, a guarded delete (rootid
        // capture + delete + commit-kick + bounded `btrfs subvolume sync`
        // wait + visibility commit) must actually RECLAIM, not just detect.
        // The commit kick is what starts the cleaner (`subvolume sync` only
        // polls), so when the fill stopped at its ~96% target with dd still
        // succeeding — free space remains for the cleaner's metadata — the
        // asserted contract is:
        //
        //   (a) the space is reclaimed synchronously, OR
        //   (c) the space is reclaimed within the bounded drain window.
        //
        // Merely DETECTING a stall (victim still on the dead list) does NOT
        // pass in that regime: after a proper kick, an undrained zombie means
        // the kick-before-wait ordering regressed (review P1). Detection
        // alone is only tolerated on the true-ENOSPC edge (a fill round's dd
        // actually failed), where the cleaner legitimately cannot progress
        // and the WARN-with-recovery-guidance path is the correct outcome:
        //
        //   (b) hit_enospc AND the stall is DETECTED.
        //
        // Runs on a DEDICATED 2GiB loop fs: an ENOSPC-traumatized btrfs can
        // misbehave for subsequent operations, so this test must neither
        // poison nor be poisoned by the shared /mnt/btrfs-workspace.
        //
        // The victim is created BEFORE the fill: on a full backend, rollback
        // deletes a pre-existing subvolume (the old workspace generation);
        // nothing new of substance can be written at that point.
        let img = PathBuf::from("/tmp/ws-ckpt-fullfs-test.img");
        let mnt = PathBuf::from("/mnt/ws-ckpt-fullfs-test");
        let filler = mnt.join("filler");
        let victim = mnt.join("victim");
        let img_str = img.to_string_lossy().to_string();
        let mnt_str = mnt.to_string_lossy().to_string();

        async fn sh(cmd: &str, args: &[&str]) -> Result<()> {
            let out = Command::new(cmd)
                .args(args)
                .output()
                .await
                .with_context(|| format!("spawn {cmd}"))?;
            if !out.status.success() {
                bail!(
                    "{cmd} {:?} failed: {}",
                    args,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(())
        }

        let _ = tokio::fs::remove_file(&img).await;
        tokio::fs::create_dir_all(&mnt).await.expect("mkdir mnt");

        // Scenario body: everything fallible; teardown below runs regardless.
        let mut loop_dev: Option<String> = None;
        let outcome: Result<FullFsOutcome> = async {
            sh("truncate", &["-s", "2G", img_str.as_str()]).await?;
            let out = Command::new("losetup")
                .args(["--find", "--show", img_str.as_str()])
                .output()
                .await
                .context("spawn losetup")?;
            if !out.status.success() {
                bail!(
                    "losetup failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            let dev = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // Register the loop device BEFORE mkfs: if mkfs fails the
            // scenario returns early, and teardown must still `losetup -d` —
            // otherwise the device stays attached to a since-deleted image
            // (leaked loop + anonymous inode pinning the space).
            loop_dev = Some(dev.clone());
            sh("mkfs.btrfs", &["-f", dev.as_str()]).await?;
            sh("mount", &[dev.as_str(), mnt_str.as_str()]).await?;

            // 1. Victim subvolume with real data.
            create_subvolume(&victim).await?;
            let victim_data = format!("of={}", victim.join("data").display());
            sh(
                "dd",
                &["if=/dev/zero", victim_data.as_str(), "bs=1M", "count=20"],
            )
            .await?;

            // 2. Fill in 64MiB rounds up to >=96%, COMMITTING each round:
            //    buffered writes hide behind delayed allocation, leaving
            //    Free (estimated) — and thus the guard decision — stale until
            //    writeback (observed empirically: uncommitted fills read ~17%
            //    while the fs was actually at ENOSPC).
            create_subvolume(&filler).await?;
            let mut pct = 0.0f64;
            let mut hit_enospc = false;
            for round in 0..40u32 {
                commit_filesystem(&mnt).await;
                let (total, used) = get_filesystem_usage(&mnt).await?;
                pct = if total > 0 {
                    used as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                if pct >= 96.0 {
                    break;
                }
                let f = filler.join(format!("f{round}"));
                let of = format!("of={}", f.display());
                let wrote = sh("dd", &["if=/dev/zero", of.as_str(), "bs=1M", "count=64"])
                    .await
                    .is_ok();
                if !wrote {
                    // True-ENOSPC edge: contract branch (b) becomes legal.
                    hit_enospc = true;
                    // Transient ENOSPC from delalloc reservations: commit and
                    // squeeze a smaller block in; a failure here means the fs
                    // is truly at the edge — usage is then past the threshold.
                    commit_filesystem(&mnt).await;
                    let _ = sh("dd", &["if=/dev/zero", of.as_str(), "bs=1M", "count=16"]).await;
                    break;
                }
            }
            commit_filesystem(&mnt).await;

            // 3. The production classifier must see High; then run the guarded
            //    delete and measure what the guard actually achieved.
            let risk = assess_space_risk(&mnt).await;
            let victim_id = get_subvolume_id(&victim).await?;
            let (_t, used_before) = get_filesystem_usage(&mnt).await?;
            let deleted = delete_subvolume_with_risk(&victim, &mnt, SpaceRisk::High).await;
            let (_t, used_after) = get_filesystem_usage(&mnt).await?;
            let zombies_at_check = list_deleted_subvolumes(&mnt).await.unwrap_or_default();

            // 4. Bounded late-drain window (contract branch c), doubling as
            //    pre-umount cleanup so the umount is not stuck dropping data.
            let late_drained = wait_no_deleted_subvolumes(&mnt, 1200).await;
            commit_filesystem(&mnt).await;
            let (_t, used_late) = get_filesystem_usage(&mnt).await?;

            let _ = delete_subvolume(&filler).await;
            let _ = wait_no_deleted_subvolumes(&mnt, 600).await;
            commit_filesystem(&mnt).await;

            Ok(FullFsOutcome {
                pct,
                risk,
                victim_id,
                deleted,
                used_before,
                used_after,
                zombies_at_check,
                late_drained,
                used_late,
                hit_enospc,
            })
        }
        .await;

        // Teardown ALWAYS runs (also on scenario error): umount (bounded —
        // btrfs umount drains pending deletes synchronously and can be slow),
        // detach the loop, drop the image. Nothing may leak into the host.
        let umounted =
            tokio::time::timeout(Duration::from_secs(300), sh("umount", &[mnt_str.as_str()]))
                .await
                .map_err(|_| anyhow::anyhow!("umount timed out"))
                .and_then(|r| r);
        if umounted.is_err() {
            let _ = sh("umount", &["-l", mnt_str.as_str()]).await;
        }
        if let Some(dev) = loop_dev {
            let _ = sh("losetup", &["-d", dev.as_str()]).await;
        }
        let _ = tokio::fs::remove_file(&img).await;
        let _ = tokio::fs::remove_dir(&mnt).await;

        let o = outcome.expect("full-fs scenario setup/execution failed");

        // Contract asserts (after teardown — failures must not leak state).
        assert_eq!(
            o.risk,
            SpaceRisk::High,
            "fill stopped at {:.1}% — expected >=95% guard regime",
            o.pct
        );
        o.deleted.expect("guarded delete on full fs failed");
        let sync_reclaimed = o.used_before.saturating_sub(o.used_after) >= 15 * 1024 * 1024
            && o.zombies_at_check.is_empty();
        let stall_detected = o.zombies_at_check.contains(&o.victim_id);
        let late_reclaimed =
            o.late_drained && o.used_before.saturating_sub(o.used_late) >= 15 * 1024 * 1024;
        if o.hit_enospc {
            // True-ENOSPC edge (a fill dd failed): the cleaner legitimately
            // cannot progress, so DETECTION — the WARN-with-recovery-guidance
            // path — is an acceptable outcome alongside actual reclaim.
            assert!(
                sync_reclaimed || stall_detected || late_reclaimed,
                "guard contract violated at {:.1}% (true ENOSPC): used {} -> {} (late {}), \
                 zombies {:?}, victim id {}, late_drained {} — space was pinned SILENTLY",
                o.pct,
                o.used_before,
                o.used_after,
                o.used_late,
                o.zombies_at_check,
                o.victim_id,
                o.late_drained
            );
        } else {
            // Fill stopped at its target with dd still succeeding: the guard
            // kicked the cleaner via commit BEFORE waiting, so the drop must
            // complete within the bounded windows. A stall_detected-only
            // outcome here means the kick-before-wait ordering regressed —
            // a wait-only sync never starts a stalled cleaner (review P1).
            assert!(
                sync_reclaimed || late_reclaimed,
                "guard failed to RECLAIM at {:.1}% (no true ENOSPC): used {} -> {} (late {}), \
                 zombies {:?}, victim id {}, late_drained {} — cleaner was kicked but the drop \
                 never drained; kick-before-wait ordering regressed?",
                o.pct,
                o.used_before,
                o.used_after,
                o.used_late,
                o.zombies_at_check,
                o.victim_id,
                o.late_drained
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn zombie_sweep_noop_on_clean_fs() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        // Earlier tests' async deletes may still be draining on the cleaner's
        // ~30s cycle; establish a clean baseline before exercising the sweep.
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");
        // Clean fs → sweep must return immediately without errors/panics
        // (both policies short-circuit on an empty dead list).
        sweep_zombie_subvolumes(
            &mount,
            ZombieSweepPolicy::KickThenWait {
                wait_timeout: Duration::from_secs(5),
            },
        )
        .await;
        sweep_zombie_subvolumes(&mount, ZombieSweepPolicy::KickOnly).await;
        assert!(list_deleted_subvolumes(&mount).await.unwrap().is_empty());
    }

    // ── Zombie subvolume parsing & space-risk decision (#3053) ──
    // Pure functions; no root/btrfs required.

    #[test]
    fn parse_deleted_subvolume_ids_real_progs_output() {
        // Without `-a`, btrfs-progs prints unresolved paths as bare DELETED.
        // A true deleted root has top level 0; a live subvolume whose ancestor
        // was deleted retains a non-zero top level and must never enter sync.
        let output = "\
ID 259 gen 40 top level 0 path DELETED
ID 262 gen 43 top level 256 path DELETED
ID 263 gen 44 top level 0 path DELETED
";
        assert_eq!(parse_deleted_subvolume_ids(output).unwrap(), vec![259, 263]);
    }

    #[test]
    fn parse_deleted_subvolume_ids_filters_live_entries_defensively() {
        // `list -d` never emits normal live entries (only_deleted filtering),
        // but the parser stays robust if fed an unfiltered `list` output.
        let output = "\
ID 256 gen 34 top level 5 path <FS_TREE>/ws-ckpt-data
ID 259 gen 40 top level 0 path DELETED
ID 260 gen 41 top level 256 path <FS_TREE>/snapshots/ws-abc/snap-1
";
        assert_eq!(parse_deleted_subvolume_ids(output).unwrap(), vec![259]);
    }

    #[test]
    fn parse_deleted_subvolume_ids_rejects_malformed_nonempty_output() {
        assert!(parse_deleted_subvolume_ids("").unwrap().is_empty());
        for output in [
            "garbage line\n",
            "ID notanumber top level 0 path DELETED\n",
            "ID 259 gen 40 path DELETED\n",
        ] {
            assert!(parse_deleted_subvolume_ids(output).is_err(), "{output:?}");
        }
    }

    #[test]
    fn parse_deleted_subvolume_ids_excludes_live_orphans() {
        let bare = "ID 300 gen 55 top level 256 path DELETED\n";
        assert!(parse_deleted_subvolume_ids(bare).unwrap().is_empty());

        // `-a` is not used, but prefixed output remains harmless if supplied.
        let prefixed = "ID 301 gen 56 top level 256 path <FS_TREE>/DELETED\n";
        assert!(parse_deleted_subvolume_ids(prefixed).unwrap().is_empty());
    }

    #[test]
    fn space_risk_threshold_boundary() {
        let total = 1000;
        // Just below the 95% threshold → Low.
        assert_eq!(space_risk_from_usage(total, 949), SpaceRisk::Low);
        // Exactly at the threshold → High.
        assert_eq!(space_risk_from_usage(total, 950), SpaceRisk::High);
        // Above → High (including the fully-pinned ENOSPC case).
        assert_eq!(space_risk_from_usage(total, 1000), SpaceRisk::High);
    }

    #[test]
    fn space_risk_unknown_capacity_fails_closed() {
        // total==0 means the capacity is UNKNOWN (unreadable/unparsable
        // usage output). Unknown must fail CLOSED (review P1-2): the
        // degraded-probe regime is where ENOSPC zombies breed, and the
        // guarded path can never turn a working delete into a failing one.
        assert_eq!(space_risk_from_usage(0, 0), SpaceRisk::High);
        assert_eq!(space_risk_from_usage(0, 100), SpaceRisk::High);
    }

    #[test]
    fn unparsable_usage_output_fails_closed() {
        // Localized/garbled `btrfs filesystem usage` output degrades to
        // total=0 in the parser — which the classifier must treat as High.
        let (total, used) = parse_filesystem_usage("vollig unerwartete Ausgabe\n").unwrap();
        assert_eq!(total, 0);
        assert_eq!(space_risk_from_usage(total, used), SpaceRisk::High);
    }

    // ── Mount point resolution for recovery guidance (#3053) ──

    #[test]
    fn mount_points_longest_prefix_wins() {
        let mounts = "\
proc /proc proc rw 0 0
/dev/sda1 / ext4 rw 0 0
/dev/loop0 /mnt/btrfs btrfs rw,relatime 0 0
";
        // btrfs-base: data_root is a subdirectory of the real mount.
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/mnt/btrfs/ws-ckpt-data")),
            vec![PathBuf::from("/mnt/btrfs")]
        );
        // Exact mount point resolves to itself (btrfs-loop case).
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/mnt/btrfs")),
            vec![PathBuf::from("/mnt/btrfs")]
        );
        // Falls back to the root fs for paths outside the btrfs mount.
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/var/lib/ws-ckpt")),
            vec![PathBuf::from("/")]
        );
    }

    #[test]
    fn mount_points_collects_whole_device() {
        // Review P2: the same btrfs device mounted at several points (another
        // subvolume, a bind mount) — ALL of them must be reported, because
        // the cleaner only resets when the LAST mount goes away.
        let mounts = "\
/dev/loop0 /mnt/btrfs btrfs rw,relatime 0 0
/dev/loop0 /mnt/btrfs-alt btrfs rw,subvol=other 0 0
/dev/loop0 /opt/bind btrfs rw 0 0
/dev/sda1 / ext4 rw 0 0
";
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/mnt/btrfs/ws-ckpt-data")),
            vec![
                PathBuf::from("/mnt/btrfs"),
                PathBuf::from("/mnt/btrfs-alt"),
                PathBuf::from("/opt/bind"),
            ]
        );
        // A path under the OTHER subvolume mount resolves to the same device set.
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/opt/bind/x")).len(),
            3
        );
        // A different device contributes nothing.
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/etc/hosts")),
            vec![PathBuf::from("/")]
        );
    }

    #[test]
    fn mount_points_no_sibling_prefix_confusion() {
        // "/mnt/btrfs2" must not be treated as containing "/mnt/btrfs2x/...".
        let mounts = "/dev/loop0 /mnt/btrfs2 btrfs rw 0 0\n";
        assert!(mount_points_for_device_in(mounts, Path::new("/mnt/btrfs2x/data")).is_empty());
    }

    #[test]
    fn mount_points_decodes_octal_escapes() {
        // Spaces in mount points are octal-escaped in /proc/mounts.
        let mounts = "/dev/loop0 /mnt/my\\040disk btrfs rw 0 0\n";
        assert_eq!(
            mount_points_for_device_in(mounts, Path::new("/mnt/my disk/ws-ckpt-data")),
            vec![PathBuf::from("/mnt/my disk")]
        );
    }

    #[test]
    fn mount_points_garbage_lines_skipped() {
        assert!(mount_points_for_device_in("", Path::new("/a")).is_empty());
        assert!(mount_points_for_device_in("oneword\n", Path::new("/a")).is_empty());
    }

    // ── Recovery guidance phrasing (#3053 review P1-a / P2) ──

    #[test]
    fn recovery_guidance_single_mount_names_real_mount_point() {
        let text = recovery_guidance_text(
            &[PathBuf::from("/mnt/btrfs")],
            Path::new("/mnt/btrfs/ws-ckpt-data"),
        );
        assert!(text.contains("stop ws-ckpt"), "{}", text);
        assert!(text.contains("umount \"/mnt/btrfs\""), "{}", text);
        assert!(
            text.contains("containing \"/mnt/btrfs/ws-ckpt-data\""),
            "{}",
            text
        );
    }

    #[test]
    fn recovery_guidance_multi_mount_requires_umount_all() {
        // Review P2: naming ONE mount point of a multi-mount device would
        // leave the superblock alive and the cleaner un-reset — the guidance
        // must list every mount point.
        let text = recovery_guidance_text(
            &[PathBuf::from("/mnt/btrfs"), PathBuf::from("/mnt/btrfs-alt")],
            Path::new("/mnt/btrfs/ws-ckpt-data"),
        );
        assert!(text.contains("umount ALL of"), "{}", text);
        assert!(text.contains("/mnt/btrfs-alt"), "{}", text);
        assert!(text.contains("LAST mount"), "{}", text);
    }

    #[test]
    fn recovery_guidance_root_mount_recommends_reboot() {
        // A btrfs-base data root on the host ROOT filesystem cannot be
        // umounted live — guidance must not suggest the impossible (and
        // dangerous) `umount /`.
        let text = recovery_guidance_text(&[PathBuf::from("/")], Path::new("/ws-ckpt-data"));
        assert!(text.contains("reboot the host"), "{}", text);
        assert!(!text.contains("stop ws-ckpt"), "{}", text);
        // Root mount anywhere in the device's mount set still means reboot.
        let text = recovery_guidance_text(
            &[PathBuf::from("/"), PathBuf::from("/mnt/data")],
            Path::new("/ws-ckpt-data"),
        );
        assert!(text.contains("reboot the host"), "{}", text);
    }

    #[test]
    fn recovery_guidance_unresolved_falls_back_to_fs_root() {
        let text = recovery_guidance_text(&[], Path::new("/mnt/btrfs/ws-ckpt-data"));
        assert!(
            text.contains("umount \"/mnt/btrfs/ws-ckpt-data\""),
            "{}",
            text
        );
    }

    // ── Call-structure tests via a PATH-front `btrfs` shim (#3053 review) ──
    //
    // The shim logs every `btrfs` invocation (one line per call, argv joined
    // by spaces) and then delegates to the real binary, so filesystem effects
    // stay real while assertions check the call structure. Every ignored
    // real-btrfs test holds REAL_BTRFS_TEST_LOCK for its full async lifetime;
    // PATH is restored on Drop, including on panic.

    struct BtrfsShim {
        _dir: tempfile::TempDir,
        log: PathBuf,
        saved_path: std::ffi::OsString,
    }

    impl BtrfsShim {
        /// Install the shim. With `fail_rootid_for`, ONLY
        /// `inspect-internal rootid <that exact path>` fails (exit 1); with
        /// `fail_usage`, every `filesystem usage` call fails. Everything else
        /// delegates to the real binary.
        fn install(fail_rootid_for: Option<&Path>, fail_usage: bool) -> BtrfsShim {
            let which = std::process::Command::new("which")
                .arg("btrfs")
                .output()
                .expect("spawn which");
            assert!(which.status.success(), "btrfs binary not found on PATH");
            let real = String::from_utf8_lossy(&which.stdout).trim().to_string();

            let dir = tempfile::tempdir().expect("tempdir");
            let log = dir.path().join("calls.log");
            let fail_target = fail_rootid_for
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let fail_usage_flag = if fail_usage { "1" } else { "" };
            let script = format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"$*\" >> '{log}'\n",
                    "if [ \"$1\" = 'inspect-internal' ] && [ \"$2\" = 'rootid' ] \\\n",
                    "   && [ -n '{fail}' ] && [ \"$3\" = '{fail}' ]; then\n",
                    "  echo 'shim: injected rootid failure' >&2\n",
                    "  exit 1\n",
                    "fi\n",
                    "if [ \"$1\" = 'filesystem' ] && [ \"$2\" = 'usage' ] \\\n",
                    "   && [ '{fail_usage}' = '1' ]; then\n",
                    "  echo 'shim: injected usage failure' >&2\n",
                    "  exit 1\n",
                    "fi\n",
                    "exec '{real}' \"$@\"\n",
                ),
                log = log.display(),
                fail = fail_target,
                fail_usage = fail_usage_flag,
                real = real
            );
            let shim = dir.path().join("btrfs");
            std::fs::write(&shim, script).expect("write shim");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");

            let saved_path = std::env::var_os("PATH").unwrap_or_default();
            let new_path = format!(
                "{}:{}",
                dir.path().display(),
                std::env::var("PATH").unwrap_or_default()
            );
            std::env::set_var("PATH", new_path);
            BtrfsShim {
                _dir: dir,
                log,
                saved_path,
            }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// Number of logged calls whose argv contains `needle`.
        fn count_containing(&self, needle: &str) -> usize {
            self.calls()
                .iter()
                .filter(|line| line.contains(needle))
                .count()
        }
    }

    impl Drop for BtrfsShim {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.saved_path);
        }
    }

    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn diff_reassesses_space_after_diff_commands() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let src = mount.join("test-diff-risk-src");
        let snap = mount.join("test-diff-risk-snap");
        let _ = delete_subvolume(&snap).await;
        let _ = delete_subvolume(&src).await;
        create_subvolume(&src).await.expect("create source");
        create_snapshot(&src, &snap, true)
            .await
            .expect("create base snapshot");
        tokio::fs::write(src.join("change"), b"changed")
            .await
            .expect("write change");

        let shim = BtrfsShim::install(None, false);
        diff_against_live(&snap, &src, &mount, &mount)
            .await
            .expect("diff against live");
        let calls = shim.calls();
        let last_diff_call = calls
            .iter()
            .rposition(|call| call.starts_with("send ") || call.starts_with("receive "))
            .expect("diff command not logged");
        let usage_call = calls
            .iter()
            .rposition(|call| call.starts_with("filesystem usage "))
            .expect("space assessment not logged");
        assert!(
            usage_call > last_diff_call,
            "final cleanup must reassess after diff; calls: {:?}",
            calls
        );

        drop(shim);
        let _ = delete_subvolume(&snap).await;
        let _ = delete_subvolume(&src).await;
    }

    /// Review P1-b: when the rootid capture fails but the delete succeeds,
    /// the cleaner KICK (a transaction commit) must still run — skipping it
    /// would reproduce exactly the #3053 failure (delete queued, cleaner
    /// never woken) while returning Ok(()).
    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn guarded_delete_kicks_cleaner_when_rootid_capture_fails() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let path = mount.join("test-rootid-fail-delete");
        let _ = delete_subvolume(&path).await;
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");
        create_subvolume(&path).await.expect("create_subvolume");

        let shim = BtrfsShim::install(Some(&path), false);
        let commits_before = shim.count_containing("filesystem sync");
        let syncs_before = shim.count_containing("subvolume sync");

        delete_subvolume_with_risk(&path, &mount, SpaceRisk::High)
            .await
            .expect("guarded delete must succeed despite the rootid failure");
        assert!(!path.exists());
        assert!(
            shim.count_containing("filesystem sync") > commits_before,
            "cleaner kick (filesystem sync) must run even without a subvolume id"
        );
        assert_eq!(
            shim.count_containing("subvolume sync"),
            syncs_before,
            "without an id there is nothing to wait on — sync must not run"
        );
        drop(shim);
        assert!(wait_clean_baseline(&mount).await, "baseline not restored");
    }

    /// Review P1-2: a FAILED usage probe must fail CLOSED — the delete goes
    /// through the guarded path (cleaner kick observed) instead of the plain
    /// async delete that breeds zombies in exactly this degraded regime.
    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn unknown_usage_fails_closed_into_guarded_path() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let path = mount.join("test-failclosed-delete");
        let _ = delete_subvolume(&path).await;
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");
        create_subvolume(&path).await.expect("create_subvolume");

        let shim = BtrfsShim::install(None, true);
        // The injected `filesystem usage` failure must classify as High...
        assert_eq!(assess_space_risk(&mount).await, SpaceRisk::High);
        let commits_before = shim.count_containing("filesystem sync");
        // ...and the space-aware delete must then run the guarded (kick) path.
        delete_subvolume_space_aware(&path, &mount)
            .await
            .expect("space-aware delete failed");
        assert!(!path.exists());
        assert!(
            shim.count_containing("filesystem sync") > commits_before,
            "guarded path (cleaner kick) must run when the usage probe fails"
        );
        drop(shim);
        assert!(wait_clean_baseline(&mount).await, "baseline not restored");
    }

    /// Review P1-c: a guarded batch pays ONE kick + ONE multi-id bounded wait
    /// + ONE visibility commit for the WHOLE batch — the worst-case added
    /// latency must be constant in batch size, not per item.
    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn batch_guarded_delete_uses_single_sync_wait() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| mount.join(format!("test-batch-delete-{}", i)))
            .collect();
        for p in &paths {
            let _ = delete_subvolume(p).await;
        }
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");
        for p in &paths {
            create_subvolume(p).await.expect("create_subvolume");
            // A little data so the drop is real cleaner work.
            tokio::fs::write(p.join("data"), vec![0u8; 1024 * 1024])
                .await
                .expect("write data");
        }

        let shim = BtrfsShim::install(None, false);
        let commits_before = shim.count_containing("filesystem sync");
        let syncs_before = shim.count_containing("subvolume sync");

        let outcomes = delete_subvolumes_guarded(&paths, &mount).await;
        assert!(
            outcomes
                .iter()
                .all(|o| *o == SnapshotDeleteOutcome::Removed),
            "all three must be Removed, got {:?}",
            outcomes
        );
        for p in &paths {
            assert!(!p.exists());
        }
        assert_eq!(
            shim.count_containing("subvolume sync") - syncs_before,
            1,
            "the batch must share ONE bounded subvolume sync wait"
        );
        assert_eq!(
            shim.count_containing("filesystem sync") - commits_before,
            2,
            "exactly one kick commit + one visibility commit for the batch"
        );
        drop(shim);
        assert!(
            list_deleted_subvolumes(&mount).await.unwrap().is_empty(),
            "batch wait must leave no zombies behind"
        );
    }

    /// Review P1-a: on a SHARED backend the KickOnly sweep kicks the cleaner
    /// but must NOT block startup waiting on dead-list entries ws-ckpt does
    /// not own — no `subvolume sync` may run and the call must return
    /// promptly.
    #[tokio::test]
    #[ignore = "requires root + btrfs filesystem"]
    async fn shared_sweep_kicks_without_waiting_on_foreign_zombies() {
        let _test_guard = lock_real_btrfs_test().await;
        let mount = PathBuf::from("/mnt/btrfs-workspace");
        let path = mount.join("test-shared-sweep-victim");
        let _ = delete_subvolume(&path).await;
        assert!(wait_clean_baseline(&mount).await, "baseline not clean");

        create_subvolume(&path).await.expect("create_subvolume");
        tokio::fs::write(path.join("data"), vec![0u8; 4 * 1024 * 1024])
            .await
            .expect("write data");
        // Unguarded delete → a transient dead-list entry standing in for a
        // "foreign" deletion on a shared partition. It appears immediately
        // after the delete ioctl; the idle cleaner needs up to a ~30s cycle
        // to drain it, so the observation window is wide.
        delete_subvolume(&path).await.expect("plain delete");
        let mut seen = false;
        for _ in 0..50 {
            if !list_deleted_subvolumes(&mount)
                .await
                .unwrap_or_default()
                .is_empty()
            {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            seen,
            "no dead-list entry appeared after an unguarded delete"
        );

        let shim = BtrfsShim::install(None, false);
        let commits_before = shim.count_containing("filesystem sync");
        let syncs_before = shim.count_containing("subvolume sync");

        // KickOnly has no wait budget at all — the call must return promptly.
        let started = std::time::Instant::now();
        sweep_zombie_subvolumes(&mount, ZombieSweepPolicy::KickOnly).await;
        let elapsed = started.elapsed();

        assert_eq!(
            shim.count_containing("subvolume sync") - syncs_before,
            0,
            "KickOnly sweep must NOT wait via subvolume sync on foreign entries"
        );
        // Race guard: if the (healthy, idle-cycle) cleaner happened to drain
        // the entry between our observation and the sweep's own list, the
        // sweep legitimately short-circuits without a kick.
        let dead_now = list_deleted_subvolumes(&mount).await.unwrap_or_default();
        assert!(
            shim.count_containing("filesystem sync") > commits_before || dead_now.is_empty(),
            "KickOnly sweep must still kick the cleaner with a commit when zombies are present"
        );
        assert!(
            elapsed < Duration::from_secs(60),
            "Shared sweep blocked startup for {:?} — it must be kick-only",
            elapsed
        );
        drop(shim);

        // The kick started the cleaner; the drain happens in the background.
        assert!(
            wait_clean_baseline(&mount).await,
            "cleaner did not drain after the shared-sweep kick"
        );
    }
}
