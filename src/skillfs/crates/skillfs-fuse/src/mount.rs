//! Public mount entry points for the SkillFS FUSE filesystem.
//!
//! The internal [`mount_inner`] does the actual `fuser::mount2` call and
//! threads in the optional security configuration. The preferred entry
//! points are [`mount_configured`] / [`mount_background_configured`]
//! which accept a [`MountConfig`] struct. The legacy per-feature
//! functions are deprecated but remain for backward compatibility.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use skillfs_core::SharedSkillStore;
use skillfs_core::os_adapter::OsAdapterStage;
use skillfs_core::transform::TransformPipeline;
// Only the Linux stale-mount cleanup warns.
#[cfg(target_os = "linux")]
use tracing::warn;
use tracing::{error, info};

use crate::path::SkillLayout;
use crate::security::{
    ActiveSkillResolver, InstallerStagingController, NotifyController, PendingInstallController,
    PostPublishGraceController, QuietTimeoutController, RefreshController, RuntimeMetricsSink,
    SecurityPolicy, SkillEventSink, StagingMatcher, TrustedWriterConfig,
};
use crate::{FuseError, MountHandle, MountOptions, SkillFs};

/// Runtime configuration for mount security features.
#[derive(Default)]
pub struct MountConfig {
    pub event_sink: Option<Arc<dyn SkillEventSink>>,
    pub policy: Option<Arc<dyn SecurityPolicy>>,
    pub active_resolver: Option<Arc<ActiveSkillResolver>>,
    pub refresh_controller: Option<Arc<RefreshController>>,
    pub notify_controller: Option<Arc<NotifyController>>,
    pub trusted_writer: Option<TrustedWriterConfig>,
    pub staging_matcher: Option<Arc<StagingMatcher>>,
    pub staging_controller: Option<Arc<InstallerStagingController>>,
    pub quiet_timeout_controller: Option<Arc<QuietTimeoutController>>,
    pub pending_install_controller: Option<Arc<PendingInstallController>>,
    pub post_publish_controller: Option<Arc<PostPublishGraceController>>,
    /// Best-effort runtime metric delta sink. When `Some`, agent-visible skill
    /// opens and security/policy decisions emit `runtime_metric` JSONL records.
    pub runtime_metrics: Option<Arc<RuntimeMetricsSink>>,
    pub skill_layout: Option<SkillLayout>,
    /// Opt-in OS adapter stage, pre-compiled and validated at mount startup.
    /// When `Some`, it is appended after the directive stage in the read-time
    /// transform pipeline. `None` keeps the default directive-only pipeline.
    pub os_adapter: Option<OsAdapterStage>,
    /// Directive/compiler stage toggle. `None` keeps the default (enabled);
    /// `Some(false)` disables directive compilation for `SKILL.md` reads.
    pub directive_enabled: Option<bool>,
    /// Workload-visible root used for paths emitted by `skill-discover`.
    ///
    /// `None` preserves the default physical source paths. Set this when the
    /// reader runs in a different mount namespace and can only access the
    /// SkillFS view, for example `<mountpoint>/skills` in a sidecar workload.
    pub skill_discover_root: Option<PathBuf>,
    /// The `max_skill_size` the store was loaded with. `None` keeps
    /// [`skillfs_core::DEFAULT_MAX_SKILL_SIZE`]; set it whenever the store was
    /// loaded with a non-default [`skillfs_core::ParseConfig`] so reads,
    /// listings and reparses apply the same per-file ceiling.
    pub max_skill_size: Option<usize>,
}

/// Mount the SkillFS FUSE filesystem (blocking) with a unified
/// configuration struct.
pub fn mount_configured(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    config: MountConfig,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint,
        source,
        store,
        options,
        in_place,
        config.event_sink,
        config.policy,
        config.active_resolver,
        config.refresh_controller,
        config.notify_controller,
        config.trusted_writer,
        config.staging_matcher,
        config.staging_controller,
        config.quiet_timeout_controller,
        config.pending_install_controller,
        config.post_publish_controller,
        config.runtime_metrics,
        config.skill_layout,
        config.os_adapter,
        config.directive_enabled,
        config.skill_discover_root,
        config.max_skill_size,
    )
}

/// How long the background entry points wait for the mount to come live
/// (or fail) before giving up on it.
///
/// [`mount_inner`] constructs the [`SkillFs`] runtime — including the
/// directive stage's environment-profile probe loop, which walks `PATH` for
/// every whitelisted command — *before* creating the FUSE session, and on a
/// loaded host that probing alone can take seconds. The budget bounds only
/// this wait: the mounting thread keeps running, and a mount it still creates
/// is either torn down by the reaper or, once the reaper cancelled the
/// attempt, by the creating side itself.
const BACKGROUND_MOUNT_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between polls of the mounting thread's outcome.
const BACKGROUND_MOUNT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Serializes mount creation and teardown on one mountpoint.
///
/// `umount(2)` takes a path, not a mount instance, so "check the identity,
/// then unmount" is not atomic: a concurrent cleanup plus retry can replace
/// the mount between the check and the command, and the command — which only
/// ever receives a path — then detaches somebody else's session. Re-checking
/// the identity cannot close that window, because it cannot constrain what
/// an already-started command operates on.
///
/// Holding one lock across creation (until FUSE_INIT publishes the identity)
/// and across check-plus-unmount makes those sequences mutually exclusive,
/// which is what binds a path-based unmount to the mount it was checked
/// against. Scoped per mountpoint so a wedged creation only holds up that
/// path's own retries.
fn mount_lifecycle_lock(mountpoint: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = registry.get(mountpoint).and_then(Weak::upgrade) {
        return lock;
    }
    // Drop entries whose lock nobody holds any more, so the registry does not
    // accumulate every mountpoint the process ever used.
    registry.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    registry.insert(mountpoint.to_path_buf(), Arc::downgrade(&lock));
    lock
}

/// Detach the mount this session owns, escalating through the lazy-unmount
/// steps: a process still holding the mountpoint makes a plain
/// `fusermount3 -u` fail with EBUSY for as long as it lives.
///
/// The caller must hold the mountpoint's lifecycle lock. That is what keeps
/// a newer session from being created between an identity check and the
/// unmount command, which only receives a path.
#[cfg(target_os = "linux")]
fn teardown_owned_mount(outcome: &BackgroundMountOutcome, path: &Path) {
    for (program, args) in MountHandle::unmount_commands(path) {
        if !outcome.owns_mount_at(path) {
            return;
        }
        let _ = std::process::Command::new(program).args(&args).output();
    }
}

/// Per-session facts the filesystem publishes at FUSE_INIT and the
/// background mount entry reads.
///
/// Separate from [`BackgroundMountOutcome`] so the filesystem layer only
/// sees the signal it writes, not the caller's wait state.
pub(crate) struct SessionSignal {
    ready: AtomicBool,
    /// The mount ID this session owns. Recorded at FUSE_INIT because a
    /// mountpoint alone cannot identify a mount: a later session may mount
    /// the same path, and teardown must never touch that one.
    mount_id: OnceLock<u64>,
}

impl SessionSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: AtomicBool::new(false),
            mount_id: OnceLock::new(),
        })
    }

    /// Record that this session serves the mountpoint, together with the
    /// mount ID it owns (`None` where no mount table identifies it).
    ///
    /// Readiness is published *before* the creating side reads the
    /// cancellation flag, and the cancelling side does the reverse. Both
    /// pairs are sequentially consistent so that at least one side observes
    /// the other and performs the teardown — see [`BackgroundMountOutcome::cancel`].
    pub(crate) fn mark_ready(&self, mount_id: Option<u64>) {
        if let Some(id) = mount_id {
            let _ = self.mount_id.set(id);
        }
        self.ready.store(true, Ordering::SeqCst);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    #[cfg(target_os = "linux")]
    fn mount_id(&self) -> Option<u64> {
        self.mount_id.get().copied()
    }
}

/// Outcome channel shared between a background mounting thread and the
/// caller waiting for it.
///
/// The mounting thread publishes the mountpoint it resolved, the session's
/// FUSE_INIT callback marks it ready, and a failed mount records its error.
/// The caller reads those facts rather than inferring the mount's state from
/// the filesystem, which cannot tell a live session from the plain directory
/// underneath it, nor a failure from a slow start.
struct BackgroundMountOutcome {
    /// This session's readiness and mount identity, written at FUSE_INIT.
    signal: Arc<SessionSignal>,
    /// Set once the reaper discharges its cleanup responsibility. A mounting
    /// thread still blocked in startup I/O must not create a mount after
    /// this point: nobody would hold the handle that unmounts it.
    cancelled: AtomicBool,
    /// The resolved mountpoint, published before the session is created.
    mountpoint: OnceLock<PathBuf>,
    /// The error [`mount_inner`] failed with, if it did.
    failure: Mutex<Option<FuseError>>,
}

impl BackgroundMountOutcome {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            signal: SessionSignal::new(),
            cancelled: AtomicBool::new(false),
            mountpoint: OnceLock::new(),
            failure: Mutex::new(None),
        })
    }

    /// The signal to hand to the filesystem so its FUSE_INIT callback can
    /// report this session's readiness and mount identity.
    fn session_signal(&self) -> Arc<SessionSignal> {
        Arc::clone(&self.signal)
    }

    fn is_ready(&self) -> bool {
        self.signal.is_ready()
    }

    /// Whether the mount currently at `path` is the one this session created.
    ///
    /// A lazy unmount detaches the mount while a holder's descriptor keeps
    /// the session alive, so the same path can carry a *new* session long
    /// before this one's thread exits. Comparing mount IDs is what keeps
    /// this session's cleanup from tearing that new mount down.
    #[cfg(target_os = "linux")]
    fn owns_mount_at(&self, path: &Path) -> bool {
        match (self.signal.mount_id(), MountHandle::path_mount_id(path)) {
            (Some(ours), Some(current)) => ours == current,
            // Without a recorded identity there is no way to tell our mount
            // from somebody else's, so teardown stands down rather than risk
            // unmounting an innocent session.
            _ => false,
        }
    }

    /// Discharge cleanup responsibility: the mounting thread must no longer
    /// create a mount, because nothing will unmount it.
    ///
    /// Handshake with the creating side, which publishes readiness before it
    /// reads this flag while this side publishes the flag before it reads
    /// readiness. Sequential consistency on both pairs guarantees at least
    /// one side sees the other, so a mount can never be created after the
    /// reaper left with nobody responsible for it. Both sides may act; the
    /// second teardown is a no-op because the identity no longer matches.
    #[cfg(target_os = "linux")]
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn publish_mountpoint(&self, mountpoint: PathBuf) {
        let _ = self.mountpoint.set(mountpoint);
    }

    fn mountpoint(&self) -> Option<PathBuf> {
        self.mountpoint.get().cloned()
    }

    /// The resolved mountpoint, or `fallback` while the thread has not
    /// published one — it either failed before resolving or has not got
    /// there yet.
    fn resolved_mountpoint(&self, fallback: &Path) -> PathBuf {
        self.mountpoint().unwrap_or_else(|| fallback.to_path_buf())
    }

    fn record_failure(&self, error: FuseError) {
        *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
    }

    fn take_failure(&self) -> Option<FuseError> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// Wait until the background mount is live or has failed.
///
/// On success returns the session thread — so it can be stored in the
/// [`MountHandle`] — together with the mountpoint the thread resolved, which
/// is the spelling the handle must use for `is_mounted` and unmount. On
/// failure the thread has already exited and is joined here so nothing leaks.
///
/// * A failure recorded by the mounting thread is returned as `Err` — the
///   original error, not a generic timeout.
/// * Readiness is this session's own FUSE_INIT callback, which the kernel
///   delivers only once the session serves, so `Ok` means this mount is
///   live. The mount table is deliberately not consulted to infer readiness:
///   an entry at the mountpoint cannot say which session produced it, so a
///   stale mount torn down and remounted by this thread races every
///   presence-based probe, and a session that completed before the first
///   probe is indistinguishable from the leftover it replaced.
/// * An exited thread is joined and its failure slot re-read once more, so
///   an error recorded between the last empty read and the exit is still the
///   one reported; only a slot empty after the join is an abnormal exit.
/// * If `budget` elapses with neither outcome (a pathologically slow
///   environment probe), an explicit [`FuseError::MountPending`] is returned
///   instead of the historical optimistic success, and the still-running
///   thread is handed to a reaper that tears a late mount down again, so no
///   unmanaged session is left behind.
#[cfg(target_os = "linux")]
fn wait_for_background_mount(
    mountpoint: &Path,
    session: std::thread::JoinHandle<()>,
    outcome: &Arc<BackgroundMountOutcome>,
    budget: Duration,
) -> Result<(std::thread::JoinHandle<()>, PathBuf), FuseError> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Some(error) = outcome.take_failure() {
            // The mounting thread exits right after recording a failure,
            // so joining it here cannot block.
            let _ = session.join();
            return Err(error);
        }
        if outcome.is_ready() {
            return Ok((session, outcome.resolved_mountpoint(mountpoint)));
        }
        if session.is_finished() {
            return Err(finish_after_thread_exit(session, outcome));
        }
        if std::time::Instant::now() >= deadline {
            reap_late_background_mount(
                session,
                Arc::clone(outcome),
                mountpoint.to_path_buf(),
                BACKGROUND_MOUNT_READINESS_TIMEOUT.saturating_mul(2),
            );
            return Err(FuseError::MountPending(format!(
                "mountpoint {}: no ready session within the {:?} readiness \
                 budget; a late mount is torn down automatically",
                mountpoint.display(),
                budget,
            )));
        }
        std::thread::sleep(BACKGROUND_MOUNT_POLL_INTERVAL);
    }
}

/// Join an exited mounting thread and re-read its failure slot once more:
/// the thread may have recorded its error between the wait loop's last empty
/// read and its exit — `is_finished()` only proves the thread exited, not
/// that it failed silently. Only a slot still empty after the join is
/// reported as an abnormal exit.
#[cfg(target_os = "linux")]
fn finish_after_thread_exit(
    session: std::thread::JoinHandle<()>,
    outcome: &BackgroundMountOutcome,
) -> FuseError {
    let _ = session.join();
    outcome.take_failure().unwrap_or_else(|| {
        FuseError::MountFailed(
            "background mount thread exited without reporting a failure".to_string(),
        )
    })
}

/// Tear down a late background mount after the caller gave up on it: the
/// readiness budget expired, the mounting thread keeps running, and a
/// session that still comes up would be unmanaged — nobody holds the handle
/// that would unmount it. The reaper polls until the thread exits,
/// unmounting only the mount this session owns, and stops polling once
/// `cleanup_budget` elapses.
///
/// The budget bounds the polling, not the final handover: giving up takes the
/// mountpoint's lifecycle lock, so a creating side that is inside its own
/// critical section — which may run up to three unmount subprocesses — can
/// hold the reaper past the budget. Correctness requires that wait, because
/// the reaper is the side that must act when the creator observed no
/// cancellation; a wedged unmount helper therefore delays the reaper's exit
/// rather than being skipped.
///
/// On giving up it cancels the outcome, which stops the mounting thread from
/// creating a mount later: startup I/O can block past any budget, and a
/// mount placed after the reaper left would be unmanaged.
///
/// `mountpoint` is the caller's spelling and only names the mount in
/// diagnostics; teardown addresses the mountpoint the thread published.
#[cfg(target_os = "linux")]
fn reap_late_background_mount(
    session: std::thread::JoinHandle<()>,
    outcome: Arc<BackgroundMountOutcome>,
    mountpoint: PathBuf,
    cleanup_budget: Duration,
) {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + cleanup_budget;
        while !session.is_finished() {
            if outcome.is_ready() {
                if let Some(resolved) = outcome.mountpoint() {
                    let lock = mount_lifecycle_lock(&resolved);
                    // Never block on a creation in progress: it holds the
                    // lock until FUSE_INIT, and then checks cancellation and
                    // cleans up itself. Taking the lock here is what makes
                    // the identity check and the unmount one indivisible
                    // step with respect to a retry on the same path.
                    if let Ok(_guard) = lock.try_lock() {
                        teardown_owned_mount(&outcome, &resolved);
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                // Publish the cancellation, then read readiness — the reverse
                // of the creating side's order, so one of the two always sees
                // the other and tears the mount down.
                outcome.cancel();
                if outcome.is_ready() {
                    if let Some(resolved) = outcome.mountpoint() {
                        let lock = mount_lifecycle_lock(&resolved);
                        // Readiness implies FUSE_INIT already ran, so this
                        // never waits on creation itself — only on the
                        // creator leaving its critical section, which can
                        // still include its own unmount subprocesses.
                        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        teardown_owned_mount(&outcome, &resolved);
                    }
                }
                error!(
                    mountpoint = %mountpoint.display(),
                    "giving up on reaping a late background mount; \
                     the mounting thread is cancelled"
                );
                return;
            }
            std::thread::sleep(BACKGROUND_MOUNT_POLL_INTERVAL);
        }
        let _ = session.join();
    });
}

/// Non-Linux fallback: without `/proc/mounts` there is no authoritative
/// liveness probe. Keep the historical brief grace period, but still check
/// the recorded failure afterwards so fast validation errors surface.
#[cfg(not(target_os = "linux"))]
fn wait_for_background_mount(
    mountpoint: &Path,
    session: std::thread::JoinHandle<()>,
    outcome: &Arc<BackgroundMountOutcome>,
    _budget: Duration,
) -> Result<(std::thread::JoinHandle<()>, PathBuf), FuseError> {
    std::thread::sleep(Duration::from_millis(100));
    if let Some(error) = outcome.take_failure() {
        let _ = session.join();
        return Err(error);
    }
    Ok((session, outcome.resolved_mountpoint(mountpoint)))
}

/// Mount the SkillFS FUSE filesystem in the background with a unified
/// configuration struct.
///
/// "Background" refers to the FUSE event loop, which serves on its own
/// thread — unlike [`mount_configured`], which blocks until the session is
/// unmounted. This call still waits for a real outcome within the readiness
/// budget: a mount that fails before or while creating the FUSE session is
/// returned as `Err` carrying the original error, instead of a handle whose
/// failure was only visible to a tracing subscriber, and `Ok` is only
/// returned once this session serves the mountpoint. A mount still neither
/// ready nor failed when the budget expires returns
/// [`FuseError::MountPending`]; a session coming up during the cleanup window
/// is torn down again, and the mounting thread is then cancelled so it
/// cannot leave an unmanaged mount behind.
///
/// The handle addresses the resolved mountpoint, so `is_mounted` and
/// `unmount` also work when the caller spelled it relatively, with `..`, or
/// through a symlink.
pub fn mount_background_configured(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    config: MountConfig,
) -> Result<MountHandle, FuseError> {
    let (session, outcome) =
        spawn_background_mount(mountpoint, source, store, options, in_place, config);

    let (session, mountpoint_path) = wait_for_background_mount(
        mountpoint,
        session,
        &outcome,
        BACKGROUND_MOUNT_READINESS_TIMEOUT,
    )?;

    Ok(MountHandle {
        mountpoint: mountpoint_path,
        session: Some(session),
    })
}

/// Spawn the mounting thread together with the outcome channel it reports
/// through. The caller decides how long to wait for that outcome.
fn spawn_background_mount(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    config: MountConfig,
) -> (std::thread::JoinHandle<()>, Arc<BackgroundMountOutcome>) {
    // Only copy the caller's path here, never resolve it: `canonicalize`
    // stats every component and would block this thread indefinitely on a
    // stale FUSE endpoint. The mounting thread resolves it after tearing
    // down any stale mount, and publishes the result through the outcome.
    let mountpoint_for_thread = mountpoint.to_path_buf();
    let source_path = source.to_path_buf();

    let outcome = BackgroundMountOutcome::new();
    let thread_outcome = Arc::clone(&outcome);

    let session = std::thread::spawn(move || {
        let mut opts = options;
        opts.foreground = true;
        if let Err(e) = mount_inner_with_outcome(
            &mountpoint_for_thread,
            &source_path,
            store,
            opts,
            in_place,
            config.event_sink,
            config.policy,
            config.active_resolver,
            config.refresh_controller,
            config.notify_controller,
            config.trusted_writer,
            config.staging_matcher,
            config.staging_controller,
            config.quiet_timeout_controller,
            config.pending_install_controller,
            config.post_publish_controller,
            config.runtime_metrics,
            config.skill_layout,
            config.os_adapter,
            config.directive_enabled,
            config.skill_discover_root,
            config.max_skill_size,
            Some(Arc::clone(&thread_outcome)),
        ) {
            error!(error = %e, "background mount failed");
            thread_outcome.record_failure(e);
        }
    });

    (session, outcome)
}

#[allow(clippy::too_many_arguments)]
fn mount_inner(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
    notify_controller: Option<Arc<NotifyController>>,
    trusted_writer: Option<TrustedWriterConfig>,
    staging_matcher: Option<Arc<StagingMatcher>>,
    staging_controller: Option<Arc<InstallerStagingController>>,
    quiet_timeout_controller: Option<Arc<QuietTimeoutController>>,
    pending_install_controller: Option<Arc<PendingInstallController>>,
    post_publish_controller: Option<Arc<PostPublishGraceController>>,
    runtime_metrics: Option<Arc<RuntimeMetricsSink>>,
    skill_layout: Option<SkillLayout>,
    os_adapter: Option<OsAdapterStage>,
    directive_enabled: Option<bool>,
    skill_discover_root: Option<PathBuf>,
    max_skill_size: Option<usize>,
) -> Result<(), FuseError> {
    mount_inner_with_outcome(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        refresh_controller,
        notify_controller,
        trusted_writer,
        staging_matcher,
        staging_controller,
        quiet_timeout_controller,
        pending_install_controller,
        post_publish_controller,
        runtime_metrics,
        skill_layout,
        os_adapter,
        directive_enabled,
        skill_discover_root,
        max_skill_size,
        None,
    )
}

/// Internal mount that accepts optional Skill Security overrides. Public
/// `mount` and `mount_background` keep their existing signatures and pass
/// `None` for both; test/embedder callers reach the sink/policy injection
/// path through [`mount_background_with_security`].
///
/// `outcome` is `Some` for the background entries, whose caller waits on it
/// and whose handle must address the resolved mountpoint it publishes.
/// Blocking callers pass `None` and get a private one, because every mount —
/// blocking included — must publish readiness to release the lifecycle lock
/// and must exclude teardown on its mountpoint while it is being created.
#[allow(clippy::too_many_arguments)]
fn mount_inner_with_outcome(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
    notify_controller: Option<Arc<NotifyController>>,
    trusted_writer: Option<TrustedWriterConfig>,
    staging_matcher: Option<Arc<StagingMatcher>>,
    staging_controller: Option<Arc<InstallerStagingController>>,
    quiet_timeout_controller: Option<Arc<QuietTimeoutController>>,
    pending_install_controller: Option<Arc<PendingInstallController>>,
    post_publish_controller: Option<Arc<PostPublishGraceController>>,
    runtime_metrics: Option<Arc<RuntimeMetricsSink>>,
    skill_layout: Option<SkillLayout>,
    os_adapter: Option<OsAdapterStage>,
    directive_enabled: Option<bool>,
    skill_discover_root: Option<PathBuf>,
    max_skill_size: Option<usize>,
    outcome: Option<Arc<BackgroundMountOutcome>>,
) -> Result<(), FuseError> {
    info!(mountpoint = %mountpoint.display(), source = %source.display(), in_place, "mounting SkillFS");

    if let Some(root) = &skill_discover_root {
        if !root.is_absolute() {
            return Err(FuseError::InvalidSkillDiscoverRoot(format!(
                "path must be absolute, got '{}'",
                root.display()
            )));
        }
    }

    if !mountpoint.exists() {
        return Err(FuseError::InvalidMountPoint(
            "mount point does not exist".to_string(),
        ));
    }
    if !mountpoint.is_dir() {
        return Err(FuseError::InvalidMountPoint(
            "mount point is not a directory".to_string(),
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let mountinfo = std::fs::read("/proc/mounts").ok();
        if let Some(bytes) = mountinfo {
            use std::os::unix::ffi::OsStrExt;
            if crate::proc_mounts::mounts_contain_target(&bytes, mountpoint.as_os_str().as_bytes())
            {
                warn!(mountpoint = %mountpoint.display(), "mount point already mounted, attempting cleanup");
                // Raw OS-string argument: the byte-exact mount check above
                // matched this path, so the unmount must address the same
                // bytes — a lossy UTF-8 view of an invalid-byte mountpoint
                // names a different path and the cleanup silently fails.
                let _ = std::process::Command::new("fusermount3")
                    .arg("-u")
                    .arg(mountpoint.as_os_str())
                    .output();
                // Give the kernel time to process the unmount
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }

    // One resolved spelling for the whole mount lifecycle: the kernel mount
    // table renders the resolved path, so the session, the handle's
    // `is_mounted` and unmount must all address it — the caller's relative,
    // `..` or symlinked spelling would never match the table's entry.
    //
    // This runs here, on the mounting thread and after the stale-mount
    // cleanup above, because `canonicalize` stats every path component and
    // blocks uninterruptibly on a stale FUSE endpoint: on the caller's thread
    // it would hang before that cleanup could ever run. A path that cannot be
    // resolved keeps the caller's spelling. Raw OS strings flow through, so
    // non-UTF-8 mountpoints resolve and compare as bytes end to end.
    let resolved_mountpoint =
        std::fs::canonicalize(mountpoint).unwrap_or_else(|_| mountpoint.to_path_buf());
    // Blocking entries have no caller waiting on the outcome and no reaper,
    // but they still need the session's readiness signal: the lifecycle lock
    // below is released when FUSE_INIT publishes it, and a blocking mount
    // that skipped the lock could be detached by a background reaper's
    // in-flight unmount on the same path.
    let outcome = outcome.unwrap_or_else(BackgroundMountOutcome::new);
    outcome.publish_mountpoint(resolved_mountpoint.clone());
    let mountpoint = resolved_mountpoint.as_path();

    let mut fuse_opts: Vec<fuser::MountOption> = vec![];
    fuse_opts.push(fuser::MountOption::NoAtime);
    if options.read_only {
        fuse_opts.push(fuser::MountOption::RO);
    }
    if options.allow_other {
        fuse_opts.push(fuser::MountOption::AllowOther);
    }

    // Start from an empty pipeline and enable stages from configuration below.
    // This keeps `EnvironmentProfile::detect()` gated on the directive stage
    // actually being enabled, unlike the public `SkillFs::new` default.
    let mut fs = SkillFs::new_with_pipeline_and_staging(
        mountpoint.to_path_buf(),
        source.to_path_buf(),
        store,
        in_place,
        TransformPipeline::empty(),
        staging_matcher.clone(),
    );
    // This session's readiness signal: FUSE_INIT sets it, the background
    // entry's wait loop polls it, and the lifecycle lock is released on it.
    fs = fs.with_session_signal(outcome.session_signal());
    if let Some(root) = skill_discover_root {
        fs = fs.with_skill_discover_root(root);
    }
    if let Some(limit) = max_skill_size {
        fs = fs.with_max_skill_size(limit);
    }
    if let Some(sink) = event_sink {
        fs = fs.with_event_sink(sink);
    }
    if let Some(p) = policy {
        fs = fs.with_policy(p);
    }
    if let Some(r) = active_resolver {
        fs = fs.with_active_resolver(r);
    }
    if let Some(c) = refresh_controller {
        fs = fs.with_refresh_controller(c);
    }
    if let Some(c) = notify_controller {
        fs = fs.with_notify_controller(c);
    }
    if let Some(t) = trusted_writer {
        let enabled = t.is_enabled();
        let exe_enabled = t.is_exe_enabled();
        let name = t.expected_process_name().map(|s| s.to_string());
        let exe = t.expected_exe_path().map(|p| p.display().to_string());
        fs = fs.with_trusted_writer(t);
        if enabled {
            if exe_enabled {
                info!(
                    trusted_writer_exe = %exe.unwrap_or_default(),
                    trusted_writer_comm = %name.unwrap_or_default(),
                    "trusted writer gate enabled (executable identity)"
                );
            } else {
                info!(
                    trusted_writer = %name.unwrap_or_default(),
                    "trusted writer gate enabled (compat: TID -> TGID comm + starttime)"
                );
            }
        }
    }
    if let Some(c) = staging_controller {
        fs = fs.with_staging_controller(c);
    }
    if let Some(c) = quiet_timeout_controller {
        fs = fs.with_quiet_timeout_controller(c);
    }
    if let Some(c) = pending_install_controller {
        fs = fs.with_pending_install_controller(c);
    }
    if let Some(c) = post_publish_controller {
        fs = fs.with_post_publish_controller(c);
    }
    if let Some(s) = runtime_metrics {
        fs = fs.with_runtime_metrics(s);
    }
    if let Some(layout) = skill_layout {
        fs = fs.with_skill_layout(layout);
    }
    // Directive/compiler stage defaults to enabled; `None` means "no config
    // opinion". Enabling detects the environment, so a config that disables the
    // directive stage skips that probe entirely.
    fs = fs.with_directive_enabled(directive_enabled.unwrap_or(true));
    if let Some(stage) = os_adapter {
        // Content-free initialization diagnostics: pipeline stage order,
        // resolved target OS, rule digest, and rule counts. No Skill content.
        info!(
            target_os = stage.target().as_str(),
            rule_digest = stage.rule_digest(),
            total_rules = stage.total_rules(),
            active_rules = stage.active_rules(),
            "os_adapter transform stage enabled"
        );
        fs = fs.with_os_adapter_stage(stage);
    }
    info!(stages = ?fs.transform_stage_names(), "read-time transform pipeline ready");
    info!("starting FUSE filesystem");

    // Neutralize the daemon process's file-creation mask. The FUSE protocol
    // delivers the caller's umask to `create()` / `mkdir()` callbacks and we
    // apply it explicitly via `effective_mode = mode & !umask`; without this
    // call the daemon's own umask (typically `0o022` inherited from the shell
    // that started `skillfs mount`) would still mask the `mode` argument of
    // the daemon-side `openat`/`mkdirat`, double-masking and clamping bits
    // the caller actually requested. Linux's `umask(2)` is async-signal-safe
    // and always succeeds; we set it once here and leave it for the lifetime
    // of the FUSE event loop.
    //
    // In-process tests that need a non-zero umask wrap their own callers in
    // the `UmaskGuard` defined in
    // `crates/skillfs-fuse/tests/posix_create_mkdir_inode_tests.rs`, which
    // mutates the process umask under a serialization mutex; this startup
    // call merely sets the default daemon umask, not the test-time guard.
    #[cfg(target_family = "unix")]
    unsafe {
        libc::umask(0);
    }

    // Creation and teardown on this mountpoint must not overlap (see
    // mount_lifecycle_lock). The lock has to cover the mount syscall itself,
    // so the session runs on its own thread and the lock is released once
    // FUSE_INIT has published this session's identity — not when the session
    // ends, which would block every retry for the mount's whole lifetime.
    let lock = mount_lifecycle_lock(mountpoint);
    let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Startup I/O above can block past any readiness budget — reading a
    // `skillfs-views.toml` that is a FIFO waits for a writer indefinitely.
    // Once the caller gave up, creating a mount would leave it unmanaged.
    if outcome.is_cancelled() {
        return Err(FuseError::MountPending(format!(
            "mountpoint {}: cancelled after the readiness budget expired; \
             no mount was created",
            mountpoint.display(),
        )));
    }

    let session_mountpoint = mountpoint.to_path_buf();
    let session = std::thread::spawn(move || fuser::mount2(fs, session_mountpoint, &fuse_opts));

    while !outcome.is_ready() && !session.is_finished() {
        std::thread::sleep(BACKGROUND_MOUNT_POLL_INTERVAL);
    }

    // The reaper may have given up while this creation was in progress — it
    // cancels without taking the lock, so it stays bounded. Now that the
    // identity is published, cleanup belongs to this side: no other session
    // can be created here while the lock is held, so the teardown below can
    // only ever detach this session's own mount.
    #[cfg(target_os = "linux")]
    if outcome.is_ready() && outcome.is_cancelled() {
        teardown_owned_mount(&outcome, mountpoint);
    }
    drop(guard);

    match session.join() {
        Ok(Ok(())) => {
            info!("filesystem unmounted");
            Ok(())
        }
        Ok(Err(e)) => Err(FuseError::MountFailed(e.to_string())),
        Err(_) => Err(FuseError::MountFailed(
            "FUSE session thread panicked".to_string(),
        )),
    }
}

/// Mount the SkillFS FUSE filesystem (blocking).
#[deprecated(note = "use mount_configured")]
pub fn mount(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint, source, store, options, in_place, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None,
    )
}

/// Mount the SkillFS FUSE filesystem (blocking) with optional Skill Security
/// overrides.
///
/// Both `event_sink` and `policy` default to the values used by
/// [`SkillFs::new`] when set to `None`; supplying `Some(...)` replaces them
/// before the FUSE event loop starts. This is the blocking analog of
/// [`mount_background_with_security`] and is the entry point CLI/operator
/// callers use when wiring runtime audit configuration through to the
/// mount.
///
/// **Stable signature.** D1.1 deliberately did not extend this function
/// — the resolver-aware variant is
/// [`mount_with_security_and_active_resolver`]. Callers that already
/// pass `event_sink` and `policy` keep compiling unchanged.
#[deprecated(note = "use mount_configured")]
pub fn mount_with_security(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint, source, store, options, in_place, event_sink, policy, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None,
    )
}

/// Blocking mount with the D1.1 ledger active-skill resolver attached.
///
/// Same semantics as [`mount_with_security`] for the existing
/// `event_sink` / `policy` parameters; in addition, when
/// `active_resolver` is `Some(_)` the read paths under `/skills`
/// (readdir, lookup, getattr, open/read of `SKILL.md` and ordinary
/// passthrough files) consult the resolver to decide visibility and
/// which physical directory backs each skill — see
/// [`SkillFs::with_active_resolver`] for the full contract. Passing
/// `None` for `active_resolver` is exactly equivalent to calling
/// [`mount_with_security`].
#[deprecated(note = "use mount_configured")]
pub fn mount_with_security_and_active_resolver(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Blocking mount with the refresh controller attached.
///
/// Same semantics as
/// [`mount_with_security_and_active_resolver`] for the
/// `event_sink` / `policy` / `active_resolver` parameters; in addition,
/// when `demo_refresh` is `Some(_)` successful mutating FUSE callbacks
/// observe the change through the controller (debounced per skill on
/// its own worker). Passing `None` for `demo_refresh` is exactly
/// equivalent to calling [`mount_with_security_and_active_resolver`].
#[allow(clippy::too_many_arguments)]
#[deprecated(note = "use mount_configured")]
pub fn mount_with_security_active_resolver_and_demo_refresh(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        refresh_controller,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Blocking mount that additionally accepts the trusted
/// writer configuration.
///
/// Same semantics as
/// [`mount_with_security_active_resolver_and_demo_refresh`] for every
/// existing parameter. When `trusted_writer` is `Some(_)` and enabled,
/// `.skill-meta/**` mutation requests whose FUSE-caller pid resolves
/// to the configured process name bypass
/// [`SkillMetaProtectionPolicy`]'s deny path; the bypass is observed
/// through the configured event sink as a `PolicyDecision` (allowed)
/// record carrying `trusted_writer=<name>` in the audit detail.
/// Passing `None` is exactly equivalent to
/// [`mount_with_security_active_resolver_and_demo_refresh`].
#[allow(clippy::too_many_arguments)]
#[deprecated(note = "use mount_configured")]
pub fn mount_with_security_active_resolver_demo_refresh_and_trusted_writer(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
    trusted_writer: Option<TrustedWriterConfig>,
) -> Result<(), FuseError> {
    mount_inner(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        refresh_controller,
        None,
        trusted_writer,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Mount in background (non-blocking).
#[deprecated(note = "use mount_background_configured")]
#[allow(deprecated)]
pub fn mount_background(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
) -> Result<MountHandle, FuseError> {
    mount_background_with_security(mountpoint, source, store, options, in_place, None, None)
}

/// Mount in background with optional Skill Security overrides.
///
/// Both `event_sink` and `policy` default to the values used by
/// [`SkillFs::new`] when set to `None`; supplying `Some(...)` replaces them
/// before the FUSE event loop starts. This is the entry point integration
/// tests use to capture audit events through a real mount without changing
/// any other call site.
///
/// **Stable signature.** D1.1 deliberately did not extend this function
/// — the resolver-aware variant is
/// [`mount_background_with_security_and_active_resolver`].
#[deprecated(note = "use mount_background_configured")]
#[allow(deprecated)]
pub fn mount_background_with_security(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
) -> Result<MountHandle, FuseError> {
    mount_background_with_security_active_resolver_demo_refresh_and_trusted_writer(
        mountpoint, source, store, options, in_place, event_sink, policy, None, None, None,
    )
}

/// Background mount with the D1.1 ledger active-skill resolver attached.
///
/// Same semantics as [`mount_background_with_security`] for the
/// existing `event_sink` / `policy` parameters; in addition, when
/// `active_resolver` is `Some(_)` the read paths under `/skills`
/// consult the resolver to decide visibility and which physical
/// directory backs each skill (see
/// [`SkillFs::with_active_resolver`] for the full contract). Passing
/// `None` for `active_resolver` is exactly equivalent to calling
/// [`mount_background_with_security`].
#[deprecated(note = "use mount_background_configured")]
#[allow(deprecated)]
pub fn mount_background_with_security_and_active_resolver(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
) -> Result<MountHandle, FuseError> {
    mount_background_with_security_active_resolver_demo_refresh_and_trusted_writer(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        None,
        None,
    )
}

/// Background mount with the refresh controller attached.
///
/// Same semantics as
/// [`mount_background_with_security_and_active_resolver`] for the
/// existing parameters; when `demo_refresh` is `Some(_)` successful
/// mutating FUSE callbacks observe the change through the controller
/// (debounced per skill on its own worker). Passing `None` for
/// `demo_refresh` is exactly equivalent to calling
/// [`mount_background_with_security_and_active_resolver`].
#[allow(clippy::too_many_arguments)]
#[deprecated(note = "use mount_background_configured")]
#[allow(deprecated)]
pub fn mount_background_with_security_active_resolver_and_demo_refresh(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
) -> Result<MountHandle, FuseError> {
    mount_background_with_security_active_resolver_demo_refresh_and_trusted_writer(
        mountpoint,
        source,
        store,
        options,
        in_place,
        event_sink,
        policy,
        active_resolver,
        refresh_controller,
        None,
    )
}

/// Background mount that additionally accepts the trusted
/// writer configuration.
///
/// Same semantics as
/// [`mount_background_with_security_active_resolver_and_demo_refresh`]
/// for every existing parameter. When `trusted_writer` is `Some(_)`
/// and enabled, `.skill-meta/**` mutation requests whose FUSE-caller
/// pid resolves to the configured process name are allowed and the
/// bypass is observed through the configured event sink. Passing
/// `None` is exactly equivalent to
/// [`mount_background_with_security_active_resolver_and_demo_refresh`].
#[allow(clippy::too_many_arguments)]
#[deprecated(note = "use mount_background_configured")]
pub fn mount_background_with_security_active_resolver_demo_refresh_and_trusted_writer(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    event_sink: Option<Arc<dyn SkillEventSink>>,
    policy: Option<Arc<dyn SecurityPolicy>>,
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    refresh_controller: Option<Arc<RefreshController>>,
    trusted_writer: Option<TrustedWriterConfig>,
) -> Result<MountHandle, FuseError> {
    // Every funnel-specific parameter has a `MountConfig` field; the rest
    // stay at their defaults, which is what this entry passed before.
    let config = MountConfig {
        event_sink,
        policy,
        active_resolver,
        refresh_controller,
        trusted_writer,
        ..MountConfig::default()
    };
    mount_background_configured(mountpoint, source, store, options, in_place, config)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn fuse_available() -> bool {
        if !std::path::Path::new("/dev/fuse").exists() {
            return false;
        }
        // /dev/fuse may exist without permission to open it.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .is_ok()
    }

    /// A session that is already ready when the wait starts returns `Ok` on
    /// the first check — no probe interval, no budget — and hands back the
    /// mountpoint the thread published rather than the caller's spelling.
    #[test]
    fn a_ready_session_returns_immediately() {
        let outcome = BackgroundMountOutcome::new();
        outcome.session_signal().mark_ready(None);
        outcome.publish_mountpoint(PathBuf::from("/resolved/mountpoint"));
        let session = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
        });
        let started = std::time::Instant::now();
        let (_session, mountpoint) = wait_for_background_mount(
            std::path::Path::new("/caller/spelling"),
            session,
            &outcome,
            Duration::from_secs(30),
        )
        .expect("a ready session is Ok");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "readiness must return without polling, took {:?}",
            started.elapsed()
        );
        assert_eq!(
            mountpoint,
            PathBuf::from("/resolved/mountpoint"),
            "the handle must address the mountpoint the thread resolved"
        );
    }

    /// A session that is neither ready nor failed within the budget returns
    /// the explicit `MountPending` error — not a handle to a mount that may
    /// not exist.
    #[test]
    fn a_never_ready_session_times_out_as_pending() {
        let outcome = BackgroundMountOutcome::new();
        let session = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
        });
        let started = std::time::Instant::now();
        let error = wait_for_background_mount(
            std::path::Path::new("/nonexistent"),
            session,
            &outcome,
            Duration::from_millis(100),
        )
        .expect_err("a never-ready session must not return Ok");
        assert!(
            matches!(error, FuseError::MountPending(ref message) if message.contains("/nonexistent")),
            "the timeout must be the explicit pending outcome, got: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the timeout must honor the budget, took {:?}",
            started.elapsed()
        );
    }

    /// Joining an exited thread and re-reading its failure slot preserves an
    /// error the thread recorded just before exiting — the window an earlier
    /// empty read cannot see.
    #[test]
    fn a_late_recorded_error_survives_the_thread_exit() {
        let outcome = BackgroundMountOutcome::new();
        let thread_outcome = Arc::clone(&outcome);
        let session = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            thread_outcome.record_failure(FuseError::InvalidMountPoint("late failure".to_string()));
        });
        while !session.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = finish_after_thread_exit(session, &outcome);
        assert!(
            matches!(error, FuseError::InvalidMountPoint(ref message) if message.contains("late failure")),
            "the original error must be preserved after the join, got: {error:?}"
        );
    }

    /// The end-to-end interleaving: whichever way the record/exit race
    /// against the wait loop's reads resolves, the caller always sees the
    /// original error — never the generic abnormal-exit one.
    #[test]
    fn an_exiting_thread_always_publishes_its_original_error() {
        for round in 0..25 {
            let outcome = BackgroundMountOutcome::new();
            let thread_outcome = Arc::clone(&outcome);
            let session = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(round % 5));
                thread_outcome
                    .record_failure(FuseError::PermissionDenied(format!("round {round}")));
            });
            let error = wait_for_background_mount(
                std::path::Path::new("/nonexistent"),
                session,
                &outcome,
                Duration::from_secs(10),
            )
            .expect_err("a failed session must surface its error");
            assert!(
                matches!(error, FuseError::PermissionDenied(ref message) if message.contains(&format!("round {round}"))),
                "round {round}: the original error must win the race, got: {error:?}"
            );
        }
    }

    /// A store loaded from `source`, shared the way the mount entries take it.
    fn shared_store(source: &Path) -> skillfs_core::SharedSkillStore {
        let mut store = skillfs_core::store::SkillStore::new();
        store.load_from_directory(source, &skillfs_core::ParseConfig::default());
        Arc::new(parking_lot::RwLock::new(store))
    }

    /// A live background session started through the real mounting path, so
    /// its outcome carries the readiness and mount identity FUSE_INIT
    /// published. Fabricating them would not exercise the identity check.
    fn live_session(
        source: &Path,
        mountpoint: &Path,
    ) -> (std::thread::JoinHandle<()>, Arc<BackgroundMountOutcome>) {
        let (session, outcome) = spawn_background_mount(
            mountpoint,
            source,
            shared_store(source),
            MountOptions::default(),
            false,
            MountConfig::default(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !outcome.is_ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "the session must become ready, failure: {:?}",
                outcome.take_failure()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        (session, outcome)
    }

    /// The resolved mountpoint a live session published.
    fn resolved(outcome: &BackgroundMountOutcome) -> PathBuf {
        outcome
            .mountpoint()
            .expect("the session published its mountpoint")
    }

    /// Wait, bounded, for the mount at `path` to go away.
    fn wait_until_unmounted(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while MountHandle::path_is_mounted(path) {
            assert!(
                std::time::Instant::now() < deadline,
                "the reaper must tear the late mount down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A live session handed to the reaper is unmounted and its thread
    /// joined, so no unmanaged mount outlives the caller that gave up on it.
    #[test]
    fn a_late_live_mount_is_torn_down_and_reaped() {
        if !fuse_available() {
            return;
        }
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let (session, outcome) = live_session(source.path(), mountpoint.path());
        let path = resolved(&outcome);
        assert!(
            MountHandle::path_is_mounted(&path),
            "precondition: the session is live"
        );
        reap_late_background_mount(session, outcome, path.clone(), Duration::from_secs(15));
        wait_until_unmounted(&path);
    }

    /// A late mount a process still holds must be reaped too: a plain
    /// `fusermount3 -u` fails with EBUSY for as long as the holder lives, so
    /// the reaper has to escalate through the lazy-unmount steps. Agent
    /// workloads reach this when a process keeps its cwd or an open file
    /// inside the mountpoint.
    #[test]
    fn a_busy_late_mount_is_reaped_through_lazy_unmount() {
        if !fuse_available() {
            return;
        }
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let (session, outcome) = live_session(source.path(), mountpoint.path());
        let path = resolved(&outcome);

        let holder = std::fs::File::open(&path).expect("hold the mountpoint open");
        reap_late_background_mount(session, outcome, path.clone(), Duration::from_secs(15));
        wait_until_unmounted(&path);
        drop(holder);
    }

    /// A lazy unmount detaches the mount while a holder's descriptor keeps
    /// the old session alive, so its reaper keeps polling long after its own
    /// mount is gone. A new session mounted on the same path in that window
    /// must survive: teardown compares mount identity, not path presence.
    /// Without the identity check the old reaper unmounts the retry, which
    /// in the in-place layout re-exposes the raw source tree.
    #[test]
    fn a_reaper_leaves_a_newer_session_on_the_same_path_alone() {
        if !fuse_available() {
            return;
        }
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let (old_session, old_outcome) = live_session(source.path(), mountpoint.path());
        let path = resolved(&old_outcome);

        let holder = std::fs::File::open(&path).expect("hold the old mount open");
        reap_late_background_mount(
            old_session,
            Arc::clone(&old_outcome),
            path.clone(),
            Duration::from_secs(30),
        );
        wait_until_unmounted(&path);

        // A normal retry mounts the same path while that reaper is still live.
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared_store(source.path()),
            MountOptions::default(),
            false,
            MountConfig::default(),
        )
        .expect("remount the same path");
        assert!(handle.is_mounted(), "precondition: the new session is live");

        // Many reaper poll intervals pass; the new mount must stay live and
        // keep serving reads throughout.
        std::thread::sleep(Duration::from_millis(1000));
        assert!(
            handle.is_mounted(),
            "the old reaper must not unmount a newer session on the same path"
        );
        std::fs::read_dir(&path).expect("the new session must still serve reads");

        drop(holder);
        handle.unmount().expect("unmount the new session");
    }

    /// Startup I/O can block past every budget: a `skillfs-views.toml` that
    /// is a FIFO makes the mounting thread wait for a writer. Once the reaper
    /// gave up it cancels the outcome, so a thread resuming afterwards must
    /// stand down instead of creating a mount nobody would manage.
    #[test]
    fn a_thread_resuming_after_the_cleanup_deadline_creates_no_mount() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;

        if !fuse_available() {
            return;
        }
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let store = shared_store(source.path());

        let fifo = source.path().join("skillfs-views.toml");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
        // SAFETY: c_path is a valid NUL-terminated path for mkfifo.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let (session, outcome) = spawn_background_mount(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            false,
            MountConfig::default(),
        );

        // The caller gave up; the reaper's cleanup budget then expires while
        // the mounting thread is still blocked in startup I/O.
        reap_late_background_mount(
            session,
            Arc::clone(&outcome),
            mountpoint.path().to_path_buf(),
            Duration::from_millis(300),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !outcome.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "the reaper must cancel the outcome when it gives up"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Startup I/O completes only now, long after the reaper left. Opening
        // for writing unblocks the reader; closing it delivers EOF. Open
        // without blocking so a thread that died early cannot hang the test:
        // ENXIO means no reader has reached the FIFO yet.
        let open_deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                Ok(writer) => {
                    drop(writer);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                    assert!(
                        std::time::Instant::now() < open_deadline,
                        "the mounting thread never reached the views-config read"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("open the fifo for writing: {e}"),
            }
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let failure = loop {
            assert!(
                !MountHandle::path_is_mounted(mountpoint.path()),
                "no unmanaged mount may be created after the cleanup deadline"
            );
            if let Some(error) = outcome.take_failure() {
                break error;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the cancelled thread must report its outcome and exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            matches!(failure, FuseError::MountPending(ref message) if message.contains("cancelled")),
            "the thread must stand down instead of mounting, got: {failure:?}"
        );
        assert!(
            !MountHandle::path_is_mounted(mountpoint.path()),
            "no unmanaged mount may be created after the cleanup deadline"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial interleavings
    //
    // The two tests below reproduce the windows that a check-then-act fix
    // cannot close: replacing the mount *after* the identity check but before
    // the unmount command runs, and completing creation *after* the
    // cancellation check. Both need to stretch a helper invocation, so they
    // run in a dedicated child process whose PATH resolves `fusermount3`
    // through a wrapper. Mutating PATH in the shared test process would race
    // every other test that spawns the helper.
    // -----------------------------------------------------------------------

    const ADVERSARIAL_ENV: &str = "SKILLFS_ADVERSARIAL_SCENARIO";

    /// The real helper, resolved before any wrapper is installed.
    fn real_fusermount3() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("fusermount3"))
            .find(|candidate| candidate.is_file())
    }

    /// Install a `fusermount3` wrapper ahead of the real one in this
    /// process's PATH. The returned directory must outlive the scenario.
    fn install_fusermount_wrapper(body: &str) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("wrapper dir");
        let wrapper = dir.path().join("fusermount3");
        std::fs::write(&wrapper, body).expect("write wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("chmod wrapper");
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // SAFETY: callers install the wrapper in the dedicated child process
        // before spawning any mount or session thread, so no other thread
        // exists that could read the environment concurrently.
        unsafe { std::env::set_var("PATH", path) };
        dir
    }

    /// Re-execute this test binary in a child that runs only `test`, so the
    /// scenario can install a PATH wrapper or a preload without affecting
    /// other tests.
    fn run_adversarial_child_with(test: &str, env: &[(&str, &str)]) {
        let exe = std::env::current_exe().expect("current exe");
        let mut command = std::process::Command::new(exe);
        command
            .arg("--exact")
            .arg(test)
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(ADVERSARIAL_ENV, test);
        for (key, value) in env {
            command.env(key, value);
        }
        let status = command.status().expect("spawn adversarial child");
        assert!(
            status.success(),
            "the adversarial scenario {test} failed in its child process"
        );
    }

    fn run_adversarial_child(test: &str) {
        run_adversarial_child_with(test, &[]);
    }

    /// Build a preload that delays `fuse_session_mount`, the call fuser makes
    /// through libfuse3. Mounting does not go through `fusermount3` when
    /// libfuse3 is linked, so a PATH wrapper cannot stretch it.
    ///
    /// Returns `None` when no C compiler is available, so the scenario skips
    /// instead of failing on a host that cannot build the shim.
    fn mount_delay_shim() -> Option<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().ok()?;
        let source = dir.path().join("delay.c");
        let shim = dir.path().join("delay.so");
        std::fs::write(
            &source,
            "#define _GNU_SOURCE\n\
             #include <dlfcn.h>\n\
             #include <stdlib.h>\n\
             #include <unistd.h>\n\
             int fuse_session_mount(void *se, const char *mountpoint) {\n\
             \x20 const char *delay = getenv(\"SKILLFS_TEST_MOUNT_DELAY_SECS\");\n\
             \x20 if (delay != NULL) { sleep((unsigned int)atoi(delay)); }\n\
             \x20 int (*real_mount)(void *, const char *) = dlsym(RTLD_NEXT, \"fuse_session_mount\");\n\
             \x20 return real_mount(se, mountpoint);\n\
             }\n",
        )
        .ok()?;
        let status = std::process::Command::new("cc")
            .arg("-shared")
            .arg("-fPIC")
            .arg("-o")
            .arg(&shim)
            .arg(&source)
            .arg("-ldl")
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        Some((dir, shim))
    }

    /// Run a child scenario, exiting promptly even when it panics: a detached
    /// helper subprocess or a still-live mount must not keep the child alive
    /// and turn a failed assertion into a suite-wide timeout.
    fn run_child_scenario(scenario: impl FnOnce()) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(scenario)).is_err() {
            std::process::exit(1);
        }
    }

    /// Releases the paused helper even when the scenario panics, so a failing
    /// assertion cannot leave a wrapper subprocess spinning forever.
    struct ReleaseOnDrop(PathBuf);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"");
        }
    }

    /// Wait, bounded, for `path` to appear.
    fn wait_for_file(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "{} never appeared",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// An identity check cannot constrain an unmount command that has already
    /// started: the command only receives a path, so a concurrent cleanup plus
    /// retry can replace the mount before the command acts. Creation and
    /// teardown must therefore be mutually exclusive on that path.
    ///
    /// The paused `-u` stands in for the reviewer's PATH-wrapper repro: while
    /// it is in flight the old mount is detached independently and a retry is
    /// attempted. The retry must not produce a mount until teardown finished,
    /// and the released command must not detach it afterwards.
    #[test]
    fn a_paused_teardown_excludes_a_concurrent_remount() {
        if !fuse_available() {
            return;
        }
        let test = "mount::tests::a_paused_teardown_excludes_a_concurrent_remount";
        if std::env::var_os(ADVERSARIAL_ENV).is_none() {
            run_adversarial_child(test);
            return;
        }

        run_child_scenario(|| {
            let real = real_fusermount3().expect("fusermount3 on PATH");
            let source = tempfile::tempdir().expect("source tempdir");
            let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
            let markers = tempfile::tempdir().expect("marker tempdir");
            let started = markers.path().join("started");
            let release = markers.path().join("release");
            let _release_guard = ReleaseOnDrop(release.clone());
            // The spelling the mount will resolve to, known before mounting so
            // the wrapper can be installed while no mount thread exists yet.
            let path = mountpoint
                .path()
                .canonicalize()
                .expect("canonical mountpoint");

            // Only `-u` is intercepted, so installing this before the mount
            // does not affect it: mounting goes through libfuse3. The wait is
            // bounded because unwinding deletes the marker directory after the
            // release guard wrote the sentinel, and a later unmount from a
            // detached thread must not block forever on a file that is gone.
            let _wrapper = install_fusermount_wrapper(&format!(
                "#!/bin/sh\n\
             if [ \"$1\" = \"-u\" ]; then\n\
             \x20 case \"$*\" in\n\
             \x20   *\"{path}\"*)\n\
             \x20     touch \"{started}\"\n\
             \x20     i=0\n\
             \x20     while [ ! -f \"{release}\" ] && [ \"$i\" -lt 600 ]; do\n\
             \x20       sleep 0.05; i=$((i+1))\n\
             \x20     done\n\
             \x20     ;;\n\
             \x20 esac\n\
             fi\n\
             exec \"{real}\" \"$@\"\n",
                path = path.display(),
                started = started.display(),
                release = release.display(),
                real = real.display(),
            ));

            let (session, outcome) = live_session(source.path(), mountpoint.path());
            assert_eq!(
                resolved(&outcome),
                path,
                "the wrapper must target the spelling the mount resolved to"
            );
            // Keep the old session alive across its own detach, so its reaper is
            // still polling when the retry happens.
            let holder = std::fs::File::open(&path).expect("hold the old mount open");

            reap_late_background_mount(
                session,
                Arc::clone(&outcome),
                path.clone(),
                Duration::from_secs(60),
            );
            wait_for_file(&started);

            // Another cleanup detaches the old mount while our command is paused.
            let _ = std::process::Command::new("umount")
                .arg("-l")
                .arg(&path)
                .output();
            wait_until_unmounted(&path);

            let retry_mountpoint = mountpoint.path().to_path_buf();
            let retry_source = source.path().to_path_buf();
            let retry = std::thread::spawn(move || {
                mount_background_configured(
                    &retry_mountpoint,
                    &retry_source,
                    shared_store(&retry_source),
                    MountOptions::default(),
                    false,
                    MountConfig::default(),
                )
            });

            // The identity check already passed and the command is in flight: a
            // mount created now would be the one it detaches. Creation must wait.
            std::thread::sleep(Duration::from_millis(500));
            assert!(
                !MountHandle::path_is_mounted(&path),
                "creation must be excluded while a teardown holds the lifecycle lock"
            );

            std::fs::write(&release, b"").expect("release the paused unmount");
            let handle = retry
                .join()
                .expect("retry thread")
                .expect("the retry must succeed once teardown finished");
            assert!(handle.is_mounted(), "precondition: the retry is live");
            std::thread::sleep(Duration::from_millis(1000));
            assert!(
                handle.is_mounted(),
                "the released teardown must not detach the newer session"
            );
            std::fs::read_dir(&path).expect("the newer session must serve reads");

            drop(holder);
            handle.unmount().expect("unmount the retry");
        });
    }

    /// A single cancellation check before the mount call cannot cover the
    /// mount call itself: the helper can block for longer than every budget.
    /// The creating side must therefore re-check cancellation once FUSE_INIT
    /// has published its identity, and tear its own mount down.
    ///
    /// The delayed mount helper stands in for the reviewer's LD_PRELOAD
    /// repro: creation completes after the caller gave up and the reaper
    /// cancelled, so only the creating side can still clean up.
    #[test]
    fn a_creation_completing_after_cancellation_tears_itself_down() {
        if !fuse_available() {
            return;
        }
        let test = "mount::tests::a_creation_completing_after_cancellation_tears_itself_down";
        if std::env::var_os(ADVERSARIAL_ENV).is_none() {
            let Some((_shim_dir, shim)) = mount_delay_shim() else {
                // No C compiler on this host: the delay cannot be injected.
                return;
            };
            run_adversarial_child_with(
                test,
                &[
                    ("LD_PRELOAD", shim.to_str().expect("shim path")),
                    ("SKILLFS_TEST_MOUNT_DELAY_SECS", "3"),
                ],
            );
            return;
        }

        run_child_scenario(|| {
            let source = tempfile::tempdir().expect("source tempdir");
            let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");

            let (session, outcome) = spawn_background_mount(
                mountpoint.path(),
                source.path(),
                shared_store(source.path()),
                MountOptions::default(),
                false,
                MountConfig::default(),
            );
            // Prove the injection actually stretches creation before relying
            // on it. An undelayed mount is ready in well under a second, so
            // still being pending here is what shows the delay applies. If a
            // different fuser mount backend ever made the preload a no-op,
            // the reaper would tear down an on-time mount and the assertions
            // below would pass without exercising the creator-side cleanup.
            std::thread::sleep(Duration::from_millis(800));
            assert!(
                !outcome.is_ready(),
                "the fuse_session_mount delay is not taking effect; \
                 the mount came up on time"
            );

            // The caller gave up; the reaper's budget expires while the mount
            // helper is still delayed, so it cancels and leaves.
            reap_late_background_mount(
                session,
                Arc::clone(&outcome),
                mountpoint.path().to_path_buf(),
                Duration::from_millis(300),
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !outcome.is_cancelled() {
                if std::time::Instant::now() >= deadline {
                    panic!(
                        "the reaper must cancel when it gives up; ready={} failure={:?}",
                        outcome.is_ready(),
                        outcome.take_failure()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Still pending at the moment responsibility was discharged: this
            // is the interleaving the creator-side cleanup exists for.
            assert!(
                !outcome.is_ready(),
                "the mount must still be pending when the reaper cancels"
            );

            // Creation now completes past every deadline.
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while !outcome.is_ready() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the delayed creation must still reach FUSE_INIT"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while MountHandle::path_is_mounted(mountpoint.path()) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "a mount created after cancellation must be torn down by its creator"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
}
