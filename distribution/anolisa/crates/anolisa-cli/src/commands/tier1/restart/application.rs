//! Application orchestration for `anolisa restart`.

use anolisa_core::domain::{Installation, ProviderBinding};
use anolisa_core::execution::ExecutionIntent;
use anolisa_core::facts::{JournalEvidence, pending_journal_for};
use anolisa_core::lock::InstallLock;
use anolisa_core::{
    ObjectKind, ServiceManager, ServiceScope, ServiceState,
    service_for_install_mode as service_factory,
    user_service_for_install_mode as user_service_factory,
};
use anolisa_env::EnvService;
use anolisa_platform::command::CommandRunner;
use anolisa_platform::pkg_query::PackageQueryError;
use anolisa_platform::rpm_query::RpmPackageQuery;

use crate::commands::common;
use crate::commands::tier1::recovery::{LockedJournalGate, pending_operation_error};
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::response::CliError;

use super::COMMAND;

/// Typed input for one restart request.
pub(super) struct RestartRequest<'a> {
    pub(super) component: &'a str,
    pub(super) intent: ExecutionIntent,
}

/// Resolved component and manager facts carried to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RestartSummary {
    pub(super) component: String,
    pub(super) install_mode: String,
    pub(super) manager: String,
    pub(super) supported: bool,
}

/// Typed result for one restartable service unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RestartUnitOutcome {
    /// Plan-only result for a unit that the selected manager can drive.
    Planned {
        component: String,
        unit: String,
        manager: String,
    },
    /// Result returned after the manager dispatched the restart.
    Dispatched {
        component: String,
        unit: String,
        state: ServiceState,
        changed: bool,
        manager: String,
        message: String,
    },
    /// Deliberate skip because the unit's scope has no supported manager.
    Unsupported {
        component: String,
        unit: String,
        manager: String,
        message: String,
    },
    /// Best-effort restart failure retained as a per-unit result and warning.
    Failed {
        component: String,
        unit: String,
        manager: String,
        message: String,
    },
}

/// Typed application result consumed by the compatibility renderer.
pub(super) enum RestartApplicationOutcome {
    /// The package exposes only templates whose instances cannot be inferred.
    Guidance {
        summary: RestartSummary,
        intent: ExecutionIntent,
        warnings: Vec<String>,
    },
    /// Plan-only result; no exclusive lock or service mutation was performed.
    Preview {
        summary: RestartSummary,
        units: Vec<RestartUnitOutcome>,
        warnings: Vec<String>,
    },
    /// Applied result; service failures remain best-effort unit outcomes.
    Applied {
        summary: RestartSummary,
        units: Vec<RestartUnitOutcome>,
        warnings: Vec<String>,
    },
}

/// Run a restart request against production service managers.
pub(super) fn run(
    request: RestartRequest<'_>,
    ctx: &CliContext,
) -> Result<RestartApplicationOutcome, CliError> {
    run_with_managers(request, ctx, None)
}

/// Run a restart request with optional injected service managers.
///
/// Tests inject recording managers so plan-only execution cannot pass merely
/// because the host selected a manager that skips every operation.
pub(super) fn run_with_managers(
    request: RestartRequest<'_>,
    ctx: &CliContext,
    managers: Option<(&dyn ServiceManager, &dyn ServiceManager)>,
) -> Result<RestartApplicationOutcome, CliError> {
    let command = format!("{COMMAND} {}", request.component);
    let install_mode = ctx.install_mode.as_str();
    let layout = common::resolve_layout(ctx);

    // Preview reads recorded units and journals without creating or
    // exclusively locking a root-owned state directory.
    let exclusive_lock = match request.intent {
        ExecutionIntent::Plan => None,
        ExecutionIntent::Apply => Some(InstallLock::acquire(&layout.lock_file).map_err(
            |error| CliError::Runtime {
                command: command.clone(),
                reason: format!("failed to acquire install lock: {error}"),
            },
        )?),
    };

    let (resolved, view) = common::resolve_mutation_target(request.component, ctx, &command)?;
    let journal_dir = rpm_install::journal_dir(&layout);
    let evidence = JournalEvidence::new(&journal_dir, &view.writable.state.operations);
    if let Some(ref lock) = exclusive_lock {
        let journal_gate = LockedJournalGate::load(lock, evidence, &command)?;
        journal_gate.ensure_clear(&resolved, &command)?;
    } else if let Some(path) =
        pending_journal_for(evidence, &resolved).map_err(|error| CliError::Runtime {
            command: command.clone(),
            reason: format!("failed to inspect operation journals: {error}"),
        })?
    {
        return Err(pending_operation_error(&command, &resolved, &path));
    }

    let component = view
        .writable
        .state
        .find(ObjectKind::Component, &resolved)
        .ok_or_else(|| CliError::NotInstalled {
            command: command.clone(),
            reason: format!(
                "component '{resolved}' is not installed — nothing to restart (run `anolisa status` to see what is installed)"
            ),
        })?;

    let (units, mut warnings) = collect_restart_units(component, &resolved)?;
    if units.is_empty() {
        if warnings.is_empty() {
            return Err(CliError::InvalidArgument {
                command,
                reason: format!("component '{resolved}' has no restartable systemd service units"),
            });
        }
        return Ok(guidance_only_outcome(
            &resolved,
            install_mode,
            warnings,
            request.intent,
        ));
    }

    let detected_system;
    let detected_user;
    let (system_manager, user_manager) = match managers {
        Some(pair) => pair,
        None => {
            let env = EnvService::detect();
            detected_system = service_factory(install_mode, &env);
            detected_user = user_service_factory(install_mode, &env);
            (detected_system.as_ref(), detected_user.as_ref())
        }
    };

    let uses_system = units.iter().any(|unit| unit.scope == ServiceScope::System);
    let uses_user = units.iter().any(|unit| unit.scope == ServiceScope::User);
    let supported =
        (uses_system && system_manager.supported()) || (uses_user && user_manager.supported());
    let manager = match (uses_system, uses_user) {
        (true, true) => format!("{}+{}", system_manager.manager(), user_manager.manager()),
        (true, false) => system_manager.manager().to_string(),
        (false, true) => user_manager.manager().to_string(),
        (false, false) => unreachable!("restartable units present but no scope flagged"),
    };
    let summary = RestartSummary {
        component: resolved,
        install_mode: install_mode.to_string(),
        manager,
        supported,
    };

    match request.intent {
        ExecutionIntent::Plan => Ok(RestartApplicationOutcome::Preview {
            units: preview_restart_units(&units, system_manager, user_manager),
            summary,
            warnings,
        }),
        ExecutionIntent::Apply => Ok(RestartApplicationOutcome::Applied {
            units: execute_restart_units(
                &units,
                system_manager,
                user_manager,
                uses_system,
                uses_user,
                &mut warnings,
            ),
            summary,
            warnings,
        }),
    }
}

/// Stable manager label for a guidance-only result where no manager is used.
pub(super) const GUIDANCE_MANAGER: &str = "none";

/// Build a successful outcome when only unexpandable template units were found.
///
/// No manager is engaged, so the summary carries [`GUIDANCE_MANAGER`] rather
/// than an empty label and preserves the discovery notes as user guidance.
pub(super) fn guidance_only_outcome(
    component: &str,
    install_mode: &str,
    warnings: Vec<String>,
    intent: ExecutionIntent,
) -> RestartApplicationOutcome {
    RestartApplicationOutcome::Guidance {
        summary: RestartSummary {
            component: component.to_string(),
            install_mode: install_mode.to_string(),
            manager: GUIDANCE_MANAGER.to_string(),
            supported: true,
        },
        intent,
        warnings,
    }
}

/// Describe units apply would touch without contacting either service manager.
pub(super) fn preview_restart_units(
    units: &[RestartUnit],
    system_manager: &dyn ServiceManager,
    user_manager: &dyn ServiceManager,
) -> Vec<RestartUnitOutcome> {
    units
        .iter()
        .map(|unit| {
            let manager = manager_for_scope(unit.scope, system_manager, user_manager);
            if manager.supported() {
                RestartUnitOutcome::Planned {
                    component: unit.component.clone(),
                    unit: unit.unit.clone(),
                    manager: manager.manager().to_string(),
                }
            } else {
                unsupported_restart_outcome(unit, manager)
            }
        })
        .collect()
}

fn execute_restart_units(
    units: &[RestartUnit],
    system_manager: &dyn ServiceManager,
    user_manager: &dyn ServiceManager,
    uses_system: bool,
    uses_user: bool,
    warnings: &mut Vec<String>,
) -> Vec<RestartUnitOutcome> {
    if uses_system
        && system_manager.supported()
        && let Err(error) = system_manager.daemon_reload()
    {
        warnings.push(format!("daemon-reload (system scope) failed: {error}"));
    }
    if uses_user
        && user_manager.supported()
        && let Err(error) = user_manager.daemon_reload()
    {
        warnings.push(format!("daemon-reload (user scope) failed: {error}"));
    }

    let mut outcomes = Vec::with_capacity(units.len());
    for unit in units {
        let manager = manager_for_scope(unit.scope, system_manager, user_manager);
        if !manager.supported() {
            outcomes.push(unsupported_restart_outcome(unit, manager));
            continue;
        }
        match manager.restart_service(&unit.unit) {
            Ok(outcome) => {
                if matches!(outcome.state, ServiceState::Failed | ServiceState::Unknown) {
                    warnings.push(format!(
                        "{}/{} reports state '{}' after restart",
                        unit.component,
                        unit.unit,
                        outcome.state.as_str()
                    ));
                }
                outcomes.push(RestartUnitOutcome::Dispatched {
                    component: unit.component.clone(),
                    unit: unit.unit.clone(),
                    state: outcome.state,
                    changed: outcome.changed,
                    manager: outcome.manager,
                    message: outcome.message,
                });
            }
            Err(error) => {
                let message = error.to_string();
                warnings.push(format!(
                    "service restart skipped for {}/{}: {message}",
                    unit.component, unit.unit
                ));
                outcomes.push(RestartUnitOutcome::Failed {
                    component: unit.component.clone(),
                    unit: unit.unit.clone(),
                    manager: manager.manager().to_string(),
                    message,
                });
            }
        }
    }
    outcomes
}

fn manager_for_scope<'a>(
    scope: ServiceScope,
    system_manager: &'a dyn ServiceManager,
    user_manager: &'a dyn ServiceManager,
) -> &'a dyn ServiceManager {
    match scope {
        ServiceScope::System => system_manager,
        ServiceScope::User => user_manager,
    }
}

fn unsupported_restart_outcome(
    unit: &RestartUnit,
    manager: &dyn ServiceManager,
) -> RestartUnitOutcome {
    RestartUnitOutcome::Unsupported {
        component: unit.component.clone(),
        unit: unit.unit.clone(),
        manager: manager.manager().to_string(),
        message: manager
            .unsupported_reason()
            .unwrap_or("service manager not supported in this environment")
            .to_string(),
    }
}

/// Restartable unit resolved from component state or an RPM file manifest.
#[derive(Debug)]
pub(super) struct RestartUnit {
    pub(super) component: String,
    pub(super) unit: String,
    pub(super) scope: ServiceScope,
}

/// Directories systemd searches for system unit files.
const SYSTEM_UNIT_DIRS: &[&str] = &[
    "/usr/lib/systemd/system",
    "/usr/local/lib/systemd/system",
    "/etc/systemd/system",
    "/run/systemd/system",
];

/// Directories systemd searches for user unit files.
const USER_UNIT_DIRS: &[&str] = &[
    "/usr/lib/systemd/user",
    "/usr/local/lib/systemd/user",
    "/etc/systemd/user",
];

/// Collect restartable units and discovery notes from the component's backend.
///
/// Owned installs use recorded service references; delegated installs query
/// the RPM file manifest because their state records no services.
fn collect_restart_units(
    component: &Installation,
    component_name: &str,
) -> Result<(Vec<RestartUnit>, Vec<String>), CliError> {
    match &component.binding {
        ProviderBinding::Delegated { package, .. } => discover_rpm_units(
            package.resolved_name(),
            component_name,
            &RpmPackageQuery::system(),
        ),
        ProviderBinding::Owned { artifact } => {
            let units = artifact
                .services
                .iter()
                .filter(|service| service.restartable)
                .map(|service| RestartUnit {
                    component: component_name.to_string(),
                    unit: service.name.clone(),
                    scope: service.scope,
                })
                .collect();
            Ok((units, Vec::new()))
        }
    }
}

/// Discover an RPM component's service units from its package file manifest.
///
/// [`classify_unit_files`] accepts only service files directly under a known
/// systemd unit directory. Template units cannot be expanded from package
/// metadata, so they produce guidance instead of restart targets.
///
/// # Errors
///
/// Returns [`CliError::Runtime`] when the component records no package name,
/// RPM tooling is unavailable, or the package file query fails.
pub(super) fn discover_rpm_units<R: CommandRunner>(
    package: Option<&str>,
    component: &str,
    query: &RpmPackageQuery<R>,
) -> Result<(Vec<RestartUnit>, Vec<String>), CliError> {
    let command = format!("{COMMAND} {component}");
    let package = package.ok_or_else(|| CliError::Runtime {
        command: command.clone(),
        reason: format!(
            "component '{component}' is RPM-backed but its state records no package name; run `anolisa repair {component}` to refresh rpm metadata"
        ),
    })?;

    let paths = match query.list_files(package) {
        Ok(paths) => paths,
        Err(PackageQueryError::CommandMissing { .. }) => {
            return Err(rpm_tooling_missing_error(&command));
        }
        Err(error) => {
            return Err(CliError::Runtime {
                command,
                reason: format!("could not list files for RPM package '{package}': {error}"),
            });
        }
    };

    let mut units = Vec::new();
    let mut notes = Vec::new();
    for (unit, scope) in classify_unit_files(&paths) {
        if is_template_unit(&unit) {
            notes.push(template_guidance(&unit, scope));
        } else {
            units.push(RestartUnit {
                component: component.to_string(),
                unit,
                scope,
            });
        }
    }
    Ok((units, notes))
}

/// Return the shared actionable error for missing RPM package tooling.
pub(super) fn rpm_tooling_missing_error(command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: "rpm/dnf not found: cannot restart an RPM-backed component without the package manager. Install rpm/dnf and retry".to_string(),
    }
}

/// Keep service files directly under known systemd unit directories.
///
/// Requiring an exact parent directory rejects enablement symlinks below
/// `*.target.wants` and drop-ins below `*.service.d`.
pub(super) fn classify_unit_files(paths: &[String]) -> Vec<(String, ServiceScope)> {
    let mut units = Vec::new();
    for path in paths {
        let path = path.trim();
        if !path.ends_with(".service") {
            continue;
        }
        let Some((directory, file)) = path.rsplit_once('/') else {
            continue;
        };
        let scope = if SYSTEM_UNIT_DIRS.contains(&directory) {
            ServiceScope::System
        } else if USER_UNIT_DIRS.contains(&directory) {
            ServiceScope::User
        } else {
            continue;
        };
        units.push((file.to_string(), scope));
    }
    units
}

/// Return whether a service name denotes an uninstantiated systemd template.
pub(super) fn is_template_unit(unit: &str) -> bool {
    unit.ends_with("@.service")
}

/// Explain how to choose an instance for a template restart cannot expand.
pub(super) fn template_guidance(unit: &str, scope: ServiceScope) -> String {
    let base = unit.trim_end_matches("@.service");
    match scope {
        ServiceScope::User => format!(
            "{unit} is a per-user template; restart cannot expand its instances — enable one as the target user: `systemctl --user enable --now {base}@<user>.service` (and `loginctl enable-linger <user>` to keep it running after logout)"
        ),
        ServiceScope::System => format!(
            "{unit} is a systemd template; restart cannot pick an instance — start a concrete one with `systemctl start {base}@<instance>.service`"
        ),
    }
}
