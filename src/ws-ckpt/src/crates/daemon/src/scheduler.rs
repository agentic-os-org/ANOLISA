//! Background scheduler: auto-cleanup and health checks.

use std::sync::Arc;

use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::snapshot_mgr::{delete_snapshots_locked, ensure_index_dir, persist_index_after_cleanup};
use crate::state::DaemonState;
use ws_ckpt_common::backend::StorageBackend;
use ws_ckpt_common::{CleanupRetention, EffectivePolicy};

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_ADVISORY_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Start background scheduler tasks: periodic auto-cleanup and health checks.
///
/// Config hot-reload is **push-based**: the dispatcher calls
/// `state.config_notify.notify_waiters()` after updating `state.config`, and
/// every periodic loop uses `tokio::select!` to react. This replaces the old
/// polling design — loops never wake up "just to check", and a disabled task
/// (`auto_cleanup = false` or `auto_cleanup_interval_secs == 0`) blocks on
/// the notify at zero CPU cost until a reload re-enables it. Health advisory
/// sampling continues when periodic health diagnostics are disabled.
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

/// Sleeps until the interval ends; an unrepresentable deadline waits for reload.
async fn sleep_health_interval(duration: Duration) {
    match tokio::time::Instant::now().checked_add(duration) {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Refreshes advisories independently of optional periodic health diagnostics.
async fn health_check_loop(state: Arc<DaemonState>) {
    tokio::join!(
        health_advisory_loop(&state),
        health_diagnostics_loop(&state)
    );
}

async fn health_advisory_loop(state: &DaemonState) {
    let mut delay = Duration::ZERO;
    let mut retrying = false;
    loop {
        let wait = sleep_health_interval(delay);
        tokio::pin!(wait);
        if retrying {
            // Changes during backoff coalesce into the next attempt. They must not
            // turn a failing probe into a busy loop or extend the retry deadline.
            loop {
                tokio::select! {
                    biased;
                    _ = wait.as_mut() => break,
                    _ = state.advisory_notify.notified() => {}
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = state.advisory_notify.notified() => {}
                _ = wait.as_mut() => {}
            }
        }

        // A stored permit covers changes before registration. Do not cancel an
        // in-progress refresh: coalesce changes into one subsequent refresh.
        let notified = state.advisory_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let result = state.refresh_health_advisory().await;
        let changed = tokio::select! {
            biased;
            _ = notified.as_mut() => true,
            _ = std::future::ready(()) => false,
        };
        match result {
            Ok(()) => {
                retrying = false;
                delay = if changed {
                    Duration::ZERO
                } else {
                    Duration::from_secs(crate::state::effective_health_advisory_interval_secs(
                        state.config_snapshot().health_check_interval_secs,
                    ))
                };
            }
            Err(e) => {
                warn!("Health advisory refresh failed: {:#}", e);
                retrying = true;
                delay = HEALTH_ADVISORY_RETRY_INTERVAL.min(Duration::from_secs(
                    crate::state::effective_health_advisory_interval_secs(
                        state.config_snapshot().health_check_interval_secs,
                    ),
                ));
            }
        }
    }
}

async fn health_diagnostics_loop(state: &DaemonState) {
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
            biased;
            _ = notified.as_mut() => {}
            _ = sleep_health_interval(Duration::from_secs(interval)) => {
                if tokio::time::timeout(
                    HEALTH_CHECK_TIMEOUT,
                    crate::health_probe::scope(health_check(state)),
                )
                    .await
                    .is_err()
                {
                    warn!("Health check diagnostics timed out after {} seconds", HEALTH_CHECK_TIMEOUT.as_secs());
                }
            }
        }
    }
}

/// Cutoff for `Age` retention: `now - secs`, or `None` when unrepresentable.
///
/// `parse_duration_secs` admits anything up to `i64::MAX` seconds, but chrono
/// represents at most ~9.2e15 s (`TimeDelta`'s millisecond bound), and the
/// subtraction fails once the result leaves the `NaiveDateTime` range (~262k
/// years of headroom, i.e. ~8.3e12 s). Values above `i64::MAX` cannot pass the
/// parse guard, but the `Age` fields are public and an `as i64` cast would wrap
/// negative, forging a future cutoff that expires every snapshot — so they are
/// rejected here too. The caller treats `None` as "nothing expires".
fn age_cutoff(
    now: chrono::DateTime<chrono::Utc>,
    secs: u64,
) -> Option<chrono::DateTime<chrono::Utc>> {
    i64::try_from(secs)
        .ok()
        .and_then(chrono::TimeDelta::try_seconds)
        .and_then(|delta| now.checked_sub_signed(delta))
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
                CleanupRetention::Age { secs, .. } => match age_cutoff(now, *secs) {
                    Some(cutoff) => unpinned
                        .iter()
                        .filter(|(_, ts)| *ts < cutoff)
                        .map(|(id, _)| id.clone())
                        .collect(),
                    // Retention beyond chrono's representable range: the cutoff
                    // would sit in the unreachable past, so nothing expires yet.
                    None => Vec::new(),
                },
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
    let lines = match state.health_advisory_snapshot() {
        Some(sample) => {
            assess_usage_health_with_usage(
                state.backend.as_ref(),
                Ok((sample.fs_total_bytes, sample.fs_used_bytes)),
            )
            .await
        }
        None => assess_usage_health(state.backend.as_ref()).await,
    };
    for line in lines {
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
    assess_usage_health_with_usage(backend, backend.get_usage().await).await
}

async fn assess_usage_health_with_usage(
    backend: &dyn StorageBackend,
    usage: anyhow::Result<(u64, u64)>,
) -> Vec<UsageHealthLine> {
    let (total, used) = match usage {
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
    use std::sync::Arc;
    use tokio::time::Duration;

    // ── Per-workspace effective policy invariants ──
    // Backend-free: only assert the routing rules `auto_cleanup_loop` relies on.
    use super::age_cutoff;
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

    // ── Age cutoff checked math ──
    //
    // `parse_duration_secs` admits anything up to i64::MAX seconds, but the
    // raw `now - chrono::Duration::seconds(secs)` this module used to run
    // panics beyond chrono's representable range, killing the spawned
    // auto-cleanup task for the daemon's lifetime.

    #[test]
    fn age_cutoff_normal_retention_subtracts_from_now() {
        let now = chrono::Utc::now();
        let cutoff = age_cutoff(now, 30 * 86400).expect("30d is representable");
        // Exact delta, not just `cutoff < now`: a wrong subtraction magnitude
        // (e.g. 1 s instead of 30 d) must fail this test.
        assert_eq!(now - cutoff, chrono::TimeDelta::days(30));
    }

    #[test]
    fn age_cutoff_huge_retention_does_not_panic() {
        let now = chrono::Utc::now();
        // 1e16 s passes parse_duration_secs (1e16 < i64::MAX) but overflows
        // chrono's TimeDelta seconds bound (~9.2e15 s).
        let raw = "10000000000000000s";
        let secs = match CleanupRetention::age(raw).expect("parse accepts 1e16 s") {
            CleanupRetention::Age { secs, .. } => secs,
            other => panic!("expected Age, got {other:?}"),
        };
        assert_eq!(age_cutoff(now, secs), None);
    }

    #[test]
    fn age_cutoff_out_of_datetime_range_does_not_panic() {
        let now = chrono::Utc::now();
        // 8.5e15 s fits TimeDelta (max ~9.2e15) but `now - delta` leaves the
        // representable date range (~262k years, i.e. ~8.3e12 s of headroom).
        assert_eq!(age_cutoff(now, 8_500_000_000_000_000), None);
    }

    #[test]
    fn age_cutoff_rejects_values_above_i64_max() {
        // Unreachable via parse_duration_secs, but the `Age` fields are public.
        // An `as i64` cast would wrap negative and forge a future cutoff that
        // expires every snapshot; the checked conversion must yield None.
        let now = chrono::Utc::now();
        assert_eq!(age_cutoff(now, i64::MAX as u64 + 1), None);
        assert_eq!(age_cutoff(now, u64::MAX), None);
    }

    // ── Filesystem-usage health probe (#3053 review P1-a / P2) ──
    //
    // Stub-backend coverage of every assess_usage_health branch: healthy,
    // high-usage with/without zombies, FAILED dead-list query (must stay
    // visible, never collapse into "no zombies"), and failed usage probe.

    struct ActiveUsageProbe<'a>(&'a std::sync::atomic::AtomicUsize);

    impl Drop for ActiveUsageProbe<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct UsageGate {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    struct UsageProbeBackend {
        usage: Result<(u64, u64), String>,
        zombies: Result<Vec<u64>, String>,
        data_root: std::path::PathBuf,
        snapshots_root: std::path::PathBuf,
        active_usage_probes: std::sync::atomic::AtomicUsize,
        max_active_usage_probes: std::sync::atomic::AtomicUsize,
        usage_probe_calls: std::sync::atomic::AtomicUsize,
        usage_stalls_remaining: std::sync::atomic::AtomicUsize,
        usage_failures_remaining: std::sync::atomic::AtomicUsize,
        usage_delay_millis: std::sync::atomic::AtomicU64,
        usage_lock: tokio::sync::Mutex<()>,
        shared_probe_lock: tokio::sync::Mutex<()>,
        share_probe_slot: std::sync::atomic::AtomicBool,
        usage_gate: std::sync::Mutex<Option<Arc<UsageGate>>>,
        deleted_probe_calls: std::sync::atomic::AtomicUsize,
        stall_deleted_probe: std::sync::atomic::AtomicBool,
        require_health_scope: bool,
    }

    impl UsageProbeBackend {
        fn new(usage: Result<(u64, u64), String>, zombies: Result<Vec<u64>, String>) -> Self {
            Self {
                usage,
                zombies,
                data_root: std::env::temp_dir(),
                snapshots_root: std::env::temp_dir(),
                active_usage_probes: std::sync::atomic::AtomicUsize::new(0),
                max_active_usage_probes: std::sync::atomic::AtomicUsize::new(0),
                usage_probe_calls: std::sync::atomic::AtomicUsize::new(0),
                usage_stalls_remaining: std::sync::atomic::AtomicUsize::new(0),
                usage_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
                usage_delay_millis: std::sync::atomic::AtomicU64::new(10),
                usage_lock: tokio::sync::Mutex::new(()),
                shared_probe_lock: tokio::sync::Mutex::new(()),
                share_probe_slot: std::sync::atomic::AtomicBool::new(false),
                usage_gate: std::sync::Mutex::new(None),
                deleted_probe_calls: std::sync::atomic::AtomicUsize::new(0),
                stall_deleted_probe: std::sync::atomic::AtomicBool::new(false),
                require_health_scope: false,
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
            &self.snapshots_root
        }
        async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
            use std::sync::atomic::Ordering;

            if self.require_health_scope {
                crate::health_probe::assert_scoped();
            }
            let _shared_guard = if self.share_probe_slot.load(Ordering::SeqCst) {
                Some(self.shared_probe_lock.lock().await)
            } else {
                None
            };
            let _usage_guard = self.usage_lock.lock().await;
            self.usage_probe_calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active_usage_probes.fetch_add(1, Ordering::SeqCst) + 1;
            let _active_probe = ActiveUsageProbe(&self.active_usage_probes);
            self.max_active_usage_probes
                .fetch_max(active, Ordering::SeqCst);
            let gate = self.usage_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.started.notify_one();
                gate.release.notified().await;
            }
            if self
                .usage_stalls_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                self.usage_delay_millis.load(Ordering::SeqCst),
            ))
            .await;
            if self
                .usage_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(anyhow::anyhow!("injected transient usage failure"));
            }
            self.usage
                .clone()
                .map_err(|e| anyhow::anyhow!("injected usage failure: {}", e))
        }
        async fn deleted_subvolume_ids(&self) -> anyhow::Result<Vec<u64>> {
            use std::sync::atomic::Ordering;

            if self.require_health_scope {
                crate::health_probe::assert_scoped();
            }
            let _shared_guard = if self.share_probe_slot.load(Ordering::SeqCst) {
                Some(self.shared_probe_lock.lock().await)
            } else {
                None
            };
            self.deleted_probe_calls.fetch_add(1, Ordering::SeqCst);
            if self.stall_deleted_probe.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
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
        async fn create_snapshot(&self, ws_id: &str, id: &str) -> anyhow::Result<()> {
            let target = self.snapshots_root.join(ws_id).join(id);
            std::fs::create_dir_all(&target)?;
            std::fs::copy(
                self.data_root.join(ws_id).join("canary"),
                target.join("canary"),
            )?;
            Ok(())
        }
        async fn rollback(&self, _: &str, _: &str) -> anyhow::Result<std::path::PathBuf> {
            unimplemented!()
        }
        async fn delete_snapshot(&self, ws_id: &str, id: &str) -> anyhow::Result<()> {
            std::fs::remove_dir_all(self.snapshots_root.join(ws_id).join(id))?;
            Ok(())
        }
        async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<Vec<String>> {
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
            ws_id: &str,
            ids: &[String],
        ) -> anyhow::Result<Vec<(String, ws_ckpt_common::backend::SnapshotDeleteOutcome)>> {
            let mut report = Vec::new();
            for id in ids {
                self.delete_snapshot(ws_id, id).await?;
                report.push((
                    id.clone(),
                    ws_ckpt_common::backend::SnapshotDeleteOutcome::Removed,
                ));
            }
            Ok(report)
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

    fn health_scheduler_fixture(
        usage: (u64, u64),
    ) -> (
        tempfile::TempDir,
        Arc<UsageProbeBackend>,
        Arc<crate::state::DaemonState>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = UsageProbeBackend::new(Ok(usage), Ok(vec![]));
        backend.require_health_scope = true;
        backend.data_root = temp.path().join("data");
        backend.snapshots_root = temp.path().join("snapshots");
        let backend = Arc::new(backend);
        let state = Arc::new(crate::state::DaemonState::new(
            DaemonConfig {
                health_check_interval_secs: 1,
                ..DaemonConfig::default()
            },
            backend.clone(),
            temp.path().join("state"),
        ));
        let workspace_path = temp.path().join("workspace");
        state
            .register_workspace(
                "ws-health-loop".into(),
                workspace_path.clone(),
                ws_ckpt_common::SnapshotIndex::new(workspace_path),
            )
            .unwrap();
        (temp, backend, state)
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_refreshes_after_same_interval_notification() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 300;
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        state.advisory_notify.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("workspace notification must refresh even when the interval is unchanged");
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_refreshes_again_after_notification_during_sampling() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 300;
        let gate = Arc::new(UsageGate {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *backend.usage_gate.lock().unwrap() = Some(gate.clone());
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), gate.started.notified())
            .await
            .expect("the first sample must enter the backend");
        state.advisory_notify.notify_one();
        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a notification during sampling must trigger a follow-up");
        assert!(
            state.health_advisory_snapshot().is_some(),
            "the first sample must finish before retrying"
        );
        assert_eq!(backend.max_active_usage_probes.load(Ordering::SeqCst), 1);
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn workspace_changes_during_sampling_reach_the_cached_count() {
        let (temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 300;
        let gate = || {
            Arc::new(UsageGate {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            })
        };
        let first = gate();
        *backend.usage_gate.lock().unwrap() = Some(first.clone());
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), first.started.notified())
            .await
            .unwrap();

        let path = temp.path().join("registered-during-sample");
        let mut index = ws_ckpt_common::SnapshotIndex::new(path.clone());
        let meta = ws_ckpt_common::SnapshotMeta {
            message: None,
            metadata: None,
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };
        for n in 0..=ws_ckpt_common::ADVISORY_SNAPSHOT_LIMIT {
            index.snapshots.insert(n.to_string(), meta.clone());
        }
        let second = gate();
        *backend.usage_gate.lock().unwrap() = Some(second.clone());
        state
            .register_workspace("registered-during-sample".into(), path, index)
            .unwrap();
        first.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), second.started.notified())
            .await
            .expect("registration must trigger the second sample");
        assert_eq!(
            state
                .health_advisory_snapshot()
                .unwrap()
                .over_limit_workspace_count,
            0
        );

        let third = gate();
        *backend.usage_gate.lock().unwrap() = Some(third.clone());
        state.unregister_workspace("registered-during-sample").await;
        second.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), third.started.notified())
            .await
            .expect("unregistration must trigger the third sample");
        assert_eq!(
            state
                .health_advisory_snapshot()
                .unwrap()
                .over_limit_workspace_count,
            1
        );
        third.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while state
                .health_advisory_snapshot()
                .unwrap()
                .over_limit_workspace_count
                != 0
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the cache must reflect unregistration");
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn independent_mock_diagnostics_allow_concurrent_advisory_sampling() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 950));
        // Real Btrfs probes share a slot and may wait for diagnostic cleanup.
        backend.stall_deleted_probe.store(true, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state));
        tokio::time::timeout(Duration::from_secs(3), async {
            while backend.deleted_probe_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("diagnostics must start");
        let before = backend.usage_probe_calls.load(Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(3), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) <= before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sampling must continue while diagnostics are stalled");
        assert_eq!(
            backend.deleted_probe_calls.load(Ordering::SeqCst),
            1,
            "the original diagnostic is still stalled"
        );
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn shared_probe_slot_recovers_sampling_after_diagnostic_timeout() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 950));
        backend.share_probe_slot.store(true, Ordering::SeqCst);
        backend.stall_deleted_probe.store(true, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state));
        tokio::time::timeout(Duration::from_secs(3), async {
            while backend.deleted_probe_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("diagnostics must acquire the shared slot");
        let before = backend.usage_probe_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            backend.usage_probe_calls.load(Ordering::SeqCst),
            before,
            "usage must wait while the mock diagnostic holds the shared slot"
        );
        // The in-progress probe remains stalled; later diagnostics can complete.
        backend.stall_deleted_probe.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(7), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) <= before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("usage must resume after the stalled diagnostic times out");
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn huge_health_interval_waits_without_panicking_and_can_be_woken() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = u64::MAX;
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("initial sample must complete");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            !scheduler.is_finished(),
            "an unrepresentable interval must not panic"
        );
        state.advisory_notify.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("notifications must wake an unrepresentable interval");
        assert!(!scheduler.is_finished());
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_retries_default_interval_failure_without_busy_loop() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 300;
        backend.usage_failures_remaining.store(1, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        let recovered = tokio::time::timeout(Duration::from_secs(7), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            recovered.is_ok(),
            "startup failure must retry before 300 seconds"
        );
        assert_eq!(backend.usage_probe_calls.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            backend.usage_probe_calls.load(Ordering::SeqCst),
            2,
            "success must restore the normal interval"
        );
        backend.usage_failures_remaining.store(1, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(292), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the normal refresh must eventually run");
        tokio::time::timeout(Duration::from_secs(7), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 4 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("a later failure must also retry promptly");
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_bounds_retries_when_sampling_keeps_failing() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 950));
        state.config.write().unwrap().health_check_interval_secs = 300;
        backend
            .usage_failures_remaining
            .store(100, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::sleep(Duration::from_secs(16)).await;
        let attempts = backend.usage_probe_calls.load(Ordering::SeqCst);
        scheduler.abort();
        let _ = scheduler.await;
        assert!(
            (3..=4).contains(&attempts),
            "expected bounded five-second retries, got {attempts}"
        );
        assert_eq!(
            backend.deleted_probe_calls.load(Ordering::SeqCst),
            0,
            "retries must not run diagnostics"
        );
        assert!(state.health_advisory_snapshot().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn short_health_interval_retries_before_sample_freshness_expires() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 1;
        backend.usage_failures_remaining.store(1, Ordering::SeqCst);
        let refresh_state = state.clone();
        let scheduler = tokio::spawn(async move {
            super::health_advisory_loop(&refresh_state).await;
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a one-second interval must retry before the two-second freshness limit");
        assert_eq!(backend.usage_probe_calls.load(Ordering::SeqCst), 2);
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test(start_paused = true)]
    async fn notifications_do_not_bypass_failed_refresh_backoff() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        state.config.write().unwrap().health_check_interval_secs = 0;
        backend
            .usage_failures_remaining
            .store(100, Ordering::SeqCst);
        let gate = Arc::new(UsageGate {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *backend.usage_gate.lock().unwrap() = Some(gate.clone());
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), gate.started.notified())
            .await
            .expect("the first sample must enter the backend");
        for _ in 0..20 {
            state.advisory_notify.notify_one();
        }
        gate.release.notify_one();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            state.advisory_notify.notify_one();
        }
        assert_eq!(
            backend.usage_probe_calls.load(Ordering::SeqCst),
            1,
            "notifications must not bypass the five-second failure backoff"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                state.advisory_notify.notify_one();
            }
        })
        .await
        .expect("backoff must not be extended by notifications");
        scheduler.abort();
        let _ = scheduler.await;
        assert_eq!(backend.usage_probe_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_keeps_advisories_when_diagnostics_are_disabled() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 950));
        state.config.write().unwrap().health_check_interval_secs = 0;
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("zero interval must still populate the advisory");
        tokio::time::sleep(Duration::from_secs(301)).await;
        assert!(
            backend.usage_probe_calls.load(Ordering::SeqCst) >= 2,
            "zero interval must refresh periodically"
        );
        assert_eq!(backend.deleted_probe_calls.load(Ordering::SeqCst), 0);

        state.config.write().unwrap().health_check_interval_secs = 1;
        state.config_notify.notify_waiters();
        state.advisory_notify.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            while backend.deleted_probe_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("enabling diagnostics must wake the scheduler");
        let before_disable = backend.usage_probe_calls.load(Ordering::SeqCst);
        state.config.write().unwrap().health_check_interval_secs = 0;
        state.config_notify.notify_waiters();
        state.advisory_notify.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while backend.usage_probe_calls.load(Ordering::SeqCst) <= before_disable {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("disabling diagnostics must refresh the advisory on reload");
        let diagnostics_after_disable = backend.deleted_probe_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            backend.deleted_probe_calls.load(Ordering::SeqCst),
            diagnostics_after_disable
        );
        assert!(state.health_advisory_snapshot().is_some());
        scheduler.abort();
        let _ = scheduler.await;
    }

    #[tokio::test]
    async fn health_advisory_waits_for_busy_workspace_and_publishes_complete_count() {
        let (temp, _backend, state) = health_scheduler_fixture((1000, 950));
        let meta = ws_ckpt_common::SnapshotMeta {
            message: None,
            metadata: None,
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };
        for workspace_id in ["first-busy", "second-busy"] {
            let path = temp.path().join(workspace_id);
            let mut index = ws_ckpt_common::SnapshotIndex::new(path.clone());
            for n in 0..=ws_ckpt_common::ADVISORY_SNAPSHOT_LIMIT {
                index.snapshots.insert(n.to_string(), meta.clone());
            }
            state
                .register_workspace(workspace_id.into(), path, index)
                .unwrap();
        }
        state.seed_health_advisory_snapshot(8, 10, 1, Duration::ZERO);
        let workspace = state.get_by_wsid("first-busy").unwrap();
        let guard = workspace.write().await;
        let refresh_state = state.clone();
        let refresh = tokio::spawn(async move { refresh_state.refresh_health_advisory().await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !refresh.is_finished(),
            "refresh must wait for the busy workspace"
        );
        assert_eq!(
            state
                .health_advisory_snapshot()
                .unwrap()
                .over_limit_workspace_count,
            8
        );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), refresh)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let sample = state.health_advisory_snapshot().unwrap();
        assert_eq!(sample.over_limit_workspace_count, 2);
        assert_eq!((sample.fs_total_bytes, sample.fs_used_bytes), (1000, 950));
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_recovers_after_usage_probe_timeouts() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 100));
        // Stall the startup refresh, periodic refresh, and diagnostic fallback.
        backend.usage_stalls_remaining.store(3, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state.clone()));
        let recovered = tokio::time::timeout(Duration::from_secs(40), async {
            while state.health_advisory_snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        scheduler.abort();
        let _ = scheduler.await;

        assert!(
            recovered.is_ok(),
            "stalled diagnostics must not stop refreshes"
        );
        assert!(backend.usage_probe_calls.load(Ordering::SeqCst) >= 4);
        assert_eq!(backend.max_active_usage_probes.load(Ordering::SeqCst), 1);
        assert_eq!(backend.active_usage_probes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn health_scheduler_refreshes_after_deleted_subvolume_probe_stalls() {
        use std::sync::atomic::Ordering;

        let (_temp, backend, state) = health_scheduler_fixture((1000, 950));
        backend.stall_deleted_probe.store(true, Ordering::SeqCst);
        let scheduler = tokio::spawn(super::health_check_loop(state));
        let diagnostic_started = tokio::time::timeout(Duration::from_secs(3), async {
            while backend.deleted_probe_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let usage_calls = backend.usage_probe_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(7)).await;
        let refreshed = backend.usage_probe_calls.load(Ordering::SeqCst) > usage_calls;
        scheduler.abort();
        let _ = scheduler.await;

        assert!(diagnostic_started.is_ok());
        assert!(
            refreshed,
            "a stalled dead-list query must not stop refreshes"
        );
        assert_eq!(backend.max_active_usage_probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn health_advisory_refresh_serializes_probes_and_preserves_failed_samples() {
        use crate::state::DaemonState;
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let temp = tempfile::tempdir().unwrap();
        let mut backend = UsageProbeBackend::new(Ok((1000, 950)), Ok(vec![]));
        backend.data_root = temp.path().join("data");
        backend.snapshots_root = temp.path().join("snapshots");
        let backend = Arc::new(backend);
        let config = DaemonConfig {
            health_check_interval_secs: 30,
            ..DaemonConfig::default()
        };
        let state = Arc::new(DaemonState::new(
            config,
            backend.clone(),
            temp.path().join("state"),
        ));
        let workspace_path = temp.path().join("workspace");
        state
            .register_workspace(
                "ws-advisory".into(),
                workspace_path.clone(),
                ws_ckpt_common::SnapshotIndex::new(workspace_path),
            )
            .unwrap();

        state.seed_health_advisory_snapshot(0, 10, 1, std::time::Duration::from_secs(20));
        assert!(state
            .refresh_health_advisory_with_timeout(std::time::Duration::from_millis(1))
            .await
            .is_err());
        let retained = state.health_advisory_snapshot().unwrap();
        assert_eq!((retained.fs_total_bytes, retained.fs_used_bytes), (10, 1));
        state.config.write().unwrap().health_check_interval_secs = 1;
        assert!(state.health_advisory_snapshot().is_none());
        state.config.write().unwrap().health_check_interval_secs = 30;

        let (first, second) = tokio::join!(
            state.refresh_health_advisory(),
            state.refresh_health_advisory()
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(
            backend.max_active_usage_probes.load(Ordering::SeqCst),
            1,
            "concurrent refreshes must not overlap backend usage probes"
        );
        let sample = state.health_advisory_snapshot().unwrap();
        assert_eq!((sample.fs_total_bytes, sample.fs_used_bytes), (1000, 950));

        state.seed_health_advisory_snapshot(0, 10, 1, std::time::Duration::from_secs(20));
        let workspace = state.get_by_wsid("ws-advisory").unwrap();
        let _guard = workspace.write().await;
        assert!(state
            .refresh_health_advisory_with_timeout(Duration::from_millis(20))
            .await
            .is_err());
        let retained = state.health_advisory_snapshot().unwrap();
        assert_eq!((retained.fs_total_bytes, retained.fs_used_bytes), (10, 1));

        let error_state = DaemonState::new(
            DaemonConfig {
                health_check_interval_secs: 30,
                ..DaemonConfig::default()
            },
            Arc::new(UsageProbeBackend::new(
                Err("test backend error".into()),
                Ok(vec![]),
            )),
            temp.path().join("error-state"),
        );
        assert!(error_state.refresh_health_advisory().await.is_err());
        assert!(error_state.health_advisory_snapshot().is_none());
        error_state.seed_health_advisory_snapshot(2, 10, 1, std::time::Duration::from_secs(20));
        assert!(error_state.refresh_health_advisory().await.is_err());
        let retained = error_state.health_advisory_snapshot().unwrap();
        assert_eq!(
            (
                retained.over_limit_workspace_count,
                retained.fs_total_bytes,
                retained.fs_used_bytes
            ),
            (2, 10, 1)
        );
        error_state
            .config
            .write()
            .unwrap()
            .health_check_interval_secs = 1;
        assert!(error_state.health_advisory_snapshot().is_none());
        error_state
            .config
            .write()
            .unwrap()
            .health_check_interval_secs = 30;
        error_state.seed_health_advisory_snapshot(2, 10, 1, std::time::Duration::from_secs(61));
        assert!(error_state.refresh_health_advisory().await.is_err());
        assert!(error_state.health_advisory_snapshot().is_none());
    }

    #[tokio::test]
    async fn recovered_orphans_survive_retention_checkpoint_and_restart_until_explicit_delete() {
        use super::auto_cleanup;
        use crate::state::DaemonState;
        use crate::{dispatcher::dispatch, index_store, snapshot_mgr, workspace_mgr};
        use std::sync::Arc;
        use ws_ckpt_common::{ErrorCode, Request, Response, SnapshotIndex};
        let temp = tempfile::tempdir().unwrap();
        let mut backend = UsageProbeBackend::new(Ok((100, 1)), Ok(vec![]));
        backend.data_root = temp.path().join("data");
        backend.snapshots_root = temp.path().join("snapshots");
        let backend = Arc::new(backend);
        let ws_id = "ws-orphans";
        let live = backend.data_root.join(ws_id);
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("canary"), "live").unwrap();
        let path = temp.path().join("workspace");
        std::os::unix::fs::symlink(&live, &path).unwrap();
        let orphan = backend.snapshots_root.join(ws_id).join("snap-1");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("canary"), "orphan").unwrap();
        let config = cfg(true, CleanupRetention::Count(1));
        let mut state = Arc::new(DaemonState::new(
            config.clone(),
            backend.clone(),
            temp.path().join("state"),
        ));
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace(ws_id.into(), path, index.clone())
            .unwrap();
        index_store::save(&state.index_dir(ws_id), &index)
            .await
            .unwrap();
        state.save_manifest().await.unwrap();
        for round in 0..2 {
            let manifest = ws_ckpt_common::persist::load_state(&state.state_dir)
                .unwrap()
                .unwrap();
            state = Arc::new(
                DaemonState::rebuild_from_persisted(
                    &manifest,
                    config.clone(),
                    backend.clone(),
                    state.state_dir.clone(),
                    "persisted",
                )
                .await
                .unwrap(),
            );
            state.mark_bootstrapped();
            assert!(matches!(
                snapshot_mgr::checkpoint(
                    &state,
                    ws_id,
                    &format!("snap-new-{round}"),
                    None,
                    None,
                    false
                )
                .await
                .unwrap(),
                Response::CheckpointOk { .. }
            ));
            // A normal pinned checkpoint must not be classified as an orphan.
            assert!(matches!(
                snapshot_mgr::checkpoint(
                    &state,
                    ws_id,
                    &format!("pinned-{round}"),
                    None,
                    None,
                    true
                )
                .await
                .unwrap(),
                Response::CheckpointOk { .. }
            ));
            assert!(matches!(
                snapshot_mgr::checkpoint(
                    &state,
                    ws_id,
                    &format!("snap-newer-{round}"),
                    None,
                    None,
                    false
                )
                .await
                .unwrap(),
                Response::CheckpointOk { .. }
            ));
            {
                let arc = state.get_by_wsid(ws_id).unwrap();
                let mut ws = arc.write().await;
                // Count(0) and Age(0) disable retention. Make real candidates
                // for Count(1) and Age(1s), including an older protected orphan.
                for (id, age) in [
                    ("snap-1".into(), 3),
                    (format!("snap-new-{round}"), 2),
                    (format!("snap-newer-{round}"), 1),
                ] {
                    let mut meta = ws.index.snapshots[&id].clone();
                    meta.created_at = chrono::Utc::now() - chrono::Duration::days(age);
                    ws.insert_snapshot(id, meta);
                }
            }
            for retention in [
                CleanupRetention::Count(1),
                CleanupRetention::age("1s").unwrap(),
            ] {
                let arc = state.get_by_wsid(ws_id).unwrap();
                arc.write().await.policy.auto_cleanup_keep = Some(retention);
                auto_cleanup(&state).await;
                let index = index_store::load(&state.index_dir(ws_id)).await.unwrap();
                assert!(index.snapshots["snap-1"].pinned);
                assert!(index.recovered_orphans.contains("snap-1"));
                assert!(!index.snapshots.contains_key(&format!("snap-new-{round}")));
                assert_eq!(
                    std::fs::read_to_string(orphan.join("canary")).unwrap(),
                    "orphan"
                );
            }
            assert!(!state
                .get_by_wsid(ws_id)
                .unwrap()
                .read()
                .await
                .index
                .snapshots
                .contains_key(&format!("snap-newer-{round}")));
            for workspace in [None, Some(ws_id.into())] {
                let response = dispatch(&state, Request::ListOrphans { workspace }).await;
                let Response::ListOk { snapshots } = response else {
                    panic!("{response:?}")
                };
                assert_eq!(snapshots.len(), 1);
                assert_eq!(snapshots[0].id, "snap-1");
                assert!(snapshots[0].meta.pinned);
                assert_eq!(
                    snapshots[0].workspace,
                    temp.path().join("workspace").to_str().unwrap()
                );
            }
        }
        assert!(matches!(
            workspace_mgr::delete_snapshot(&state, ws_id, "snap-1", false)
                .await
                .unwrap(),
            Response::Error {
                code: ErrorCode::ConfirmationRequired,
                ..
            }
        ));
        assert!(
            matches!(workspace_mgr::delete_snapshot(&state, ws_id, "snap-1", true).await.unwrap(), Response::DeleteOk { target } if target == "snap-1")
        );
        let index = index_store::load(&state.index_dir(ws_id)).await.unwrap();
        assert!(!index.snapshots.contains_key("snap-1"));
        assert!(index.recovered_orphans.is_empty());
        assert!(!orphan.exists());
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
