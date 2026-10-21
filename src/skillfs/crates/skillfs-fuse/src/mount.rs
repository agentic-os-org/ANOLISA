//! Public mount entry points for the SkillFS FUSE filesystem.
//!
//! The internal [`mount_inner`] does the actual `fuser::mount2` call and
//! threads in the optional security configuration. The preferred entry
//! points are [`mount_configured`] / [`mount_background_configured`]
//! which accept a [`MountConfig`] struct. The legacy per-feature
//! functions are deprecated but remain for backward compatibility.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use skillfs_core::SharedSkillStore;
use skillfs_core::os_adapter::OsAdapterStage;
use skillfs_core::transform::TransformPipeline;
use tracing::{error, info, warn};

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
    )
}

/// Failure slot shared between a background mounting thread and the
/// spawning thread. The background thread records the [`FuseError`]
/// returned by [`mount_inner`]; the spawn-side wait reads it, so a mount
/// that never came up is reported to the caller instead of only to a
/// tracing subscriber the caller never installed.
type BackgroundMountFailure = Arc<Mutex<Option<FuseError>>>;

/// How long the background entry points wait for the mount to come live
/// (or fail) before handing out the handle.
///
/// [`mount_inner`] constructs the [`SkillFs`] runtime — including the
/// directive stage's environment-profile probe loop, which shells out to
/// `which` for every whitelisted command — *before* creating the FUSE
/// session, and on a loaded host that probing alone can take seconds. The
/// budget only bounds this wait; the mount itself is never cancelled.
#[cfg(target_os = "linux")]
const BACKGROUND_MOUNT_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between readiness polls of the mount table.
#[cfg(target_os = "linux")]
const BACKGROUND_MOUNT_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn record_background_mount_failure(slot: &BackgroundMountFailure, error: FuseError) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
}

fn take_background_mount_failure(slot: &BackgroundMountFailure) -> Option<FuseError> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Wait until the background mount is live or has failed.
///
/// Returns the session thread back on success so it can be stored in the
/// [`MountHandle`]; on failure the thread has already exited and is joined
/// here so nothing leaks.
///
/// * A failure recorded by the mounting thread is returned as `Err` — the
///   original error, not a generic timeout.
/// * This session's readiness is its own FUSE_INIT callback
///   ([`SkillFs::init`] stores it in the shared flag the thread's
///   filesystem carries): the kernel delivers FUSE_INIT as soon as the
///   session serves, so `Ok` means this mount is live. The mount table is
///   deliberately not consulted to infer readiness — an entry at the
///   mountpoint cannot say which session produced it: a stale mount torn
///   down and remounted by this thread raced every presence-based probe,
///   and a session that completed before the first probe was
///   indistinguishable from the leftover it replaced (kongche-jbw P2 on
///   #7780).
/// * A thread that exited is joined and the failure slot re-read once
///   more, so an error recorded in the window between the last empty read
///   and the thread's exit is still the one reported; only a slot that is
///   empty after the join is an abnormal exit (kongche-jbw P2 on #7780).
/// * If `budget` elapses with neither outcome (a pathologically slow
///   environment probe), an explicit [`FuseError::MountPending`] is
///   returned instead of the historical optimistic success — and the
///   still-running thread is handed to a reaper that tears a late mount
///   down again, so no unmanaged session is left behind (kongche-jbw P1
///   on #7780).
#[cfg(target_os = "linux")]
fn wait_for_background_mount(
    mountpoint: &Path,
    session: std::thread::JoinHandle<()>,
    failure: &BackgroundMountFailure,
    session_ready: Arc<AtomicBool>,
    budget: Duration,
) -> Result<std::thread::JoinHandle<()>, FuseError> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Some(error) = take_background_mount_failure(failure) {
            // The mounting thread exits right after recording a failure,
            // so joining it here cannot block.
            let _ = session.join();
            return Err(error);
        }
        if session_ready.load(Ordering::Acquire) {
            // This session's own FUSE_INIT: it is serving.
            return Ok(session);
        }
        if session.is_finished() {
            return Err(finish_after_thread_exit(session, failure));
        }
        if std::time::Instant::now() >= deadline {
            reap_late_background_mount(session, session_ready, mountpoint.to_path_buf());
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

/// Join an exited mounting thread and re-read the failure slot once more:
/// the thread may have recorded its error in the window between the wait
/// loop's last empty read and its exit — `is_finished()` only proves the
/// thread exited, not that it failed silently. Only a slot that is still
/// empty after the join is reported as an abnormal exit.
fn finish_after_thread_exit(
    session: std::thread::JoinHandle<()>,
    failure: &BackgroundMountFailure,
) -> FuseError {
    let _ = session.join();
    take_background_mount_failure(failure).unwrap_or_else(|| {
        FuseError::MountFailed(
            "background mount thread exited without reporting a failure".to_string(),
        )
    })
}

/// Tear down a late background mount after the caller gave up on it: the
/// readiness budget expired, the mounting thread keeps running, and a
/// session that still comes up would be unmanaged — nobody holds the
/// handle that would unmount it. The reaper polls until the thread exits,
/// unmounting whenever this session reports ready and its mountpoint is
/// actually mounted, and is bounded so a wedged session cannot leak the
/// reaper thread forever.
fn reap_late_background_mount(
    session: std::thread::JoinHandle<()>,
    session_ready: Arc<AtomicBool>,
    mountpoint: PathBuf,
) {
    std::thread::spawn(move || {
        let deadline =
            std::time::Instant::now() + BACKGROUND_MOUNT_READINESS_TIMEOUT.saturating_mul(2);
        while !session.is_finished() {
            if session_ready.load(Ordering::Acquire) && MountHandle::path_is_mounted(&mountpoint) {
                let _ = std::process::Command::new("fusermount3")
                    .arg("-u")
                    .arg(mountpoint.as_os_str())
                    .output();
            }
            if std::time::Instant::now() >= deadline {
                error!(
                    mountpoint = %mountpoint.display(),
                    "giving up on reaping a late background mount"
                );
                return;
            }
            std::thread::sleep(BACKGROUND_MOUNT_POLL_INTERVAL);
        }
        let _ = session.join();
    });
}

/// Non-Linux fallback: without `/proc/mounts` there is no authoritative
/// readiness probe. Keep the historical brief grace period, but still
/// check the failure slot afterwards so fast validation errors surface.
#[cfg(not(target_os = "linux"))]
fn wait_for_background_mount(
    _mountpoint: &Path,
    session: std::thread::JoinHandle<()>,
    failure: &BackgroundMountFailure,
    _session_ready: Arc<AtomicBool>,
    _budget: Duration,
) -> Result<std::thread::JoinHandle<()>, FuseError> {
    std::thread::sleep(Duration::from_millis(100));
    if let Some(error) = take_background_mount_failure(failure) {
        let _ = session.join();
        return Err(error);
    }
    Ok(session)
}

/// Mount the SkillFS FUSE filesystem in the background with a unified
/// configuration struct.
///
/// "Background" refers to the FUSE event loop, which serves on its own
/// thread — unlike [`mount_configured`], which blocks until the session
/// is unmounted. This call still waits for a real outcome within the
/// BACKGROUND_MOUNT_READINESS_TIMEOUT budget: a mount that fails
/// before or while creating the FUSE session is returned as `Err` instead
/// of a handle whose failure was only visible to a tracing subscriber,
/// and `Ok` is only returned once the mountpoint is being served (or the
/// budget ran out on a pathologically slow host).
pub fn mount_background_configured(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    config: MountConfig,
) -> Result<MountHandle, FuseError> {
    // One canonical spelling for the whole mount lifecycle
    // (kongche-jbw P2 on #7780): the kernel mount table renders the
    // resolved path, so the readiness probe, the handle's `is_mounted`
    // and the unmount must all address it. The caller's spelling —
    // relative, containing `..`, or through a symlink — would never
    // match the table's entry. A path that cannot be canonicalized
    // (it does not exist yet) keeps the raw spelling and fails inside
    // mount_inner as before. Raw OS strings flow through: non-UTF-8
    // mountpoints canonicalize and compare as bytes end to end.
    let mountpoint_path =
        std::fs::canonicalize(mountpoint).unwrap_or_else(|_| mountpoint.to_path_buf());
    let source_path = source.to_path_buf();

    let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
    let record = Arc::clone(&failure);
    // This session's own readiness signal, set by the filesystem's
    // FUSE_INIT callback: the one signal that unambiguously belongs to
    // this mount (kongche-jbw P2 on #7780).
    let session_ready = Arc::new(AtomicBool::new(false));
    // The thread takes its own mountpoint clone; the wait below keeps
    // this one.
    let mountpoint_for_thread = mountpoint_path.clone();
    let thread_session_ready = Arc::clone(&session_ready);

    let session = std::thread::spawn(move || {
        let mut opts = options;
        opts.foreground = true;
        if let Err(e) = mount_inner_with_session_ready(
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
            Some(thread_session_ready),
        ) {
            error!(error = %e, "background mount failed");
            record_background_mount_failure(&record, e);
        }
    });

    let session = wait_for_background_mount(
        &mountpoint_path,
        session,
        &failure,
        Arc::clone(&session_ready),
        BACKGROUND_MOUNT_READINESS_TIMEOUT,
    )?;

    Ok(MountHandle {
        mountpoint: mountpoint_path,
        session: Some(session),
    })
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
) -> Result<(), FuseError> {
    mount_inner_with_session_ready(
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
        None,
    )
}

/// Internal mount that accepts optional Skill Security overrides. Public
/// `mount` and `mount_background` keep their existing signatures and pass
/// `None` for both; test/embedder callers reach the sink/policy injection
/// path through [`mount_background_with_security`].
#[allow(clippy::too_many_arguments)]
fn mount_inner_with_session_ready(
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
    session_ready: Option<Arc<AtomicBool>>,
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
    let mut fs = SkillFs::new_with_pipeline(
        mountpoint.to_path_buf(),
        source.to_path_buf(),
        store,
        in_place,
        TransformPipeline::empty(),
    );
    // This session's readiness signal: FUSE_INIT sets it, and the
    // background entry's wait loop polls it (kongche-jbw P2 on #7780).
    if let Some(signal) = session_ready {
        fs = fs.with_session_ready_signal(signal);
    }
    if let Some(root) = skill_discover_root {
        fs = fs.with_skill_discover_root(root);
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
    if let Some(m) = staging_matcher {
        fs = fs.with_staging_matcher(m);
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

    match fuser::mount2(fs, mountpoint, &fuse_opts) {
        Ok(()) => {
            info!("filesystem unmounted");
            Ok(())
        }
        Err(e) => Err(FuseError::MountFailed(e.to_string())),
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
        None, None, None, None, None, None, None, None, None,
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
        None, None, None, None, None, None, None, None, None, None,
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
    // One canonical spelling for the whole mount lifecycle
    // (kongche-jbw P2 on #7780): the kernel mount table renders the
    // resolved path, so the readiness probe, the handle's `is_mounted`
    // and the unmount must all address it. The caller's spelling —
    // relative, containing `..`, or through a symlink — would never
    // match the table's entry. A path that cannot be canonicalized
    // (it does not exist yet) keeps the raw spelling and fails inside
    // mount_inner as before. Raw OS strings flow through: non-UTF-8
    // mountpoints canonicalize and compare as bytes end to end.
    let mountpoint_path =
        std::fs::canonicalize(mountpoint).unwrap_or_else(|_| mountpoint.to_path_buf());
    let source_path = source.to_path_buf();

    let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
    let record = Arc::clone(&failure);
    // This session's own readiness signal, set by the filesystem's
    // FUSE_INIT callback: the one signal that unambiguously belongs to
    // this mount (kongche-jbw P2 on #7780).
    let session_ready = Arc::new(AtomicBool::new(false));
    // The thread takes its own mountpoint clone; the wait below keeps
    // this one.
    let mountpoint_for_thread = mountpoint_path.clone();
    let thread_session_ready = Arc::clone(&session_ready);

    let session = std::thread::spawn(move || {
        let mut opts = options;
        opts.foreground = true;
        if let Err(e) = mount_inner_with_session_ready(
            &mountpoint_for_thread,
            &source_path,
            store,
            opts,
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
            Some(thread_session_ready),
        ) {
            error!(error = %e, "background mount failed");
            record_background_mount_failure(&record, e);
        }
    });

    let session = wait_for_background_mount(
        &mountpoint_path,
        session,
        &failure,
        Arc::clone(&session_ready),
        BACKGROUND_MOUNT_READINESS_TIMEOUT,
    )?;

    Ok(MountHandle {
        mountpoint: mountpoint_path,
        session: Some(session),
    })
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

    /// kongche-jbw P2 ("completion before the first probe"): a session
    /// that is already ready when the wait starts returns Ok on the
    /// first check — no probe interval, no budget.
    #[test]
    fn a_ready_session_returns_immediately() {
        let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
        let ready = Arc::new(AtomicBool::new(true));
        let session = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
        });
        let started = std::time::Instant::now();
        let _handle = wait_for_background_mount(
            std::path::Path::new("/nonexistent"),
            session,
            &failure,
            ready,
            Duration::from_secs(30),
        )
        .expect("a ready session is Ok");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "readiness must return without polling, took {:?}",
            started.elapsed()
        );
    }

    /// kongche-jbw P1: a session that is neither ready nor failed within
    /// the budget returns the explicit `MountPending` error — not a
    /// handle to a mount that may not exist.
    #[test]
    fn a_never_ready_session_times_out_as_pending() {
        let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
        let ready = Arc::new(AtomicBool::new(false));
        let session = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
        });
        let started = std::time::Instant::now();
        let error = wait_for_background_mount(
            std::path::Path::new("/nonexistent"),
            session,
            &failure,
            ready,
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

    /// kongche-jbw P2 (error publication): joining an exited thread and
    /// re-reading the slot preserves an error the thread recorded just
    /// before exiting — the window an earlier empty read cannot see.
    #[test]
    fn a_late_recorded_error_survives_the_thread_exit() {
        let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
        let thread_failure = Arc::clone(&failure);
        let session = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            record_background_mount_failure(
                &thread_failure,
                FuseError::InvalidMountPoint("late failure".to_string()),
            );
        });
        while !session.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = finish_after_thread_exit(session, &failure);
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
            let failure: BackgroundMountFailure = Arc::new(Mutex::new(None));
            let thread_failure = Arc::clone(&failure);
            let session = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(round % 5));
                record_background_mount_failure(
                    &thread_failure,
                    FuseError::PermissionDenied(format!("round {round}")),
                );
            });
            let error = wait_for_background_mount(
                std::path::Path::new("/nonexistent"),
                session,
                &failure,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(10),
            )
            .expect_err("a failed session must surface its error");
            assert!(
                matches!(error, FuseError::PermissionDenied(ref message) if message.contains(&format!("round {round}"))),
                "round {round}: the original error must win the race, got: {error:?}"
            );
        }
    }

    /// kongche-jbw P1 (late-mount cleanup): a live session handed to the
    /// reaper is unmounted and its thread joined — the shape a timeout
    /// hands off when the mount still comes up after the caller gave up.
    #[test]
    fn a_late_live_mount_is_torn_down_and_reaped() {
        if !fuse_available() {
            return;
        }
        let source = tempfile::tempdir().expect("source tempdir");
        let mut store = skillfs_core::store::SkillStore::new();
        store.load_from_directory(source.path(), &skillfs_core::ParseConfig::default());
        let store: skillfs_core::SharedSkillStore = Arc::new(parking_lot::RwLock::new(store));
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");

        let mut handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            false,
            MountConfig::default(),
        )
        .expect("background mount");
        assert!(handle.is_mounted(), "precondition: the session is live");

        let session = handle
            .session
            .take()
            .expect("the handle owns the session thread");
        reap_late_background_mount(
            session,
            Arc::new(AtomicBool::new(true)),
            handle.mountpoint.clone(),
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while MountHandle::path_is_mounted(&handle.mountpoint) {
            assert!(
                std::time::Instant::now() < deadline,
                "the reaper must tear the late mount down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
