//! `anolisa adapter` sub-surface: scan, enable, disable, status.
//!
//! Adapters bridge ANOLISA-managed components into agent frameworks
//! (e.g. `tokenless/openclaw`). Component install lays each adapter's
//! resources under `{datadir}/adapters/<component>/<framework>/`; this
//! surface drives the framework side from those resources via the
//! [`AdapterManager`], which owns the install lock, receipts, and the
//! controlled framework-CLI boundary.
//!
//! ## `adapter scan`
//!
//! Read-only: reads installed component manifests, then overlays local
//! resource-directory, framework-detection, and receipt state.
//!
//! ## `adapter enable <component> [<framework>]`
//!
//! Runs the framework driver's enable (e.g. registers an OpenClaw plugin)
//! and writes a receipt. `--dry-run` (global flag) reports the plan
//! without touching framework state. `<framework>` may be omitted when the
//! component ships adapters for exactly one framework.
//!
//! ## `adapter disable <component> [<framework>]`
//!
//! Reverses enable using the receipt only, then removes it. Idempotent:
//! disabling something not enabled is a successful no-op. If cleanup
//! cannot complete, the receipt is kept (marked `cleanup_failed`) and the
//! command exits degraded so the state is not silently lost.
//!
//! ## `adapter status [<component>]`
//!
//! Read-only: reports each receipt's health summary and the individual
//! conditions behind it. Verification that cannot run reports `unknown`
//! rather than a faked healthy/absent verdict.

use clap::{ArgAction, Parser, Subcommand};
use serde::Serialize;

use anolisa_core::adapter::AdapterError;
use anolisa_core::adapter::claim::{AdapterClaim, ClaimStatus};
use anolisa_core::adapter::driver::{AdapterStatusReport, DriverPlan};
use anolisa_core::adapter::manager::{
    AdapterManager, AdapterSourceStatus, EnableOptions, ScanEntry, ScanReport, StatusReport,
};
use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::manifest::{AdapterNotice, NoticeLevel, NoticeWhen};

use crate::commands::common;
use crate::context::CliContext;
use crate::response::{CliError, render_json};

use self::application::{
    AdapterApplicationOutcome, AdapterApplied, AdapterPreview, AdapterRequest,
};

mod application;

/// CLI arguments for the `adapter` sub-surface.
#[derive(Parser)]
pub struct AdapterArgs {
    /// Adapter subcommand.
    #[command(subcommand)]
    pub command: AdapterCommands,
}

/// Subcommands under `anolisa adapter`.
#[derive(Subcommand)]
pub enum AdapterCommands {
    /// Discover installed adapter declarations and local resource/receipt state.
    Scan,
    /// Enable a component's adapter for a framework.
    Enable {
        /// Component name (e.g. `tokenless`).
        component: String,
        /// Target framework (e.g. `openclaw`). Omit when the component
        /// ships adapters for exactly one framework.
        framework: Option<String>,
        /// Explicit safety bypass: authorize an unsafe plugin install.
        ///
        /// This is an explicit security-bypass authorization. When set,
        /// ANOLISA is permitted to pass the OpenClaw framework's own
        /// unsafe-install flag if the installed OpenClaw advertises it as
        /// effective. Releases where that flag is a deprecated no-op are
        /// rejected and require operator-owned `security.installPolicy`
        /// configuration instead.
        /// Only valid for an OpenClaw plugin adapter; using it with any
        /// other framework or a skill-only adapter is rejected. Off by
        /// default; a normal install never bypasses OpenClaw's checks.
        #[arg(long)]
        allow_unsafe_plugin_install: bool,
        /// dsh profile to enable. Repeat for multiple profiles; required
        /// when the selected framework is profile-scoped.
        #[arg(long = "profile", action = ArgAction::Append)]
        profiles: Vec<String>,
    },
    /// Disable a previously enabled adapter.
    Disable {
        /// Component name.
        component: String,
        /// Target framework. Omit to disable the component's single
        /// enabled adapter.
        framework: Option<String>,
    },
    /// Report adapter receipt status.
    Status {
        /// Limit to one component; omit for all receipts.
        component: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// JSON payloads
// ---------------------------------------------------------------------------

/// One row of `adapter scan` JSON output.
#[derive(Serialize)]
struct ScanRow {
    component: String,
    framework: String,
    declared: bool,
    resource_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_root: Option<String>,
    driver_available: bool,
    framework_detected: bool,
    /// The `adapter_type` declared in the component manifest, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter_type: Option<String>,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim_status: Option<ClaimStatus>,
    /// Receipt source health. Present only when a receipt exists, so JSON
    /// consumers can distinguish a disabled declaration from an orphaned
    /// enabled receipt whose source component disappeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_status: Option<&'static str>,
    /// Human-readable explanation for [`Self::source_status`] when the source
    /// is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_reason: Option<String>,
}

/// `adapter scan` JSON output.
#[derive(Serialize)]
struct ScanPayload {
    adapters: Vec<ScanRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// One notice in `--json` output. A stable shape: `when` and `level` are
/// always present (never elided for defaults) so consumers can rely on the
/// field layout; `command` is optional display-only text.
#[derive(Serialize)]
struct NoticeRow {
    when: NoticeWhen,
    level: NoticeLevel,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

impl From<&AdapterNotice> for NoticeRow {
    fn from(notice: &AdapterNotice) -> Self {
        NoticeRow {
            when: notice.when,
            level: notice.level,
            text: notice.text.clone(),
            command: notice.command.clone(),
        }
    }
}

/// `adapter enable` JSON output.
#[derive(Serialize)]
struct EnablePayload {
    component: String,
    framework: String,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<DriverPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim: Option<AdapterClaim>,
    /// Notices displayed (real enable) or that would be displayed
    /// (dry-run). Always present; empty when the adapter declares none.
    notices: Vec<NoticeRow>,
}

/// `adapter disable` JSON output.
#[derive(Serialize)]
struct DisablePayload {
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    framework: Option<String>,
    dry_run: bool,
    claim_removed: bool,
    cleanup_complete: bool,
    messages: Vec<String>,
    /// Notices displayed (real disable) or that would be displayed
    /// (dry-run). Always present; empty when none apply.
    notices: Vec<NoticeRow>,
}

/// One row of `adapter status` JSON output.
#[derive(Serialize)]
struct StatusRow {
    component: String,
    framework: String,
    #[serde(flatten)]
    report: AdapterStatusReport,
}

/// `adapter status` JSON output.
#[derive(Serialize)]
struct StatusPayload {
    receipts: Vec<StatusRow>,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Entry point for `anolisa adapter`.
pub fn handle(args: AdapterArgs, ctx: &CliContext) -> Result<(), CliError> {
    match args.command {
        AdapterCommands::Scan => handle_scan(ctx),
        AdapterCommands::Enable {
            component,
            framework,
            allow_unsafe_plugin_install,
            profiles,
        } => handle_enable(
            ctx,
            &component,
            framework.as_deref(),
            allow_unsafe_plugin_install,
            profiles,
        ),
        AdapterCommands::Disable {
            component,
            framework,
        } => handle_disable(ctx, &component, framework.as_deref()),
        AdapterCommands::Status { component } => handle_status(ctx, component.as_deref()),
    }
}

/// Build a manager for the active layout.
fn build_manager(ctx: &CliContext) -> AdapterManager {
    common::build_adapter_manager(ctx)
}

// ---------------------------------------------------------------------------
// scan
// ---------------------------------------------------------------------------

fn handle_scan(ctx: &CliContext) -> Result<(), CliError> {
    const COMMAND: &str = "adapter scan";
    let manager = build_manager(ctx);
    let report: ScanReport = manager.scan().map_err(|e| map_err(COMMAND, e))?;

    if ctx.json {
        let adapters = report.entries.iter().map(scan_row_from_entry).collect();
        return render_json(
            COMMAND,
            ScanPayload {
                adapters,
                warnings: report.warnings,
            },
        );
    }

    // Warnings, the empty-state line, and the table are all non-error
    // output. Gate the whole human report so `--quiet` matches `list`.
    if ctx.quiet {
        return Ok(());
    }

    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }

    if report.entries.is_empty() {
        println!("No adapter declarations or resources found.");
        return Ok(());
    }
    println!(
        "{:<16} {:<10} {:<14} {:<9} {:<9} {:<9} {:<8} {:<9} STATE",
        "COMPONENT", "FRAMEWORK", "TYPE", "DECLARED", "RESOURCE", "DRIVER", "DETECTED", "SOURCE"
    );
    for row in &report.entries {
        println!(
            "{:<16} {:<10} {:<14} {:<9} {:<9} {:<9} {:<8} {:<9} {}",
            row.component,
            row.framework,
            row.adapter_type.as_deref().unwrap_or("-"),
            yes_no(row.declared),
            if row.resource_root.is_some() {
                "present"
            } else {
                "missing"
            },
            yes_no(row.driver_available),
            yes_no(row.framework_detected),
            source_status_label(row.source_status),
            scan_state_label(row),
        );
        if let Some(reason) = &row.source_reason {
            println!("  source: {reason}");
        }
    }
    Ok(())
}

fn scan_row_from_entry(row: &ScanEntry) -> ScanRow {
    ScanRow {
        component: row.component.clone(),
        framework: row.framework.clone(),
        declared: row.declared,
        resource_present: row.resource_root.is_some(),
        resource_root: row
            .resource_root
            .as_ref()
            .map(|path| path.display().to_string()),
        driver_available: row.driver_available,
        framework_detected: row.framework_detected,
        adapter_type: row.adapter_type.clone(),
        enabled: row.enabled,
        claim_status: row.claim_status,
        source_status: row.source_status.map(AdapterSourceStatus::label),
        source_reason: row.source_reason.clone(),
    }
}

fn scan_state_label(row: &ScanEntry) -> &'static str {
    match (row.enabled, row.claim_status, row.source_status) {
        (true, Some(ClaimStatus::CleanupFailed), _) => "cleanup_failed",
        (true, _, Some(AdapterSourceStatus::Missing)) => "orphaned",
        (true, Some(ClaimStatus::Enabled), _) => "enabled",
        (true, None, _) => "enabled",
        (false, _, _) => "-",
    }
}

fn source_status_label(status: Option<AdapterSourceStatus>) -> &'static str {
    status.map(AdapterSourceStatus::label).unwrap_or("-")
}

// ---------------------------------------------------------------------------
// enable
// ---------------------------------------------------------------------------

fn handle_enable(
    ctx: &CliContext,
    component: &str,
    framework: Option<&str>,
    allow_unsafe_plugin_install: bool,
    profiles: Vec<String>,
) -> Result<(), CliError> {
    const COMMAND: &str = "adapter enable";
    let outcome = application::run(
        AdapterRequest::Enable {
            component,
            framework,
            options: EnableOptions {
                allow_unsafe_plugin_install,
                profiles,
            },
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
    )?;

    match outcome {
        AdapterApplicationOutcome::Preview(AdapterPreview::Enable { plan, notices }) => {
            if ctx.json {
                let payload = EnablePayload {
                    component: plan.component.clone(),
                    framework: plan.framework.clone(),
                    dry_run: true,
                    plan: Some(plan),
                    claim: None,
                    notices: notices.iter().map(NoticeRow::from).collect(),
                };
                return render_json(COMMAND, payload);
            }
            println!(
                "[dry-run] would enable {}/{}:",
                plan.component, plan.framework
            );
            for action in &plan.actions {
                println!("  - {action}");
            }
            if let Some(cmd) = &plan.register_command {
                println!("  command: {cmd}");
            }
            print_notices(&notices, true, ctx.quiet);
            Ok(())
        }
        AdapterApplicationOutcome::Applied {
            result: AdapterApplied::Enable { claim, notices },
            outcome,
        } => {
            match outcome.status() {
                CommandOutcomeStatus::Completed => {}
                CommandOutcomeStatus::Partial { reason } => {
                    return Err(CliError::Degraded {
                        command: COMMAND.to_string(),
                        reason: reason.clone(),
                    });
                }
                CommandOutcomeStatus::Failed { reason } => {
                    return Err(CliError::Runtime {
                        command: COMMAND.to_string(),
                        reason: reason.clone(),
                    });
                }
            }
            if ctx.json {
                let payload = EnablePayload {
                    component: claim.component.clone(),
                    framework: claim.framework.clone(),
                    dry_run: false,
                    plan: None,
                    claim: Some(*claim),
                    notices: notices.iter().map(NoticeRow::from).collect(),
                };
                return render_json(COMMAND, payload);
            }
            println!("Enabled {}/{}.", claim.component, claim.framework);
            if let Some(pid) = &claim.plugin_id {
                println!("  plugin: {pid}");
            }
            print_notices(&notices, false, ctx.quiet);
            Ok(())
        }
        AdapterApplicationOutcome::Preview(AdapterPreview::Disable(_))
        | AdapterApplicationOutcome::Applied {
            result: AdapterApplied::Disable(_),
            ..
        } => unreachable!("an enable request cannot return a disable result"),
    }
}

// ---------------------------------------------------------------------------
// disable
// ---------------------------------------------------------------------------

fn handle_disable(
    ctx: &CliContext,
    component: &str,
    framework: Option<&str>,
) -> Result<(), CliError> {
    const COMMAND: &str = "adapter disable";
    let outcome = application::run(
        AdapterRequest::Disable {
            component,
            framework,
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
    )?;

    match outcome {
        AdapterApplicationOutcome::Preview(AdapterPreview::Disable(outcome)) => {
            if ctx.json {
                let payload = DisablePayload {
                    component: outcome.component.clone(),
                    framework: outcome.framework.clone(),
                    dry_run: true,
                    claim_removed: false,
                    cleanup_complete: outcome.report.cleanup_complete,
                    messages: outcome.report.messages.clone(),
                    notices: outcome.notices.iter().map(NoticeRow::from).collect(),
                };
                return render_json(COMMAND, payload);
            }
            let target = disable_target(&outcome.component, outcome.framework.as_deref());
            println!("[dry-run] would disable {target}:");
            for msg in &outcome.report.messages {
                println!("  - {msg}");
            }
            print_notices(&outcome.notices, true, ctx.quiet);
            Ok(())
        }
        AdapterApplicationOutcome::Applied {
            result: AdapterApplied::Disable(result),
            outcome,
        } => {
            let terminal_error = match outcome.status() {
                CommandOutcomeStatus::Completed => None,
                CommandOutcomeStatus::Partial { reason } => Some(CliError::Degraded {
                    command: COMMAND.to_string(),
                    reason: reason.clone(),
                }),
                CommandOutcomeStatus::Failed { reason } => Some(CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: reason.clone(),
                }),
            };

            if ctx.json {
                if let Some(err) = terminal_error {
                    return Err(err);
                }
                let payload = DisablePayload {
                    component: result.component.clone(),
                    framework: result.framework.clone(),
                    dry_run: false,
                    claim_removed: result.claim_removed,
                    cleanup_complete: result.report.cleanup_complete,
                    messages: result.report.messages.clone(),
                    notices: result.notices.iter().map(NoticeRow::from).collect(),
                };
                return render_json(COMMAND, payload);
            }

            let target = disable_target(&result.component, result.framework.as_deref());
            if result.claim_removed {
                println!("Disabled {target}.");
            } else if result.report.cleanup_complete {
                println!("Nothing to disable for {target}.");
            } else {
                println!("Disable of {target} did not complete cleanly:");
            }
            for msg in &result.report.messages {
                println!("  - {msg}");
            }
            print_notices(&result.notices, false, ctx.quiet);
            terminal_error.map_or(Ok(()), Err)
        }
        AdapterApplicationOutcome::Preview(AdapterPreview::Enable { .. })
        | AdapterApplicationOutcome::Applied {
            result: AdapterApplied::Enable { .. },
            ..
        } => unreachable!("a disable request cannot return an enable result"),
    }
}

/// Print notices for human output. Suppressed entirely under `--quiet`.
/// Under `dry_run` the notices are only a preview of what a real operation
/// would display, so the heading says so. Control characters are escaped to
/// keep untrusted contract or receipt text from altering terminal state.
fn print_notices(notices: &[AdapterNotice], dry_run: bool, quiet: bool) {
    if quiet || notices.is_empty() {
        return;
    }
    if dry_run {
        println!("  would show notices:");
    } else {
        println!("  notices:");
    }
    for notice in notices {
        let level = match notice.level {
            NoticeLevel::Info => "info",
            NoticeLevel::Warning => "warning",
        };
        println!("    [{level}] {}", escape_notice_for_terminal(&notice.text));
        if let Some(command) = &notice.command {
            println!("      command: {}", escape_notice_for_terminal(command));
        }
    }
}

/// Escape control characters while preserving printable Unicode text.
fn escape_notice_for_terminal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// Human-facing `component/framework` label for a disable outcome, falling
/// back to the component alone when no framework was resolved.
fn disable_target(component: &str, framework: Option<&str>) -> String {
    match framework {
        Some(framework) => format!("{component}/{framework}"),
        None => component.to_string(),
    }
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn handle_status(ctx: &CliContext, component: Option<&str>) -> Result<(), CliError> {
    const COMMAND: &str = "adapter status";
    let (component, manager) = match component {
        Some(name) => {
            let (component, view) = common::resolve_adapter_target(name, ctx, COMMAND)?;
            (
                Some(component),
                common::build_adapter_manager_from_view(ctx, &view),
            )
        }
        None => (None, build_manager(ctx)),
    };
    let report: StatusReport = manager
        .status(component.as_deref())
        .map_err(|e| map_err(COMMAND, e))?;

    if ctx.json {
        let receipts = report
            .entries
            .into_iter()
            .map(|e| StatusRow {
                component: e.component,
                framework: e.framework,
                report: e.report,
            })
            .collect();
        return render_json(COMMAND, StatusPayload { receipts });
    }

    if ctx.quiet {
        return Ok(());
    }

    if report.entries.is_empty() {
        println!("No adapter receipts.");
        return Ok(());
    }
    for e in &report.entries {
        println!(
            "{}/{}: {}",
            e.component,
            e.framework,
            summary_label(&e.report.summary),
        );
        for cond in &e.report.conditions {
            let reason = cond
                .reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            println!("  {:?} = {:?}{}", cond.kind, cond.status, reason);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map an [`AdapterError`] to the CLI error model. Input/environment
/// problems are `INVALID_ARGUMENT` (exit 2), an absent component is
/// `NOT_INSTALLED` (also exit 2); machine-side failures (CLI spawn, lock,
/// state/log IO) are `EXECUTION_FAILED` (exit 1).
fn map_err(command: &str, err: AdapterError) -> CliError {
    match err {
        AdapterError::UnknownPlaceholder { .. }
        | AdapterError::RelativeTemplateExpansion { .. }
        | AdapterError::UnknownFramework { .. }
        | AdapterError::AmbiguousFramework { .. }
        | AdapterError::UnsupportedAdapterType { .. }
        | AdapterError::InvalidAdapterInput { .. }
        | AdapterError::AdapterNotDeclared { .. }
        | AdapterError::ResourceRootNotFound { .. }
        | AdapterError::ContractResourceRootNotFound { .. }
        | AdapterError::FrameworkNotDetected { .. }
        | AdapterError::FrameworkVersionMismatch { .. }
        | AdapterError::BundleInvalid { .. }
        | AdapterError::UnsafeInstallNotApplicable { .. }
        | AdapterError::ClaimValidation(_) => CliError::InvalidArgument {
            command: command.to_string(),
            reason: err.to_string(),
        },
        // Same condition the lifecycle commands report as NOT_INSTALLED:
        // the target is absent from state. Routing it anywhere else would
        // make the code depend on which command the caller happened to run.
        AdapterError::ComponentNotInstalled { .. } => CliError::NotInstalled {
            command: command.to_string(),
            reason: err.to_string(),
        },
        AdapterError::AdapterManifest { .. }
        | AdapterError::MissingAdapterManifest { .. }
        | AdapterError::FrameworkCli { .. }
        | AdapterError::ReenableCleanupIncomplete { .. }
        | AdapterError::Lock(_)
        | AdapterError::State(_)
        | AdapterError::Log(_)
        | AdapterError::Io { .. }
        | AdapterError::SettingsUnparseable { .. } => CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        },
    }
}

/// Human-facing one-line label for a status summary.
fn summary_label(summary: &anolisa_core::adapter::driver::AdapterSummary) -> &'static str {
    use anolisa_core::adapter::driver::AdapterSummary::*;
    match summary {
        Healthy => "healthy",
        Degraded => "degraded",
        CleanupFailed => "cleanup_failed",
        Unknown => "unknown",
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so we can parse the adapter subcommand in isolation.
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: AdapterCommands,
    }

    #[test]
    fn global_dry_run_maps_to_execution_intent() {
        assert_eq!(execution_intent(true), ExecutionIntent::Plan);
        assert_eq!(execution_intent(false), ExecutionIntent::Apply);
    }

    #[test]
    fn enable_parses_optional_framework() {
        let cli = TestCli::try_parse_from(["x", "enable", "tokenless"]).expect("parse");
        match cli.command {
            AdapterCommands::Enable {
                component,
                framework,
                allow_unsafe_plugin_install,
                profiles,
            } => {
                assert_eq!(component, "tokenless");
                assert!(framework.is_none());
                assert!(
                    !allow_unsafe_plugin_install,
                    "unsafe install must default to false"
                );
                assert!(profiles.is_empty());
            }
            _ => panic!("expected enable"),
        }

        let cli = TestCli::try_parse_from(["x", "enable", "tokenless", "openclaw"]).expect("parse");
        match cli.command {
            AdapterCommands::Enable { framework, .. } => {
                assert_eq!(framework.as_deref(), Some("openclaw"));
            }
            _ => panic!("expected enable"),
        }
    }

    #[test]
    fn enable_parses_allow_unsafe_plugin_install_flag() {
        let cli = TestCli::try_parse_from([
            "x",
            "enable",
            "tokenless",
            "openclaw",
            "--allow-unsafe-plugin-install",
        ])
        .expect("parse");
        match cli.command {
            AdapterCommands::Enable {
                component,
                framework,
                allow_unsafe_plugin_install,
                profiles,
            } => {
                assert_eq!(component, "tokenless");
                assert_eq!(framework.as_deref(), Some("openclaw"));
                assert!(
                    allow_unsafe_plugin_install,
                    "flag must be captured when passed"
                );
                assert!(profiles.is_empty());
            }
            _ => panic!("expected enable"),
        }
    }

    #[test]
    fn enable_parses_repeatable_profiles() {
        let cli = TestCli::try_parse_from([
            "x",
            "enable",
            "tokenless",
            "dsh",
            "--profile",
            "web",
            "--profile",
            "headless",
        ])
        .expect("parse");
        match cli.command {
            AdapterCommands::Enable { profiles, .. } => {
                assert_eq!(profiles, vec!["web", "headless"]);
            }
            _ => panic!("expected enable"),
        }
    }

    /// An absent component reports the same code here as it does from
    /// `uninstall`/`update`/`repair`, so a caller never has to know which
    /// command surfaced the condition.
    #[test]
    fn component_not_installed_maps_to_not_installed() {
        let err = map_err(
            "adapter enable",
            AdapterError::ComponentNotInstalled {
                component: "cosh".to_string(),
            },
        );
        assert_eq!(err.code(), "NOT_INSTALLED");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn unsafe_install_not_applicable_maps_to_invalid_argument() {
        let err = map_err(
            "adapter enable",
            AdapterError::UnsafeInstallNotApplicable {
                component: "agent-sec".to_string(),
                framework: "hermes".to_string(),
                adapter_type: None,
            },
        );
        assert!(matches!(err, CliError::InvalidArgument { .. }));
    }

    #[test]
    fn status_component_is_optional() {
        let cli = TestCli::try_parse_from(["x", "status"]).expect("parse");
        assert!(matches!(
            cli.command,
            AdapterCommands::Status { component: None }
        ));
    }

    #[test]
    fn ambiguous_framework_maps_to_invalid_argument() {
        let err = map_err(
            "adapter enable",
            AdapterError::AmbiguousFramework {
                component: "x".to_string(),
                frameworks: vec!["a".to_string(), "b".to_string()],
            },
        );
        assert!(matches!(err, CliError::InvalidArgument { .. }));
    }

    #[test]
    fn framework_cli_failure_maps_to_runtime() {
        let err = map_err(
            "adapter enable",
            AdapterError::FrameworkCli {
                program: "openclaw".to_string(),
                reason: "boom".to_string(),
            },
        );
        assert!(matches!(err, CliError::Runtime { .. }));
    }

    #[test]
    fn reenable_cleanup_failure_maps_to_runtime() {
        let err = map_err(
            "adapter enable",
            AdapterError::ReenableCleanupIncomplete {
                component: "tokenless".to_string(),
                framework: "openclaw".to_string(),
                reason: "uninstall failed".to_string(),
            },
        );
        assert!(matches!(err, CliError::Runtime { .. }));
    }

    #[test]
    fn missing_manifest_maps_to_runtime_and_says_missing() {
        let err = map_err(
            "adapter enable",
            AdapterError::MissingAdapterManifest {
                component: "agent-memory".to_string(),
                searched: vec![std::path::PathBuf::from(
                    "/var/lib/anolisa/component-manifests/agent-memory/component.toml",
                )],
            },
        );
        match err {
            CliError::Runtime { reason, .. } => {
                assert!(
                    reason.contains("missing"),
                    "error should say 'missing', got: {reason}"
                );
                assert!(
                    !reason.contains("invalid"),
                    "error must not say 'invalid' for a missing file, got: {reason}"
                );
            }
            other => panic!("expected Runtime, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_adapter_type_maps_to_invalid_argument() {
        let err = map_err(
            "adapter enable",
            AdapterError::UnsupportedAdapterType {
                component: "tokenless".to_string(),
                framework: "openclaw".to_string(),
                adapter_type: "skill_bundle".to_string(),
            },
        );
        assert!(matches!(err, CliError::InvalidArgument { .. }));
    }

    #[test]
    fn scan_row_includes_adapter_type() {
        let entry = ScanEntry {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            declared: true,
            resource_root: None,
            driver_available: true,
            framework_detected: true,
            adapter_type: Some("plugin".to_string()),
            enabled: false,
            claim_status: None,
            source_status: None,
            source_reason: None,
        };
        let row = scan_row_from_entry(&entry);
        assert_eq!(row.adapter_type.as_deref(), Some("plugin"));
    }

    #[test]
    fn scan_row_adapter_type_none_when_not_declared() {
        let entry = ScanEntry {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            declared: false,
            resource_root: Some(std::path::PathBuf::from("/tmp/adapters/tokenless/openclaw")),
            driver_available: true,
            framework_detected: true,
            adapter_type: None,
            enabled: false,
            claim_status: None,
            source_status: None,
            source_reason: None,
        };
        let row = scan_row_from_entry(&entry);
        assert!(row.adapter_type.is_none());
    }

    #[test]
    fn scan_row_includes_source_health_for_receipt() {
        let entry = ScanEntry {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            declared: false,
            resource_root: None,
            driver_available: true,
            framework_detected: true,
            adapter_type: Some("plugin".to_string()),
            enabled: true,
            claim_status: Some(ClaimStatus::Enabled),
            source_status: Some(AdapterSourceStatus::Missing),
            source_reason: Some("no visible installed component".to_string()),
        };

        let row = scan_row_from_entry(&entry);

        assert_eq!(row.source_status, Some("missing"));
        assert_eq!(
            row.source_reason.as_deref(),
            Some("no visible installed component")
        );
        assert_eq!(scan_state_label(&entry), "orphaned");
    }
}
