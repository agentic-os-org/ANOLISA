//! Runtime-dependency preflight and auto-provisioning for the `install`
//! command.

use anolisa_core::facts::JournalInventory;
use anolisa_core::{
    ComponentManifest, DependencyResolver, DependencyStatus, ProvisionPlan, ProvisionStrategy,
    ResolverEnv,
};
use anolisa_platform::command::CommandRunner;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::package_manager::{PackageManager, PkgError, detect_package_manager};

use crate::commands::tier1::rpm_install;
use crate::context::{CliContext, InstallMode};
use crate::response::CliError;

/// Project detected host facts onto the slice the dependency resolver needs.
pub(crate) fn resolver_env_from_facts(facts: &anolisa_env::EnvFacts) -> ResolverEnv {
    ResolverEnv {
        kernel: facts.kernel.clone(),
        // `os_id` (raw `/etc/os-release` ID) maps to the coarse rpm/deb family;
        // the legacy `EnvFacts::pkg_base` is Anolis-specific and unsuitable here.
        pkg_base: facts
            .os_id
            .as_deref()
            .and_then(anolisa_env::pkg_base_from_id),
        btf: facts.btf,
        cap_bpf: facts.cap_bpf,
    }
}

/// Runtime-dependency preflight shared by the fresh-install (`execute_raw`) and
/// update (`execute_raw_update`) paths. Probes every declared dependency
/// through the system resolver and returns the satisfied plan's (soft)
/// warnings, or an error listing every miss so the caller can refuse **before
/// touching the host**. Empty `runtime_deps` is a no-op. The RPM backend never
/// calls this — dnf owns its `Requires`, so a dependency is never resolved
/// twice. Pure probe: never mutates.
pub(crate) fn run_runtime_preflight(
    manifest: &ComponentManifest,
    env: &anolisa_env::EnvFacts,
    command: &str,
) -> Result<Vec<String>, CliError> {
    let resolver = DependencyResolver::system();
    run_runtime_preflight_with(manifest, env, command, &resolver)
}

fn run_runtime_preflight_with<R: CommandRunner, P: Fn() -> std::io::Result<String>>(
    manifest: &ComponentManifest,
    env: &anolisa_env::EnvFacts,
    command: &str,
    resolver: &DependencyResolver<R, P>,
) -> Result<Vec<String>, CliError> {
    if manifest.runtime_deps.is_empty() {
        return Ok(Vec::new());
    }
    let plan = resolver
        .resolve(&manifest.runtime_deps, &resolver_env_from_facts(env))
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("invalid runtime dependency declaration: {err}"),
        })?;
    if !plan.is_satisfied() {
        let heading = if plan
            .resolutions
            .iter()
            .any(|resolution| matches!(resolution.status, DependencyStatus::ProbeFailed { .. }))
        {
            "runtime dependency probes failed"
        } else {
            "missing runtime dependencies"
        };
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "{heading}; no files were changed:\n  {}",
                plan.unsatisfied_lines().join("\n  ")
            ),
        });
    }
    Ok(plan.warnings)
}

/// Provision-aware dependency handling that replaces the old fail-fast
/// `run_runtime_preflight` in the `execute_raw` path.
///
/// Behavior depends on `ctx.install_mode`:
/// - **System**: auto-install missing system packages via the host package
///   manager, then re-verify only the provisioned deps. Manual-only deps
///   (e.g. `language-runtime` without a `packages` mapping) remain
///   non-blocking warnings. Unresolvable platform capabilities fail fast.
///   Packages reserved by a live pending fresh RPM install journal are
///   refused before the package manager runs.
/// - **User**: report missing deps with remediation commands and return an
///   error (the caller should exit without modifying the host).
///
/// `journals` is the inventory validated under the install lock; it guards
/// the auto-install against packages a pending RPM install still reserves
/// for another component.
///
/// Returns the list of package names that were auto-installed (empty in user
/// mode or when all deps were already satisfied).
pub(crate) fn run_provision(
    manifest: &ComponentManifest,
    env: &anolisa_env::EnvFacts,
    ctx: &CliContext,
    command: &str,
    warnings: &mut Vec<String>,
    journals: &JournalInventory,
    layout: &FsLayout,
) -> Result<Vec<String>, CliError> {
    let resolver = DependencyResolver::system();
    run_provision_with(
        manifest,
        env,
        ctx,
        command,
        warnings,
        journals,
        layout,
        &resolver,
        detect_package_manager,
    )
}

// The business inputs stay explicit; only the two host collaborators are
// injected, so introducing a second request object would obscure the existing
// production signature without reducing call-site complexity.
#[allow(clippy::too_many_arguments)]
fn run_provision_with<R, P, F>(
    manifest: &ComponentManifest,
    env: &anolisa_env::EnvFacts,
    ctx: &CliContext,
    command: &str,
    warnings: &mut Vec<String>,
    journals: &JournalInventory,
    layout: &FsLayout,
    resolver: &DependencyResolver<R, P>,
    detect_manager: F,
) -> Result<Vec<String>, CliError>
where
    R: CommandRunner,
    P: Fn() -> std::io::Result<String>,
    F: FnOnce(Option<&str>) -> Result<Box<dyn PackageManager>, PkgError>,
{
    if manifest.runtime_deps.is_empty() {
        return Ok(Vec::new());
    }

    let resolver_env = resolver_env_from_facts(env);
    let plan = resolver
        .resolve(&manifest.runtime_deps, &resolver_env)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("invalid runtime dependency declaration: {err}"),
        })?;
    warnings.extend(plan.warnings.clone());

    // Classify the resolver results into a provision plan.
    let provision = ProvisionPlan::from_resolution(&plan, &manifest.runtime_deps, &resolver_env);

    // Unresolvable deps (platform capabilities) are always fatal.
    if provision.has_blockers() {
        let lines: Vec<String> = provision
            .unresolvable
            .iter()
            .map(|u| format!("  {} [unresolvable]: {}", u.name, u.reason))
            .collect();
        let heading = if plan
            .resolutions
            .iter()
            .any(|resolution| matches!(resolution.status, DependencyStatus::ProbeFailed { .. }))
        {
            "runtime dependency probes failed"
        } else {
            "unsatisfiable platform requirements"
        };
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!("{heading}; no files were changed:\n{}", lines.join("\n")),
        });
    }

    // If everything is satisfied, nothing to do.
    if provision.is_satisfied() {
        return Ok(Vec::new());
    }

    // Select strategy based on install mode.
    let strategy = select_provision_strategy(ctx);

    match strategy {
        ProvisionStrategy::ReportAndExit => {
            // User mode: report missing deps and exit.
            let mut lines = Vec::new();
            for pkg in &provision.installable {
                lines.push(format!("  {} (not installed)", pkg.name));
            }
            for dep in &provision.manual {
                lines.push(format!("  {} (manual): {}", dep.name, dep.hint));
            }

            let remediation_cmds: Vec<&str> = provision
                .installable
                .iter()
                .map(|p| p.remediation.as_str())
                .collect();

            let mut reason = format!(
                "missing system dependencies in user mode; no files were changed:\n{}",
                lines.join("\n")
            );
            if !remediation_cmds.is_empty() {
                reason.push_str(&format!(
                    "\n\nInstall them with:\n  {}\n\nThen retry:\n  anolisa install {}",
                    remediation_cmds.join("\n  "),
                    manifest.component.name
                ));
            }

            Err(CliError::Runtime {
                command: command.to_string(),
                reason,
            })
        }
        ProvisionStrategy::Auto => {
            // System mode: auto-install missing packages.
            if !provision.has_installable() {
                // Only manual deps remain; warn but continue.
                for dep in &provision.manual {
                    warnings.push(format!(
                        "dependency '{}' requires manual installation: {}",
                        dep.name, dep.hint
                    ));
                }
                return Ok(Vec::new());
            }

            let pkg_names = provision.installable_package_names();
            let pkg_base = resolver_env.pkg_base.as_deref();

            // Detect the host package manager.
            let mgr = detect_manager(pkg_base).map_err(|err| CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "cannot auto-install dependencies: {err}; install manually:\n  {}",
                    provision
                        .installable
                        .iter()
                        .map(|p| p.remediation.as_str())
                        .collect::<Vec<_>>()
                        .join("\n  ")
                ),
            })?;

            // Execute the install, refusing packages a pending RPM install
            // journal still reserves for another component.
            install_unreserved_packages(&pkg_names, journals, layout, &*mgr, command)?;

            // Re-verify only the provisioned deps (manual deps stay as warnings).
            let recheck = resolver
                .resolve(&manifest.runtime_deps, &resolver_env)
                .map_err(|err| CliError::Runtime {
                    command: command.to_string(),
                    reason: format!("dependency re-verification failed: {err}"),
                })?;
            let provisioned_dep_names: std::collections::HashSet<&str> = provision
                .installable
                .iter()
                .map(|p| p.name.as_str())
                .collect();
            let still_failed: Vec<String> = recheck
                .resolutions
                .iter()
                .filter(|r| !matches!(r.status, DependencyStatus::Resolved))
                .filter(|r| {
                    // Only fail on deps we actually tried to provision.
                    provisioned_dep_names.contains(r.name.as_str())
                })
                .map(|r| match &r.status {
                    DependencyStatus::ProbeFailed { error } => format!(
                        "{} [{}]: dependency probe failed: {error}",
                        r.name,
                        r.kind.as_str()
                    ),
                    _ => format!("{} [{}]", r.name, r.kind.as_str()),
                })
                .collect();
            if !still_failed.is_empty() {
                let installed_names: Vec<String> =
                    pkg_names.iter().map(|s| s.to_string()).collect();
                let note = retained_packages_note(&installed_names);
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "dependencies still unsatisfied after install:\n  {}{note}",
                        still_failed.join("\n  ")
                    ),
                });
            }

            // Warn about manual deps.
            for dep in &provision.manual {
                warnings.push(format!(
                    "dependency '{}' requires manual installation: {}",
                    dep.name, dep.hint
                ));
            }

            let installed: Vec<String> = pkg_names.iter().map(|s| s.to_string()).collect();
            Ok(installed)
        }
    }
}

/// Install missing system packages, refusing the whole batch when any
/// package is reserved by a live pending fresh RPM install journal.
///
/// Installing a reserved package would record it under the new component's
/// `provisioned_packages` while the pending operation still owns it, so a
/// later repair or uninstall of that component could remove a package the
/// new component depends on. The caller must hold the install lock that
/// protects `journals`.
fn install_unreserved_packages(
    pkg_names: &[&str],
    journals: &JournalInventory,
    layout: &FsLayout,
    mgr: &dyn PackageManager,
    command: &str,
) -> Result<(), CliError> {
    if let Some(claim) =
        rpm_install::find_pending_rpm_claim_for_packages(journals, layout, pkg_names, command)?
    {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "system package '{}' is reserved by component '{}', which has a pending RPM install journal at {}; run `anolisa repair {}` before retrying",
                claim.package,
                claim.component,
                claim.journal_path.display(),
                claim.component
            ),
        });
    }
    mgr.install(pkg_names).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to install system dependencies: {err}"),
    })
}

/// Build the note suffix appended to error messages when system packages were
/// provisioned but the install did not complete. Returns an empty string when
/// no packages were installed.
pub(crate) fn retained_packages_note(provisioned: &[String]) -> String {
    if provisioned.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nnote: system packages were installed and retained: {}",
            provisioned.join(", ")
        )
    }
}

/// Select provision strategy based on install mode.
pub(crate) fn select_provision_strategy(ctx: &CliContext) -> ProvisionStrategy {
    if ctx.install_mode == InstallMode::System {
        ProvisionStrategy::Auto
    } else {
        ProvisionStrategy::ReportAndExit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tier1::rpm_install;
    use crate::test_support::TestSandbox;
    use anolisa_core::domain::NativePm;
    use anolisa_core::facts::JournalEvidence;
    use anolisa_core::manifest::{DependencyKind, PackageNames, RuntimeDependency};
    use anolisa_core::state::OperationRecord;
    use anolisa_core::transaction::{
        DelegatedRecordAction, DelegatedRecoveryContext, Transaction, TransactionOutcomeStatus,
        TransactionStep,
    };
    use anolisa_core::{DependencyResolution, ResolutionPlan};
    use anolisa_platform::command::CommandOutput;
    use anolisa_platform::package_manager::PkgError;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::rc::Rc;

    type CommandCall = (String, Vec<String>);

    #[derive(Clone)]
    struct ScriptedRunner {
        calls: Rc<RefCell<Vec<CommandCall>>>,
        codes: Rc<RefCell<VecDeque<Option<i32>>>>,
    }

    impl ScriptedRunner {
        fn with_codes(codes: impl IntoIterator<Item = Option<i32>>) -> Self {
            Self {
                calls: Rc::new(RefCell::new(Vec::new())),
                codes: Rc::new(RefCell::new(codes.into_iter().collect())),
            }
        }

        fn calls(&self) -> Vec<CommandCall> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|arg| (*arg).to_string()).collect(),
            ));
            let code = self
                .codes
                .borrow_mut()
                .pop_front()
                .expect("unexpected dependency probe");
            Ok(CommandOutput {
                code,
                stdout: if program == "rpm" && args.len() == 2 && code == Some(1) {
                    format!("package {} is not installed\n", args[1])
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        }
    }

    struct EvidenceRunner {
        outputs: RefCell<VecDeque<std::io::Result<CommandOutput>>>,
        calls: RefCell<Vec<CommandCall>>,
    }

    impl CommandRunner for &EvidenceRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "rpm");
            assert_eq!(args[0], "-q");
            assert_eq!(args.len(), 2);
            self.calls
                .borrow_mut()
                .push((program.into(), args.iter().map(|s| s.to_string()).collect()));
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("unexpected RPM query")
        }
    }

    fn missing_rpm(package: &str) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            code: Some(1),
            stdout: format!("package {package} is not installed\n"),
            stderr: String::new(),
        })
    }

    #[test]
    fn native_rpm_probe_failure_blocks_mixed_provisioning_before_manager_creation() {
        for mode in [InstallMode::System, InstallMode::User] {
            for failure in ["rpmdb", "permission", "missing-command", "signal", "stdout"] {
                let sandbox = TestSandbox::new();
                let ctx = sandbox.context(mode);
                let layout = ctx.layout();
                let inventory = inventory_for(layout, &[]);
                let output = match failure {
                    "stdout" => Ok(CommandOutput {
                        code: Some(1),
                        stdout: "package libbar is not installed\nextra\n".into(),
                        stderr: String::new(),
                    }),
                    "permission" => Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "denied",
                    )),
                    "missing-command" => {
                        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
                    }
                    _ => Ok(CommandOutput {
                        code: if failure == "signal" { None } else { Some(1) },
                        stdout: "package libbar is not installed\n".into(),
                        stderr: "rpmdb diagnostic\n".into(),
                    }),
                };
                let runner = EvidenceRunner {
                    outputs: RefCell::new(VecDeque::from([
                        missing_rpm("libfoo"),
                        output,
                        Ok(CommandOutput {
                            code: Some(0),
                            stdout: String::new(),
                            stderr: String::new(),
                        }),
                    ])),
                    calls: RefCell::new(Vec::new()),
                };
                let resolver = DependencyResolver::with_probes(&runner, || {
                    panic!("unexpected filesystem probe")
                });
                let manifest = manifest_with_deps(vec![
                    system_dep("foo", "libfoo"),
                    system_dep("bar", "libbar"),
                    system_dep("baz", "libbaz"),
                ]);
                let manager = FakePackageManager::default();
                let detections = Cell::new(0);
                let error = run_provision_with(
                    &manifest,
                    &rpm_host_env(),
                    &ctx,
                    "install component-a",
                    &mut Vec::new(),
                    &inventory,
                    layout,
                    &resolver,
                    |_| {
                        detections.set(detections.get() + 1);
                        Ok(Box::new(manager.clone()))
                    },
                )
                .expect_err("query failure blocks all automatic installs");
                assert_eq!(error.code(), "EXECUTION_FAILED");
                assert_eq!(error.exit_code(), 1);
                assert!(error.reason().contains("runtime dependency probes failed"));
                assert!(error.reason().contains("bar"));
                if failure == "stdout" {
                    assert!(error.reason().contains("code Some(1)"));
                    assert!(
                        error
                            .reason()
                            .contains("package libbar is not installed\nextra\n")
                    );
                }
                assert!(!error.reason().contains("sudo dnf install libbar"));
                assert_eq!(detections.get(), 0);
                assert!(manager.install_calls().is_empty());
                assert_eq!(
                    runner
                        .calls
                        .borrow()
                        .iter()
                        .map(|(_, args)| args[1].as_str())
                        .collect::<Vec<_>>(),
                    ["libfoo", "libbar", "libbaz"]
                );
                assert!(runner.outputs.borrow().is_empty());
                let preflight = run_runtime_preflight_with(
                    &manifest,
                    &rpm_host_env(),
                    "install component-a",
                    &DependencyResolver::with_probes(
                        ScriptedRunner::with_codes([Some(2), Some(0), Some(0)]),
                        || panic!("no btrfs"),
                    ),
                )
                .expect_err("read-only preflight also blocks");
                assert!(preflight.reason().contains("dependency probe failed"));
            }
        }
    }

    #[test]
    fn native_rpm_recheck_failure_retains_packages_and_diagnostic() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let runner = EvidenceRunner {
            outputs: RefCell::new(VecDeque::from([
                missing_rpm("libfoo"),
                Ok(CommandOutput {
                    code: Some(1),
                    stdout: "package libfoo is not installed\n".into(),
                    stderr: "rpmdb recheck failed\n".into(),
                }),
            ])),
            calls: RefCell::new(Vec::new()),
        };
        let resolver = DependencyResolver::with_probes(&runner, || panic!("no btrfs"));
        let manager = FakePackageManager::default();
        let detections = Cell::new(0);
        let error = run_provision_with(
            &manifest,
            &rpm_host_env(),
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| {
                assert_eq!(
                    runner.calls.borrow().len(),
                    1,
                    "detect manager after initial query"
                );
                detections.set(detections.get() + 1);
                Ok(Box::new(manager.clone()))
            },
        )
        .expect_err("post-install observation failed");
        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert_eq!(error.exit_code(), 1);
        assert!(
            error
                .reason()
                .contains("dependencies still unsatisfied after install")
        );
        assert!(error.reason().contains("rpmdb recheck failed\n"));
        assert!(
            error
                .reason()
                .contains(&retained_packages_note(&["libfoo".into()]))
        );
        assert_eq!(detections.get(), 1);
        assert_eq!(manager.install_calls(), [vec!["libfoo".to_string()]]);
        assert_eq!(runner.calls.borrow().len(), 2);
        assert!(runner.outputs.borrow().is_empty());
    }

    /// Records install calls instead of touching the host.
    #[derive(Clone, Default)]
    struct FakePackageManager {
        installs: Rc<RefCell<Vec<Vec<String>>>>,
        install_error: Option<String>,
    }

    impl FakePackageManager {
        fn failing(reason: &str) -> Self {
            Self {
                install_error: Some(reason.to_string()),
                ..Default::default()
            }
        }

        fn install_calls(&self) -> Vec<Vec<String>> {
            self.installs.borrow().clone()
        }
    }

    impl PackageManager for FakePackageManager {
        fn install(&self, packages: &[&str]) -> Result<(), PkgError> {
            self.installs
                .borrow_mut()
                .push(packages.iter().map(|package| package.to_string()).collect());
            match &self.install_error {
                Some(reason) => Err(PkgError::CommandFailed(reason.clone())),
                None => Ok(()),
            }
        }

        fn remove(&self, _packages: &[&str]) -> Result<(), PkgError> {
            Ok(())
        }

        fn is_installed(&self, _package: &str) -> bool {
            false
        }
    }

    fn layout_under(tmp: &tempfile::TempDir) -> FsLayout {
        FsLayout::system(Some(tmp.path().to_path_buf()))
    }

    fn inventory_for(layout: &FsLayout, operations: &[OperationRecord]) -> JournalInventory {
        let journal_dir = rpm_install::journal_dir(layout);
        JournalInventory::load(JournalEvidence::new(&journal_dir, operations))
            .expect("journal inventory must load")
    }

    fn rpm_env() -> ResolverEnv {
        ResolverEnv {
            pkg_base: Some("rpm".to_string()),
            ..Default::default()
        }
    }

    fn rpm_host_env() -> anolisa_env::EnvFacts {
        anolisa_env::EnvFacts {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: Some("glibc".to_string()),
            kernel: Some("5.10.0".to_string()),
            pkg_base: Some("alinux4".to_string()),
            os_id: Some("alinux".to_string()),
            os_id_like: None,
            os_version: Some("4".to_string()),
            btf: Some(true),
            cap_bpf: Some(true),
            container: None,
            user: "root".to_string(),
            uid: 0,
            home: PathBuf::from("/root"),
        }
    }

    fn manifest_with_deps(deps: Vec<RuntimeDependency>) -> ComponentManifest {
        let mut manifest = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "component-a"
            version = "1.0.0"
            "#,
        )
        .expect("minimal manifest");
        manifest.runtime_deps = deps;
        manifest
    }

    fn language_dep(name: &str) -> RuntimeDependency {
        RuntimeDependency {
            name: name.to_string(),
            kind: DependencyKind::LanguageRuntime,
            version: None,
            probe: Some(format!("{name} --version")),
            source: Some("https://example.invalid/runtime".to_string()),
            packages: PackageNames::default(),
            check: None,
            min_kernel: None,
        }
    }

    fn platform_dep(name: &str, check: &str) -> RuntimeDependency {
        RuntimeDependency {
            name: name.to_string(),
            kind: DependencyKind::PlatformCapability,
            version: None,
            probe: None,
            source: None,
            packages: PackageNames::default(),
            check: Some(check.to_string()),
            min_kernel: None,
        }
    }

    fn system_dep(name: &str, package_name: &str) -> RuntimeDependency {
        RuntimeDependency {
            name: name.to_string(),
            kind: DependencyKind::SystemPackage,
            version: None,
            probe: None,
            source: None,
            packages: PackageNames {
                rpm: Some(package_name.to_string()),
                deb: None,
            },
            check: None,
            min_kernel: None,
        }
    }

    fn unresolved(name: &str, package_name: &str) -> DependencyResolution {
        DependencyResolution {
            name: name.to_string(),
            kind: DependencyKind::SystemPackage,
            status: DependencyStatus::Unresolved {
                remediation: format!("sudo dnf install {package_name}"),
            },
            detail: None,
        }
    }

    fn resolved(name: &str) -> DependencyResolution {
        DependencyResolution {
            name: name.to_string(),
            kind: DependencyKind::SystemPackage,
            status: DependencyStatus::Resolved,
            detail: None,
        }
    }

    #[test]
    fn injected_runtime_preflight_uses_scripted_resolver() {
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let runner = ScriptedRunner::with_codes([Some(0)]);
        let resolver = DependencyResolver::with_runner(runner.clone());

        assert_eq!(
            run_runtime_preflight_with(&manifest, &env, "update", &resolver)
                .expect("scripted dependency is present"),
            Vec::<String>::new()
        );
        assert_eq!(
            runner.calls(),
            vec![(
                "rpm".to_string(),
                vec!["-q".to_string(), "libfoo".to_string()]
            )]
        );
    }

    #[test]
    fn injected_btrfs_reader_controls_preflight_and_provisioning() {
        for supported in [true, false] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let layout = ctx.layout();
            let inventory = inventory_for(layout, &[]);
            let manifest = manifest_with_deps(vec![platform_dep("btrfs", "btrfs")]);
            let env = rpm_host_env();
            let reads = Cell::new(0);
            let runner = ScriptedRunner::with_codes([]);
            let resolver = DependencyResolver::with_probes(runner.clone(), || {
                reads.set(reads.get() + 1);
                Ok(if supported {
                    "nodev\tbtrfs\n"
                } else {
                    "ext4\n"
                }
                .to_string())
            });

            let preflight = run_runtime_preflight_with(&manifest, &env, "update", &resolver);
            assert_eq!(reads.get(), 1);
            let mut warnings = Vec::new();
            let provision = run_provision_with(
                &manifest,
                &env,
                &ctx,
                "install component-a",
                &mut warnings,
                &inventory,
                layout,
                &resolver,
                |_| panic!("platform capabilities must not invoke a package manager"),
            );
            if supported {
                assert!(preflight.unwrap().is_empty());
                assert!(provision.unwrap().is_empty());
            } else {
                let preflight = preflight.unwrap_err().reason();
                assert!(preflight.contains("missing runtime dependencies; no files were changed"));
                let provision = provision.unwrap_err().reason();
                assert!(
                    provision
                        .contains("unsatisfiable platform requirements; no files were changed")
                );
                for reason in [preflight, provision] {
                    assert!(
                        reason.contains(
                            "btrfs is not supported by the running kernel (absent from /proc/filesystems)"
                        )
                    );
                }
            }
            assert_eq!(reads.get(), 2);
            assert!(runner.calls().is_empty());
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn injected_btrfs_read_failure_blocks_before_manager_detection() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![platform_dep("btrfs", "btrfs")]);
        let reads = Cell::new(0);
        let resolver = DependencyResolver::with_probes(ScriptedRunner::with_codes([]), || {
            reads.set(reads.get() + 1);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "scripted denial",
            ))
        });
        let err = run_provision_with(
            &manifest,
            &rpm_host_env(),
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| panic!("an unreadable capability probe must block before manager detection"),
        )
        .unwrap_err();
        assert!(
            err.reason()
                .contains("could not read /proc/filesystems to verify btrfs support")
        );
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn provisioning_reuses_btrfs_reader_after_install() {
        for recheck_code in [Some(0), Some(1)] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let layout = ctx.layout();
            let inventory = inventory_for(layout, &[]);
            let manifest = manifest_with_deps(vec![
                system_dep("foo", "libfoo"),
                platform_dep("btrfs", "btrfs"),
            ]);
            let runner = ScriptedRunner::with_codes([Some(1), recheck_code]);
            let manager = FakePackageManager::default();
            let reads = Cell::new(0);
            let detections = Cell::new(0);
            let resolver = DependencyResolver::with_probes(runner.clone(), || {
                reads.set(reads.get() + 1);
                assert_eq!(runner.calls().len(), reads.get());
                if reads.get() == 1 {
                    assert_eq!(detections.get(), 0);
                    assert!(manager.install_calls().is_empty());
                } else {
                    assert_eq!(detections.get(), 1);
                    assert_eq!(manager.install_calls(), vec![vec!["libfoo".to_string()]]);
                }
                Ok("btrfs\n".to_string())
            });
            let result = run_provision_with(
                &manifest,
                &rpm_host_env(),
                &ctx,
                "install component-a",
                &mut Vec::new(),
                &inventory,
                layout,
                &resolver,
                |pkg_base| {
                    assert_eq!(reads.get(), 1);
                    assert_eq!(pkg_base, Some("rpm"));
                    detections.set(detections.get() + 1);
                    Ok(Box::new(manager.clone()))
                },
            );
            if recheck_code == Some(0) {
                assert_eq!(result.unwrap(), vec!["libfoo".to_string()]);
            } else {
                let reason = result.unwrap_err().reason();
                assert!(reason.contains("dependencies still unsatisfied after install"));
                assert!(reason.contains("system packages were installed and retained: libfoo"));
            }
            assert_eq!(reads.get(), 2);
            assert_eq!(detections.get(), 1);
            assert_eq!(manager.install_calls(), vec![vec!["libfoo".to_string()]]);
            assert_eq!(
                runner.calls(),
                vec![
                    (
                        "rpm".to_string(),
                        vec!["-q".to_string(), "libfoo".to_string()]
                    ),
                    (
                        "rpm".to_string(),
                        vec!["-q".to_string(), "libfoo".to_string()]
                    ),
                ]
            );
        }
    }

    #[test]
    fn no_op_provisioning_never_detects_package_manager() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let env = rpm_host_env();

        let empty = manifest_with_deps(Vec::new());
        let empty_resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([]));
        assert!(
            run_provision_with(
                &empty,
                &env,
                &ctx,
                "install component-a",
                &mut Vec::new(),
                &inventory,
                layout,
                &empty_resolver,
                |_| -> Result<Box<dyn PackageManager>, PkgError> {
                    panic!("manager detection must not run for an empty dependency list")
                },
            )
            .expect("empty dependency list is a no-op")
            .is_empty()
        );

        let satisfied = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let runner = ScriptedRunner::with_codes([Some(0)]);
        let resolver = DependencyResolver::with_runner(runner.clone());
        assert!(
            run_provision_with(
                &satisfied,
                &env,
                &ctx,
                "install component-a",
                &mut Vec::new(),
                &inventory,
                layout,
                &resolver,
                |_| -> Result<Box<dyn PackageManager>, PkgError> {
                    panic!("manager detection must not run for satisfied dependencies")
                },
            )
            .expect("satisfied dependencies are a no-op")
            .is_empty()
        );
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn user_mode_reports_missing_dependency_without_detecting_manager() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::User);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([Some(1)]));

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| -> Result<Box<dyn PackageManager>, PkgError> {
                panic!("user mode must not detect a package manager")
            },
        )
        .expect_err("user mode must report the missing package");

        assert!(
            err.reason()
                .contains("missing system dependencies in user mode")
        );
        assert!(err.reason().contains("sudo dnf install libfoo"));
    }

    #[test]
    fn system_provisioning_detects_once_installs_and_rechecks() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let runner = ScriptedRunner::with_codes([Some(1), Some(0)]);
        let resolver = DependencyResolver::with_runner(runner.clone());
        let manager = FakePackageManager::default();
        let manager_for_factory = manager.clone();
        let detection_calls = Cell::new(0);

        let installed = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |pkg_base| {
                detection_calls.set(detection_calls.get() + 1);
                assert_eq!(pkg_base, Some("rpm"));
                Ok(Box::new(manager_for_factory))
            },
        )
        .expect("provisioning succeeds after the scripted recheck");

        assert_eq!(installed, vec!["libfoo".to_string()]);
        assert_eq!(detection_calls.get(), 1);
        assert_eq!(manager.install_calls(), vec![vec!["libfoo".to_string()]]);
        assert_eq!(
            runner.calls(),
            vec![
                (
                    "rpm".to_string(),
                    vec!["-q".to_string(), "libfoo".to_string()]
                ),
                (
                    "rpm".to_string(),
                    vec!["-q".to_string(), "libfoo".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn manager_detection_failure_keeps_existing_error_projection() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([Some(1)]));

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| Err(PkgError::Unsupported("rpm".to_string())),
        )
        .expect_err("manager detection failure must abort provisioning");

        assert!(err.reason().contains("cannot auto-install dependencies"));
        assert!(err.reason().contains("unsupported package base: rpm"));
        assert!(err.reason().contains("sudo dnf install libfoo"));
    }

    #[test]
    fn manager_install_failure_keeps_existing_error_projection() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([Some(1)]));
        let manager = FakePackageManager::failing("scripted install failure");
        let manager_for_factory = manager.clone();

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| Ok(Box::new(manager_for_factory)),
        )
        .expect_err("manager install failure must abort provisioning");

        assert!(
            err.reason().contains(
                "failed to install system dependencies: package manager command failed: scripted install failure"
            )
        );
        assert_eq!(manager.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn failed_recheck_reports_retained_package() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![system_dep("foo", "libfoo")]);
        let env = rpm_host_env();
        let resolver =
            DependencyResolver::with_runner(ScriptedRunner::with_codes([Some(1), Some(1)]));
        let manager = FakePackageManager::default();
        let manager_for_factory = manager.clone();

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| Ok(Box::new(manager_for_factory)),
        )
        .expect_err("an unsatisfied recheck must report retained packages");

        assert!(
            err.reason()
                .contains("dependencies still unsatisfied after install")
        );
        assert!(err.reason().contains("foo [system-package]"));
        assert!(
            err.reason()
                .contains("system packages were installed and retained: libfoo")
        );
        assert_eq!(manager.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn manual_only_dependency_warns_without_detecting_manager() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![language_dep("node")]);
        let env = rpm_host_env();
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([Some(1)]));
        let mut warnings = vec!["existing warning".to_string()];

        let installed = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut warnings,
            &inventory,
            layout,
            &resolver,
            |_| -> Result<Box<dyn PackageManager>, PkgError> {
                panic!("manual-only dependencies must not detect a package manager")
            },
        )
        .expect("manual-only dependency is non-fatal in system mode");

        assert!(installed.is_empty());
        assert_eq!(warnings[0], "existing warning");
        assert!(warnings[1].contains("dependency 'node' requires manual installation"));
    }

    #[test]
    fn resolver_declaration_failure_precedes_manager_detection() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![platform_dep("future-cap", "unknown-check")]);
        let env = rpm_host_env();
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([]));

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| -> Result<Box<dyn PackageManager>, PkgError> {
                panic!("invalid declarations must fail before manager detection")
            },
        )
        .expect_err("unknown platform check must fail");

        assert!(
            err.reason()
                .contains("invalid runtime dependency declaration")
        );
        assert!(err.reason().contains("unknown-check"));
    }

    #[test]
    fn platform_blocker_precedes_manager_detection() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let layout = ctx.layout();
        let inventory = inventory_for(layout, &[]);
        let manifest = manifest_with_deps(vec![platform_dep("kernel-btf", "btf")]);
        let mut env = rpm_host_env();
        env.btf = Some(false);
        let resolver = DependencyResolver::with_runner(ScriptedRunner::with_codes([]));

        let err = run_provision_with(
            &manifest,
            &env,
            &ctx,
            "install component-a",
            &mut Vec::new(),
            &inventory,
            layout,
            &resolver,
            |_| -> Result<Box<dyn PackageManager>, PkgError> {
                panic!("platform blockers must fail before manager detection")
            },
        )
        .expect_err("missing platform capability must block provisioning");

        assert!(err.reason().contains("unsatisfiable platform requirements"));
        assert!(err.reason().contains("kernel-btf"));
        assert!(err.reason().contains("kernel BTF"));
    }

    /// Leave an in-flight delegated fresh RPM install journal reserving
    /// `package` for `component` (the modern subject-journal shape).
    fn drop_pending_delegated_journal(layout: &FsLayout, component: &str, package: &str) {
        let mut journal = Transaction::begin_with_subject(
            "install",
            Some(component),
            layout.state_dir.join("installed.toml"),
            &rpm_install::journal_dir(layout),
        )
        .expect("begin delegated journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some(package.to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: None,
                },
                [TransactionStep::planned(
                    "native-txn",
                    package,
                    "install",
                    None,
                )],
            )
            .expect("record delegated steps");
        // Dropped in flight: the journal stays pending.
    }

    #[test]
    fn retained_packages_note_empty_when_no_packages() {
        assert_eq!(retained_packages_note(&[]), "");
    }

    #[test]
    fn retained_packages_note_lists_provisioned_packages() {
        let pkgs = vec!["nodejs".to_string(), "jq".to_string()];
        let note = retained_packages_note(&pkgs);
        assert!(note.contains("system packages were installed and retained"));
        assert!(note.contains("nodejs"));
        assert!(note.contains("jq"));
    }

    #[test]
    fn pending_delegated_rpm_journal_blocks_dependency_provisioning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        drop_pending_delegated_journal(&layout, "component-b", "libfoo");
        let inventory = inventory_for(&layout, &[]);

        // A missing runtime dependency of component-a maps to the reserved
        // package.
        let dep = system_dep("foo", "libfoo");
        let resolution = ResolutionPlan {
            resolutions: vec![unresolved("foo", "libfoo")],
            warnings: Vec::new(),
        };
        let plan = ProvisionPlan::from_resolution(&resolution, &[dep], &rpm_env());
        let pkg_names = plan.installable_package_names();
        assert_eq!(pkg_names, vec!["libfoo"]);

        let mgr = FakePackageManager::default();
        let err = install_unreserved_packages(
            &pkg_names,
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect_err("a reserved package must block provisioning");

        let reason = err.reason();
        assert!(reason.contains("component-b"), "{reason}");
        assert!(reason.contains("libfoo"), "{reason}");
        assert!(reason.contains("anolisa repair component-b"), "{reason}");
        assert!(
            mgr.install_calls().is_empty(),
            "package manager must not run"
        );
    }

    #[test]
    fn pending_legacy_rpm_journal_blocks_dependency_provisioning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        let pending = rpm_install::begin_fresh_install(
            &layout,
            "component-b",
            "libfoo",
            "install component-b",
        )
        .expect("begin legacy journal");
        drop(pending);
        let inventory = inventory_for(&layout, &[]);

        let dep = system_dep("foo", "libfoo");
        let resolution = ResolutionPlan {
            resolutions: vec![unresolved("foo", "libfoo")],
            warnings: Vec::new(),
        };
        let plan = ProvisionPlan::from_resolution(&resolution, &[dep], &rpm_env());
        let pkg_names = plan.installable_package_names();

        let mgr = FakePackageManager::default();
        let err = install_unreserved_packages(
            &pkg_names,
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect_err("a legacy reserved package must block provisioning");

        let reason = err.reason();
        assert!(reason.contains("component-b"), "{reason}");
        assert!(reason.contains("libfoo"), "{reason}");
        assert!(reason.contains("anolisa repair component-b"), "{reason}");
        assert!(
            mgr.install_calls().is_empty(),
            "package manager must not run"
        );
    }

    #[test]
    fn terminal_rpm_journal_does_not_block_provisioning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        let mut journal = Transaction::begin_with_subject(
            "install",
            Some("component-b"),
            layout.state_dir.join("installed.toml"),
            &rpm_install::journal_dir(&layout),
        )
        .expect("begin delegated journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("libfoo".to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: None,
                },
                [TransactionStep::planned(
                    "native-txn",
                    "libfoo",
                    "install",
                    None,
                )],
            )
            .expect("record delegated steps");
        journal
            .finish(TransactionOutcomeStatus::Ok)
            .expect("finish journal");
        let inventory = inventory_for(&layout, &[]);

        let mgr = FakePackageManager::default();
        install_unreserved_packages(
            &["libfoo"],
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect("a settled journal must not block provisioning");

        assert_eq!(mgr.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn committed_legacy_rpm_journal_does_not_block_provisioning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        let pending = rpm_install::begin_fresh_install(
            &layout,
            "component-b",
            "libfoo",
            "install component-b",
        )
        .expect("begin legacy journal");
        let operation_id = pending.transaction.operation_id.clone();
        drop(pending);
        let operations = vec![OperationRecord {
            id: operation_id,
            command: "install component-b".to_string(),
            status: "ok".to_string(),
            started_at: "2026-07-27T00:00:00Z".to_string(),
            finished_at: Some("2026-07-27T00:00:01Z".to_string()),
            parent_operation_id: None,
        }];
        let inventory = inventory_for(&layout, &operations);

        let mgr = FakePackageManager::default();
        install_unreserved_packages(
            &["libfoo"],
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect("a committed legacy journal must not block provisioning");

        assert_eq!(mgr.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn provisioning_proceeds_without_pending_claims() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        let inventory = inventory_for(&layout, &[]);

        let dep = system_dep("foo", "libfoo");
        let resolution = ResolutionPlan {
            resolutions: vec![unresolved("foo", "libfoo")],
            warnings: Vec::new(),
        };
        let plan = ProvisionPlan::from_resolution(&resolution, &[dep], &rpm_env());
        let pkg_names = plan.installable_package_names();

        let mgr = FakePackageManager::default();
        install_unreserved_packages(&pkg_names, &inventory, &layout, &mgr, "install component-a")
            .expect("no pending claim means provisioning proceeds");

        assert_eq!(mgr.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn satisfied_dependency_is_not_rejected_by_its_reserved_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        // libfoo is reserved by a pending journal for component-b, but
        // component-a's dependency on it is already satisfied; only the
        // missing dependency's package is installable.
        drop_pending_delegated_journal(&layout, "component-b", "libfoo");
        let inventory = inventory_for(&layout, &[]);

        let deps = vec![system_dep("foo", "libfoo"), system_dep("bar", "libbar")];
        let resolution = ResolutionPlan {
            resolutions: vec![resolved("foo"), unresolved("bar", "libbar")],
            warnings: Vec::new(),
        };
        let plan = ProvisionPlan::from_resolution(&resolution, &deps, &rpm_env());
        assert_eq!(plan.satisfied_count, 1);
        let pkg_names = plan.installable_package_names();
        assert_eq!(pkg_names, vec!["libbar"]);

        let mgr = FakePackageManager::default();
        install_unreserved_packages(&pkg_names, &inventory, &layout, &mgr, "install component-a")
            .expect("a reserved package that is not being installed must not block");

        assert_eq!(mgr.install_calls(), vec![vec!["libbar".to_string()]]);
    }

    #[test]
    fn pending_rpm_journal_for_another_package_does_not_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        drop_pending_delegated_journal(&layout, "component-b", "libother");
        let inventory = inventory_for(&layout, &[]);

        let mgr = FakePackageManager::default();
        install_unreserved_packages(
            &["libfoo"],
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect("a claim on another package must not block");

        assert_eq!(mgr.install_calls(), vec![vec!["libfoo".to_string()]]);
    }

    #[test]
    fn malformed_live_legacy_journal_fails_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_under(&tmp);
        let mut pending = rpm_install::begin_fresh_install(
            &layout,
            "component-b",
            "libfoo",
            "install component-b",
        )
        .expect("begin legacy journal");
        // Reverse the steps so the live journal keeps legacy markers but no
        // longer matches the safe two-step shape.
        pending.transaction.steps.reverse();
        std::fs::write(
            &pending.transaction.journal_path,
            toml::to_string_pretty(&pending.transaction).expect("serialize journal"),
        )
        .expect("rewrite journal");
        drop(pending);
        let inventory = inventory_for(&layout, &[]);

        let mgr = FakePackageManager::default();
        let err = install_unreserved_packages(
            &["libfoo"],
            &inventory,
            &layout,
            &mgr,
            "install component-a",
        )
        .expect_err("an ambiguous live journal must fail closed");

        assert!(
            err.reason().contains("automatic recovery is unsafe"),
            "{}",
            err.reason()
        );
        assert!(
            mgr.install_calls().is_empty(),
            "package manager must not run"
        );
    }
}
