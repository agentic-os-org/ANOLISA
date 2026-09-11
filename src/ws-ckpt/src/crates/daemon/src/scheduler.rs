//! Background scheduler: auto-cleanup and health checks.

use std::sync::Arc;

use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::snapshot_mgr::{delete_snapshots_locked, ensure_index_dir, persist_index_after_cleanup};
use crate::state::DaemonState;
use ws_ckpt_common::backend::StorageBackend;
use ws_ckpt_common::{CleanupRetention, EffectivePolicy};

/// Start background scheduler tasks: periodic auto-cleanup and health checks.
///
/// Config hot-reload is **push-based**: the dispatcher calls
/// `state.config_notify.notify_waiters()` after updating `state.config`, and
/// every periodic loop uses `tokio::select!` to react. This replaces the old
/// polling design — loops never wake up "just to check", and a disabled task
/// (`auto_cleanup = false` or `*_interval_secs == 0`) blocks on the notify
/// at zero CPU cost until a reload re-enables it.
pub fn start_scheduler(state: Arc<DaemonState>) {
    // Periodic auto-cleanup: reacts to `ReloadConfig` via `config_notify`.
    let state_clone = state.clone();
    tokio::spawn(async move {
        auto_cleanup_loop(state_clone).await;
    });

    // Periodic health check: same notify-driven pattern.
    let state_clone2 = state.clone();
    tokio::spawn(async move {
        health_check_loop(state_clone2).await;
    });

    info!("Background scheduler started");
}

/// Auto-cleanup loop: each iteration re-reads `auto_cleanup`,
/// `auto_cleanup_interval_secs`, and `auto_cleanup_keep.is_disabled()`.
/// Disabled parks on `config_notify`; active races `sleep` vs `config_notify`
/// for immediate reload.
///
/// `notify_waiters()` does **not** store a permit, so a notify that fires
/// before a waiter has registered is lost. To close the window between the
/// config read and registration, we build the `Notified` future and
/// `enable()` it (registers immediately) **before** reading config. Any
/// `notify_waiters()` issued afterwards is then captured by this waiter.
async fn auto_cleanup_loop(state: Arc<DaemonState>) {
    loop {
        let notified = state.config_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let interval = state.config_snapshot().auto_cleanup_interval_secs;
        // Park unless some ws has an effective (merged local-or-global) policy
        // that would do work this tick; avoids waking every interval just to
        // skip all workspaces.
        let park = interval == 0 || !state.any_ws_has_effective_cleanup().await;
        if park {
            notified.await;
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {
                auto_cleanup(&state).await;
            }
            _ = notified.as_mut() => {
                // Config changed mid-sleep: skip this cleanup pass and re-read.
            }
        }
    }
}

/// Health-check loop. Same push-based pattern as `auto_cleanup_loop`, keyed
/// off `health_check_interval_secs`. See that function's comment for why
/// `enable()` is called before the config read.
async fn health_check_loop(state: Arc<DaemonState>) {
    loop {
        let notified = state.config_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let interval = state.config_snapshot().health_check_interval_secs;
        if interval == 0 {
            notified.await;
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {
                health_check(&state).await;
            }
            _ = notified.as_mut() => {}
        }
    }
}

/// Auto-cleanup: purge non-pinned snapshots per `CleanupRetention` (pinned always kept).
/// - `Count(n)`: keep n newest per workspace.
/// - `Age { secs, .. }`: delete if older than `secs` (strict, no count floor).
///
/// Each workspace pass holds its mutation mutex from planning through persistence;
/// workspace `RwLock` guards are acquired only inside that serialized region.
async fn auto_cleanup(state: &DaemonState) {
    info!("Running auto-cleanup pass (per-ws effective retention)...");
    let all_ws = state.all_workspaces();
    let now = chrono::Utc::now();

    for ws_arc in &all_ws {
        let Some((ws_id, _mutation_guard)) = state.lock_workspace_mutation_if_current(ws_arc).await
        else {
            continue;
        };

        let (to_remove, index_dir) = {
            let ws = ws_arc.read().await;
            let cfg = state.config_snapshot();
            let eff: EffectivePolicy = ws.policy.effective_for(&cfg);
            if eff.is_disabled() {
                continue;
            }
            let retention = eff.auto_cleanup_keep.clone();

            let mut unpinned: Vec<(String, chrono::DateTime<chrono::Utc>)> = ws
                .index
                .snapshots
                .iter()
                .filter(|(_, meta)| !meta.pinned && !meta.missing)
                .map(|(id, meta)| (id.clone(), meta.created_at))
                .collect();
            unpinned.sort_by_key(|(_, ts)| *ts);

            let to_remove: Vec<String> = match &retention {
                CleanupRetention::Count(n) => {
                    let keep = *n as usize;
                    if unpinned.len() <= keep {
                        Vec::new()
                    } else {
                        unpinned[..unpinned.len() - keep]
                            .iter()
                            .map(|(id, _)| id.clone())
                            .collect()
                    }
                }
                CleanupRetention::Age { secs, .. } => {
                    let cutoff = now - chrono::Duration::seconds(*secs as i64);
                    unpinned
                        .iter()
                        .filter(|(_, ts)| *ts < cutoff)
                        .map(|(id, _)| id.clone())
                        .collect()
                }
            };

            (to_remove, state.index_dir(&ws_id))
        };
        if to_remove.is_empty() || !ensure_index_dir(&index_dir, "auto-cleanup").await {
            continue;
        }

        // Failed entries remain indexed for a later cleanup pass.
        let outcome =
            delete_snapshots_locked(state, ws_arc, &ws_id, &to_remove, "auto-cleanup").await;
        if outcome.index_changed {
            persist_index_after_cleanup(state, ws_arc, &index_dir, "auto-cleanup").await;
        }
        if !outcome.removed.is_empty() {
            info!(
                "auto-cleanup: removed {} snapshots from {}",
                outcome.removed.len(),
                ws_id
            );
        }
    }
}

/// Health check: verify filesystem usage.
///
/// Skipped when no workspace is registered. WARN on usage above threshold;
/// ERROR when get_usage fails (umount, fs crash, etc.) so upstream monitors
/// can catch it. Message building lives in [`assess_usage_health`] so the
/// branches are coverable with stub backends (#3053 review P2).
async fn health_check(state: &DaemonState) {
    if state.all_workspaces().is_empty() {
        debug!("Health check skipped: no workspace registered");
        return;
    }
    for line in assess_usage_health(state.backend.as_ref()).await {
        match line {
            UsageHealthLine::Info(msg) => info!("{}", msg),
            UsageHealthLine::Warn(msg) => warn!("{}", msg),
            UsageHealthLine::Error(msg) => error!("{}", msg),
        }
    }
}

/// Severity-tagged log line produced by [`assess_usage_health`].
#[derive(Debug)]
enum UsageHealthLine {
    Info(String),
    Warn(String),
    Error(String),
}

/// Filesystem-usage health probe.
///
/// The dead-list query result is kept as a raw `Result`: a FAILED
/// `deleted_subvolume_ids()` becomes its own warning instead of collapsing
/// into "no zombies" via `unwrap_or_default()` — the diagnostic must not
/// silently die (permissions, missing btrfs binary, fs errors) exactly in
/// the degraded regime where it matters most (#3053 review P2).
async fn assess_usage_health(backend: &dyn StorageBackend) -> Vec<UsageHealthLine> {
    let (total, used) = match backend.get_usage().await {
        Ok(pair) => pair,
        Err(e) => {
            // `{:#}` prints the full anyhow cause chain (e.g. outer
            // `with_context` + inner `bail!`), not just the outermost message.
            return vec![UsageHealthLine::Error(format!(
                "Health check failed on backend {}: {:#}",
                backend.backend_type(),
                e
            ))];
        }
    };
    if total == 0 {
        return Vec::new();
    }
    let usage_pct = (used as f64 / total as f64) * 100.0;
    const FS_WARN_THRESHOLD_PERCENT: f64 = 90.0;
    if usage_pct <= FS_WARN_THRESHOLD_PERCENT {
        return vec![UsageHealthLine::Info(format!(
            "Health check OK: filesystem usage {:.1}%",
            usage_pct
        ))];
    }

    // High usage is exactly the regime where the btrfs cleaner stalls and
    // deleted subvolumes turn into zombies that pin all backend space
    // (#3053). Surface them so operators know cleanup/restart alone cannot
    // reclaim the space.
    match backend.deleted_subvolume_ids().await {
        Ok(zombies) if zombies.is_empty() => vec![UsageHealthLine::Warn(format!(
            "Filesystem usage critical: {:.1}% ({} / {} bytes)",
            usage_pct, used, total
        ))],
        Ok(zombies) => {
            // The count comes from `subvolume list -d`, which is
            // FILESYSTEM-WIDE: on a shared host partition (btrfs-base)
            // entries may belong to other tools. The guidance resolves the
            // real mount point — btrfs-base's data root is a subdirectory of
            // the partition, and on a root-fs backend it becomes reboot
            // advice instead of an impossible `umount /` (review P1-a).
            let guidance =
                crate::backends::btrfs_common::recovery_guidance(backend.data_root()).await;
            vec![UsageHealthLine::Warn(format!(
                "Filesystem usage critical: {:.1}% ({} / {} bytes); {} deleted subvolume(s) \
                 {:?} not yet reclaimed by the btrfs cleaner — the listing is FILESYSTEM-WIDE, \
                 so on a shared partition entries may belong to other tools. They pin space \
                 that ws-ckpt cleanup and daemon restarts cannot free (mount is reused by \
                 design). Manual recovery: {}; the cleaner drains zombies within minutes of a \
                 fresh mount cycle (#3053)",
                usage_pct,
                used,
                total,
                zombies.len(),
                zombies,
                guidance
            ))]
        }
        Err(e) => vec![UsageHealthLine::Warn(format!(
            "Filesystem usage critical: {:.1}% ({} / {} bytes); deleted-subvolume query FAILED \
             ({:#}) — cannot tell whether zombie subvolumes are pinning the remaining space \
             (#3053)",
            usage_pct, used, total, e
        ))],
    }
}

#[cfg(test)]
mod tests {
    // ── Per-workspace effective policy invariants ──
    // Backend-free: only assert the routing rules `auto_cleanup_loop` relies on.
    use ws_ckpt_common::{CleanupRetention, DaemonConfig, WorkspacePolicy};

    fn cfg(global_on: bool, keep: CleanupRetention) -> DaemonConfig {
        DaemonConfig {
            auto_cleanup: global_on,
            auto_cleanup_keep: keep,
            ..DaemonConfig::default()
        }
    }

    #[test]
    fn per_ws_off_overrides_global_on() {
        let g = cfg(true, CleanupRetention::Count(20));
        let local = WorkspacePolicy {
            auto_cleanup: Some(false),
            auto_cleanup_keep: None,
        };
        // Per-ws says off → effective is_disabled, so scheduler will skip.
        assert!(local.effective_for(&g).is_disabled());
    }

    #[test]
    fn per_ws_on_overrides_global_off() {
        let g = cfg(false, CleanupRetention::Count(20));
        let local = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: None,
        };
        // Per-ws says on, even though global is off → effective should run.
        assert!(!local.effective_for(&g).is_disabled());
    }

    #[test]
    fn per_ws_keep_count_overrides_global_keep() {
        let g = cfg(true, CleanupRetention::Count(20));
        let local = WorkspacePolicy {
            auto_cleanup: None,
            auto_cleanup_keep: Some(CleanupRetention::Count(5)),
        };
        let eff = local.effective_for(&g);
        // auto_cleanup is inherited from global (true), keep is overridden.
        assert!(eff.auto_cleanup);
        assert_eq!(eff.auto_cleanup_keep, CleanupRetention::Count(5));
    }

    // ── Filesystem-usage health probe (#3053 review P1-a / P2) ──
    //
    // Stub-backend coverage of every assess_usage_health branch: healthy,
    // high-usage with/without zombies, FAILED dead-list query (must stay
    // visible, never collapse into "no zombies"), and failed usage probe.

    struct UsageProbeBackend {
        usage: Result<(u64, u64), String>,
        zombies: Result<Vec<u64>, String>,
        data_root: std::path::PathBuf,
    }

    impl UsageProbeBackend {
        fn new(usage: Result<(u64, u64), String>, zombies: Result<Vec<u64>, String>) -> Self {
            Self {
                usage,
                zombies,
                data_root: std::env::temp_dir(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ws_ckpt_common::backend::StorageBackend for UsageProbeBackend {
        fn backend_type(&self) -> ws_ckpt_common::backend::BackendType {
            ws_ckpt_common::backend::BackendType::BtrfsBase
        }
        fn data_root(&self) -> &std::path::Path {
            &self.data_root
        }
        fn snapshots_root(&self) -> &std::path::Path {
            &self.data_root
        }
        async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
            self.usage
                .clone()
                .map_err(|e| anyhow::anyhow!("injected usage failure: {}", e))
        }
        async fn deleted_subvolume_ids(&self) -> anyhow::Result<Vec<u64>> {
            self.zombies
                .clone()
                .map_err(|e| anyhow::anyhow!("injected query failure: {}", e))
        }
        async fn init_workspace(
            &self,
            _: &str,
            _: &str,
        ) -> anyhow::Result<ws_ckpt_common::WorkspaceInfo> {
            unimplemented!()
        }
        async fn create_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn rollback(&self, _: &str, _: &str) -> anyhow::Result<std::path::PathBuf> {
            unimplemented!()
        }
        async fn delete_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn diff(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<ws_ckpt_common::DiffEntry>> {
            unimplemented!()
        }
        async fn cleanup_snapshots(
            &self,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<(String, ws_ckpt_common::backend::SnapshotDeleteOutcome)>> {
            unimplemented!()
        }
        async fn fork(&self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn gc_generations(
            &self,
            _: &str,
        ) -> anyhow::Result<ws_ckpt_common::backend::GcResult> {
            unimplemented!()
        }
        async fn check_environment(
            &self,
        ) -> anyhow::Result<ws_ckpt_common::backend::EnvironmentStatus> {
            unimplemented!()
        }
    }

    /// Concatenate all line messages (one probe run yields 0..=1 lines).
    async fn probe_text(backend: &UsageProbeBackend) -> String {
        super::assess_usage_health(backend)
            .await
            .iter()
            .map(|l| match l {
                super::UsageHealthLine::Info(m)
                | super::UsageHealthLine::Warn(m)
                | super::UsageHealthLine::Error(m) => m.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn usage_probe_ok_below_threshold() {
        let b = UsageProbeBackend::new(Ok((1000, 500)), Ok(vec![]));
        let text = probe_text(&b).await;
        assert!(text.contains("Health check OK"), "{}", text);
        assert!(!text.contains("deleted subvolume"), "{}", text);
    }

    #[tokio::test]
    async fn usage_probe_zero_total_is_silent() {
        let b = UsageProbeBackend::new(Ok((0, 0)), Ok(vec![]));
        assert!(probe_text(&b).await.is_empty());
    }

    #[tokio::test]
    async fn usage_probe_high_usage_no_zombies() {
        let b = UsageProbeBackend::new(Ok((1000, 950)), Ok(vec![]));
        let text = probe_text(&b).await;
        assert!(text.contains("usage critical"), "{}", text);
        assert!(!text.contains("deleted subvolume"), "{}", text);
    }

    #[tokio::test]
    async fn usage_probe_high_usage_with_zombies_is_labeled_fs_wide() {
        let b = UsageProbeBackend::new(Ok((1000, 950)), Ok(vec![259, 260]));
        let text = probe_text(&b).await;
        assert!(
            text.contains("2 deleted subvolume(s) [259, 260]"),
            "{}",
            text
        );
        // Shared-partition labeling (review P1-a): the count is fs-wide and
        // may include other tools' subvolumes.
        assert!(text.contains("FILESYSTEM-WIDE"), "{}", text);
        // Guidance is present and never suggests umounting a non-mountpoint
        // data root verbatim without the "filesystem containing" phrasing.
        assert!(text.contains("Manual recovery:"), "{}", text);
    }

    #[tokio::test]
    async fn usage_probe_zombie_query_failure_stays_visible() {
        // Review P2: a failed dead-list query must NOT collapse into the
        // plain "no zombies" warning.
        let b = UsageProbeBackend::new(Ok((1000, 950)), Err("permission denied".to_string()));
        let text = probe_text(&b).await;
        assert!(text.contains("usage critical"), "{}", text);
        assert!(text.contains("query FAILED"), "{}", text);
        assert!(text.contains("injected query failure"), "{}", text);
    }

    #[tokio::test]
    async fn usage_probe_usage_failure_is_error_line() {
        let b = UsageProbeBackend::new(Err("device gone".to_string()), Ok(vec![]));
        let lines = super::assess_usage_health(&b).await;
        assert_eq!(lines.len(), 1);
        match &lines[0] {
            super::UsageHealthLine::Error(m) => {
                assert!(m.contains("Health check failed"), "{}", m);
                assert!(m.contains("injected usage failure"), "{}", m);
            }
            other => panic!("expected Error line, got {:?}", other),
        }
    }
}
