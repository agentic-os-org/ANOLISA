//! `SkillFs` filesystem implementation.
//!
//! This module owns the `SkillFs` struct, its inherent helper methods,
//! and the single `impl Filesystem for SkillFs` trait block. Per the
//! refactor contract, the trait impl is kept as one block and delegates
//! to inherent methods on `SkillFs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use fuser::{
    FUSE_ROOT_ID, FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};
use skillfs_core::os_adapter::OsAdapterStage;
use skillfs_core::transform::{DirectiveStage, TransformPipeline};
use skillfs_core::{SharedSkillStore, env::EnvironmentProfile, views::ViewsConfig};
use tracing::{info, warn};

use crate::handles::HandleManager;
use crate::inode::InodeManager;
use crate::path::SkillLayout;
use crate::security::{
    ActiveSkillResolver, InstallerStagingController, NoopEventSink, NotifyController,
    PendingInstallController, PostPublishGraceController, ProcessIdentityResolver,
    QuietTimeoutController, RefreshController, RuntimeMetricsSink, SecurityPolicy, SkillEventSink,
    SkillMetaProtectionPolicy, StagingMatcher, TrustedWriterConfig, default_identity_resolver,
};
use crate::sync::{SyncEvent, spawn_sync_worker};

mod callbacks;
mod discover;
mod events;
mod paths;
mod policy;
mod read_resolution;
mod transform_cache;

// ---------------------------------------------------------------------------
// Filesystem Implementation
// ---------------------------------------------------------------------------

/// SkillFS FUSE filesystem implementation.
pub struct SkillFs {
    #[allow(dead_code)]
    mountpoint: PathBuf,
    /// Physical source directory (where skillfs-views.toml lives).
    source: PathBuf,
    store: SharedSkillStore,
    handles: HandleManager,
    inodes: InodeManager,
    /// Ordered read-time transform pipeline applied to `SKILL.md` bytes.
    ///
    /// Constructed empty; `mount_inner` enables stages from configuration. The
    /// directive/compiler stage, when enabled, always runs first; the opt-in OS
    /// adapter, when configured, runs second, preserving the fixed
    /// `directive -> os_adapter` order. Either stage may be absent (adapter-only
    /// or fully raw). Built once at mount startup so the per-read hot path only
    /// runs in-memory string work. Environment detection happens only when the
    /// directive stage is enabled.
    transform_pipeline: TransformPipeline,
    transform_cache: transform_cache::TransformCache,
    /// View configuration loaded from skillfs-views.toml (if present).
    views_config: Option<ViewsConfig>,
    /// Optional reader-visible root for paths emitted by `skill-discover`.
    /// `None` keeps the physical source-path output used by local mounts.
    skill_discover_root: Option<PathBuf>,
    /// Pre-opened fd to source dir (in-place mode). Bypasses the FUSE mount
    /// layer so file reads still reach the real inode after over-mounting.
    source_dirfd: Option<std::fs::File>,
    /// Whether we are mounted in-place (source == mountpoint).
    in_place: bool,
    /// Skill directory layout mode (flat or hermes).
    skill_layout: SkillLayout,
    /// Channel to send sync events to the background sync worker.
    sync_tx: Option<std::sync::mpsc::Sender<SyncEvent>>,
    /// Skill Security policy. The S1 default is
    /// [`SkillMetaProtectionPolicy`], which denies mutating operations
    /// under `.skill-meta/**`. Embedders/tests can swap it for
    /// [`security::PermissivePolicy`] via [`SkillFs::with_policy`].
    policy: Arc<dyn SecurityPolicy>,
    /// Skill Security event sink (Package S0 seam). Default drops events.
    event_sink: Arc<dyn SkillEventSink>,
    /// D1.1 ledger-driven active-skill mapping. When `Some`, the read
    /// paths under `/skills` (readdir, lookup, getattr, open/read)
    /// consult the resolver to decide whether each skill is hidden,
    /// served from the live source, or served from a trusted snapshot
    /// under `.skill-meta/versions/...`. When `None` the pre-D1.1 read
    /// paths are preserved bit-for-bit. Write paths (create, write,
    /// rename, unlink, setattr) intentionally never consult the
    /// resolver — D1.1 is read-only by design; snapshots are read-only
    /// and writes still target the live source.
    active_resolver: Option<Arc<ActiveSkillResolver>>,
    /// Per-skill debounce + refresh controller. When `Some`, successful
    /// mutating FUSE callbacks (`mkdir`, `create`, `write`, `rename`,
    /// `unlink`, `rmdir`, truncate-via-`setattr`) enqueue a debounced
    /// refresh through the External Decision pipeline; the controller
    /// then updates [`Self::active_resolver`] and emits a security
    /// event. Default is `None`. The controller itself filters out
    /// `.skill-meta/**`, `skill-discover`, and lifecycle reserved
    /// roots so observation cannot create a feedback loop with the
    /// ledger's own snapshot writes.
    refresh_controller: Option<Arc<RefreshController>>,
    /// N2 notify controller. When `Some`, successful mutating FUSE
    /// callbacks enqueue debounced `skill_ledger.skillfs_notify_change`
    /// notifications to the external daemon. Notify failure is
    /// diagnostic only and never changes the active resolver.
    notify_controller: Option<Arc<NotifyController>>,
    /// Trusted writer process gate. Default disabled, in
    /// which case `.skill-meta/**` mutation falls through to the
    /// existing [`SkillMetaProtectionPolicy`] deny path. When enabled
    /// the gate compares the FUSE caller pid's resolved process name
    /// against the configured trusted writer name; on match,
    /// `.skill-meta/**` mutation is allowed and an audit
    /// `PolicyDecision` (allowed) record carries
    /// `trusted_writer=<name>` in its `detail` string. The bypass is
    /// scoped strictly to `.skill-meta/**` and never relaxes
    /// lifecycle reservation, virtual paths, `skill-discover`, or
    /// other policy.
    trusted_writer: TrustedWriterConfig,
    /// Identity resolver paired with [`Self::trusted_writer`]. Default
    /// is the Linux `/proc/<pid>/comm` resolver; tests inject a
    /// deterministic in-memory resolver via
    /// [`SkillFs::with_trusted_writer_identity`].
    trusted_writer_identity: Arc<dyn ProcessIdentityResolver>,
    staging_matcher: Option<Arc<StagingMatcher>>,
    staging_controller: Option<Arc<InstallerStagingController>>,
    quiet_timeout_controller: Option<Arc<QuietTimeoutController>>,
    pending_install_controller: Option<Arc<PendingInstallController>>,
    post_publish_controller: Option<Arc<PostPublishGraceController>>,
    /// Best-effort runtime metric delta sink. When `Some`, agent-visible skill
    /// opens emit a `skill_hit` delta and — when an active resolver made a real
    /// security decision — a `policy_allow` / `policy_fallback` / `policy_denied`
    /// delta. Metric emission is best-effort and never changes FUSE behavior.
    runtime_metrics: Option<Arc<RuntimeMetricsSink>>,
}

impl SkillFs {
    /// Create a new SkillFS filesystem.
    ///
    /// `in_place`: the FUSE mount will be placed on `source` itself, so all
    /// physical reads must go through the pre-opened fd (`/proc/self/fd/{n}`)
    /// to bypass the FUSE layer.
    ///
    /// The read-time transform pipeline defaults to the directive/compiler
    /// stage enabled (byte-for-byte compatible with pre-pipeline SkillFS), so
    /// embedders that construct `SkillFs` directly keep compiling `SKILL.md` on
    /// read. This detects the runtime [`EnvironmentProfile`]. Managed mounts go
    /// through [`Self::new_with_pipeline`] instead, which lets `mount_inner`
    /// build an adapter-only or empty pipeline without paying for detection.
    pub fn new(
        mountpoint: PathBuf,
        source: PathBuf,
        store: SharedSkillStore,
        in_place: bool,
    ) -> Self {
        Self::new_with_pipeline(
            mountpoint,
            source,
            store,
            in_place,
            TransformPipeline::directive_only(EnvironmentProfile::detect()),
        )
    }

    /// Crate-internal constructor accepting a preconfigured transform pipeline.
    ///
    /// `mount_inner` uses this to install the pipeline it derived from mount
    /// configuration — enabling the directive stage (and thus environment
    /// detection) only when configured, so an adapter-only or fully-raw mount
    /// never runs `EnvironmentProfile::detect`.
    pub(crate) fn new_with_pipeline(
        mountpoint: PathBuf,
        source: PathBuf,
        store: SharedSkillStore,
        in_place: bool,
        transform_pipeline: TransformPipeline,
    ) -> Self {
        // Load views config from the source directory if present.
        let views_config = ViewsConfig::load(&source);
        if views_config.is_some() {
            info!("loaded skillfs-views.toml from {}", source.display());
        }

        // In in-place mode open the source dir before the mount so we hold an
        // fd that still points at the underlying directory after over-mounting.
        let source_dirfd = if in_place {
            match std::fs::File::open(&source) {
                Ok(f) => {
                    info!(
                        "opened source dirfd for in-place mount: {}",
                        source.display()
                    );
                    Some(f)
                }
                Err(e) => {
                    warn!("failed to open source dirfd ({}): {}", source.display(), e);
                    None
                }
            }
        } else {
            None
        };

        // Spawn the background sync worker.
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let sync_store = store.clone();
        spawn_sync_worker(sync_rx, sync_store);

        let fs = Self {
            mountpoint,
            source,
            store,
            handles: HandleManager::new(),
            inodes: InodeManager::new(),
            transform_pipeline,
            transform_cache: transform_cache::TransformCache::default(),
            views_config,
            skill_discover_root: None,
            source_dirfd,
            in_place,
            skill_layout: SkillLayout::Flat,
            sync_tx: Some(sync_tx),
            policy: Arc::new(SkillMetaProtectionPolicy),
            event_sink: Arc::new(NoopEventSink),
            active_resolver: None,
            refresh_controller: None,
            notify_controller: None,
            trusted_writer: TrustedWriterConfig::disabled(),
            trusted_writer_identity: default_identity_resolver(),
            staging_matcher: None,
            staging_controller: None,
            quiet_timeout_controller: None,
            pending_install_controller: None,
            post_publish_controller: None,
            runtime_metrics: None,
        };

        // In normal mode pre-populate the /skills inode.
        // In in-place mode the root IS the skills dir — no sub-inode needed.
        if !in_place {
            fs.inodes
                .allocate("/skills", FileType::Directory, FUSE_ROOT_ID);
        }

        // L1: pre-allocate the inbox virtual root in both modes so
        // root readdir/opendir snapshots include a stable inode for
        // it. The inbox is always visible regardless of in-place vs
        // normal layout.
        fs.inodes
            .allocate("/.skillfs-inbox", FileType::Directory, FUSE_ROOT_ID);

        fs
    }

    /// Override the Skill Security policy. The S1 default is
    /// [`SkillMetaProtectionPolicy`]; tests/embedders that need fully
    /// permissive behaviour can plug in
    /// [`security::PermissivePolicy`] here.
    ///
    /// Builder-style; preserves backward compatibility with the existing
    /// `SkillFs::new` callers that do not configure security.
    pub fn with_policy(mut self, policy: Arc<dyn SecurityPolicy>) -> Self {
        self.policy = policy;
        self
    }

    /// Override the Skill Security event sink. Default is [`NoopEventSink`].
    pub fn with_event_sink(mut self, sink: Arc<dyn SkillEventSink>) -> Self {
        self.event_sink = sink;
        self
    }

    /// Advertise secondary skills through a reader-visible view root.
    ///
    /// Each `skill-discover` `source_path` becomes
    /// `<root>/<skill-name>/SKILL.md`. This is useful when readers share the
    /// FUSE view but cannot access the physical source directory. Without this
    /// override, `skill-discover` retains its physical-path output.
    pub fn with_skill_discover_root(mut self, root: PathBuf) -> Self {
        self.skill_discover_root = Some(root);
        self
    }

    /// Attach a D1.1 ledger-driven active-skill resolver.
    ///
    /// When attached, the `/skills` read paths (readdir, lookup, getattr,
    /// open/read of `SKILL.md` and ordinary passthrough files) consult the
    /// resolver to decide visibility and which physical directory backs
    /// each skill:
    ///
    /// * [`ActiveTarget::Current`] — read the live source directory
    ///   (same behavior as the no-resolver default).
    /// * [`ActiveTarget::Snapshot`] — read the trusted snapshot
    ///   directory under `<skill>/.skill-meta/versions/...`.
    ///   `SKILL.md` reads still go through
    ///   [`compiler::compile`] but against the snapshot's `SKILL.md`,
    ///   preserving the compiled-read semantics demanded by the
    ///   invariants section of `CLAUDE.md`.
    /// * [`ActiveTarget::Hidden`] — the skill is omitted from
    ///   `/skills` readdir and `lookup` returns `ENOENT`.
    ///
    /// Skills that are present in the store but absent from the
    /// resolver default to **hidden** by default: the security flow wants
    /// "not certified yet" skills to be invisible until the ledger
    /// explicitly publishes a decision.
    ///
    /// The skill-discover virtual skill is never gated by the resolver
    /// — it remains visible and read-only so operators always have an
    /// entry point to inspect secondary views.
    ///
    /// Write paths (create / write / rename / unlink / setattr / mkdir
    /// / rmdir / symlink / link / mknod / xattr) intentionally do
    /// **not** consult the resolver in D1.1 — writes still target the
    /// live source and snapshots are strictly read-only.
    pub fn with_active_resolver(mut self, resolver: Arc<ActiveSkillResolver>) -> Self {
        self.active_resolver = Some(resolver);
        self
    }

    /// Attach a per-skill debounce refresh controller. After successful
    /// mutating FUSE operations on ordinary skill paths, SkillFS calls
    /// [`RefreshController::observe_mutation`] with the owning
    /// skill, the relative path, and a [`MutationKind`] tag; the
    /// controller debounces per skill and runs the External Decision
    /// pipeline on its own worker.
    ///
    /// Without this builder call SkillFS behaves exactly as before
    /// even when an [`ActiveSkillResolver`] is attached. The controller
    /// respects skill-discover, lifecycle reserved roots, and
    /// `.skill-meta/**` paths internally so the FUSE wiring does not
    /// need to repeat those filters.
    pub fn with_refresh_controller(mut self, controller: Arc<RefreshController>) -> Self {
        self.refresh_controller = Some(controller);
        self
    }

    #[deprecated(note = "use with_refresh_controller")]
    pub fn with_demo_refresh_controller(self, controller: Arc<RefreshController>) -> Self {
        self.with_refresh_controller(controller)
    }

    /// Attach an N2 notify controller. After successful mutating FUSE
    /// operations, the controller debounces per skill and sends
    /// `skill_ledger.skillfs_notify_change` to the external daemon.
    /// Notify failure is diagnostic only and never changes the active
    /// resolver.
    pub fn with_notify_controller(mut self, controller: Arc<NotifyController>) -> Self {
        self.notify_controller = Some(controller);
        self
    }

    /// Configure the Trusted writer gate.
    ///
    /// When `config` is enabled (`expected_process_name = Some(...)`),
    /// `.skill-meta/**` mutation requests whose FUSE-caller pid
    /// resolves to the configured process name are allowed despite
    /// [`SkillMetaProtectionPolicy`] denying them. The bypass is
    /// strictly scoped to `.skill-meta/**`; lifecycle reserved roots,
    /// virtual paths, `skill-discover`, xattr policy, symlink/link/FIFO
    /// policy, and every non-`.skill-meta` write surface are
    /// unaffected. Default is disabled.
    pub fn with_trusted_writer(mut self, config: TrustedWriterConfig) -> Self {
        self.trusted_writer = config;
        self
    }

    /// Override the identity resolver used together with
    /// [`Self::with_trusted_writer`]. Default is
    /// [`security::LinuxProcCommResolver`] (Linux-only); non-Linux
    /// targets resolve to `None`, which the gate treats as deny.
    /// Tests inject a deterministic resolver here.
    pub fn with_trusted_writer_identity(
        mut self,
        resolver: Arc<dyn ProcessIdentityResolver>,
    ) -> Self {
        self.trusted_writer_identity = resolver;
        self
    }

    pub fn with_staging_matcher(mut self, matcher: Arc<StagingMatcher>) -> Self {
        self.staging_matcher = Some(matcher);
        self
    }

    pub fn with_staging_controller(mut self, controller: Arc<InstallerStagingController>) -> Self {
        self.staging_controller = Some(controller);
        self
    }

    pub fn with_quiet_timeout_controller(
        mut self,
        controller: Arc<QuietTimeoutController>,
    ) -> Self {
        self.quiet_timeout_controller = Some(controller);
        self
    }

    pub fn with_pending_install_controller(
        mut self,
        controller: Arc<PendingInstallController>,
    ) -> Self {
        self.pending_install_controller = Some(controller);
        self
    }

    pub fn with_post_publish_controller(
        mut self,
        controller: Arc<PostPublishGraceController>,
    ) -> Self {
        self.post_publish_controller = Some(controller);
        self
    }

    /// Attach a best-effort runtime metric delta sink. When set, agent-visible
    /// skill opens and real security/policy decisions emit `runtime_metric`
    /// JSONL deltas. Emission never changes FUSE behavior or errno mapping.
    pub fn with_runtime_metrics(mut self, sink: Arc<RuntimeMetricsSink>) -> Self {
        self.runtime_metrics = Some(sink);
        self
    }

    /// Record one agent-visible skill open (`skill_hit` delta). Best-effort.
    pub(super) fn metric_skill_hit(&self) {
        if let Some(s) = &self.runtime_metrics {
            s.record_skill_hit();
        }
    }

    /// Record a security/policy allow decision (`policy_allow` delta). Best-effort.
    pub(super) fn metric_policy_allow(&self) {
        if let Some(s) = &self.runtime_metrics {
            s.record_policy_allow();
        }
    }

    /// Record a security/policy fallback decision (`policy_fallback` delta). Best-effort.
    pub(super) fn metric_policy_fallback(&self) {
        if let Some(s) = &self.runtime_metrics {
            s.record_policy_fallback();
        }
    }

    /// Record a security/policy denial (`policy_denied` delta). Best-effort.
    pub(super) fn metric_policy_denied(&self) {
        if let Some(s) = &self.runtime_metrics {
            s.record_policy_denied();
        }
    }

    pub fn with_skill_layout(mut self, layout: SkillLayout) -> Self {
        self.skill_layout = layout;
        self
    }

    /// Enable or disable the directive/compiler transform stage.
    ///
    /// Enabling it detects the runtime [`EnvironmentProfile`] and binds a fresh
    /// [`DirectiveStage`]; disabling it clears the stage. Environment detection
    /// (which probes the command whitelist via `which`) therefore happens
    /// **only** when the directive stage is enabled — an adapter-only or fully
    /// raw pipeline never probes the environment. Idempotent — the stage can
    /// never be present twice.
    pub fn with_directive_enabled(mut self, enabled: bool) -> Self {
        let stage = enabled.then(|| DirectiveStage::new(EnvironmentProfile::detect()));
        self.transform_pipeline.set_directive(stage);
        self
    }

    /// Set the opt-in OS adapter as the second transform stage.
    ///
    /// The stage always runs after the directive/compiler stage, preserving
    /// the fixed `directive -> os_adapter` order. Only `SKILL.md` reads flow
    /// through the pipeline, so the adapter never sees other file types. The
    /// stage is pre-compiled (rules already parsed, validated, and specialized
    /// for the resolved target), so no YAML parsing, OS detection, or I/O
    /// occurs on the per-read hot path. Idempotent — the adapter can never be
    /// present twice.
    pub fn with_os_adapter_stage(mut self, stage: OsAdapterStage) -> Self {
        self.transform_pipeline.set_os_adapter(stage);
        self
    }

    /// Ordered transform stage names, for content-free init diagnostics.
    pub(crate) fn transform_stage_names(&self) -> Vec<&'static str> {
        self.transform_pipeline.stage_names()
    }

    /// Parse a FUSE path using this filesystem's layout and in-place mode.
    ///
    /// For [`SkillLayout::Hermes`] the lexical parser cannot tell a
    /// top-level skill (`skill/SKILL.md`) from a category container
    /// (`category/skill/SKILL.md`) — it classifies every top-level entry
    /// as a category. Real Hermes workspaces mix both, so this wrapper
    /// probes the physical source and rewrites a top-level skill and its
    /// descendants back into the flat [`PathType`] variants
    /// (`SkillDir` / `SkillMd` / `Passthrough`) that the flat code paths
    /// already handle. Categorized nested skills are left untouched.
    pub(super) fn parse_fuse_path(&self, path: &std::path::Path) -> crate::path::PathType {
        use crate::path::PathType;
        let parsed = crate::path::parse_path_with_layout(path, self.in_place, self.skill_layout);
        if self.skill_layout != SkillLayout::Hermes {
            return parsed;
        }
        if self.hermes_has_symlinked_boundary(&parsed) {
            return PathType::Invalid;
        }
        match parsed {
            PathType::CategoryDir { category } if self.hermes_is_top_level_skill(&category) => {
                PathType::SkillDir {
                    skill_name: category,
                }
            }
            PathType::NestedSkillDir {
                category,
                skill_name,
            } if self.hermes_is_top_level_skill(&category) => {
                if skill_name == "SKILL.md" {
                    PathType::SkillMd {
                        skill_name: category,
                    }
                } else {
                    PathType::Passthrough {
                        skill_name: category,
                        relative_path: std::path::PathBuf::from(skill_name),
                    }
                }
            }
            PathType::NestedSkillMd {
                category,
                skill_name,
            } if self.hermes_is_top_level_skill(&category) => PathType::Passthrough {
                skill_name: category,
                relative_path: std::path::Path::new(&skill_name).join("SKILL.md"),
            },
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } if self.hermes_is_top_level_skill(&category) => PathType::Passthrough {
                skill_name: category,
                relative_path: std::path::Path::new(&skill_name).join(relative_path),
            },
            // A plain *file* directly under a category (e.g.
            // `apple/README.md`) is lexically labelled `NestedSkillDir`
            // even though it is not a directory. Rewrite it to
            // `CategoryPassthrough` so `lookup`/`open`/`read` treat it as an
            // ordinary passthrough file instead of a (broken) skill dir.
            //
            // Non-skill *directories* (`apple/docs`) and their descendants
            // are intentionally NOT reclassified: they stay
            // `NestedSkillDir` / `NestedPassthrough` and are ungated via
            // `resolve_hermes_nested_read`. This keeps brand-new install /
            // staging directories (which have no `SKILL.md` yet) on the
            // nested-skill code path the installer machinery depends on.
            PathType::NestedSkillDir {
                category,
                skill_name,
            } if self.hermes_category_child_is_file(&category, &skill_name) => {
                PathType::CategoryPassthrough {
                    name: category,
                    relative_path: std::path::PathBuf::from(skill_name),
                }
            }
            other => other,
        }
    }

    fn hermes_has_symlinked_boundary(&self, path_type: &crate::path::PathType) -> bool {
        use crate::path::PathType;

        let (category, nested) = match path_type {
            PathType::CategoryDir { category } => (category, None),
            PathType::NestedSkillDir {
                category,
                skill_name,
            }
            | PathType::NestedSkillMd {
                category,
                skill_name,
            }
            | PathType::NestedPassthrough {
                category,
                skill_name,
                ..
            } => (category, Some(skill_name)),
            _ => return false,
        };

        let category_path = self.source_base().join(category);
        if std::fs::symlink_metadata(&category_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return true;
        }

        // A second Hermes component is structural only below a category.
        // Below a real top-level Skill it is ordinary passthrough content,
        // whose existing same-Skill symlink policy remains unchanged.
        if self.hermes_is_top_level_skill(category) {
            return false;
        }

        nested.is_some_and(|name| {
            std::fs::symlink_metadata(category_path.join(name))
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
    }

    fn virtual_file_attr(&self, size: u64) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: 0,
            size,
            blocks: size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    fn dir_attr(&self) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    /// Emit a WARN log when a write operation is rejected on the read-only mount.
    fn ro_warn(&self, op: &str, path_hint: &str) {
        let mountpoint = self.mountpoint.display().to_string();
        warn!(
            op,
            path = path_hint,
            mountpoint,
            "SkillFS is read-only while mounted — write op rejected. \
             To install or modify skills, unmount first:\n  \
             fusermount3 -u '{mountpoint}'\n  \
             or press Ctrl-C / send SIGTERM to the skillfs process."
        );
    }
}

impl Filesystem for SkillFs {
    fn forget(&mut self, _req: &Request, ino: u64, nlookup: u64) {
        self.inodes.forget(ino, nlookup);
    }

    fn batch_forget(&mut self, _req: &Request, nodes: &[fuser::fuse_forget_one]) {
        let items: Vec<(u64, u64)> = nodes.iter().map(|n| (n.nodeid, n.nlookup)).collect();
        self.inodes.batch_forget(&items);
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &std::ffi::OsStr, reply: ReplyEntry) {
        self.lookup_impl(_req, parent, name, reply)
    }

    fn getattr(&mut self, _req: &Request, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        self.getattr_impl(_req, ino, fh, reply)
    }

    fn read(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        self.read_impl(req, ino, fh, offset, size, _flags, _lock_owner, reply)
    }

    fn open(&mut self, req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        self.open_impl(req, ino, flags, reply)
    }

    fn release(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.release_impl(_req, _ino, fh, _flags, _lock_owner, _flush, reply)
    }

    fn flush(&mut self, _req: &Request, _ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        self.flush_impl(_req, _ino, fh, _lock_owner, reply)
    }

    fn fsync(&mut self, _req: &Request, _ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        self.fsync_impl(_req, _ino, fh, datasync, reply)
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        self.opendir_impl(_req, ino, _flags, reply)
    }

    fn readdir(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory) {
        self.readdir_impl(_req, ino, fh, offset, reply)
    }

    fn releasedir(&mut self, _req: &Request, ino: u64, fh: u64, _flags: i32, reply: ReplyEmpty) {
        self.releasedir_impl(_req, ino, fh, _flags, reply)
    }

    // -----------------------------------------------------------------------
    // Write operations — passthrough to physical filesystem.
    // Only readdir is virtualized; all other I/O goes to the underlying
    // directory via source_base() (which uses /proc/self/fd/{n} in in-place
    // mode to bypass the FUSE layer).
    // -----------------------------------------------------------------------

    fn write(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        self.write_impl(
            req,
            ino,
            fh,
            offset,
            data,
            _write_flags,
            _flags,
            _lock_owner,
            reply,
        )
    }

    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        self.create_impl(req, parent, name, mode, umask, flags, reply)
    }

    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        self.mkdir_impl(req, parent, name, mode, umask, reply)
    }

    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        self.mknod_impl(req, parent, name, mode, umask, _rdev, reply)
    }

    fn unlink(&mut self, req: &Request, parent: u64, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        self.unlink_impl(req, parent, name, reply)
    }

    fn rmdir(&mut self, req: &Request, parent: u64, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        self.rmdir_impl(req, parent, name, reply)
    }

    fn rename(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        newparent: u64,
        newname: &std::ffi::OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        self.rename_impl(req, parent, name, newparent, newname, flags, reply)
    }

    fn setattr(
        &mut self,
        req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        self.setattr_impl(
            req, ino, mode, uid, gid, size, atime, mtime, _ctime, _fh, _crtime, _chgtime,
            _bkuptime, _flags, reply,
        )
    }

    fn readlink(&mut self, req: &Request, ino: u64, reply: ReplyData) {
        self.readlink_impl(req, ino, reply)
    }

    fn symlink(
        &mut self,
        req: &Request,
        parent: u64,
        link_name: &std::ffi::OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        self.symlink_impl(req, parent, link_name, target, reply)
    }

    fn link(
        &mut self,
        req: &Request,
        ino: u64,
        newparent: u64,
        newname: &std::ffi::OsStr,
        reply: ReplyEntry,
    ) {
        self.link_impl(req, ino, newparent, newname, reply)
    }

    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        self.statfs_impl(_req, _ino, reply)
    }

    fn access(&mut self, req: &Request, ino: u64, mask: i32, reply: ReplyEmpty) {
        self.access_impl(req, ino, mask, reply)
    }

    fn fsyncdir(&mut self, _req: &Request, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        self.fsyncdir_impl(_req, ino, fh, datasync, reply)
    }

    // -----------------------------------------------------------------------
    // Extended attributes (Package T3 — minimal Linux passthrough)
    //
    // Only the `user.*` namespace is accepted for ordinary passthrough leaves
    // under a skill. `security.*`, `trusted.*`, `system.*`, and any unknown
    // namespace are rejected up-front with `EOPNOTSUPP` so SkillFS does not
    // become a back door for namespace categories whose policy lives in the
    // kernel/LSM and not in this filesystem.
    //
    // Virtual paths (root, `/skills`, skill dirs, compiled `SKILL.md`,
    // `skill-discover/SKILL.md`, and the lifecycle reserved roots) do not
    // persist xattrs. They return `EOPNOTSUPP` for every xattr surface so
    // callers see a deterministic, non-leaking answer regardless of whether
    // a physical backing path happens to exist.
    //
    // `.skill-meta/**` mutations route through the existing
    // `SkillMetaProtectionPolicy` gate via `enforce_skill_meta`, which emits a
    // `PolicyDenied` event and surfaces `EACCES`. Reads/list under
    // `.skill-meta/**` follow physical errno so administrators can still
    // inspect metadata xattrs through the mount.
    //
    // Physical passthrough goes through the no-follow xattr syscalls
    // (`lgetxattr` / `lsetxattr` / `llistxattr` / `lremovexattr`) to match
    // the `symlink_metadata`-based lookup/getattr behavior introduced in
    // Package I — a symlink leaf operates on the symlink's own xattrs rather
    // than silently following to the target.
    // -----------------------------------------------------------------------

    fn getxattr(
        &mut self,
        req: &Request,
        ino: u64,
        name: &std::ffi::OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        self.getxattr_impl(req, ino, name, size, reply)
    }

    fn listxattr(&mut self, req: &Request, ino: u64, size: u32, reply: ReplyXattr) {
        self.listxattr_impl(req, ino, size, reply)
    }

    fn setxattr(
        &mut self,
        req: &Request,
        ino: u64,
        name: &std::ffi::OsStr,
        value: &[u8],
        flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        self.setxattr_impl(req, ino, name, value, flags, _position, reply)
    }

    fn removexattr(&mut self, req: &Request, ino: u64, name: &std::ffi::OsStr, reply: ReplyEmpty) {
        self.removexattr_impl(req, ino, name, reply)
    }
}
