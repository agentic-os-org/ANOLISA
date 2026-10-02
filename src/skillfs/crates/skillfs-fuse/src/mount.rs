//! Public mount entry points for the SkillFS FUSE filesystem.
//!
//! The internal [`mount_inner`] does the actual `fuser::mount2` call and
//! threads in the optional security configuration. The preferred entry
//! points are [`mount_configured`] / [`mount_background_configured`]
//! which accept a [`MountConfig`] struct. The legacy per-feature
//! functions are deprecated but remain for backward compatibility.

use std::path::{Path, PathBuf};
use std::sync::Arc;
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

/// Mount the SkillFS FUSE filesystem in the background (non-blocking)
/// with a unified configuration struct.
pub fn mount_background_configured(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
    config: MountConfig,
) -> Result<MountHandle, FuseError> {
    let mountpoint_path = mountpoint.to_path_buf();
    let source_path = source.to_path_buf();

    let handle = std::thread::spawn(move || {
        let mut opts = options;
        opts.foreground = true;
        if let Err(e) = mount_inner(
            &mountpoint_path,
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
        ) {
            error!(error = %e, "background mount failed");
        }
    });

    std::thread::sleep(Duration::from_millis(100));

    Ok(MountHandle {
        mountpoint: mountpoint.to_path_buf(),
        session: Some(handle),
    })
}

/// Internal mount that accepts optional Skill Security overrides. Public
/// `mount` and `mount_background` keep their existing signatures and pass
/// `None` for both; test/embedder callers reach the sink/policy injection
/// path through [`mount_background_with_security`].
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
        let mountinfo = std::fs::read_to_string("/proc/mounts").ok();
        if let Some(info) = mountinfo {
            let mount_str = mountpoint.to_string_lossy();
            if info
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(&*mount_str))
            {
                warn!(mountpoint = %mountpoint.display(), "mount point already mounted, attempting cleanup");
                let _ = std::process::Command::new("fusermount3")
                    .args(["-u", &mountpoint.to_string_lossy()])
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
    let mountpoint_path = mountpoint.to_path_buf();
    let source_path = source.to_path_buf();

    let handle = std::thread::spawn(move || {
        let mut opts = options;
        opts.foreground = true;
        if let Err(e) = mount_inner(
            &mountpoint_path,
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
        ) {
            error!(error = %e, "background mount failed");
        }
    });

    std::thread::sleep(Duration::from_millis(100));

    Ok(MountHandle {
        mountpoint: mountpoint.to_path_buf(),
        session: Some(handle),
    })
}
