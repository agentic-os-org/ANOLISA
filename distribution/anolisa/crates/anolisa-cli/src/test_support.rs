//! Isolated filesystem and runtime inputs shared by CLI unit tests.

use std::path::{Path, PathBuf};

use anolisa_platform::fs_layout::FsLayout;

use crate::context::{CliContext, InstallMode, ResolvedLayouts};
use crate::packaged::PackagedDataProbe;

pub(crate) fn write_legacy_state(
    state: &anolisa_core::InstalledState,
    path: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().expect("fixture parent"))?;
    std::fs::write(path, toml::to_string_pretty(state).expect("legacy fixture"))
}

/// Explicit fake effect factories for lifecycle tests that do not inspect calls.
pub(crate) fn raw_effects() -> crate::commands::tier1::install::RawEffectFactories<'static> {
    crate::commands::tier1::install::RawEffectFactories {
        service: &|_, _, scope| Box::new(anolisa_core::FakeServiceManager::with_scope(scope)),
        capability: &|_, _| Box::new(anolisa_core::FakeCapabilityManager::new()),
    }
}

/// Records raw effects across freshly-created managers, including compensation.
#[derive(Clone)]
pub(crate) struct RawEffectRecorder {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    capability_contents: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    pub(crate) supported: bool,
    pub(crate) fail_service: Option<anolisa_core::ServiceOp>,
    pub(crate) fail_capability: bool,
}

impl Default for RawEffectRecorder {
    fn default() -> Self {
        Self {
            calls: Default::default(),
            capability_contents: Default::default(),
            supported: true,
            fail_service: None,
            fail_capability: false,
        }
    }
}

impl RawEffectRecorder {
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    pub(crate) fn capability_contents(&self) -> Vec<Vec<u8>> {
        self.capability_contents
            .lock()
            .expect("contents lock")
            .clone()
    }

    fn record(&self, event: String) {
        self.calls.lock().expect("calls lock").push(event);
    }

    pub(crate) fn with<T>(
        &self,
        run: impl FnOnce(crate::commands::tier1::install::RawEffectFactories<'_>) -> T,
    ) -> T {
        let service = |mode: &str, _: &anolisa_env::EnvFacts, scope| {
            self.record(format!("service factory {mode} {scope:?}"));
            Box::new(RecordingService {
                recorder: self.clone(),
                scope,
                inner: anolisa_core::FakeServiceManager::with_scope(scope),
            }) as Box<dyn anolisa_core::ServiceManager>
        };
        let capability = |mode: &str, _: &anolisa_env::EnvFacts| {
            self.record(format!("capability factory {mode}"));
            Box::new(RecordingCapability(self.clone())) as Box<dyn anolisa_core::CapabilityManager>
        };
        run(crate::commands::tier1::install::RawEffectFactories {
            service: &service,
            capability: &capability,
        })
    }
}

struct RecordingService {
    recorder: RawEffectRecorder,
    scope: anolisa_core::ServiceScope,
    inner: anolisa_core::FakeServiceManager,
}

impl RecordingService {
    fn call(
        &self,
        op: anolisa_core::ServiceOp,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        use anolisa_core::{ServiceManager, ServiceOp};
        self.recorder
            .record(format!("{op:?} {:?} {unit}", self.scope));
        if self.recorder.fail_service == Some(op) {
            self.inner.fail(op, unit);
        }
        match op {
            ServiceOp::Probe => self.inner.probe_service(unit),
            ServiceOp::Start => self.inner.start_service(unit),
            ServiceOp::Stop => self.inner.stop_service(unit),
            ServiceOp::Restart => self.inner.restart_service(unit),
            ServiceOp::Enable => self.inner.enable_service(unit),
            ServiceOp::Disable => self.inner.disable_service(unit),
            ServiceOp::DaemonReload => self.inner.daemon_reload(),
        }
    }
}

impl anolisa_core::ServiceManager for RecordingService {
    fn manager(&self) -> &str {
        "fake"
    }
    fn supported(&self) -> bool {
        self.recorder.supported
    }
    fn unsupported_reason(&self) -> Option<&str> {
        Some("fixture unsupported")
    }
    fn handles_scope(&self, scope: anolisa_core::ServiceScope) -> bool {
        self.scope == scope
    }
    fn daemon_reload(&self) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::DaemonReload, "")
    }
    fn probe_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Probe, unit)
    }
    fn start_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Start, unit)
    }
    fn stop_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Stop, unit)
    }
    fn restart_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Restart, unit)
    }
    fn enable_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Enable, unit)
    }
    fn disable_service(
        &self,
        unit: &str,
    ) -> Result<anolisa_core::ServiceOutcome, anolisa_core::ServiceError> {
        self.call(anolisa_core::ServiceOp::Disable, unit)
    }
}

struct RecordingCapability(RawEffectRecorder);

impl anolisa_core::CapabilityManager for RecordingCapability {
    fn manager(&self) -> &str {
        "fake"
    }
    fn supported(&self) -> bool {
        self.0.supported
    }
    fn unsupported_reason(&self) -> Option<&str> {
        Some("fixture unsupported")
    }
    fn apply(
        &self,
        path: &Path,
        caps: &[String],
    ) -> Result<anolisa_core::CapabilityOutcome, anolisa_core::CapabilityError> {
        assert!(
            path.is_file(),
            "capability applied only after placing/restoring a file"
        );
        self.0
            .capability_contents
            .lock()
            .expect("contents lock")
            .push(std::fs::read(path).expect("capability target"));
        self.0
            .record(format!("apply {} {}", path.display(), caps.join(",")));
        let inner = anolisa_core::FakeCapabilityManager::new();
        if self.0.fail_capability {
            inner.fail(path);
        }
        inner.apply(path, caps)
    }
}

/// Output and execution flags for an isolated test context.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TestContextOptions {
    pub(crate) json: bool,
    pub(crate) dry_run: bool,
    pub(crate) verbose: bool,
    pub(crate) quiet: bool,
    pub(crate) no_color: bool,
}

impl Default for TestContextOptions {
    fn default() -> Self {
        Self {
            json: false,
            dry_run: false,
            verbose: false,
            quiet: true,
            no_color: true,
        }
    }
}

/// Owns an isolated user layout, system layout, repository root, and fake bin.
pub(crate) struct TestSandbox {
    tmp: tempfile::TempDir,
    user_layout: FsLayout,
    system_layout: FsLayout,
    repo_root: PathBuf,
    fake_bin: PathBuf,
}

impl TestSandbox {
    /// Create a sandbox whose writable paths are all below one temporary root.
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("test sandbox");
        let root = tmp.path();
        let user_layout = isolated_user_layout(root);
        let system_layout = FsLayout::system(Some(root.join("system")));
        assert_layout_contained(&user_layout, root);
        assert_layout_contained(&system_layout, root);
        Self {
            repo_root: root.join("repo"),
            fake_bin: root.join("fake-bin"),
            tmp,
            user_layout,
            system_layout,
        }
    }

    /// Temporary root retained for the sandbox lifetime.
    pub(crate) fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// Local repository root available to tests.
    pub(crate) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Directory where tests may install fake executables.
    pub(crate) fn fake_bin(&self) -> &Path {
        &self.fake_bin
    }

    /// Build a context with default quiet, colorless test output.
    pub(crate) fn context(&self, install_mode: InstallMode) -> CliContext {
        self.context_with(install_mode, TestContextOptions::default())
    }

    /// Build a context with explicit output and execution flags.
    pub(crate) fn context_with(
        &self,
        install_mode: InstallMode,
        options: TestContextOptions,
    ) -> CliContext {
        let prefix = Some(self.system_layout.prefix.clone());
        let writable = match install_mode {
            InstallMode::System => self.system_layout.clone(),
            InstallMode::User => self.user_layout.clone(),
        };
        CliContext::from_resolved(
            install_mode,
            prefix,
            options.json,
            options.dry_run,
            options.verbose,
            options.quiet,
            options.no_color,
            ResolvedLayouts::new(writable, self.system_layout.clone()),
            PackagedDataProbe::from_inputs(None, None),
        )
    }
}

/// Build an isolated context around a temporary root owned by the caller.
pub(crate) fn context_for_root(
    root: &Path,
    install_mode: InstallMode,
    cli_prefix: Option<PathBuf>,
    options: TestContextOptions,
) -> CliContext {
    let system_root = cli_prefix
        .as_ref()
        .filter(|prefix| prefix.is_absolute() && prefix.starts_with(root))
        .cloned()
        .unwrap_or_else(|| root.join("system"));
    let system_layout = FsLayout::system(Some(system_root));
    let writable = match install_mode {
        InstallMode::System => system_layout.clone(),
        InstallMode::User => isolated_user_layout(root),
    };
    assert_layout_contained(&writable, root);
    assert_layout_contained(&system_layout, root);
    CliContext::from_resolved(
        install_mode,
        cli_prefix,
        options.json,
        options.dry_run,
        options.verbose,
        options.quiet,
        options.no_color,
        ResolvedLayouts::new(writable, system_layout),
        PackagedDataProbe::from_inputs(None, None),
    )
}

fn isolated_user_layout(root: &Path) -> FsLayout {
    FsLayout::user_with_overrides(
        root.join("home"),
        Some(root.join("xdg-data")),
        Some(root.join("xdg-config")),
        Some(root.join("xdg-state")),
        Some(root.join("xdg-cache")),
        Some(root.join("xdg-runtime")),
    )
}

fn assert_layout_contained(layout: &FsLayout, root: &Path) {
    for path in [
        &layout.bin_dir,
        &layout.lib_dir,
        &layout.libexec_dir,
        &layout.datadir,
        &layout.etc_dir,
        &layout.state_dir,
        &layout.cache_dir,
        &layout.log_dir,
        &layout.backup_dir,
        &layout.runtime_dir,
        &layout.systemd_unit_dir,
        &layout.systemd_user_unit_dir,
    ] {
        assert!(
            path.starts_with(root),
            "test layout path {} escapes sandbox {}",
            path.display(),
            root.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_keep_writable_and_visible_system_layouts_contained() {
        let sandbox = TestSandbox::new();
        assert!(sandbox.repo_root().starts_with(sandbox.root()));
        assert!(sandbox.fake_bin().starts_with(sandbox.root()));

        for mode in [InstallMode::User, InstallMode::System] {
            let ctx = sandbox.context(mode);
            assert_layout_contained(ctx.layout(), sandbox.root());
            assert_layout_contained(ctx.visible_system_layout(), sandbox.root());
        }
    }

    #[test]
    fn user_prefix_only_selects_the_visible_system_layout() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::User);

        assert!(
            ctx.layout()
                .etc_dir
                .starts_with(sandbox.root().join("xdg-config"))
        );
        assert!(
            ctx.visible_system_layout()
                .etc_dir
                .starts_with(sandbox.root().join("system"))
        );
        assert_ne!(ctx.layout().prefix, ctx.visible_system_layout().prefix);
    }
}
