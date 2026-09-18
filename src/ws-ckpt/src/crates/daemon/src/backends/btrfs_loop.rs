use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use async_trait::async_trait;
use nix::unistd::{chown, Gid, Uid};
use tokio::process::Command;
use tracing::{error, info, warn};

use ws_ckpt_common::backend::*;
use ws_ckpt_common::persist::LoopImgState;
use ws_ckpt_common::{
    DaemonConfig, DiffEntry, WorkspaceGenerationTokenV2, WorkspaceInfo, SNAPSHOTS_DIR,
};

use super::{btrfs_common, btrfs_identity, rollback_recovery};
use crate::util::{is_mounted, run_command, run_command_checked};
use btrfs_common::{backup_path_for, recover_orphan_backup, resolve_symlink_path};

pub struct BtrfsLoopBackend {
    pub mount_path: PathBuf,
    pub img_path: PathBuf,
    pub snapshots_dir: PathBuf,
}

impl BtrfsLoopBackend {
    pub fn new(mount_path: PathBuf, img_path: PathBuf) -> Self {
        let snapshots_dir = mount_path.join(SNAPSHOTS_DIR);
        Self {
            mount_path,
            img_path,
            snapshots_dir,
        }
    }

    async fn recover_interrupted_rollbacks(&self) -> anyhow::Result<()> {
        rollback_recovery::recover_interrupted_rollbacks(&self.mount_path)
            .await
            .context("failed to recover interrupted BtrfsLoop rollback")
    }

    /// Internal init implementation; caller wraps with cleanup-on-failure. Sets `*backup_owned` after step 3.
    async fn do_init_storage(
        &self,
        original_path: &str,
        ws_id: &str,
        subvol_path: &Path,
        snap_dir: &Path,
        backup_owned: &mut bool,
    ) -> anyhow::Result<()> {
        // 1. Create subvolume
        btrfs_common::create_subvolume(subvol_path).await?;

        // 2. Create snapshots dir
        tokio::fs::create_dir_all(snap_dir)
            .await
            .context("failed to create snapshots directory")?;

        // 3. Record permissions and move original aside as backup (#673).
        //    Must happen BEFORE data migration so cleanup always restores full data.
        let orig_meta = tokio::fs::metadata(original_path)
            .await
            .context("failed to read original directory metadata")?;
        let orig_uid = orig_meta.uid();
        let orig_gid = orig_meta.gid();

        let backup_path = backup_path_for(original_path);
        // Recover orphan `.pre-init-bak` from an interrupted prior init before
        // proceeding with Step 3 (rename). See btrfs_common::recover_orphan_backup.
        recover_orphan_backup(original_path, subvol_path).await?;

        tokio::fs::rename(original_path, &backup_path)
            .await
            .context("failed to rename original directory to backup")?;
        *backup_owned = true;

        // 4. rsync migration from backup into subvolume
        let src = format!("{}/", backup_path);
        let status = Command::new("rsync")
            .args(["-a", &src, &subvol_path.to_string_lossy()])
            .status()
            .await
            .context("failed to run rsync")?;
        if !status.success() {
            anyhow::bail!("rsync failed with exit code: {:?}", status.code());
        }

        // 4a. Flush dirty data to disk so subsequent snapshots are instant
        let sync_status = Command::new("btrfs")
            .args(["filesystem", "sync", &subvol_path.to_string_lossy()])
            .status()
            .await
            .context("failed to run btrfs filesystem sync")?;
        if !sync_status.success() {
            warn!("btrfs filesystem sync returned non-zero, falling back to sync()");
            Command::new("sync").status().await.ok();
        }

        // 5. Create symlink: user path -> btrfs subvolume
        if let Some(parent) = Path::new(original_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("failed to create parent directory for symlink")?;
        }
        tokio::fs::symlink(subvol_path, original_path)
            .await
            .context("failed to create symlink")?;

        // 5a. Restore ownership on the subvolume root to match original directory
        chown(
            subvol_path,
            Some(Uid::from_raw(orig_uid)),
            Some(Gid::from_raw(orig_gid)),
        )
        .context("failed to restore subvolume ownership")?;

        // 6. Verify symlink
        let link_target = tokio::fs::read_link(original_path)
            .await
            .context("symlink verification failed: cannot read link")?;
        if link_target != subvol_path {
            anyhow::bail!(
                "symlink verification failed: expected {:?}, got {:?}",
                subvol_path,
                link_target
            );
        }

        // 7. Drop backup. A leftover .pre-init-bak blocks the next init.
        if let Err(e) = tokio::fs::remove_dir_all(&backup_path).await {
            error!(
                "init ok but backup remove failed {:?}: {}; next init will fail until removed",
                backup_path, e
            );
        }

        info!(
            "BtrfsLoopBackend: storage init complete for ws_id={}, subvol={}",
            ws_id,
            subvol_path.display()
        );
        Ok(())
    }
}

#[async_trait]
impl StorageBackend for BtrfsLoopBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::BtrfsLoop
    }

    fn data_root(&self) -> &Path {
        &self.mount_path
    }

    fn snapshots_root(&self) -> &Path {
        &self.snapshots_dir
    }

    async fn live_generation(&self, ws_id: &str) -> anyhow::Result<WorkspaceGenerationTokenV2> {
        btrfs_identity::live_generation(&self.mount_path, ws_id)
    }

    async fn init_workspace(
        &self,
        original_path: &str,
        ws_id: &str,
    ) -> anyhow::Result<WorkspaceInfo> {
        // Resolve symlink to real path to avoid copying the symlink itself
        let resolved = resolve_symlink_path(original_path).await?;
        let resolved_str = resolved.to_string_lossy().to_string();

        let subvol_path = self.mount_path.join(ws_id);
        let snap_dir = self.snapshots_dir.join(ws_id);

        let mut backup_owned = false;
        if let Err(e) = self
            .do_init_storage(
                &resolved_str,
                ws_id,
                &subvol_path,
                &snap_dir,
                &mut backup_owned,
            )
            .await
        {
            error!("init_workspace storage failed, cleaning up: {:#}", e);
            btrfs_common::cleanup_init_storage(
                &resolved_str,
                &subvol_path,
                &snap_dir,
                backup_owned,
                &self.mount_path,
            )
            .await;
            return Err(e);
        }

        Ok(WorkspaceInfo {
            ws_id: ws_id.to_string(),
            path: resolved_str,
            snapshot_count: 0,
        })
    }

    async fn create_snapshot(&self, ws_id: &str, snapshot_id: &str) -> anyhow::Result<()> {
        let ws_subvol = self.mount_path.join(ws_id);
        let snap_path = self.snapshots_dir.join(ws_id).join(snapshot_id);
        btrfs_common::create_snapshot(&ws_subvol, &snap_path, true).await
    }

    async fn rollback(&self, ws_id: &str, snapshot_id: &str) -> anyhow::Result<PathBuf> {
        let ws_path = self.mount_path.join(ws_id);
        let tmp_path = self.mount_path.join(format!("{}.rollback-tmp", ws_id));
        let snap_path = self.snapshots_dir.join(ws_id).join(snapshot_id);

        // Verify ws_path is a real subvolume, not a symlink
        let metadata = tokio::fs::symlink_metadata(&ws_path)
            .await
            .context("Failed to read workspace metadata")?;
        if metadata.file_type().is_symlink() {
            bail!("workspace path {:?} is a symlink, expected btrfs subvolume; aborting rollback to prevent symlink chain corruption", ws_path);
        }

        // Warmup snapshot metadata cache
        btrfs_common::warmup_snapshot_metadata(&snap_path).await;

        // Move current workspace aside
        tokio::fs::rename(&ws_path, &tmp_path).await?;

        // Create writable snapshot from target
        match btrfs_common::create_snapshot(&snap_path, &ws_path, false).await {
            Ok(()) => {}
            Err(e) => {
                // Rollback protection: restore original workspace
                error!("rollback snapshot failed, restoring original: {}", e);
                tokio::fs::rename(&tmp_path, &ws_path).await?;
                return Err(e);
            }
        }

        // Clean up old subvolume (non-fatal). Space-aware: on a nearly-full
        // backend the async cleaner may stall (ENOSPC) and leave a DELETED
        // zombie pinning all space, so the guarded path pushes it synchronously
        // and warns loudly with recovery guidance when it cannot (#3053).
        if let Err(e) =
            btrfs_common::delete_subvolume_space_aware(&tmp_path, &self.mount_path).await
        {
            warn!("failed to delete old subvolume (non-fatal): {}", e);
        }

        Ok(ws_path)
    }

    async fn delete_snapshot(&self, ws_id: &str, snapshot_id: &str) -> anyhow::Result<()> {
        let snap_path = self.snapshots_dir.join(ws_id).join(snapshot_id);
        btrfs_common::delete_subvolume_space_aware(&snap_path, &self.mount_path).await
    }

    async fn recover_workspace(&self, ws_id: &str, original_path: &str) -> anyhow::Result<()> {
        let subvol_path = self.mount_path.join(ws_id);
        let snap_base = self.snapshots_dir.join(ws_id);

        // 1. Remove symlink; refuse if a non-symlink directory occupies the path
        match tokio::fs::symlink_metadata(original_path).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                tokio::fs::remove_file(original_path)
                    .await
                    .context("failed to remove symlink")?;
            }
            Ok(_) => {
                bail!(
                    "cannot recover: {} is a regular directory, not a workspace symlink; \
                     move or rename it first to avoid data loss",
                    original_path
                );
            }
            Err(_) => {} // path gone, rsync will recreate
        }

        // 2. Rsync subvolume contents back to original path (restore as normal directory)
        let src = format!("{}/", subvol_path.to_string_lossy()); // trailing / is important

        // Record subvolume root permissions before rsync
        let subvol_meta = tokio::fs::metadata(&subvol_path)
            .await
            .context("failed to read subvolume metadata")?;
        let sv_uid = subvol_meta.uid();
        let sv_gid = subvol_meta.gid();
        let sv_mode = subvol_meta.mode();

        let rsync_status = Command::new("rsync")
            .args(["-a", "--delete", &src, original_path])
            .status()
            .await
            .context("failed to run rsync")?;
        if !rsync_status.success() {
            bail!(
                "rsync failed restoring {} -> {}, exit: {:?}; \
                 workspace and snapshots preserved for retry",
                src,
                original_path,
                rsync_status.code()
            );
        } else {
            // Restore directory ownership and permissions to match original
            if let Err(e) = chown(
                Path::new(original_path),
                Some(Uid::from_raw(sv_uid)),
                Some(Gid::from_raw(sv_gid)),
            ) {
                warn!("failed to restore ownership on {}: {}", original_path, e);
            }
            if let Err(e) =
                tokio::fs::set_permissions(original_path, std::fs::Permissions::from_mode(sv_mode))
                    .await
            {
                warn!("failed to restore permissions on {}: {}", original_path, e);
            }
            info!("restored workspace contents to {}", original_path);
        }

        // 3. Delete snapshots before the workspace subvolume so a partial
        //    teardown remains retryable.
        btrfs_common::delete_recovery_subvolumes(
            &snap_base,
            &subvol_path,
            &self.mount_path,
            original_path,
        )
        .await?;

        // 4. Remove snapshots/{ws_id} directory
        if let Err(e) = tokio::fs::remove_dir_all(&snap_base).await {
            warn!("failed to remove snapshots dir {:?}: {}", snap_base, e);
        }

        // 5. Clean orphan `.pre-init-bak` if it still exists (prior interrupted
        //    init). Safe to remove at this point — subvol is gone, original_path
        //    has been restored as a normal directory in steps above.
        let backup_path = backup_path_for(original_path);
        if tokio::fs::symlink_metadata(&backup_path).await.is_ok() {
            if let Err(e) = tokio::fs::remove_dir_all(&backup_path).await {
                warn!("failed to clean orphan backup {:?}: {:#}", backup_path, e);
            } else {
                info!("cleaned orphan backup {:?} during recover", backup_path);
            }
        }

        Ok(())
    }

    async fn diff(
        &self,
        ws_id: &str,
        from: &str,
        to: Option<&str>,
    ) -> anyhow::Result<Vec<DiffEntry>> {
        let snap_base = self.snapshots_dir.join(ws_id);
        let snap_from = snap_base.join(from);
        match to {
            Some(id) => btrfs_common::diff_between_snapshots(&snap_from, &snap_base.join(id)).await,
            None => {
                let live = self.data_root().join(ws_id);
                btrfs_common::diff_against_live(&snap_from, &live, &snap_base, &self.mount_path)
                    .await
            }
        }
    }

    async fn cleanup_snapshots(
        &self,
        ws_id: &str,
        snapshot_ids: &[String],
    ) -> anyhow::Result<Vec<(String, SnapshotDeleteOutcome)>> {
        // Whole batch shares ONE risk assessment and, at High risk, ONE
        // bounded cleaner wait (#3053 review P1-c).
        let snap_dir = self.snapshots_dir.join(ws_id);
        Ok(btrfs_common::cleanup_snapshots_batch(&snap_dir, &self.mount_path, snapshot_ids).await)
    }

    async fn fork(&self, ws_id: &str, snapshot_id: &str, new_ws_id: &str) -> anyhow::Result<()> {
        let snap_path = self.snapshots_dir.join(ws_id).join(snapshot_id);
        let new_ws_path = self.mount_path.join(new_ws_id);
        btrfs_common::create_snapshot(&snap_path, &new_ws_path, false).await
    }

    async fn gc_generations(&self, _ws_id: &str) -> anyhow::Result<GcResult> {
        Ok(GcResult::default())
    }

    async fn check_environment(&self) -> anyhow::Result<EnvironmentStatus> {
        let mut details = Vec::new();
        let mut healthy = true;

        // Check btrfs-progs
        match Command::new("which").arg("btrfs").output().await {
            Ok(output) if output.status.success() => {
                details.push("btrfs-progs: installed".to_string())
            }
            _ => {
                healthy = false;
                details.push("btrfs-progs: NOT installed".to_string());
            }
        }

        // Check root privileges
        if nix::unistd::geteuid().is_root() {
            details.push("privileges: root".to_string());
        } else {
            healthy = false;
            details.push("privileges: NOT root".to_string());
        }

        // Check loop device availability
        match Command::new("losetup").arg("--list").output().await {
            Ok(output) if output.status.success() => {
                details.push("loop devices: available".to_string())
            }
            _ => {
                healthy = false;
                details.push("loop devices: NOT available".to_string());
            }
        }

        Ok(EnvironmentStatus {
            backend: BackendType::BtrfsLoop,
            healthy,
            details,
        })
    }

    async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
        // Any failure is treated as a real anomaly (manual umount, fs crash, etc.); attach mount path for context.
        btrfs_common::get_filesystem_usage(&self.mount_path)
            .await
            .with_context(|| {
                format!(
                    "failed to get btrfs filesystem usage at {}",
                    self.mount_path.display()
                )
            })
    }

    async fn deleted_subvolume_ids(&self) -> anyhow::Result<Vec<u64>> {
        btrfs_common::list_deleted_subvolumes(&self.mount_path).await
    }

    async fn bootstrap(&self, config: &DaemonConfig) -> anyhow::Result<()> {
        btrfs_common::ensure_btrfs_support()
            .await
            .context("btrfs kernel support is required")?;

        let img_path_str = self.img_path.to_string_lossy().to_string();
        let img_dir = derive_img_dir(&img_path_str)
            .context("Failed to derive image directory from self.img_path")?;

        tokio::fs::create_dir_all(&img_dir)
            .await
            .context("Failed to create ws-ckpt data directory")?;

        // A newly created image already matches target; only reconcile pre-existing ones.
        let img_existed_before = tokio::fs::metadata(&self.img_path).await.is_ok();
        if !img_existed_before {
            create_sparse_image(&img_path_str, config, &img_dir).await?;
        }

        tokio::fs::create_dir_all(&self.mount_path)
            .await
            .context("Failed to create mount point directory")?;

        let mount_path_str = self.mount_path.to_string_lossy().to_string();
        let mounted = is_mounted(&mount_path_str).await?;
        let needs_mount = if mounted {
            // Check if our img is already the one mounted here (inode-based).
            let our_loop = find_loop_device_for(&img_path_str).await.ok();
            let mounted_source = run_command("findmnt", &["-no", "SOURCE", &mount_path_str])
                .await
                .ok()
                .map(|s| last_nonempty_line(&s).to_string());
            match (&our_loop, &mounted_source) {
                (Some(ours), Some(src)) if ours == src => {
                    info!("Already mounted at {:?} (loop {})", self.mount_path, ours);
                    false
                }
                _ => {
                    info!(
                        "{} mounted by {:?} but our img loop is {:?}; overmounting",
                        mount_path_str, mounted_source, our_loop
                    );
                    true
                }
            }
        } else {
            // Detach orphan loop from a prior failed mount attempt before re-attaching.
            if let Ok(orphan) = find_loop_device_for(&img_path_str).await {
                warn!(
                    "Found orphan loop {} for img (attached but not mounted); detaching",
                    orphan
                );
                let _ = run_command_checked("losetup", &["-d", &orphan]).await;
            }
            true
        };
        if needs_mount {
            let loop_device = attach_loop(&img_path_str).await?;
            if let Err(e) = run_command_checked("mount", &[&loop_device, &mount_path_str]).await {
                // Mount failed after the loop device was attached; detach it so
                // repeated failures don't leak/exhaust loop devices.
                if let Err(detach_err) = run_command_checked("losetup", &["-d", &loop_device]).await
                {
                    warn!(
                        "Failed to detach loop device {} after mount failure: {:#}",
                        loop_device, detach_err
                    );
                }
                return Err(e).context("Failed to mount btrfs image");
            }
            info!("Mounted {} at {}", loop_device, mount_path_str);
        }

        // Zombie sweep (#3053): a previous run may have deleted subvolumes the
        // kernel cleaner could not reclaim under ENOSPC. The daemon reuses this
        // mount across restarts by design (#2809), so no mount cycle ever kicks
        // the cleaner on its own — push it explicitly before anything else
        // (including img size reconcile) makes decisions based on used space.
        // KickThenWait: the loop image is ws-ckpt's own filesystem, so every
        // dead-list entry is ours and the sweep may block on a bounded sync
        // wait for the drain.
        let sweep_outcome = btrfs_common::sweep_zombie_subvolumes(
            &self.mount_path,
            btrfs_common::ZombieSweepPolicy::KickThenWait {
                wait_timeout: btrfs_common::BOOTSTRAP_ZOMBIE_SWEEP_TIMEOUT,
            },
        )
        .await;

        if img_existed_before {
            if let Err(e) =
                reconcile_img_size(&img_path_str, &mount_path_str, config, sweep_outcome).await
            {
                warn!(
                    "Failed to reconcile btrfs image size: {:#}. Continuing with current size.",
                    e
                );
            }
        }

        let snapshots_dir = self.mount_path.join(SNAPSHOTS_DIR);
        tokio::fs::create_dir_all(&snapshots_dir)
            .await
            .context("Failed to create snapshots directory")?;

        // Startup awaits bootstrap before rebuilding workspace watchers.
        self.recover_interrupted_rollbacks().await?;

        info!("BtrfsLoop bootstrap complete (img={:?})", self.img_path);
        Ok(())
    }

    async fn loop_img_state(&self) -> Option<LoopImgState> {
        let img_size_bytes = match tokio::fs::metadata(&self.img_path).await {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!("loop_img_state: stat {:?} failed: {:#}", self.img_path, e);
                return None;
            }
        };
        let img_path_str = self.img_path.to_string_lossy().to_string();
        let last_loop_device = find_loop_device_for(&img_path_str).await.ok();
        Some(LoopImgState {
            img_path: self.img_path.clone(),
            img_size_bytes,
            last_loop_device,
        })
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Effective image path resolution & legacy migration
// ────────────────────────────────────────────────────────────────────────────

/// Decide which image file the daemon will operate on this run, performing a
/// best-effort one-shot migration from the pre-FHS legacy path to the canonical
/// path when applicable. This is invoked before `BtrfsLoopBackend::new`.
///
/// Decision tree:
///
/// 1. `mount_path` already mounted — classify via `losetup -j` (inode-based):
///    - Ours (target): return target.
///    - OursLegacy: try in-place relocation (umount → rename → remount).
///      Pre-rename failure (busy / cross-fs / cmd error) → fall back to legacy.
///      Post-rename failure (losetup/mount) → return target; bootstrap will mount.
///    - External / Stale / probe error: fall through to cold path.
/// 2. Cold path (mount not active or not ours):
///    - target exists → use target.
///    - target missing && legacy exists → attempt migration.
///      - success → use target.
///      - failure → warn and fall back to legacy; daemon serves on legacy and
///        retries migration on the next start.
///    - neither exists → use target (fresh install; bootstrap will create).
pub async fn decide_effective_img_path(
    mount_path: &Path,
    target: &Path,
    legacy: &Path,
) -> anyhow::Result<PathBuf> {
    let mount_path_str = mount_path.to_string_lossy().to_string();
    match is_mounted(&mount_path_str).await {
        Ok(true) => {
            match classify_mount(&mount_path_str, target, legacy).await {
                Ok(MountDisposition::Ours(loop_dev)) => {
                    info!("Mount at {} is ours (loop {})", mount_path_str, loop_dev);
                    return Ok(target.to_path_buf());
                }
                Ok(MountDisposition::OursLegacy(loop_dev)) => {
                    info!(
                        "Mount at {} backs legacy img; attempting live relocation",
                        mount_path_str
                    );
                    match try_relocate_active_legacy_mount(
                        &mount_path_str,
                        legacy,
                        target,
                        &loop_dev,
                    )
                    .await
                    {
                        Ok(()) => {
                            info!("Live img migration complete: {:?} -> {:?}", legacy, target);
                            return Ok(target.to_path_buf());
                        }
                        Err(e) => {
                            error!(
                                "In-place relocation from legacy {:?} -> target {:?} \
                                 aborted: {:#}. Daemon will keep serving on legacy this \
                                 run; migration will retry on next start once the mount \
                                 is idle.",
                                legacy, target, e
                            );
                            return Ok(legacy.to_path_buf());
                        }
                    }
                }
                Ok(MountDisposition::Stale(loop_dev)) => {
                    warn!(
                        "Mount at {} uses loop {} which doesn't back target or legacy; \
                         bootstrap will overmount",
                        mount_path_str, loop_dev
                    );
                    // Don't umount here — stacked mount removal can cascade via
                    // propagation and destroy our own underlying mount. Bootstrap
                    // will overmount on top of the stale layer safely.
                }
                Ok(MountDisposition::External) => {
                    info!(
                        "{} has an external (non-btrfs-loop) mount; \
                         bootstrap will overmount with our btrfs image",
                        mount_path_str
                    );
                    // Fall through to cold path.
                }
                Err(e) => {
                    warn!(
                        "classify_mount({}) failed: {:#}; falling through to cold path",
                        mount_path_str, e
                    );
                }
            }
        }
        Ok(false) => {}
        Err(e) => {
            // /proc/mounts being unreadable is a degenerate state that downstream
            // `bootstrap()` will also hit (it calls `is_mounted` with `?`), so the
            // daemon won't actually drift into a wrong-img branch — bootstrap will
            // bail before touching loop/mount. Logging here and proceeding to the
            // cold path is enough; we don't need a second loud failure.
            warn!(
                "Failed to check mount state of {}: {:#}; assuming not mounted",
                mount_path_str, e
            );
        }
    }

    let target_exists = tokio::fs::metadata(target).await.is_ok();
    let legacy_exists = tokio::fs::metadata(legacy).await.is_ok();

    if target_exists {
        if legacy_exists {
            warn!(
                "Both target {:?} and legacy {:?} exist; using target. \
                 Legacy is left in place — please remove manually after verification.",
                target, legacy
            );
        }
        return Ok(target.to_path_buf());
    }

    if legacy_exists {
        info!(
            "Target img missing, legacy img at {:?} present — attempting one-shot migration",
            legacy
        );
        match migrate_legacy_img(legacy, target).await {
            Ok(()) => {
                info!("Migrated legacy img {:?} -> {:?}", legacy, target);
                return Ok(target.to_path_buf());
            }
            Err(e) => {
                error!(
                    "Failed to migrate legacy img {:?} -> {:?}: {:#}. \
                     Daemon will serve on the legacy path and retry migration on next start. \
                     Old data is intact.",
                    legacy, target, e
                );
                return Ok(legacy.to_path_buf());
            }
        }
    }

    // Fresh install: nothing exists yet.
    Ok(target.to_path_buf())
}

/// Mount ownership classification for `mount_path`.
enum MountDisposition {
    /// External mount (non-btrfs, or btrfs on a non-loop block device).
    External,
    /// Our btrfs-loop: the loop device backing `target` is what's mounted.
    Ours(String),
    /// Our btrfs-loop backing `legacy` img (needs migration).
    OursLegacy(String),
    /// A loop-backed btrfs mount that doesn't match target or legacy (stale).
    Stale(String),
}

/// Classify the mount at `mount_path` using inode-based ownership detection.
///
/// Instead of reading BACK-FILE (which breaks across mount namespaces due to
/// kernel `d_path()` rendering), we use `losetup -j <img>` which matches by
/// st_dev+st_ino — correct regardless of which namespace the loop was created in.
async fn classify_mount(
    mount_path: &str,
    target: &Path,
    legacy: &Path,
) -> anyhow::Result<MountDisposition> {
    let raw = run_command("findmnt", &["-no", "FSTYPE,SOURCE", mount_path])
        .await
        .context("findmnt FSTYPE,SOURCE failed")?;
    let line = last_nonempty_line(&raw);
    if line.is_empty() {
        return Ok(MountDisposition::External);
    }
    let mut parts = line.splitn(2, char::is_whitespace);
    let fstype = parts.next().unwrap_or("");
    let source = parts.next().unwrap_or("").trim();

    if fstype != "btrfs" || !source.starts_with("/dev/loop") {
        return Ok(MountDisposition::External);
    }

    // source is /dev/loopN. Check ownership via inode-based losetup -j.
    let target_str = target.to_string_lossy();
    if let Ok(dev) = find_loop_device_for(&target_str).await {
        if dev == source {
            return Ok(MountDisposition::Ours(source.to_string()));
        }
    }
    let legacy_str = legacy.to_string_lossy();
    if let Ok(dev) = find_loop_device_for(&legacy_str).await {
        if dev == source {
            return Ok(MountDisposition::OursLegacy(source.to_string()));
        }
    }

    // Loop device at mount_path doesn't match target or legacy — stale.
    Ok(MountDisposition::Stale(source.to_string()))
}

/// Extract the last non-empty line from command output.
/// Stacked mounts produce multiple findmnt lines; the last is the topmost.
fn last_nonempty_line(s: &str) -> &str {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("")
}

/// Live-mount migration legacy → target: idle-check, umount, rename, remount.
/// Aborts (without touching the live mount) when busy or cross-fs.
/// On failure after umount, best-effort remounts legacy before bubbling the error.
async fn try_relocate_active_legacy_mount(
    mount_path: &str,
    legacy: &Path,
    target: &Path,
    loop_device: &str,
) -> anyhow::Result<()> {
    if let Some(pids) = check_mount_busy(mount_path).await {
        bail!(
            "mount {} is in use by PIDs [{}]; refusing to umount during relocation",
            mount_path,
            pids
        );
    }

    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create target parent {:?}", parent))?;
    }
    let legacy_dev = tokio::fs::metadata(legacy)
        .await
        .with_context(|| format!("stat legacy {:?}", legacy))?
        .dev();
    let target_parent = target.parent().unwrap_or_else(|| Path::new("/"));
    let target_dev = tokio::fs::metadata(target_parent)
        .await
        .with_context(|| format!("stat target parent {:?}", target_parent))?
        .dev();
    if legacy_dev != target_dev {
        bail!(
            "legacy {:?} (dev {}) and target parent {:?} (dev {}) are on different \
             filesystems; cross-fs rename is not atomic — this is unexpected since \
             both paths should live under the host root fs",
            legacy,
            legacy_dev,
            target_parent,
            target_dev
        );
    }

    let legacy_str = legacy.to_string_lossy().to_string();

    run_command_checked("umount", &[mount_path])
        .await
        .context("umount mount_path")?;
    // From here on any failure best-effort remounts legacy so the system isn't
    // left worse than before.
    if let Err(e) = run_command_checked("losetup", &["-d", loop_device]).await {
        // Loop still attached — don't rename a file the kernel still tracks as backing.
        if let Err(re) = run_command_checked("mount", &[loop_device, mount_path]).await {
            warn!(
                "losetup -d and remount both failed; mount {} is now offline: {:#}",
                mount_path, re
            );
        }
        return Err(e).context("losetup -d failed; attempted remount of legacy (may have failed)");
    }

    if let Err(e) = tokio::fs::rename(legacy, target).await {
        if let Err(remount_err) = remount_legacy(&legacy_str, mount_path).await {
            warn!(
                "rename {:?} -> {:?} failed and remount of legacy failed too: {:#}; \
                 mount {} is now offline — manual intervention required",
                legacy, target, remount_err, mount_path
            );
        }
        return Err(anyhow::Error::from(e).context(format!("rename {:?} -> {:?}", legacy, target)));
    }

    // Past rename: legacy is gone — we return Ok(()) regardless so the caller
    // uses `target`. If remount fails, bootstrap will handle it on this same run.
    let target_str = target.to_string_lossy().to_string();
    match attach_loop(&target_str).await {
        Ok(new_loop) => {
            if let Err(e) = run_command_checked("mount", &[&new_loop, mount_path]).await {
                let _ = run_command_checked("losetup", &["-d", &new_loop]).await;
                warn!(
                    "Post-rename mount failed ({:#}); bootstrap will retry for {:?}",
                    e, target
                );
            } else {
                info!(
                    "Relocated active mount {} -> backing {:?}",
                    mount_path, target
                );
            }
        }
        Err(e) => {
            warn!(
                "Post-rename losetup --find failed ({:#}); bootstrap will retry for {:?}",
                e, target
            );
        }
    }
    Ok(())
}

/// Reattach + mount `img`; used only by `try_relocate_active_legacy_mount` rollback.
/// Called after `losetup -d` succeeded, so the original loop device is gone and we
/// must allocate a fresh one via `losetup --find`.
async fn remount_legacy(img: &str, mount_path: &str) -> anyhow::Result<()> {
    let loop_dev = attach_loop(img)
        .await
        .context("reattach legacy via losetup --find")?;
    run_command_checked("mount", &[&loop_dev, mount_path])
        .await
        .context("remount legacy after relocation rollback")
}

/// Move `legacy` to `target`. Tries atomic rename first; on `EXDEV` falls back
/// to copy-to-tmp + fsync + atomic rename + unlink-legacy. Failure leaves the
/// legacy file untouched so the next bootstrap can retry.
async fn migrate_legacy_img(legacy: &Path, target: &Path) -> anyhow::Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create target parent {:?}", parent))?;
    }

    match tokio::fs::rename(legacy, target).await {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => cross_fs_migrate(legacy, target).await,
        Err(e) => {
            Err(anyhow::Error::from(e).context(format!("rename {:?} -> {:?}", legacy, target)))
        }
    }
}

/// EXDEV fallback: long-running copy is staged at `<target>.migrate-tmp` (same
/// fs as `target`), fsync'd, then atomically renamed onto `target`. The legacy
/// file is unlinked only after the new target is fully published, so an
/// interruption never leaves a half-written `target` for the next bootstrap to
/// mount as a corrupt image.
async fn cross_fs_migrate(legacy: &Path, target: &Path) -> anyhow::Result<()> {
    let tmp = {
        let mut t = target.as_os_str().to_owned();
        t.push(".migrate-tmp");
        PathBuf::from(t)
    };
    // Drop any leftover from a previous failed attempt to keep this idempotent.
    let _ = tokio::fs::remove_file(&tmp).await;

    tokio::fs::copy(legacy, &tmp)
        .await
        .with_context(|| format!("cross-fs copy {:?} -> {:?}", legacy, tmp))?;

    let f = tokio::fs::File::open(&tmp)
        .await
        .with_context(|| format!("open tmp for fsync {:?}", tmp))?;
    f.sync_all()
        .await
        .with_context(|| format!("fsync tmp {:?}", tmp))?;
    drop(f);

    tokio::fs::rename(&tmp, target)
        .await
        .with_context(|| format!("atomic publish {:?} -> {:?}", tmp, target))?;

    // Failure to unlink legacy after a successful publish is non-fatal: the
    // next bootstrap sees both files exist and just leaves legacy for manual
    // cleanup.
    if let Err(e) = tokio::fs::remove_file(legacy).await {
        warn!(
            "Migration succeeded but failed to unlink legacy {:?}: {:#}; \
             target {:?} is fully in place — please remove legacy manually.",
            legacy, e, target
        );
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// BtrfsLoop-specific bootstrap helpers
// ────────────────────────────────────────────────────────────────────────────

/// Create a sparse image file sized by `min(img_size GB, total * img_max_percent%)`,
/// degrading to `avail * img_max_percent%` if target exceeds host avail. Then mkfs.btrfs.
async fn create_sparse_image(
    img_path: &str,
    config: &DaemonConfig,
    img_dir: &str,
) -> anyhow::Result<()> {
    // `-P` forces POSIX single-line output; long device names must not wrap.
    let df_output = run_command("df", &["-P", "-B1", img_dir])
        .await
        .context("Failed to get partition info")?;
    let total = parse_df_total(&df_output).context("Failed to parse df total")?;
    let avail = parse_df_available(&df_output).context("Failed to parse df output")?;

    let target = compute_target_size(config.img_size, config.img_max_percent, total);

    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let img_size = if target > avail {
        let degraded = (avail as f64 * config.img_max_percent / 100.0) as u64;
        warn!(
            "Target image size {:.1} GB exceeds available {:.1} GB. Degrading to {:.1} GB ({}% of available).",
            target as f64 / GB,
            avail as f64 / GB,
            degraded as f64 / GB,
            config.img_max_percent,
        );
        degraded
    } else {
        target
    };
    info!(
        "Creating sparse image {} bytes ({:.1} GB), total {:.1} GB, avail {:.1} GB",
        img_size,
        img_size as f64 / GB,
        total as f64 / GB,
        avail as f64 / GB,
    );

    run_command_checked("truncate", &["-s", &img_size.to_string(), img_path])
        .await
        .context("Failed to create sparse image file")?;
    // If mkfs fails, drop the just-truncated (zeroed) file. Leaving it behind
    // would let the next bootstrap skip formatting (it only checks existence)
    // and then fail `mount` forever on a superblock-less image.
    if let Err(e) = run_command_checked("mkfs.btrfs", &["-f", img_path]).await {
        if let Err(rm_err) = tokio::fs::remove_file(img_path).await {
            warn!(
                "Failed to remove half-created image {} after mkfs failure: {:#}",
                img_path, rm_err
            );
        }
        return Err(e).context("Failed to format btrfs image");
    }
    Ok(())
}

/// img_path must be absolute with a non-empty parent dir.
fn derive_img_dir(img_path: &str) -> anyhow::Result<String> {
    let parent = Path::new(img_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .with_context(|| {
            format!(
                "Invalid img_path {:?}: must be an absolute path containing a parent directory",
                img_path
            )
        })?;
    Ok(parent.to_string_lossy().to_string())
}

/// Smaller of: absolute cap (`img_size_gb * GiB`) and percentage cap (`total * img_max_percent%`).
fn compute_target_size(img_size_gb: u64, img_max_percent: f64, total_bytes: u64) -> u64 {
    let by_size = img_size_gb.saturating_mul(1024 * 1024 * 1024);
    let by_percent = (total_bytes as f64 * img_max_percent / 100.0) as u64;
    std::cmp::min(by_size, by_percent)
}

/// Reconcile on-disk img size against computed target.
///
/// - Equal: no-op (still runs a final `btrfs fs resize max` for self-healing).
/// - Less : grow in place via truncate + `losetup -c`, guarded by host avail.
/// - Greater: shrink via btrfs resize + unmount + truncate + remount, guarded by `fuser -m`.
async fn reconcile_img_size(
    img_path: &str,
    mount_path_str: &str,
    config: &DaemonConfig,
    sweep_outcome: btrfs_common::ZombieSweepOutcome,
) -> anyhow::Result<()> {
    let current = tokio::fs::metadata(img_path)
        .await
        .with_context(|| format!("Failed to stat image {}", img_path))?
        .len();

    let img_dir =
        derive_img_dir(img_path).context("Failed to derive image directory from img_path")?;
    let df_output = run_command("df", &["-P", "-B1", &img_dir])
        .await
        .context("Failed to get host partition info for reconcile")?;
    let total = parse_df_total(&df_output).context("Failed to parse df total")?;
    let avail = parse_df_available(&df_output).context("Failed to parse df available")?;

    let target = compute_target_size(config.img_size, config.img_max_percent, total);

    // Track whether we actually grew; only log "grown to" after the final fs resize succeeds.
    let mut grew_to: Option<u64> = None;

    match current.cmp(&target) {
        std::cmp::Ordering::Equal => {
            info!("Btrfs image size already matches target: {} bytes", current);
        }
        std::cmp::Ordering::Less => {
            let needed = target - current;
            if needed > avail {
                warn!(
                    "Cannot grow btrfs image: need {} more bytes (target {}) but avail {}. Keeping current ({} bytes).",
                    needed, target, avail, current,
                );
                return Ok(());
            }
            info!("Growing btrfs image: {} -> {} bytes", current, target);
            run_command_checked("truncate", &["-s", &target.to_string(), img_path])
                .await
                .context("Failed to grow sparse image file")?;
            let loop_device = find_loop_device_for(img_path)
                .await
                .context("Failed to locate loop device for image")?;
            run_command_checked("losetup", &["-c", &loop_device])
                .await
                .context("Failed to refresh loop device capacity")?;
            // Final `btrfs fs resize max` runs below; on failure state is recoverable.
            grew_to = Some(target);
        }
        std::cmp::Ordering::Greater => {
            if sweep_outcome != btrfs_common::ZombieSweepOutcome::Clear {
                warn!(
                    "Cannot shrink btrfs image while deleted subvolumes may still pin space. \
                     Keeping current ({} bytes).",
                    current
                );
                return Ok(());
            }
            if btrfs_common::assess_space_risk(Path::new(mount_path_str)).await
                == btrfs_common::SpaceRisk::High
            {
                warn!(
                    "Cannot shrink btrfs image while filesystem usage is high or unverifiable. \
                     Keeping current ({} bytes).",
                    current
                );
                return Ok(());
            }
            if let Some(pids) = check_mount_busy(mount_path_str).await {
                warn!(
                    "Cannot shrink btrfs image: mount {} in use by PIDs [{}]. Keeping current ({} bytes).",
                    mount_path_str, pids, current,
                );
                return Ok(());
            }
            warn!("Shrinking btrfs image: {} -> {} bytes", current, target);
            // Shrink fs first — fails cleanly if used bytes exceed target, no data loss.
            run_command_checked(
                "btrfs",
                &["filesystem", "resize", &target.to_string(), mount_path_str],
            )
            .await
            .context("Failed to shrink btrfs filesystem (data may exceed new size)")?;
            let loop_device = find_loop_device_for(img_path)
                .await
                .context("Failed to locate loop device for image")?;
            run_command_checked("umount", &[mount_path_str])
                .await
                .context("Failed to unmount for image shrink")?;
            run_command_checked("losetup", &["-d", &loop_device])
                .await
                .context("Failed to detach loop device")?;
            run_command_checked("truncate", &["-s", &target.to_string(), img_path])
                .await
                .context("Failed to truncate image file")?;
            let new_loop = attach_loop(img_path)
                .await
                .context("Failed to reattach loop device")?;
            run_command_checked("mount", &[&new_loop, mount_path_str])
                .await
                .context("Failed to remount after image shrink")?;
            info!("Btrfs image shrunk to {} bytes", target);
        }
    }

    // Single exit point: syncs fs size to loop capacity. Also self-heals a previous
    // half-done grow (truncate+losetup -c succeeded but btrfs resize died).
    run_command_checked("btrfs", &["filesystem", "resize", "max", mount_path_str])
        .await
        .context("Failed to sync btrfs filesystem size to loop capacity")?;
    if let Some(t) = grew_to {
        info!("Btrfs image grown to {} bytes", t);
    }
    Ok(())
}

/// Advisory check via `fuser -m`; returns PIDs occupying the mount or None.
/// Not routed through `run_command` because fuser exits 1 on the common
/// "nothing using it" path, which `run_command` would treat as hard failure.
async fn check_mount_busy(mount_path: &str) -> Option<String> {
    let output = Command::new("fuser")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["-m", mount_path])
        .output()
        .await
        .ok()?;
    let pids = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if pids.is_empty() {
        None
    } else {
        Some(pids)
    }
}

/// Attach `img` to a free loop device, then enable direct-IO best-effort.
///
/// The two steps are deliberately separate: a combined
/// `losetup --find --show --direct-io=on` reports a DIO failure through its
/// exit code *after* the device is attached, which would fail bootstrap (and
/// discard the freshly attached device name) on hosts where the kernel
/// rejects DIO — tmpfs backing has no `direct_IO`, and backing devices with
/// logical blocks larger than the loop's 512 (4Kn NVMe, 4096-sector LUKS2)
/// fail the block-size check. DIO is a performance optimization, not a
/// correctness requirement, so those hosts fall back to buffered mode.
///
/// The retry loop handles a device-node race: when every registered loop
/// device is attached (e.g. leftovers from containers that exited without
/// detaching), `LOOP_CTL_GET_FREE` makes the kernel register a new loop
/// device, but udev creates its /dev node asynchronously — so the very
/// losetup that triggered the registration fails with ENOENT. `losetup
/// --find` reports the device that would be handed out next, so its node can
/// be created synchronously before retrying.
async fn attach_loop(img: &str) -> anyhow::Result<String> {
    const ATTEMPTS: u32 = 5;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=ATTEMPTS {
        match run_command("losetup", &["--find", "--show", img]).await {
            Ok(dev) => {
                let dev = dev.trim().to_string();
                if let Err(e) = run_command_checked("losetup", &["--direct-io=on", &dev]).await {
                    warn!(
                        "Could not enable direct-io on {} for {}: {:#}. Continuing in buffered mode; \
                         checkpoint latency will be higher.",
                        dev, img, e
                    );
                }
                return Ok(dev);
            }
            Err(e) => {
                warn!(
                    "losetup attach attempt {}/{} for {} failed: {:#}",
                    attempt, ATTEMPTS, img, e
                );
                last_err = Some(e);
                if let Ok(free) = run_command("losetup", &["--find"]).await {
                    ensure_loop_node(free.trim()).await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::Error::msg("losetup failed with no recorded error")))
        .context("Failed to setup loop device")
}

/// Normalize `losetup --find` output to the canonical `/dev/loopN` path.
///
/// util-linux >= 2.40 appends " (lost)" when the kernel device exists but
/// its /dev node is missing — precisely the state the retry path repairs.
/// Returns None for empty output or anything that is not a loop device.
fn normalize_free_loop_dev(raw: &str) -> Option<&str> {
    let path = raw.split_whitespace().next()?;
    let name = Path::new(path).file_name()?.to_str()?;
    let index = name.strip_prefix("loop")?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(path)
}

/// Parse the `major:minor` pair exported at `/sys/block/<dev>/dev`.
///
/// The device number must come from sysfs, never from the device name: with
/// `max_part > 0` the loop driver reserves partition minors and assigns the
/// disk a shifted minor, so `loop1` can be `7:128` rather than `7:1`.
fn parse_sysfs_dev_numbers(raw: &str) -> Option<(&str, &str)> {
    let (major, minor) = raw.trim().split_once(':')?;
    let is_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if is_number(major) && is_number(minor) {
        Some((major, minor))
    } else {
        None
    }
}

/// Arguments for creating a loop device node: block device with the same
/// restrictive mode udev applies — never world-readable, since the node
/// exposes the raw workspace image once attached.
fn loop_node_mknod_args<'a>(node: &'a str, major: &'a str, minor: &'a str) -> [&'a str; 6] {
    ["-m", "0660", node, "b", major, minor]
}

/// Create the `/dev/<free_dev>` node if it does not exist yet.
///
/// Called with the device `losetup --find` would hand out next: the kernel
/// already has the device registered (so `/sys/block/<name>/dev` carries the
/// authoritative device number), only the udev node is missing. The daemon
/// runs as root; failure is non-fatal because udev may still deliver the
/// node before the next retry.
async fn ensure_loop_node(free_dev: &str) {
    let free_dev = match normalize_free_loop_dev(free_dev) {
        Some(path) => path,
        None => return,
    };
    if Path::new(free_dev).exists() {
        return;
    }
    let dev_name = match Path::new(free_dev).file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return,
    };
    let dev_str = match tokio::fs::read_to_string(format!("/sys/block/{dev_name}/dev")).await {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not read /sys/block/{}/dev: {}", dev_name, e);
            return;
        }
    };
    let (major, minor) = match parse_sysfs_dev_numbers(&dev_str) {
        Some(pair) => pair,
        None => return,
    };
    let args = loop_node_mknod_args(free_dev, major, minor);
    if let Err(e) = run_command_checked("mknod", &args).await {
        warn!("Could not create loop device node {}: {:#}", free_dev, e);
        return;
    }
    // Match udev's usual ownership (root:disk) so tools expecting disk-group
    // access behave the same as with udev-created nodes. Best-effort: the
    // disk group may be absent in minimal container images.
    if let Err(e) = run_command_checked("chown", &["root:disk", free_dev]).await {
        warn!("Could not chown {} to root:disk: {:#}", free_dev, e);
    }
}

/// Parse `losetup -j <img>` and return the backing loop device.
async fn find_loop_device_for(img_path: &str) -> anyhow::Result<String> {
    let out = run_command("losetup", &["-j", img_path])
        .await
        .context("Failed to run `losetup -j`")?;
    parse_losetup_j(&out, img_path)
}

fn parse_losetup_j(out: &str, img_path: &str) -> anyhow::Result<String> {
    // Expected: "/dev/loop0: [2049]:12345 (/var/lib/ws-ckpt/btrfs-data.img)"
    let line = out
        .lines()
        .next()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("No loop device currently backs image {}", img_path))?;
    let device = line
        .split(':')
        .next()
        .with_context(|| format!("Cannot parse losetup output: {}", line))?;
    Ok(device.trim().to_string())
}

/// Parse `Available` column (index 3) from `df -B1` output.
fn parse_df_available(output: &str) -> anyhow::Result<u64> {
    let line = output
        .lines()
        .nth(1)
        .context("df output has no data line")?;
    let avail_str = line
        .split_whitespace()
        .nth(3)
        .context("df output missing available column")?;
    avail_str
        .parse::<u64>()
        .context("Failed to parse available size from df output")
}

/// Parse `1B-blocks` (total) column (index 1) from `df -B1` output.
fn parse_df_total(output: &str) -> anyhow::Result<u64> {
    let line = output
        .lines()
        .nth(1)
        .context("df output has no data line")?;
    let total_str = line
        .split_whitespace()
        .nth(1)
        .context("df output missing total column")?;
    total_str
        .parse::<u64>()
        .context("Failed to parse total size from df output")
}

#[cfg(test)]
mod tests {
    use super::{
        compute_target_size, derive_img_dir, loop_node_mknod_args, normalize_free_loop_dev,
        parse_df_available, parse_df_total, parse_losetup_j, parse_sysfs_dev_numbers,
        reconcile_img_size, BtrfsLoopBackend,
    };
    use ws_ckpt_common::{DaemonConfig, SNAPSHOTS_DIR};

    const GB: u64 = 1024 * 1024 * 1024;

    #[tokio::test]
    async fn recovery_scans_loop_mount_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mount_path = tmp.path().join("mount");
        tokio::fs::create_dir_all(&mount_path).await.unwrap();
        let candidate = mount_path.join("ws-abc123.rollback-tmp");
        tokio::fs::write(&candidate, b"foreign").await.unwrap();
        tokio::fs::create_dir_all(mount_path.join(SNAPSHOTS_DIR).join("ws-abc123"))
            .await
            .unwrap();
        let backend = BtrfsLoopBackend::new(mount_path, tmp.path().join("image"));

        let error = backend.recover_interrupted_rollbacks().await.unwrap_err();

        assert!(format!("{error:#}").contains("ambiguous interrupted rollback"));
        assert!(candidate.exists(), "unsafe candidate must be preserved");
    }

    #[test]
    fn parses_available_column_from_df_b1() {
        let out = "Filesystem     1B-blocks        Used   Available Use% Mounted on\n\
                   /dev/sda1    107374182400 32212254720 75161927680  30% /data\n";
        assert_eq!(parse_df_available(out).unwrap(), 75_161_927_680u64);
    }

    #[test]
    fn returns_err_on_missing_data_line() {
        let out = "Filesystem 1B-blocks Used Available Use% Mounted on\n";
        assert!(parse_df_available(out).is_err());
    }

    #[test]
    fn returns_err_on_non_numeric_available() {
        let out = "Filesystem 1B-blocks Used Available Use% Mounted on\n\
                   /dev/sda1 100 10 NaN 10% /data\n";
        assert!(parse_df_available(out).is_err());
    }

    #[test]
    fn parses_loop_device_from_losetup_j() {
        let out = "/dev/loop0: [2049]:12345 (/var/lib/ws-ckpt/btrfs-data.img)\n";
        assert_eq!(
            parse_losetup_j(out, "/var/lib/ws-ckpt/btrfs-data.img").unwrap(),
            "/dev/loop0"
        );
    }

    #[test]
    fn parse_losetup_j_returns_err_on_empty() {
        assert!(parse_losetup_j("", "/var/lib/ws-ckpt/btrfs-data.img").is_err());
        assert!(parse_losetup_j("\n", "/var/lib/ws-ckpt/btrfs-data.img").is_err());
    }

    #[test]
    fn parses_total_column_from_df_b1() {
        let out = "Filesystem     1B-blocks        Used   Available Use% Mounted on\n\
                   /dev/sda1    107374182400 32212254720 75161927680  30% /data\n";
        assert_eq!(parse_df_total(out).unwrap(), 107_374_182_400u64);
    }

    #[test]
    fn compute_target_picks_size_cap_when_smaller() {
        let got = compute_target_size(30, 50.0, 100 * GB);
        assert_eq!(got, 30 * GB);
    }

    #[test]
    fn compute_target_picks_percent_cap_when_smaller() {
        let got = compute_target_size(30, 50.0, 40 * GB);
        assert_eq!(got, 20 * GB);
    }

    #[test]
    fn compute_target_handles_equal_caps() {
        let got = compute_target_size(30, 50.0, 60 * GB);
        assert_eq!(got, 30 * GB);
    }

    #[test]
    fn compute_target_saturates_on_huge_img_size() {
        let got = compute_target_size(u64::MAX / GB, 50.0, 100 * GB);
        assert_eq!(got, 50 * GB);
    }

    #[tokio::test]
    async fn unresolved_zombies_preserve_expanded_image() {
        let dir = tempdir().unwrap();
        let image = dir.path().join("data.img");
        let file = std::fs::File::create(&image).unwrap();
        file.set_len(2 * GB).unwrap();
        let config = DaemonConfig {
            img_size: 1,
            img_max_percent: 100.0,
            ..DaemonConfig::default()
        };

        reconcile_img_size(
            image.to_str().unwrap(),
            "/not-a-btrfs-mount",
            &config,
            crate::backends::btrfs_common::ZombieSweepOutcome::Unresolved,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::metadata(image).unwrap().len(), 2 * GB);
    }

    #[test]
    fn derive_img_dir_returns_parent_for_absolute_path() {
        assert_eq!(
            derive_img_dir("/var/lib/ws-ckpt/btrfs-data.img").unwrap(),
            "/var/lib/ws-ckpt"
        );
    }

    #[test]
    fn derive_img_dir_rejects_bare_filename() {
        assert!(derive_img_dir("data.img").is_err());
    }

    #[test]
    fn derive_img_dir_rejects_root_and_empty() {
        assert!(derive_img_dir("/").is_err());
        assert!(derive_img_dir("").is_err());
    }

    use super::{cross_fs_migrate, decide_effective_img_path, migrate_legacy_img};
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// On the same filesystem, `migrate_legacy_img` should take the fast path
    /// (`rename`) and leave the legacy file gone, the target file populated.
    #[tokio::test]
    async fn migrate_legacy_img_same_fs_renames_atomically() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("legacy.img");
        let target = dir.path().join("nested/target.img");
        tokio::fs::write(&legacy, b"hello-world-data")
            .await
            .unwrap();

        migrate_legacy_img(&legacy, &target).await.unwrap();

        assert!(!legacy.exists(), "legacy must be gone after migration");
        let got = tokio::fs::read(&target).await.unwrap();
        assert_eq!(got, b"hello-world-data");
        // No tmp file should leak.
        let mut tmp = target.as_os_str().to_owned();
        tmp.push(".migrate-tmp");
        assert!(!PathBuf::from(tmp).exists());
    }

    /// Direct-call test for the cross-fs path: copies, atomically publishes,
    /// unlinks legacy, and leaves no tmp behind.
    #[tokio::test]
    async fn cross_fs_migrate_publishes_and_cleans_up() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("legacy.img");
        let target = dir.path().join("target.img");
        tokio::fs::write(&legacy, b"payload-bytes").await.unwrap();

        cross_fs_migrate(&legacy, &target).await.unwrap();

        assert!(!legacy.exists());
        let got = tokio::fs::read(&target).await.unwrap();
        assert_eq!(got, b"payload-bytes");
        let mut tmp = target.as_os_str().to_owned();
        tmp.push(".migrate-tmp");
        assert!(!PathBuf::from(tmp).exists());
    }

    /// A leftover tmp from a previously interrupted attempt must not block a
    /// fresh migration — `cross_fs_migrate` overwrites it.
    #[tokio::test]
    async fn cross_fs_migrate_clobbers_stale_tmp() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("legacy.img");
        let target = dir.path().join("target.img");
        let mut tmp_os = target.as_os_str().to_owned();
        tmp_os.push(".migrate-tmp");
        let stale_tmp = PathBuf::from(tmp_os);

        tokio::fs::write(&legacy, b"new-data").await.unwrap();
        tokio::fs::write(&stale_tmp, b"stale-junk").await.unwrap();

        cross_fs_migrate(&legacy, &target).await.unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new-data");
        assert!(!legacy.exists());
        assert!(!stale_tmp.exists());
    }

    /// Both files exist (e.g. operator copied target back manually): use target
    /// and leave legacy in place untouched.
    #[tokio::test]
    async fn decide_prefers_target_when_both_exist() {
        let dir = tempdir().unwrap();
        let mount = dir.path().join("mnt-not-exist"); // unmounted
        let target = dir.path().join("target.img");
        let legacy = dir.path().join("legacy.img");
        tokio::fs::write(&target, b"t").await.unwrap();
        tokio::fs::write(&legacy, b"l").await.unwrap();

        let got = decide_effective_img_path(&mount, &target, &legacy)
            .await
            .unwrap();
        assert_eq!(got, target);
        assert!(
            legacy.exists(),
            "legacy must be left in place for manual cleanup"
        );
    }

    /// Cold path with only legacy: triggers migration; on a same-fs tempdir the
    /// migration succeeds, daemon ends up using the canonical target path.
    #[tokio::test]
    async fn decide_migrates_when_only_legacy_exists() {
        let dir = tempdir().unwrap();
        let mount = dir.path().join("mnt-not-exist");
        let target = dir.path().join("nested/target.img");
        let legacy = dir.path().join("legacy.img");
        tokio::fs::write(&legacy, b"old-data").await.unwrap();

        let got = decide_effective_img_path(&mount, &target, &legacy)
            .await
            .unwrap();
        assert_eq!(got, target);
        assert!(target.exists());
        assert!(!legacy.exists());
    }

    /// Cold path with neither file present: daemon proceeds as fresh install
    /// — return target so bootstrap can `create_sparse_image`.
    #[tokio::test]
    async fn decide_falls_through_to_target_on_fresh_install() {
        let dir = tempdir().unwrap();
        let mount = dir.path().join("mnt-not-exist");
        let target = dir.path().join("target.img");
        let legacy = dir.path().join("legacy.img");

        let got = decide_effective_img_path(&mount, &target, &legacy)
            .await
            .unwrap();
        assert_eq!(got, target);
    }

    /// Reports size; degrades `last_loop_device` to `None` when losetup is
    /// unavailable or no loop backs the file (e.g. unit tests on macOS).
    #[tokio::test]
    async fn loop_img_state_reports_size_and_tolerates_missing_losetup() {
        use ws_ckpt_common::backend::StorageBackend;

        let dir = tempdir().unwrap();
        let img = dir.path().join("data.img");
        let payload = vec![0u8; 4096];
        tokio::fs::write(&img, &payload).await.unwrap();

        let backend = super::BtrfsLoopBackend::new(dir.path().join("mnt"), img.clone());
        let st = backend.loop_img_state().await.expect("Some(state)");
        assert_eq!(st.img_path, img);
        assert_eq!(st.img_size_bytes, payload.len() as u64);
        assert!(st.last_loop_device.is_none());
    }

    /// Missing img → `None` (not yet bootstrapped).
    #[tokio::test]
    async fn loop_img_state_none_when_img_missing() {
        use ws_ckpt_common::backend::StorageBackend;

        let dir = tempdir().unwrap();
        let img = dir.path().join("never-created.img");
        let backend = super::BtrfsLoopBackend::new(dir.path().join("mnt"), img.clone());
        assert!(backend.loop_img_state().await.is_none());
    }

    #[test]
    fn normalize_free_loop_dev_accepts_plain_name() {
        assert_eq!(normalize_free_loop_dev("/dev/loop8"), Some("/dev/loop8"));
        assert_eq!(normalize_free_loop_dev("/dev/loop8\n"), Some("/dev/loop8"));
    }

    #[test]
    fn normalize_free_loop_dev_strips_lost_suffix() {
        // util-linux >= 2.40 appends " (lost)" when the node is missing.
        assert_eq!(
            normalize_free_loop_dev("/dev/loop8 (lost)"),
            Some("/dev/loop8")
        );
    }

    #[test]
    fn normalize_free_loop_dev_rejects_malformed_output() {
        for raw in [
            "",
            "   ",
            "garbage",
            "/dev/sda",
            "/dev/loop",
            "/dev/loop1p1",
        ] {
            assert_eq!(normalize_free_loop_dev(raw), None, "input: {raw:?}");
        }
    }

    #[test]
    fn parse_sysfs_dev_numbers_plain_and_shifted_minor() {
        assert_eq!(parse_sysfs_dev_numbers("7:8"), Some(("7", "8")));
        // max_part > 0 shifts the disk minor: loop1 can be 7:128, not 7:1.
        assert_eq!(parse_sysfs_dev_numbers(" 7:128 \n"), Some(("7", "128")));
    }

    #[test]
    fn parse_sysfs_dev_numbers_rejects_malformed_input() {
        for raw in ["", "7", "7:", ":8", "a:b", "7:8x"] {
            assert_eq!(parse_sysfs_dev_numbers(raw), None, "input: {raw:?}");
        }
    }

    #[test]
    fn loop_node_mknod_args_keep_restrictive_mode() {
        assert_eq!(
            loop_node_mknod_args("/dev/loop8", "7", "128"),
            ["-m", "0660", "/dev/loop8", "b", "7", "128"]
        );
    }
}
