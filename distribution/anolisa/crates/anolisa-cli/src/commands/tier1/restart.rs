//! `anolisa restart <component>` — restart a component's systemd service units.
//!
//! Brings a component's services up (`systemctl restart` starts a stopped
//! unit), routing each through the [`anolisa_core::ServiceManager`] for its
//! scope. The handler:
//!
//!   1. Loads `installed.toml` and locates the component. Absent →
//!      `NOT_INSTALLED`.
//!   2. Collects the component's restartable `.service` units, by backend:
//!      - **raw** installs: the recorded `ServiceRef`s — ANOLISA drove the
//!        activation, so state is the source of truth;
//!      - **RPM** installs (rpm-observed / rpm-managed): live `rpm -ql`
//!        discovery, because the RPM path records no services and the package
//!        owns the unit files. Template (`foo@.service`) units cannot be
//!        expanded here — instances are runtime/user state, not package files —
//!        so they degrade to a per-user guidance note instead of a restart.
//!   3. Empty set splits two ways: a component that ships no service units at
//!      all → `INVALID_ARGUMENT`; one that ships only templates restart cannot
//!      expand → exit 0 with per-user guidance (the package is fine, the
//!      operator just has to pick an instance).
//!   4. Reloads each in-play scope's unit database once (best-effort), so a
//!      freshly-placed unit (a place-only install, or an RPM whose `%post` did
//!      not reload) is loadable before restart.
//!   5. Routes each unit through the manager for its own scope — system units
//!      via `systemctl`, user units via `systemctl --user`. A unit whose scope
//!      has no driver here (a user unit in a system-mode restart, non-Linux,
//!      container) is a per-unit `not_supported` skip, never mis-driven through
//!      another namespace.
//!   6. Calls `restart_service(unit)` per unit. Per-unit failures are collected
//!      as warnings; a unit systemctl refuses does NOT abort the whole op.
//!
//! The execute path holds the exclusive state-root lock from lookup through
//! service dispatch so restart cannot race teardown and the pending-recovery
//! gate stays authoritative. `--dry-run` reads the same state and journals
//! without creating or exclusively locking that root, matching
//! forget/repair previews, so a non-root system preview can render a plan
//! on a root-owned install.

mod application;

use clap::Parser;

use anolisa_core::execution::ExecutionIntent;

use self::application::{
    RestartApplicationOutcome, RestartRequest, RestartSummary, RestartUnitOutcome,
};
use crate::color::Palette;
use crate::context::CliContext;
use crate::response::{CliError, render_json};

const COMMAND: &str = "restart";

#[derive(Parser)]
pub struct RestartArgs {
    /// Component whose services to restart
    pub component: String,
}

pub fn handle(args: RestartArgs, ctx: &CliContext) -> Result<(), CliError> {
    let payload = compute_restart(&args.component, ctx)?;
    render_restart(&payload, ctx)
}

fn compute_restart(input: &str, ctx: &CliContext) -> Result<RestartPayload, CliError> {
    application::run(
        RestartRequest {
            component: input,
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
    )
    .map(project_restart_outcome)
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

#[cfg(test)]
fn compute_restart_with(
    input: &str,
    ctx: &CliContext,
    managers: Option<(
        &dyn anolisa_core::ServiceManager,
        &dyn anolisa_core::ServiceManager,
    )>,
) -> Result<RestartPayload, CliError> {
    application::run_with_managers(
        RestartRequest {
            component: input,
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
        managers,
    )
    .map(project_restart_outcome)
}

fn project_restart_outcome(outcome: RestartApplicationOutcome) -> RestartPayload {
    match outcome {
        RestartApplicationOutcome::Guidance {
            summary,
            intent,
            warnings,
        } => project_restart_payload(
            summary,
            matches!(intent, ExecutionIntent::Plan),
            vec![],
            warnings,
        ),
        RestartApplicationOutcome::Preview {
            summary,
            units,
            warnings,
        } => project_restart_payload(summary, true, units, warnings),
        RestartApplicationOutcome::Applied {
            summary,
            units,
            warnings,
        } => project_restart_payload(summary, false, units, warnings),
    }
}

fn project_restart_payload(
    summary: RestartSummary,
    dry_run: bool,
    units: Vec<RestartUnitOutcome>,
    warnings: Vec<String>,
) -> RestartPayload {
    RestartPayload {
        component: summary.component,
        install_mode: summary.install_mode,
        manager: summary.manager,
        supported: summary.supported,
        dry_run,
        units: units.into_iter().map(project_restart_unit).collect(),
        warnings,
    }
}

fn project_restart_unit(outcome: RestartUnitOutcome) -> RestartResult {
    match outcome {
        RestartUnitOutcome::Planned {
            component,
            unit,
            manager,
        } => RestartResult {
            component,
            message: format!("dry-run: would restart {unit}"),
            unit,
            state: PLANNED_STATE.to_string(),
            changed: false,
            manager,
        },
        RestartUnitOutcome::Dispatched {
            component,
            unit,
            state,
            changed,
            manager,
            message,
        } => RestartResult {
            component,
            unit,
            state: state.as_str().to_string(),
            changed,
            manager,
            message,
        },
        RestartUnitOutcome::Unsupported {
            component,
            unit,
            manager,
            message,
        } => RestartResult {
            component,
            unit,
            state: "not_supported".to_string(),
            changed: false,
            manager,
            message,
        },
        RestartUnitOutcome::Failed {
            component,
            unit,
            manager,
            message,
        } => RestartResult {
            component,
            unit,
            state: "unknown".to_string(),
            changed: false,
            manager,
            message,
        },
    }
}

fn render_restart(payload: &RestartPayload, ctx: &CliContext) -> Result<(), CliError> {
    if ctx.json {
        return render_json(COMMAND, payload);
    }

    if !ctx.quiet {
        if payload.units.is_empty() {
            let color = Palette::new(ctx.no_color);
            println!(
                "{} {} {}",
                color.command("restart"),
                payload.component,
                color.warn("no instances to restart")
            );
            for warning in &payload.warnings {
                eprintln!("{} {}", color.warn("guidance:"), warning);
            }
        } else {
            render_human(payload, ctx.no_color);
        }
    }
    Ok(())
}

#[cfg(test)]
type RestartUnit = application::RestartUnit;

#[cfg(test)]
fn preview_restart_results(
    units: &[RestartUnit],
    system_manager: &dyn anolisa_core::ServiceManager,
    user_manager: &dyn anolisa_core::ServiceManager,
) -> Vec<RestartResult> {
    application::preview_restart_units(units, system_manager, user_manager)
        .into_iter()
        .map(project_restart_unit)
        .collect()
}

#[cfg(test)]
fn discover_rpm_units<R: anolisa_platform::command::CommandRunner>(
    package: Option<&str>,
    component: &str,
    query: &anolisa_platform::rpm_query::RpmPackageQuery<R>,
) -> Result<(Vec<RestartUnit>, Vec<String>), CliError> {
    application::discover_rpm_units(package, component, query)
}

#[cfg(test)]
fn rpm_tooling_missing_error(command: &str) -> CliError {
    application::rpm_tooling_missing_error(command)
}

#[cfg(test)]
fn guidance_only_payload(
    component: &str,
    install_mode: &str,
    warnings: Vec<String>,
    dry_run: bool,
) -> RestartPayload {
    project_restart_outcome(application::guidance_only_outcome(
        component,
        install_mode,
        warnings,
        execution_intent(dry_run),
    ))
}

#[cfg(test)]
fn classify_unit_files(paths: &[String]) -> Vec<(String, anolisa_core::ServiceScope)> {
    application::classify_unit_files(paths)
}

#[cfg(test)]
fn is_template_unit(unit: &str) -> bool {
    application::is_template_unit(unit)
}

#[cfg(test)]
fn template_guidance(unit: &str, scope: anolisa_core::ServiceScope) -> String {
    application::template_guidance(unit, scope)
}

#[derive(Debug, Clone, serde::Serialize)]
struct RestartResult {
    component: String,
    unit: String,
    state: String,
    changed: bool,
    manager: String,
    message: String,
}

/// Wire state for a dry-run unit that execute would restart.
const PLANNED_STATE: &str = "planned";

#[derive(Debug, serde::Serialize)]
struct RestartPayload {
    component: String,
    install_mode: String,
    manager: String,
    supported: bool,
    dry_run: bool,
    units: Vec<RestartResult>,
    warnings: Vec<String>,
}

/// Banner verb for the human summary. Unsupported scopes keep `skipped`
/// even on `--dry-run`, so a preview does not look successful when every
/// unit would be refused.
fn human_banner_status(payload: &RestartPayload) -> &'static str {
    if !payload.supported {
        "skipped"
    } else if payload.dry_run {
        "planned"
    } else {
        "dispatched"
    }
}

fn render_human(payload: &RestartPayload, no_color: bool) {
    let color = Palette::new(no_color);
    let status = human_banner_status(payload);
    if status == "skipped" {
        println!(
            "{} {} {} {}",
            color.command("restart"),
            payload.component,
            color.warn(status),
            color.muted(format!("(manager={} unsupported)", payload.manager))
        );
    } else {
        println!(
            "{} {} {}",
            color.command("restart"),
            payload.component,
            color.ok(status)
        );
    }
    println!("{} {}", color.label("manager:"), payload.manager);
    if !payload.units.is_empty() {
        println!("{}", color.header("units:"));
        for r in &payload.units {
            println!(
                "  - {}/{} {} (changed={})",
                r.component,
                r.unit,
                color.status(&r.state),
                color.bool_value(r.changed),
            );
        }
    }
    for w in &payload.warnings {
        eprintln!("{} {}", color.warn("warning:"), w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::tier1::rpm_install;
    use crate::context::InstallMode;
    use anolisa_core::domain::{
        Installation, InstallationScope, LifecycleStatus, OwnedArtifact, ProviderBinding,
    };
    use anolisa_core::state::ServiceRef;
    use anolisa_core::state_store::StateStore;
    use anolisa_core::transaction::Transaction;
    use anolisa_core::{
        FakeServiceManager, ObjectKind, ServiceError, ServiceManager, ServiceOp, ServiceOutcome,
        ServiceScope, ServiceState,
    };
    use anolisa_platform::command::{CommandOutput, CommandRunner};
    use anolisa_platform::fs_layout::FsLayout;
    use anolisa_platform::rpm_query::RpmPackageQuery;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn ctx_with_prefix(install_mode: InstallMode, prefix: Option<PathBuf>) -> CliContext {
        let root = prefix
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("/tmp/anolisa-restart-validation"));
        // Identity resolution consults the component index for names absent
        // from state; a seeded local index keeps fixture names supported.
        if install_mode == InstallMode::System
            && let Some(prefix) = prefix.as_ref()
        {
            crate::commands::tier1::install::tests::seed_repo_config_with_index(
                &anolisa_platform::fs_layout::FsLayout::system(Some(prefix.clone())),
                crate::commands::tier1::install::tests::TEST_INDEX_COMPONENTS,
            );
        }
        crate::test_support::context_for_root(
            root,
            install_mode,
            prefix.clone(),
            Default::default(),
        )
    }

    fn ctx_with_options(
        install_mode: InstallMode,
        prefix: Option<PathBuf>,
        options: crate::test_support::TestContextOptions,
    ) -> CliContext {
        let root = prefix
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("/tmp/anolisa-restart-validation"));
        if install_mode == InstallMode::System
            && let Some(prefix) = prefix.as_ref()
        {
            crate::commands::tier1::install::tests::seed_repo_config_with_index(
                &anolisa_platform::fs_layout::FsLayout::system(Some(prefix.clone())),
                crate::commands::tier1::install::tests::TEST_INDEX_COMPONENTS,
            );
        }
        crate::test_support::context_for_root(root, install_mode, prefix.clone(), options)
    }

    const PROBE_UNIT: &str = "anolisa-restart-dry-run-probe.service";

    fn planted_restart_ctx(dry_run: bool) -> (tempfile::TempDir, CliContext) {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix.clone()));
        plant_owned_restartable(&layout, "agentsight", PROBE_UNIT, ServiceScope::System);
        let ctx = ctx_with_options(
            InstallMode::System,
            Some(prefix),
            crate::test_support::TestContextOptions {
                dry_run,
                quiet: true,
                no_color: true,
                ..Default::default()
            },
        );
        (tmp, ctx)
    }

    fn plant_owned_restartable(layout: &FsLayout, name: &str, unit: &str, scope: ServiceScope) {
        plant_owned_restartables(layout, name, &[(unit, scope)]);
    }

    fn plant_owned_restartables(layout: &FsLayout, name: &str, services: &[(&str, ServiceScope)]) {
        let state_path = layout.state_dir.join("installed.toml");
        let mut store = StateStore::empty_for_layout(layout);
        store.upsert(Installation {
            kind: ObjectKind::Component,
            name: name.to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: "1.0.0".to_string(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files: Vec::new(),
                    services: services
                        .iter()
                        .map(|(unit, scope)| ServiceRef {
                            name: (*unit).to_string(),
                            manager: "systemd".to_string(),
                            restartable: true,
                            enabled: true,
                            scope: *scope,
                        })
                        .collect(),
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-07-21T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        store.save(&state_path).expect("save state");
    }

    /// Fake `CommandRunner` returning a canned `rpm -ql` listing (or a spawn
    /// error), so the RPM discovery path is exercised without a real `rpm`.
    struct FakeRpm {
        stdout: String,
        code: Option<i32>,
        spawn_err: Option<std::io::ErrorKind>,
    }

    impl CommandRunner for FakeRpm {
        fn run(&self, _program: &str, _args: &[&str]) -> std::io::Result<CommandOutput> {
            if let Some(kind) = self.spawn_err {
                return Err(std::io::Error::new(kind, "fake spawn failure"));
            }
            Ok(CommandOutput {
                code: self.code,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    fn fake_query(listing: &str) -> RpmPackageQuery<FakeRpm> {
        RpmPackageQuery::with_runner(FakeRpm {
            stdout: listing.to_string(),
            code: Some(0),
            spawn_err: None,
        })
    }

    struct FixedStateManager {
        state: ServiceState,
        calls: Mutex<Vec<(ServiceOp, String)>>,
    }

    impl FixedStateManager {
        fn new(state: ServiceState) -> Self {
            Self {
                state,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn outcome(&self, op: ServiceOp, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.calls
                .lock()
                .expect("poisoned")
                .push((op, unit.to_string()));
            Ok(ServiceOutcome {
                manager: "fixed-state".to_string(),
                unit: unit.to_string(),
                op,
                state: self.state,
                supported: true,
                changed: false,
                message: format!("fixed {} {}", op.as_str(), unit),
            })
        }
    }

    impl ServiceManager for FixedStateManager {
        fn manager(&self) -> &str {
            "fixed-state"
        }

        fn supported(&self) -> bool {
            true
        }

        fn daemon_reload(&self) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::DaemonReload, "")
        }

        fn probe_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Probe, unit)
        }

        fn start_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Start, unit)
        }

        fn stop_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Stop, unit)
        }

        fn restart_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Restart, unit)
        }

        fn enable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Enable, unit)
        }

        fn disable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
            self.outcome(ServiceOp::Disable, unit)
        }
    }

    #[test]
    fn restart_unknown_component_returns_not_installed() {
        let tmp = tempdir().expect("tmpdir");
        let err = handle(
            RestartArgs {
                component: "agentsight".to_string(),
            },
            &ctx_with_prefix(InstallMode::System, Some(tmp.path().to_path_buf())),
        )
        .expect_err("must error");
        assert_eq!(err.code(), "NOT_INSTALLED");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.reason().contains("not installed"),
            "reason must mention 'not installed': {}",
            err.reason()
        );
    }

    #[test]
    fn restart_refuses_a_pending_lifecycle_before_service_side_effects() {
        let tmp = tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let state_path = layout.state_dir.join("installed.toml");
        let mut store = StateStore::empty_for_layout(&layout);
        store.upsert(Installation {
            kind: ObjectKind::Component,
            name: "agentsight".to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: "1.0.0".to_string(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files: Vec::new(),
                    services: vec![ServiceRef {
                        name: "agentsight.service".to_string(),
                        manager: "systemd".to_string(),
                        restartable: true,
                        enabled: true,
                        scope: ServiceScope::System,
                    }],
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-07-21T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        store.save(&state_path).expect("save state");
        let _pending = Transaction::begin_with_subject(
            "uninstall",
            Some("agentsight"),
            state_path.clone(),
            &rpm_install::journal_dir(&layout),
        )
        .expect("begin pending uninstall");

        let err = handle(
            RestartArgs {
                component: "agentsight".to_string(),
            },
            &ctx_with_prefix(InstallMode::System, Some(tmp.path().to_path_buf())),
        )
        .expect_err("pending lifecycle must block restart");

        assert!(err.reason().contains("pending operation journal"), "{err}");
        assert!(err.reason().contains("anolisa repair agentsight"), "{err}");
    }

    #[test]
    fn classify_unit_files_keeps_direct_service_units_with_scope() {
        let paths = vec![
            // not a unit
            "/usr/local/bin/agentsight".to_string(),
            // system units in the canonical and FHS-local dirs
            "/usr/lib/systemd/system/agentsight.service".to_string(),
            "/usr/local/lib/systemd/system/ws-ckpt.service".to_string(),
            // user-scope template
            "/usr/lib/systemd/user/anolisa-memory@.service".to_string(),
            // enable symlink in a .wants subdir — not a canonical unit file
            "/etc/systemd/system/multi-user.target.wants/x.service".to_string(),
            // drop-in (.conf, not .service)
            "/usr/lib/systemd/system/foo.service.d/override.conf".to_string(),
            // non-.service unit type
            "/usr/lib/systemd/system/foo.socket".to_string(),
            // .service outside any known unit dir
            "/opt/random/bar.service".to_string(),
        ];
        assert_eq!(
            classify_unit_files(&paths),
            vec![
                ("agentsight.service".to_string(), ServiceScope::System),
                ("ws-ckpt.service".to_string(), ServiceScope::System),
                ("anolisa-memory@.service".to_string(), ServiceScope::User),
            ]
        );
    }

    #[test]
    fn is_template_unit_matches_only_bare_template() {
        assert!(is_template_unit("anolisa-memory@.service"));
        assert!(!is_template_unit("agentsight.service"));
        // A concrete instance is restartable directly, so it is not a template.
        assert!(!is_template_unit("anolisa-memory@alice.service"));
    }

    #[test]
    fn template_guidance_user_mentions_instance_and_linger() {
        let msg = template_guidance("anolisa-memory@.service", ServiceScope::User);
        assert!(msg.contains("anolisa-memory@<user>.service"), "{msg}");
        assert!(msg.contains("--user"), "{msg}");
        assert!(msg.contains("enable-linger"), "{msg}");
    }

    #[test]
    fn template_guidance_system_mentions_instance_not_user_bus() {
        let msg = template_guidance("getty@.service", ServiceScope::System);
        assert!(msg.contains("getty@<instance>.service"), "{msg}");
        assert!(!msg.contains("--user"), "{msg}");
    }

    #[test]
    fn rpm_tooling_missing_error_mentions_restart_and_tooling() {
        // P3: rpm/dnf absent must surface the same actionable message the other
        // tier-1 commands use, not a bare "command not found".
        let err = rpm_tooling_missing_error("restart agent-memory");
        assert!(
            err.reason().contains("rpm/dnf not found"),
            "{}",
            err.reason()
        );
        assert!(err.reason().contains("restart"), "{}", err.reason());
    }

    #[test]
    fn guidance_only_template_component_succeeds_not_errors() {
        // A component whose only units are templates is NOT an error — the
        // guidance-only path returns Ok with the per-user guidance.
        let ctx = ctx_with_prefix(InstallMode::System, None);
        let warnings = vec![template_guidance(
            "anolisa-memory@.service",
            ServiceScope::User,
        )];
        let res = render_restart(
            &guidance_only_payload("agent-memory", "system", warnings, ctx.dry_run),
            &ctx,
        );
        assert!(
            res.is_ok(),
            "templates-only restart must succeed, got {res:?}"
        );
    }

    #[test]
    fn guidance_only_payload_uses_stable_manager_label() {
        // Wire semantics: guidance-only carries a stable `none` sentinel (not an
        // empty string), no units, the guidance as warnings, and supported=true.
        let warnings = vec![template_guidance(
            "anolisa-memory@.service",
            ServiceScope::User,
        )];
        let payload = guidance_only_payload("agent-memory", "system", warnings, false);
        assert_eq!(payload.manager, "none");
        assert!(payload.supported);
        assert!(!payload.dry_run);
        assert!(payload.units.is_empty());
        assert_eq!(payload.warnings.len(), 1);
        assert!(
            payload.warnings[0].contains("--user"),
            "{}",
            payload.warnings[0]
        );
    }

    #[test]
    fn discover_rpm_units_splits_plain_units_and_templates() {
        // End-to-end over a fake `rpm -ql`: a plain system .service becomes a
        // restartable unit; a user template degrades to a per-user note; bins
        // and docs are ignored.
        let listing = [
            "/usr/local/bin/agent-memory",
            "/usr/lib/systemd/system/agentsight.service",
            "/usr/lib/systemd/user/anolisa-memory@.service",
            "/usr/share/doc/agent-memory/README",
        ]
        .join("\n");
        let (units, notes) =
            discover_rpm_units(Some("agent-memory"), "agent-memory", &fake_query(&listing))
                .expect("discovery ok");

        assert_eq!(units.len(), 1, "one plain unit expected: {units:?}");
        assert_eq!(units[0].unit, "agentsight.service");
        assert_eq!(units[0].scope, ServiceScope::System);
        assert_eq!(units[0].component, "agent-memory");

        assert_eq!(notes.len(), 1, "one template note expected: {notes:?}");
        assert!(
            notes[0].contains("anolisa-memory@<user>.service") && notes[0].contains("--user"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn discover_rpm_units_without_package_name_errors() {
        // RPM-backed but state lost the package name → actionable repair hint.
        let err = discover_rpm_units(None, "agent-memory", &fake_query(""))
            .expect_err("missing package name must error");
        assert!(err.reason().contains("repair"), "{}", err.reason());
    }

    #[test]
    fn discover_rpm_units_tooling_missing_maps_to_actionable_error() {
        // `rpm` absent (spawn NotFound) → the uniform rpm/dnf-not-found message,
        // not a generic "command not found".
        let query = RpmPackageQuery::with_runner(FakeRpm {
            stdout: String::new(),
            code: None,
            spawn_err: Some(std::io::ErrorKind::NotFound),
        });
        let err = discover_rpm_units(Some("agent-memory"), "agent-memory", &query)
            .expect_err("missing rpm tooling must error");
        assert!(
            err.reason().contains("rpm/dnf not found"),
            "{}",
            err.reason()
        );
    }

    #[test]
    fn dry_run_lists_recorded_units_without_dispatching() {
        // Inject supported recording managers so this cannot go green on a
        // host whose real manager is `not_supported` and therefore never
        // calls reload/restart even on the execute path.
        let (_tmp, ctx) = planted_restart_ctx(true);
        let sys = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload =
            compute_restart_with("agentsight", &ctx, Some((&sys, &user))).expect("dry-run preview");

        assert!(payload.dry_run, "preview must mark dry_run");
        assert!(payload.supported);
        assert_eq!(payload.units.len(), 1, "{:?}", payload.units);
        assert_eq!(payload.units[0].unit, PROBE_UNIT);
        assert_eq!(payload.units[0].state, PLANNED_STATE);
        assert!(!payload.units[0].changed);
        assert!(
            payload.units[0].message.contains("dry-run: would restart"),
            "{}",
            payload.units[0].message
        );
        assert!(
            sys.calls().is_empty(),
            "dry-run must not reload or restart: {:?}",
            sys.calls()
        );
        assert!(
            user.calls().is_empty(),
            "dry-run must not touch the user manager: {:?}",
            user.calls()
        );
        assert!(
            payload
                .warnings
                .iter()
                .all(|w| !w.contains("daemon-reload")),
            "dry-run must not daemon-reload: {:?}",
            payload.warnings
        );
    }

    #[test]
    fn execute_records_reload_and_restart_when_manager_is_supported() {
        let (_tmp, ctx) = planted_restart_ctx(false);
        let sys = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload =
            compute_restart_with("agentsight", &ctx, Some((&sys, &user))).expect("execute restart");

        assert!(!payload.dry_run);
        assert!(payload.supported);
        assert_eq!(payload.units.len(), 1, "{:?}", payload.units);
        assert_eq!(payload.units[0].unit, PROBE_UNIT);
        assert_ne!(payload.units[0].state, PLANNED_STATE);
        assert_eq!(
            sys.calls(),
            vec![
                (ServiceOp::DaemonReload, String::new()),
                (ServiceOp::Restart, PROBE_UNIT.to_string()),
            ]
        );
        assert!(
            user.calls().is_empty(),
            "system-only execute must not drive the user manager: {:?}",
            user.calls()
        );
    }

    #[test]
    fn execution_intent_selects_the_typed_preview_branch() {
        let (tmp, ctx) = planted_restart_ctx(false);
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let system = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let outcome = application::run_with_managers(
            RestartRequest {
                component: "agentsight",
                intent: ExecutionIntent::Plan,
            },
            &ctx,
            Some((&system, &user)),
        )
        .expect("typed preview");

        assert!(matches!(outcome, RestartApplicationOutcome::Preview { .. }));
        assert!(system.calls().is_empty());
        assert!(user.calls().is_empty());
        assert!(!layout.lock_file.exists());
    }

    #[test]
    fn apply_routes_mixed_scopes_and_reloads_each_once() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix.clone()));
        plant_owned_restartables(
            &layout,
            "agentsight",
            &[
                ("agentsight.service", ServiceScope::System),
                ("agentsight-user.service", ServiceScope::User),
            ],
        );
        let ctx = ctx_with_options(
            InstallMode::System,
            Some(prefix),
            crate::test_support::TestContextOptions {
                quiet: true,
                no_color: true,
                ..Default::default()
            },
        );
        let system = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload = compute_restart_with("agentsight", &ctx, Some((&system, &user)))
            .expect("mixed-scope restart");

        assert_eq!(payload.manager, "fake+fake");
        assert_eq!(payload.units.len(), 2);
        assert_eq!(
            system.calls(),
            vec![
                (ServiceOp::DaemonReload, String::new()),
                (ServiceOp::Restart, "agentsight.service".to_string()),
            ]
        );
        assert_eq!(
            user.calls(),
            vec![
                (ServiceOp::DaemonReload, String::new()),
                (ServiceOp::Restart, "agentsight-user.service".to_string()),
            ]
        );
    }

    #[test]
    fn apply_keeps_reload_failure_as_a_warning_and_restarts_units() {
        let (_tmp, ctx) = planted_restart_ctx(false);
        let system = FakeServiceManager::new();
        system.fail(ServiceOp::DaemonReload, "");
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload = compute_restart_with("agentsight", &ctx, Some((&system, &user)))
            .expect("reload failure stays non-fatal");

        assert!(
            payload
                .warnings
                .iter()
                .any(|warning| warning.contains("daemon-reload (system scope) failed")),
            "{:?}",
            payload.warnings
        );
        assert_eq!(
            system.calls(),
            vec![
                (ServiceOp::DaemonReload, String::new()),
                (ServiceOp::Restart, PROBE_UNIT.to_string()),
            ]
        );
        assert_ne!(payload.units[0].state, "unknown");
    }

    #[test]
    fn apply_projects_restart_failure_to_unknown_with_warning() {
        let (_tmp, ctx) = planted_restart_ctx(false);
        let system = FakeServiceManager::new();
        system.fail(ServiceOp::Restart, PROBE_UNIT);
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload = compute_restart_with("agentsight", &ctx, Some((&system, &user)))
            .expect("restart failure stays non-fatal");

        assert_eq!(payload.units[0].state, "unknown");
        assert!(!payload.units[0].changed);
        assert!(payload.units[0].message.contains("fake forced failure"));
        assert!(
            payload
                .warnings
                .iter()
                .any(|warning| warning.contains("service restart skipped")),
            "{:?}",
            payload.warnings
        );
    }

    #[test]
    fn apply_preserves_abnormal_service_state_and_warning() {
        let (_tmp, ctx) = planted_restart_ctx(false);
        let system = FixedStateManager::new(ServiceState::Failed);
        let user = FakeServiceManager::with_scope(ServiceScope::User);

        let payload = compute_restart_with("agentsight", &ctx, Some((&system, &user)))
            .expect("abnormal state remains a successful command result");

        assert_eq!(payload.units[0].state, "failed");
        assert!(
            payload
                .warnings
                .iter()
                .any(|warning| warning.contains("reports state 'failed' after restart")),
            "{:?}",
            payload.warnings
        );
    }

    #[test]
    fn dry_run_human_banner_stays_skipped_when_unsupported() {
        let unsupported = RestartPayload {
            component: "agentsight".to_string(),
            install_mode: "system".to_string(),
            manager: "not-supported".to_string(),
            supported: false,
            dry_run: true,
            units: vec![RestartResult {
                component: "agentsight".to_string(),
                unit: PROBE_UNIT.to_string(),
                state: "not_supported".to_string(),
                changed: false,
                manager: "not-supported".to_string(),
                message: "container".to_string(),
            }],
            warnings: Vec::new(),
        };
        assert_eq!(human_banner_status(&unsupported), "skipped");

        let planned = RestartPayload {
            supported: true,
            manager: "fake".to_string(),
            units: vec![RestartResult {
                manager: "fake".to_string(),
                state: PLANNED_STATE.to_string(),
                message: "dry-run: would restart".to_string(),
                ..unsupported.units[0].clone()
            }],
            ..unsupported
        };
        assert_eq!(human_banner_status(&planned), "planned");

        let dispatched = RestartPayload {
            dry_run: false,
            ..planned
        };
        assert_eq!(human_banner_status(&dispatched), "dispatched");
    }

    #[test]
    fn dry_run_still_refuses_an_absent_component() {
        let tmp = tempdir().expect("tmpdir");
        let ctx = ctx_with_options(
            InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                dry_run: true,
                quiet: true,
                no_color: true,
                ..Default::default()
            },
        );
        let err = compute_restart("agentsight", &ctx).expect_err("absent target must fail");
        assert_eq!(err.code(), "NOT_INSTALLED");
    }

    #[test]
    fn dry_run_still_refuses_a_pending_lifecycle() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix.clone()));
        plant_owned_restartable(
            &layout,
            "agentsight",
            "anolisa-restart-dry-run-probe.service",
            ServiceScope::System,
        );
        let _pending = Transaction::begin_with_subject(
            "uninstall",
            Some("agentsight"),
            layout.state_dir.join("installed.toml"),
            &rpm_install::journal_dir(&layout),
        )
        .expect("begin pending uninstall");
        let ctx = ctx_with_options(
            InstallMode::System,
            Some(prefix),
            crate::test_support::TestContextOptions {
                dry_run: true,
                quiet: true,
                no_color: true,
                ..Default::default()
            },
        );

        let err = compute_restart("agentsight", &ctx)
            .expect_err("pending lifecycle must block the preview too");
        assert!(err.reason().contains("pending operation journal"), "{err}");
    }

    #[test]
    fn dry_run_does_not_create_the_exclusive_lock() {
        let (_tmp, ctx) = planted_restart_ctx(true);
        let layout = FsLayout::system(Some(ctx.prefix.clone().expect("planted prefix")));
        assert!(
            !layout.lock_file.exists(),
            "fixture must start without a lock file"
        );

        let sys = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);
        let payload =
            compute_restart_with("agentsight", &ctx, Some((&sys, &user))).expect("dry-run preview");

        assert!(payload.dry_run);
        assert_eq!(payload.units[0].state, PLANNED_STATE);
        assert!(
            !layout.lock_file.exists(),
            "preview must not create {}",
            layout.lock_file.display()
        );
    }

    #[test]
    fn dry_run_previews_from_a_non_writable_system_state_root() {
        // Root ignores directory mode bits, so this models a normal
        // unprivileged preview against a root-owned `/var/lib/anolisa`.
        if anolisa_platform::privilege::effective_uid() == 0 {
            return;
        }

        let (tmp, dry_ctx) = planted_restart_ctx(true);
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let exec_ctx = ctx_with_options(
            InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                dry_run: false,
                quiet: true,
                no_color: true,
                ..Default::default()
            },
        );
        let _restore = RestorePermissions::dir_mode(&layout.state_dir, 0o555, 0o755);

        let sys = FakeServiceManager::new();
        let user = FakeServiceManager::with_scope(ServiceScope::User);
        let payload = compute_restart_with("agentsight", &dry_ctx, Some((&sys, &user)))
            .expect("non-writable preview must still list units");
        assert!(payload.dry_run);
        assert_eq!(payload.units[0].unit, PROBE_UNIT);
        assert_eq!(payload.units[0].state, PLANNED_STATE);
        assert!(
            !layout.lock_file.exists(),
            "preview must not create the exclusive lock"
        );

        let err = compute_restart("agentsight", &exec_ctx)
            .expect_err("execute still needs a writable exclusive lock");
        assert!(
            err.reason().contains("failed to acquire install lock"),
            "{err}"
        );
    }

    struct RestorePermissions {
        path: PathBuf,
        restore: u32,
    }

    impl RestorePermissions {
        fn dir_mode(path: &std::path::Path, restrict: u32, restore: u32) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .expect("state dir metadata")
                .permissions();
            perms.set_mode(restrict);
            std::fs::set_permissions(path, perms).expect("restrict state dir");
            Self {
                path: path.to_path_buf(),
                restore,
            }
        }
    }

    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&self.path) {
                let mut perms = metadata.permissions();
                perms.set_mode(self.restore);
                let _ = std::fs::set_permissions(&self.path, perms);
            }
        }
    }

    #[test]
    fn preview_marks_unsupported_scopes_without_planned() {
        let units = [RestartUnit {
            component: "agentsight".to_string(),
            unit: "agentsight.service".to_string(),
            scope: ServiceScope::System,
        }];
        let manager = anolisa_core::NotSupportedServiceManager::new(
            "container runtime detected — refusing to drive systemctl".to_string(),
        );
        let results = preview_restart_results(&units, &manager, &manager);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].state, "not_supported");
        assert!(!results[0].changed);
        assert_ne!(results[0].state, PLANNED_STATE);
    }
}
