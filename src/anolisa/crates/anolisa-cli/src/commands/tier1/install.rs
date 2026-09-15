//! `anolisa install` — install a component through a configured backend.
//!
//! The single-component handler maps CLI input into the lifecycle application
//! layer and renders its typed outcome. Batch orchestration remains separate.
//!
//! The **raw** family resolves an artifact from the distribution index and
//! places it through the owned executor: sha256-verified download, install
//! contract, files, capabilities, hooks, services, and an owned record. The
//! **rpm** family delegates one `dnf install` transaction and records the
//! component as delegated-managed (ANOLISA owns the removal). A component
//! already present as an unmanaged system RPM is never adopted implicitly —
//! install refuses and points at `anolisa adopt`. Other configured backends
//! (`npm`, …) remain NOT_IMPLEMENTED.
//!
//! Deliberately out of scope for this milestone: execution-policy gating and
//! health checks.

use clap::Parser;
use serde::Serialize;

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::planner::NoOpReason;

use crate::context::CliContext;
use crate::progress;
use crate::response::{CliError, render_json};

mod application;
use self::application::{InstallApplicationOutcome, InstallSubject};

mod rpm;
pub(crate) use rpm::*;

mod dispatch;
pub(crate) use dispatch::*;

mod batch;
pub(crate) use batch::*;

mod raw;
pub(crate) use raw::*;

mod io_util;
mod render;
pub(crate) use io_util::*;

mod owned_ops;
pub(crate) use owned_ops::*;

// `pub(crate)` so sibling commands exercising the raw pipeline (repair's
// owned replay) can reuse the local-repo fixtures.
#[cfg(test)]
pub(crate) mod tests;

const COMMAND: &str = "install";
const ANOLISA_RPM_REPO_ID: &str = "anolisa-configured";

#[derive(Debug, Parser)]
// `--version` here means the *component* version (the `cargo install`
// convention), so the auto-generated CLI-version flag must be disabled
// to free the name. `anolisa --version` still works at the top level.
#[command(disable_version_flag = true)]
#[command(group(
    clap::ArgGroup::new("target")
        .required(true)
        .args(["component", "all"]),
))]
pub struct InstallArgs {
    /// Component name to install
    #[arg(value_name = "COMPONENT")]
    pub component: Option<String>,
    /// Install every component in the component index (mutually exclusive with COMPONENT)
    #[arg(long, conflicts_with_all = ["component", "version", "package"])]
    pub all: bool,
    /// With --all, stop on the first failure instead of continuing
    #[arg(long, requires = "all")]
    pub fail_fast: bool,
    /// Install a specific version instead of the latest in the channel
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Backend override (raw | rpm | npm); defaults to repo.toml default_backend
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    /// One-off base_url override for the selected backend
    #[arg(long, value_name = "URL")]
    pub repo: Option<String>,
    /// Override the backend-native package name for the component
    #[arg(long, value_name = "NAME")]
    pub package: Option<String>,
}

mod types;
// Re-export shared types for external consumers (update.rs, adopt.rs, etc.)
// and for tests accessing via `super::*`.
pub(crate) use types::*;

mod provision;

pub fn handle(args: InstallArgs, ctx: &CliContext) -> Result<(), CliError> {
    if args.fail_fast && !args.all {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: "--fail-fast is only meaningful with --all".to_string(),
        });
    }
    if args.all {
        return handle_all(args, ctx);
    }
    // clap ArgGroup guarantees at least one of `component` / `--all`; with
    // `--all` ruled out above, `component` is necessarily Some.
    let component = args
        .component
        .clone()
        .expect("clap ArgGroup ensures component is set when --all is absent");
    handle_one(component, args, ctx).map(|_| ())
}

/// Run and render one component while preserving the legacy batch classification.
pub(crate) fn handle_one(
    component: String,
    args: InstallArgs,
    ctx: &CliContext,
) -> Result<InstallOutcome, CliError> {
    let outcome = application::run(
        application::InstallRequest {
            component: &component,
            args: &args,
            intent: execution_intent(ctx),
        },
        ctx,
    )?;
    let batch_outcome = outcome.batch_outcome();
    render_outcome(ctx, outcome)?;
    Ok(batch_outcome)
}

fn execution_intent(ctx: &CliContext) -> ExecutionIntent {
    if ctx.dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

/// Test entry with package backends injected so no live rpmdb/dnf is used.
#[cfg(test)]
pub(crate) fn install_component_with_deps(
    input: &str,
    args: &InstallArgs,
    ctx: &CliContext,
    query: &dyn anolisa_platform::pkg_query::PackageQuery,
    txn: &dyn anolisa_platform::pkg_transaction::PackageTransaction,
    is_root: bool,
) -> Result<InstallOutcome, CliError> {
    let env = anolisa_env::EnvService::detect();
    install_component_with_deps_and_env(
        input,
        args,
        ctx,
        &env,
        &RpmdbProbe::absent(),
        query,
        txn,
        is_root,
    )
}

/// Test entry with host facts and package backends injected.
#[cfg(test)]
#[expect(clippy::too_many_arguments)]
pub(crate) fn install_component_with_deps_and_env(
    input: &str,
    args: &InstallArgs,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn anolisa_platform::pkg_query::PackageQuery,
    txn: &dyn anolisa_platform::pkg_transaction::PackageTransaction,
    is_root: bool,
) -> Result<InstallOutcome, CliError> {
    let mut activity = crate::progress::Activity::start(
        crate::progress::feedback_for_stderr(ctx.json, ctx.quiet),
        &format!("Preparing to install {input}..."),
    );
    let outcome = application::run_with_dependencies(
        application::InstallRequest {
            component: input,
            args,
            intent: execution_intent(ctx),
        },
        ctx,
        env,
        rpmdb,
        query,
        txn,
        is_root,
        &std::collections::HashSet::new(),
        &mut activity,
        crate::test_support::raw_effects(),
    )?;
    let batch_outcome = outcome.batch_outcome();
    render_outcome(ctx, outcome)?;
    Ok(batch_outcome)
}

fn render_outcome(ctx: &CliContext, outcome: InstallApplicationOutcome) -> Result<(), CliError> {
    let payload = match outcome {
        InstallApplicationOutcome::NoOp { subject, reason } => {
            debug_assert!(matches!(
                reason,
                NoOpReason::AlreadyInstalled | NoOpReason::AlreadyTracked
            ));
            InstallResultPayload::from_subject(
                subject,
                "already-installed",
                ctx.dry_run,
                Vec::new(),
            )
        }
        InstallApplicationOutcome::Preview {
            subject,
            steps,
            warnings,
        } => {
            for warning in warnings {
                progress::suspend_output(|| eprintln!("warning: {warning}"));
            }
            InstallResultPayload::from_subject(
                subject,
                "planned",
                true,
                steps.iter().map(step_label).collect(),
            )
        }
        InstallApplicationOutcome::Applied {
            subject,
            steps,
            outcome,
        } => {
            for warning in outcome.warnings() {
                progress::suspend_output(|| eprintln!("warning: {warning}"));
            }
            let mut payload = InstallResultPayload::from_subject(
                subject,
                "installed",
                false,
                steps.iter().map(step_label).collect(),
            );
            payload.operation_id = outcome.operation_id().map(str::to_string);
            payload
        }
    };
    render_result(ctx, &payload)
}

/// JSON payload for a completed, previewed, or idempotent install.
///
/// Version-pin fields are additive and backend-specific: delegated installs
/// report the resolved EVR and NEVRA, while raw installs report the distribution
/// version and artifact URL. `version` retains the effective installed or
/// resolved version across both routes. Optional evidence is omitted instead of
/// serialized as `null` to preserve the established sparse wire envelope.
#[derive(Debug, Serialize)]
struct InstallResultPayload {
    component: String,
    /// Provider-native package identity, when resolution produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    /// Effective installed or resolved version kept for wire compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    backend: String,
    /// `installed` | `planned` (dry-run) | `already-installed`.
    action: &'static str,
    /// Durable operation identifier, present only after an applied install.
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    /// Exact `--version` value supplied for a version-pinned install.
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_version: Option<String>,
    /// Resolved EVR for delegated installs or distribution version for raw.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_version: Option<String>,
    /// DNF repository id or raw repository base URL that supplied the pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_repo: Option<String>,
    /// Resolved NEVRA for delegated installs or artifact URL for raw.
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
    dry_run: bool,
    plan: Vec<String>,
}

impl InstallResultPayload {
    fn from_subject(
        subject: InstallSubject,
        action: &'static str,
        dry_run: bool,
        plan: Vec<String>,
    ) -> Self {
        Self {
            component: subject.component,
            package: subject.package,
            version: subject.version,
            backend: subject.backend,
            action,
            operation_id: None,
            requested_version: subject.requested_version,
            resolved_version: subject.resolved_version,
            source_repo: subject.source_repo,
            artifact: subject.artifact,
            dry_run,
            plan,
        }
    }

    #[cfg(test)]
    fn with_pin(mut self, pin: &DelegatedPin) -> Self {
        self.requested_version = Some(pin.requested_version.clone());
        self.resolved_version = Some(pin.resolved_evr.clone());
        self.source_repo.clone_from(&pin.source_repo);
        self.artifact = Some(pin.artifact.clone());
        if self.version.is_none() {
            self.version = Some(pin.resolved_version.clone());
        }
        self
    }
}

fn dry_run_detail_lines(payload: &InstallResultPayload) -> Vec<String> {
    let mut lines = Vec::new();
    if payload.artifact.is_some() {
        if let Some(package) = &payload.package {
            lines.push(format!("package: {package}"));
        }
        if let Some(requested) = &payload.requested_version {
            lines.push(format!("requested version: {requested}"));
        }
        if let Some(resolved) = &payload.resolved_version {
            lines.push(format!("resolved version: {resolved}"));
        }
        if let Some(artifact) = &payload.artifact {
            lines.push(format!("artifact: {artifact}"));
        }
        if let Some(repo) = &payload.source_repo {
            lines.push(format!("repository: {repo}"));
        }
    }
    lines
}

fn render_result(ctx: &CliContext, payload: &InstallResultPayload) -> Result<(), CliError> {
    if ctx.json {
        return render_json(COMMAND, payload);
    }
    if ctx.quiet {
        return Ok(());
    }
    if payload.dry_run {
        println!("install {} (dry-run):", payload.component);
        for line in dry_run_detail_lines(payload) {
            println!("  {line}");
        }
        for label in &payload.plan {
            println!("  - {label}");
        }
        return Ok(());
    }
    match (payload.action, &payload.version) {
        ("already-installed", Some(version)) => {
            println!("{} {version} is already installed", payload.component);
        }
        ("already-installed", None) => println!("{} is already installed", payload.component),
        (_, Some(version)) => println!("installed {} {version}", payload.component),
        (_, None) => println!("installed {}", payload.component),
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::tests::*;
    use super::*;
    use clap::Parser;

    #[test]
    fn install_cli_rejects_multiple_components() {
        let err = InstallArgs::try_parse_from(["install", "agentsight", "tokenless"])
            .expect_err("must reject extra positional arguments");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn install_all_and_component_are_mutually_exclusive() {
        let err = InstallArgs::try_parse_from(["install", "--all", "tokenless"])
            .expect_err("must reject --all with positional");
        assert!(
            err.kind() == clap::error::ErrorKind::ArgumentConflict
                || err.to_string().contains("cannot be used with")
        );
    }

    #[test]
    fn install_all_conflicts_with_package() {
        let err = InstallArgs::try_parse_from(["install", "--all", "--package", "foo"])
            .expect_err("must reject --all with --package");
        assert!(
            err.kind() == clap::error::ErrorKind::ArgumentConflict
                || err.to_string().contains("cannot be used with")
        );
    }

    #[test]
    fn install_all_conflicts_with_version() {
        let err = InstallArgs::try_parse_from(["install", "--all", "--version", "1.0.0"])
            .expect_err("must reject --all with --version");
        assert!(
            err.kind() == clap::error::ErrorKind::ArgumentConflict
                || err.to_string().contains("cannot be used with")
        );
    }

    #[test]
    fn install_fail_fast_without_all_is_rejected() {
        // clap still parses it (ArgGroup + requires limitation), but
        // handle() now rejects at runtime.
        let a = InstallArgs::try_parse_from(["install", "tokenless", "--fail-fast"])
            .expect("clap allows this parse");
        assert!(!a.all);
        assert!(a.fail_fast);

        let ctx = ctx_with_prefix(false, None);
        let err = handle(a, &ctx).expect_err("handle should reject --fail-fast without --all");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn install_all_parses_successfully() {
        let a = InstallArgs::try_parse_from(["install", "--all"]).expect("should parse");
        assert!(a.all);
        assert!(a.component.is_none());
    }

    #[test]
    fn install_all_with_fail_fast_parses_successfully() {
        let a =
            InstallArgs::try_parse_from(["install", "--all", "--fail-fast"]).expect("should parse");
        assert!(a.all);
        assert!(a.fail_fast);
    }
}
