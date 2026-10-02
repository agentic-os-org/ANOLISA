//! Application orchestration for CLI self-update.

use std::path::{Path, PathBuf};

use serde::Serialize;

use anolisa_core::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::self_update::{self as core_self_update, ProgressFn};
use anolisa_core::transaction::mint_operation_id;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_transaction::RpmTransaction;

use crate::commands::common;
use crate::context::CliContext;
use crate::response::CliError;

use super::super::now_iso8601;

/// Typed input for one CLI self-update request.
pub(crate) struct SelfUpdateRequest<'a> {
    /// Manifest endpoint selected by the existing environment contract.
    pub(crate) endpoint_url: &'a str,
    /// Version of the currently running CLI binary.
    pub(crate) current_version: &'a str,
    /// Selects read-only preview or state-changing application.
    pub(crate) intent: ExecutionIntent,
}

/// Typed postcondition reported by an applied CLI self-update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelfUpdateChange {
    /// The standalone CLI binary was replaced.
    BinaryReplaced {
        /// Version running before replacement.
        from: String,
        /// Release version installed by the binary updater.
        to: String,
    },
    /// Updating the RPM-owned CLI was delegated to the package manager.
    RpmUpdateDelegated {
        /// Owning RPM package.
        package: String,
        /// Installed version observed before the transaction.
        before_version: Option<String>,
        /// Installed version observed after the transaction.
        after_version: Option<String>,
    },
}

/// Applied self-update facts needed by the compatibility renderer and audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelfUpdateApplied {
    /// The standalone binary updater completed.
    Binary {
        /// Version running before replacement.
        from: String,
        /// Release version installed by the binary updater.
        to: String,
    },
    /// The package manager accepted the RPM-owned update request.
    RpmPackage {
        /// Version running before delegation.
        from: String,
        /// Version advertised by the release manifest.
        to: String,
        /// Owning RPM package.
        package: String,
        /// Installed version observed before the transaction.
        before_version: Option<String>,
        /// Installed version observed after the transaction.
        after_version: Option<String>,
    },
}

impl SelfUpdateApplied {
    fn change(&self) -> SelfUpdateChange {
        match self {
            Self::Binary { from, to } => SelfUpdateChange::BinaryReplaced {
                from: from.clone(),
                to: to.clone(),
            },
            Self::RpmPackage {
                package,
                before_version,
                after_version,
                ..
            } => SelfUpdateChange::RpmUpdateDelegated {
                package: package.clone(),
                before_version: before_version.clone(),
                after_version: after_version.clone(),
            },
        }
    }
}

/// Typed application result separating no-op, preview, and applied states.
#[derive(Debug)]
pub(crate) enum SelfUpdateApplicationOutcome {
    /// The running CLI already matches the latest manifest version.
    AlreadyLatest {
        /// Version shared by the running CLI and release manifest.
        version: String,
    },
    /// A newer release exists, but Plan intent authorizes no effects.
    Preview {
        /// Version currently running.
        from: String,
        /// Version advertised by the release manifest.
        to: String,
    },
    /// One binary or RPM apply path completed.
    Applied {
        /// Command-specific facts needed by the existing renderer.
        result: SelfUpdateApplied,
        /// Completed outcome with the durable audit ID when one was written.
        outcome: CommandOutcome<SelfUpdateChange>,
    },
}

/// Terminal self-update error plus non-terminal audit warnings.
#[derive(Debug)]
pub(crate) struct SelfUpdateApplicationError {
    /// Existing CLI error returned by the failed operation.
    pub(crate) error: CliError,
    /// Audit-write diagnostics rendered before the terminal error.
    pub(crate) warnings: Vec<String>,
}

/// Runs self-update with production system dependencies.
pub(super) fn run(
    request: SelfUpdateRequest<'_>,
    ctx: &CliContext,
    on_progress: Option<&ProgressFn>,
) -> Result<SelfUpdateApplicationOutcome, Box<SelfUpdateApplicationError>> {
    let ops = SystemSelfUpdateOps;
    let query = RpmPackageQuery::system();
    let txn = RpmTransaction::system();
    run_application_with_deps(
        request,
        ctx,
        &ops,
        &query,
        &txn,
        privilege::is_root(),
        on_progress,
        &now_iso8601(),
    )
}

/// Failed dependency-injected run with the evidence learned before failure.
#[derive(Debug)]
pub(crate) struct SelfUpdateFailure {
    /// Existing terminal CLI error.
    pub(crate) error: CliError,
    /// Structured facts retained for the failure audit.
    pub(crate) context: SelfUpdateFailureContext,
}

/// Structured facts accumulated before a failed self-update.
#[derive(Debug, Serialize)]
pub(crate) struct SelfUpdateFailureContext {
    pub(crate) current_version: String,
    /// A failed transaction may still move rpmdb; absent means unobserved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) apply_mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rpm_version_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rpm_version_after: Option<String>,
    /// Manifest endpoint reduced to the non-secret scheme and authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) endpoint: Option<String>,
    /// URLs removed from persisted failure text; never serialized.
    #[serde(skip)]
    pub(crate) sensitive_urls: Vec<String>,
}

impl SelfUpdateFailureContext {
    /// Starts failure tracking with only the running version and endpoint.
    pub(crate) fn new(current_version: &str, endpoint_url: &str) -> Self {
        Self {
            current_version: current_version.to_string(),
            updated: None,
            latest_version: None,
            apply_mode: None,
            package: None,
            rpm_version_before: None,
            rpm_version_after: None,
            endpoint: endpoint_without_credentials(endpoint_url),
            sensitive_urls: vec![endpoint_url.to_string()],
        }
    }
}

/// Effect-free or applied result returned by the dependency-injected runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands::tier1::update) enum SelfUpdateExecution {
    /// The release manifest matches the running version.
    AlreadyLatest {
        /// Version shared by the binary and manifest.
        version: String,
    },
    /// A newer version exists and Plan intent stopped before all effects.
    Preview {
        /// Version currently running.
        from: String,
        /// Version advertised by the release manifest.
        to: String,
    },
    /// One binary or package-manager update path completed.
    Applied(SelfUpdateApplied),
}

/// Host operations used by self-update application tests.
pub(crate) trait SelfUpdateOps {
    /// Fetches and compares the release manifest.
    fn check_update(
        &self,
        endpoint_url: &str,
        current_version: &str,
    ) -> Result<Option<core_self_update::ReleaseManifest>, core_self_update::SelfUpdateError>;

    /// Resolves the running executable only after Apply intent is established.
    fn resolve_current_exe(&self) -> Result<PathBuf, core_self_update::SelfUpdateError>;

    /// Applies the existing verified binary replacement path.
    fn perform_binary_update(
        &self,
        artifact: &core_self_update::ReleaseArtifact,
        current_exe: &Path,
        on_progress: Option<&ProgressFn>,
    ) -> Result<(), core_self_update::SelfUpdateError>;
}

struct SystemSelfUpdateOps;

impl SelfUpdateOps for SystemSelfUpdateOps {
    fn check_update(
        &self,
        endpoint_url: &str,
        current_version: &str,
    ) -> Result<Option<core_self_update::ReleaseManifest>, core_self_update::SelfUpdateError> {
        core_self_update::check_update(endpoint_url, current_version)
    }

    fn resolve_current_exe(&self) -> Result<PathBuf, core_self_update::SelfUpdateError> {
        core_self_update::resolve_current_exe()
    }

    fn perform_binary_update(
        &self,
        artifact: &core_self_update::ReleaseArtifact,
        current_exe: &Path,
        on_progress: Option<&ProgressFn>,
    ) -> Result<(), core_self_update::SelfUpdateError> {
        core_self_update::perform_update(artifact, current_exe, on_progress)
    }
}

#[allow(clippy::too_many_arguments)]
/// Runs the typed application boundary with injectable host dependencies.
pub(crate) fn run_application_with_deps(
    request: SelfUpdateRequest<'_>,
    ctx: &CliContext,
    ops: &dyn SelfUpdateOps,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    on_progress: Option<&ProgressFn>,
    started_at: &str,
) -> Result<SelfUpdateApplicationOutcome, Box<SelfUpdateApplicationError>> {
    let execution = match run_self_update_with_deps(
        request.endpoint_url,
        request.current_version,
        request.intent,
        ops,
        query,
        txn,
        is_root,
        on_progress,
    ) {
        Ok(execution) => execution,
        Err(failure) => {
            let log_result =
                append_self_update_log(ctx, started_at, request.intent, Err(failure.as_ref()));
            return Err(Box::new(SelfUpdateApplicationError {
                error: failure.error,
                warnings: log_result.warnings,
            }));
        }
    };

    match execution {
        SelfUpdateExecution::AlreadyLatest { version } => {
            Ok(SelfUpdateApplicationOutcome::AlreadyLatest { version })
        }
        SelfUpdateExecution::Preview { from, to } => {
            Ok(SelfUpdateApplicationOutcome::Preview { from, to })
        }
        SelfUpdateExecution::Applied(result) => {
            Ok(finish_applied(ctx, started_at, request.intent, &result))
        }
    }
}

fn finish_applied(
    ctx: &CliContext,
    started_at: &str,
    intent: ExecutionIntent,
    result: &SelfUpdateApplied,
) -> SelfUpdateApplicationOutcome {
    let execution = SelfUpdateExecution::Applied(result.clone());
    let log_result = append_self_update_log(ctx, started_at, intent, Ok(&execution));
    let change = result.change();
    SelfUpdateApplicationOutcome::Applied {
        result: result.clone(),
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            log_result.operation_id,
            vec![change],
            log_result.warnings,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
/// Runs manifest selection and the chosen apply path without audit projection.
pub(crate) fn run_self_update_with_deps(
    endpoint_url: &str,
    current_version: &str,
    intent: ExecutionIntent,
    ops: &dyn SelfUpdateOps,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    on_progress: Option<&ProgressFn>,
) -> Result<SelfUpdateExecution, Box<SelfUpdateFailure>> {
    let mut context = SelfUpdateFailureContext::new(current_version, endpoint_url);
    match run_self_update_inner(
        endpoint_url,
        current_version,
        intent,
        ops,
        query,
        txn,
        is_root,
        on_progress,
        &mut context,
    ) {
        Ok(run) => Ok(run),
        Err(error) => Err(Box::new(SelfUpdateFailure { error, context })),
    }
}

#[expect(clippy::too_many_arguments)]
fn run_self_update_inner(
    endpoint_url: &str,
    current_version: &str,
    intent: ExecutionIntent,
    ops: &dyn SelfUpdateOps,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    on_progress: Option<&ProgressFn>,
    context: &mut SelfUpdateFailureContext,
) -> Result<SelfUpdateExecution, CliError> {
    let manifest = match ops
        .check_update(endpoint_url, current_version)
        .map_err(self_update_cli_err)?
    {
        None => {
            return Ok(SelfUpdateExecution::AlreadyLatest {
                version: current_version.to_string(),
            });
        }
        Some(manifest) => manifest,
    };
    context.latest_version = Some(manifest.version.clone());

    let os = core_self_update::current_os();
    let arch = core_self_update::current_arch();
    let artifact = manifest
        .artifact_for(os, arch)
        .ok_or_else(|| core_self_update::SelfUpdateError::NoArtifact {
            os: os.to_string(),
            arch: arch.to_string(),
        })
        .map_err(self_update_cli_err)?;
    context.sensitive_urls.push(artifact.url.clone());

    if intent == ExecutionIntent::Plan {
        return Ok(SelfUpdateExecution::Preview {
            from: current_version.to_string(),
            to: manifest.version,
        });
    }

    let current_exe = ops.resolve_current_exe().map_err(self_update_cli_err)?;
    let applied = if let Some(package) = rpm_owner_for_current_exe(query, &current_exe)? {
        context.apply_mode = Some("rpm_package");
        context.package = Some(package.clone());
        if !is_root {
            return Err(CliError::Runtime {
                command: "update self".to_string(),
                reason: format!(
                    "updating RPM-owned anolisa package '{package}' requires root privileges; re-run with sudo: `sudo anolisa update self`"
                ),
            });
        }
        let before_version = installed_package_version_best_effort(query, &package);
        context.rpm_version_before = before_version.clone();
        if let Err(err) = txn.update(&[package.as_str()]) {
            context.rpm_version_after = installed_package_version_best_effort(query, &package);
            context.updated = match (&context.rpm_version_before, &context.rpm_version_after) {
                (Some(before), Some(after)) => Some(before != after),
                _ => None,
            };
            return Err(txn_err(err, "update self"));
        }
        let after_version = installed_package_version_best_effort(query, &package);
        SelfUpdateApplied::RpmPackage {
            from: current_version.to_string(),
            to: manifest.version,
            package,
            before_version,
            after_version,
        }
    } else {
        context.apply_mode = Some("binary");
        ops.perform_binary_update(artifact, &current_exe, on_progress)
            .map_err(self_update_cli_err)?;
        SelfUpdateApplied::Binary {
            from: current_version.to_string(),
            to: manifest.version,
        }
    };

    Ok(SelfUpdateExecution::Applied(applied))
}

fn txn_err(err: PackageTransactionError, command: &str) -> CliError {
    match err {
        PackageTransactionError::CommandMissing { .. } => CliError::Runtime {
            command: command.to_string(),
            reason: "RPM transaction tool not found: cannot update an RPM-owned package. Install yum or dnf and retry".to_string(),
        },
        PackageTransactionError::PermissionDenied { command: bin } => {
            common::package_permission_error(command, &bin, "update")
        }
        PackageTransactionError::TransactionFailed { code, stderr, .. } => {
            common::package_transaction_failed_error(command, "update", code, &stderr)
        }
    }
}

fn installed_package_version_best_effort(
    query: &dyn PackageQuery,
    package: &str,
) -> Option<String> {
    query
        .query_installed(package)
        .ok()
        .flatten()
        .map(|info| info.version.to_string())
}

fn self_update_cli_err(error: core_self_update::SelfUpdateError) -> CliError {
    CliError::Runtime {
        command: "update self".to_string(),
        reason: error.to_string(),
    }
}

fn rpm_owner_for_current_exe(
    query: &dyn PackageQuery,
    current_exe: &Path,
) -> Result<Option<String>, CliError> {
    let capability = current_exe.to_str().ok_or_else(|| CliError::Runtime {
        command: "update self".to_string(),
        reason: format!(
            "current executable path is not valid UTF-8: {}",
            current_exe.display()
        ),
    })?;

    match query.what_provides_installed(capability) {
        Ok(packages) => match packages.as_slice() {
            [] => Ok(None),
            [package] => Ok(Some(package.clone())),
            _ => Err(CliError::Runtime {
                command: "update self".to_string(),
                reason: format!(
                    "current executable '{}' is provided by multiple RPM packages ({}); refusing to choose one for self-update",
                    current_exe.display(),
                    packages.join(", ")
                ),
            }),
        },
        Err(PackageQueryError::CommandMissing { .. }) => Ok(None),
        Err(error) => Err(CliError::Runtime {
            command: "update self".to_string(),
            reason: format!(
                "cannot determine whether current executable '{}' is RPM-owned: {error}",
                current_exe.display()
            ),
        }),
    }
}

#[derive(Serialize)]
struct SelfUpdateAuditDetails {
    current_version: String,
    latest_version: String,
    update_available: bool,
    updated: bool,
    apply_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm_version_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm_version_after: Option<String>,
}

fn build_audit_details(execution: &SelfUpdateExecution) -> SelfUpdateAuditDetails {
    let (
        current_version,
        latest_version,
        update_available,
        updated,
        apply_mode,
        package,
        rpm_version_before,
        rpm_version_after,
    ) = match execution {
        SelfUpdateExecution::AlreadyLatest { version } => (
            version.clone(),
            version.clone(),
            false,
            false,
            "none",
            None,
            None,
            None,
        ),
        SelfUpdateExecution::Preview { from, to } => (
            from.clone(),
            to.clone(),
            true,
            false,
            "none",
            None,
            None,
            None,
        ),
        SelfUpdateExecution::Applied(SelfUpdateApplied::Binary { from, to }) => (
            from.clone(),
            to.clone(),
            true,
            true,
            "binary",
            None,
            None,
            None,
        ),
        SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
            from,
            to,
            package,
            before_version,
            after_version,
        }) => (
            from.clone(),
            to.clone(),
            true,
            before_version
                .as_ref()
                .zip(after_version.as_ref())
                .is_some_and(|(before, after)| before != after),
            "rpm_package",
            Some(package.clone()),
            before_version.clone(),
            after_version.clone(),
        ),
    };

    SelfUpdateAuditDetails {
        current_version,
        latest_version,
        update_available,
        updated,
        apply_mode,
        package,
        rpm_version_before,
        rpm_version_after,
    }
}

#[derive(Default)]
/// Result of the best-effort central-log append.
pub(crate) struct SelfUpdateLogResult {
    /// Durable ID only when the central-log append succeeded.
    pub(crate) operation_id: Option<String>,
    /// Non-terminal audit diagnostics emitted by the command renderer.
    pub(crate) warnings: Vec<String>,
}

/// Appends one applied or failed self-update audit unless intent is Plan.
pub(crate) fn append_self_update_log(
    ctx: &CliContext,
    started_at: &str,
    intent: ExecutionIntent,
    outcome: Result<&SelfUpdateExecution, &SelfUpdateFailure>,
) -> SelfUpdateLogResult {
    if intent == ExecutionIntent::Plan {
        return SelfUpdateLogResult::default();
    }

    let (severity, status, message, objects, details) = match outcome {
        Ok(execution) => {
            let data = build_audit_details(execution);
            let (message, objects) = match execution {
                SelfUpdateExecution::AlreadyLatest { .. } | SelfUpdateExecution::Preview { .. } => {
                    return SelfUpdateLogResult::default();
                }
                SelfUpdateExecution::Applied(SelfUpdateApplied::Binary { .. }) => (
                    format!(
                        "updated the anolisa CLI binary {} → {}",
                        data.current_version, data.latest_version
                    ),
                    Vec::new(),
                ),
                SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
                    package,
                    before_version,
                    after_version,
                    ..
                }) => (
                    format!(
                        "delegated the anolisa CLI self-update to dnf package '{package}'; \
                         installed RPM version {} → {} (release manifest advertises {})",
                        before_version.as_deref().unwrap_or("unknown"),
                        after_version.as_deref().unwrap_or("unconfirmed"),
                        data.latest_version,
                    ),
                    vec![package.clone()],
                ),
            };
            (
                Severity::Info,
                LogStatus::Ok,
                message,
                objects,
                serde_json::to_value(&data).unwrap_or_default(),
            )
        }
        Err(failure) => (
            Severity::Error,
            LogStatus::Failed,
            format!(
                "anolisa CLI self-update failed: {}",
                redact_known_urls(&failure.error.reason(), &failure.context.sensitive_urls)
            ),
            failure.context.package.clone().into_iter().collect(),
            serde_json::to_value(&failure.context).unwrap_or_default(),
        ),
    };

    let operation_id = mint_operation_id("update-self");
    let layout = common::resolve_layout(ctx);
    let log = CentralLog::open(layout.central_log.clone());
    let record = LogRecord {
        kind: LogKind::Operation,
        operation_id: Some(operation_id.clone()),
        command: "update self".to_string(),
        source: "anolisa-cli".to_string(),
        component: None,
        severity,
        message,
        actor: "cli".to_string(),
        install_mode: Some(ctx.install_mode.as_str().to_string()),
        started_at: started_at.to_string(),
        finished_at: Some(now_iso8601()),
        status: Some(status),
        objects,
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details,
    };
    match log.append(&record) {
        Ok(()) => SelfUpdateLogResult {
            operation_id: Some(operation_id),
            warnings: Vec::new(),
        },
        Err(error) => SelfUpdateLogResult {
            operation_id: None,
            warnings: vec![format!("failed to write central log: {error}")],
        },
    }
}

const REDACTED: &str = "<redacted>";

/// Removes known URLs and withholds text containing any unverified URL.
pub(crate) fn redact_known_urls(text: &str, urls: &[String]) -> String {
    let mut out = text.to_string();
    for url in urls {
        out = redact_url_runs(&out, url);
    }
    if out.contains("://") {
        return "the failure text was withheld: it carried a URL that could not be \
                shown to be free of credentials"
            .to_string();
    }
    out
}

fn redact_url_runs(text: &str, url: &str) -> String {
    if url.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(url) {
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        let matched = &rest[at..];
        let end = matched
            .find(char::is_whitespace)
            .unwrap_or(matched.len())
            .max(url.len());
        rest = &matched[end..];
    }
    out.push_str(rest);
    out
}

/// Keeps only scheme and authority because paths and queries may carry secrets.
fn endpoint_without_credentials(url: &str) -> Option<String> {
    let sep = url.find("://")?;
    let remainder = &url[sep + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let (authority, tail) = remainder.split_at(authority_end);
    if tail.contains('@') {
        return None;
    }
    let host = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    if host.is_empty() {
        return None;
    }
    Some(format!("{}{host}", &url[..sep + 3]))
}
