//! `anolisa bug` — generate a local bug report with diagnostic context.
//!
//! The command is read-only. It gathers environment facts, component state,
//! and recent warn/error central-log records, then renders copyable Markdown
//! for the repository bug report issue form.

use clap::Parser;
use serde::Serialize;

use anolisa_core::{CentralLog, LogFilter, LogRecord, ObjectKind, Severity};
use anolisa_platform::fs_layout::FsLayout;

use crate::commands::common;
use crate::commands::state_view::{StateView, StateVisibility};
use crate::context::CliContext;
use crate::response::{CliError, render_json};

mod cosh_ng;

const COMMAND: &str = "bug";
const ISSUE_URL: &str = "https://github.com/alibaba/anolisa/issues/new?template=bug_report.yml";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
/// Canonical component name that activates the cosh-shell diagnostic bridge.
const COSH_NG_COMPONENT: &str = "cosh-ng";

/// Detail fields a record may carry out of the machine in a bug bundle.
///
/// An allowlist rather than a filter. `details` is free-form and a command
/// attaches whatever it finds useful — a failed hook parks up to 4 KiB of raw
/// subprocess stderr under `stderr_tail`, which no keyword scan can be trusted
/// to clean, since a bare `Bearer` value or a PEM block has no `key=value`
/// shape to catch. A bundle is written to be pasted into a public issue, so a
/// field travels only once it is known to be structured and bounded, and
/// anything new stays behind until it is listed here.
const EXPORTABLE_DETAIL_KEYS: &[&str] = &[
    "apply_mode",
    "current_version",
    "endpoint",
    "latest_version",
    "package",
    "rpm_version_after",
    "rpm_version_before",
    "updated",
];

#[derive(Parser)]
pub struct BugArgs {
    /// Limit the report to one component.
    #[arg(long, value_name = "NAME")]
    pub component: Option<String>,
    /// Maximum number of recent warn/error log records to include.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct EnvironmentSummary {
    anolisa: String,
    install_mode: String,
    os: String,
    arch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    libc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pkg_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    btf: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cap_bpf: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ComponentSummary {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RecentLogSummary {
    started_at: String,
    severity: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component: Option<String>,
    command: String,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    objects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct BugReportPayload {
    issue_url: String,
    markdown: String,
    environment: EnvironmentSummary,
    installed_components: Vec<ComponentSummary>,
    recent_logs: Vec<RecentLogSummary>,
    /// Set when the resolved component's central log lives in a scope the
    /// caller cannot read; the report then continues without log records
    /// instead of failing.
    #[serde(skip_serializing_if = "Option::is_none")]
    recent_logs_unavailable: Option<String>,
    /// cosh-shell diagnostic bridge result; only collected — and only
    /// serialized — for `--component cosh-ng`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cosh_ng_diagnostics: Option<cosh_ng::CoshNgDiagnostics>,
}

pub fn handle(args: BugArgs, ctx: &CliContext) -> Result<(), CliError> {
    let limit = validate_limit(args.limit.unwrap_or(DEFAULT_LIMIT))?;
    let payload = build_payload(args.component.as_deref(), limit, ctx)?;

    if ctx.json {
        return render_json(COMMAND, payload);
    }

    if ctx.quiet {
        println!("{}", payload.markdown);
    } else {
        println!("Bug report markdown generated below.");
        println!("Paste it into:");
        println!("{}", payload.issue_url);
        println!();
        println!("---");
        println!("{}", payload.markdown);
    }
    Ok(())
}

fn build_payload(
    component: Option<&str>,
    limit: usize,
    ctx: &CliContext,
) -> Result<BugReportPayload, CliError> {
    build_payload_with(component, limit, ctx, cosh_ng::collect)
}

/// Injectable core of [`build_payload`]: the cosh-ng bridge is passed in so
/// tests can drive the production resolution chain against a staged install
/// layout instead of the host's `cosh-shell` binary and HOME.
fn build_payload_with(
    component: Option<&str>,
    limit: usize,
    ctx: &CliContext,
    collect_cosh_ng: impl FnOnce(&FsLayout) -> cosh_ng::CoshNgDiagnostics,
) -> Result<BugReportPayload, CliError> {
    let environment = collect_environment(ctx);
    // Resolve the component identity against the user+system visible view
    // before filtering state and logs.
    let resolved = component
        .map(|name| resolve_report_component(name, ctx))
        .transpose()?;
    let components = match &resolved {
        Some(resolved) => vec![resolved.summary.clone()],
        None => collect_components(ctx)?,
    };
    // Read the central log of the scope that owns the resolved record: a
    // system-scope install logs to the system log, which the calling
    // user's log never sees. The default (no --component) report keeps the
    // caller's layout, and a user-scope hit resolves to the same layout —
    // both byte-identical to before.
    let caller_layout;
    let log_layout = match &resolved {
        Some(resolved) => &resolved.layout,
        None => {
            caller_layout = common::resolve_layout(ctx);
            &caller_layout
        }
    };
    let mut recent_logs_unavailable = None;
    let recent_logs = match collect_recent_logs(
        resolved.as_ref().map(|r| r.name.as_str()),
        limit,
        log_layout,
    ) {
        Ok(logs) => logs,
        // A cross-scope log (e.g. a root-only system log read by a plain
        // user) must not abort the report: degrade to an explicit warning
        // and continue to the diagnostic bridge. The caller's own log
        // keeps the hard error, and a missing log file stays tolerated
        // inside the query itself.
        Err(err) if resolved.as_ref().is_some_and(|r| r.cross_scope) => {
            recent_logs_unavailable = Some(format!(
                "warn/error log records unavailable: {}",
                err.reason()
            ));
            Vec::new()
        }
        Err(err) => return Err(err),
    };
    let cosh_ng_diagnostics = match &resolved {
        Some(resolved) if resolved.name == COSH_NG_COMPONENT => {
            // The collector spawns the exporter and writes a bundle, so a
            // dry run must skip it — explicitly, not disguised as
            // unavailable.
            if ctx.dry_run {
                Some(cosh_ng::CoshNgDiagnostics::Skipped)
            } else {
                Some(collect_cosh_ng(&resolved.layout))
            }
        }
        _ => None,
    };
    let markdown = render_markdown(
        &environment,
        &components,
        &recent_logs,
        recent_logs_unavailable.as_deref(),
        cosh_ng_diagnostics.as_ref(),
    );

    Ok(BugReportPayload {
        issue_url: ISSUE_URL.to_string(),
        markdown,
        environment,
        installed_components: components,
        recent_logs,
        recent_logs_unavailable,
        cosh_ng_diagnostics,
    })
}

/// A `--component` argument resolved against the visible state view: the
/// canonical name, the record's summary, and the layout of the scope that
/// owns the record.
struct ResolvedReportComponent {
    name: String,
    summary: ComponentSummary,
    layout: FsLayout,
    /// True when the record lives in a scope the current invocation cannot
    /// write (a system-scope record seen from a user-mode run). Its central
    /// log may be unreadable to the caller, so reads fail open there.
    cross_scope: bool,
}

/// Resolve a `--component` argument for a read-only report.
///
/// The user+system visible view is authoritative here, not just the
/// writable scope: a plain user reporting on a system-scope RPM install
/// must reach that install's record and its real layout — the diagnostic
/// bridge probes the system libexec for `cosh-shell` — instead of dying
/// with "unknown component". Exact state identity wins in either scope;
/// otherwise the repo-side component index stays the identity authority.
fn resolve_report_component(
    name: &str,
    ctx: &CliContext,
) -> Result<ResolvedReportComponent, CliError> {
    let writable_state = common::load_state_store(ctx, COMMAND)?;
    let view = StateView::load_with_writable_state(
        ctx,
        COMMAND,
        StateVisibility::UserPlusSystem,
        writable_state,
    )?;
    let canonical = if view.has_exact_component(name) {
        name.to_string()
    } else {
        common::lookup_component_name_in_store(name, &view.writable.state, ctx, COMMAND)?
    };
    let record = view
        .visible_components()
        .into_iter()
        .find(|record| record.active && record.object.name == canonical)
        .ok_or_else(|| CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!("unknown component '{canonical}'"),
        })?;
    Ok(ResolvedReportComponent {
        name: canonical,
        summary: summarize_component(record.object),
        layout: record.root.layout.clone(),
        cross_scope: !record.root.writable,
    })
}

fn validate_limit(limit: usize) -> Result<usize, CliError> {
    if limit > MAX_LIMIT {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!("--limit must be <= {MAX_LIMIT}, got {limit}"),
        });
    }
    Ok(limit)
}

fn collect_environment(ctx: &CliContext) -> EnvironmentSummary {
    let facts = anolisa_env::EnvService::detect();
    EnvironmentSummary {
        anolisa: env!("CARGO_PKG_VERSION").to_string(),
        install_mode: ctx.install_mode.as_str().to_string(),
        os: facts.os,
        arch: facts.arch,
        libc: facts.libc,
        kernel: facts.kernel,
        pkg_base: facts.pkg_base,
        btf: facts.btf,
        cap_bpf: facts.cap_bpf,
        container: facts.container,
    }
}

/// Human-facing version string: an owned artifact's recorded version, a
/// delegated record's cached observation (EVR preferred), else "unknown".
fn installed_version(installation: &anolisa_core::domain::Installation) -> String {
    match &installation.binding {
        anolisa_core::domain::ProviderBinding::Owned { artifact } => artifact.version.clone(),
        anolisa_core::domain::ProviderBinding::Delegated { last_observed, .. } => last_observed
            .as_ref()
            .map(|o| o.evr.clone().unwrap_or_else(|| o.version.clone()))
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Summaries of every enabled component in the writable scope, for the
/// default (no `--component`) report.
fn collect_components(ctx: &CliContext) -> Result<Vec<ComponentSummary>, CliError> {
    let state = common::load_state_store(ctx, COMMAND)?;
    Ok(state
        .installations
        .iter()
        .filter(|o| o.kind == ObjectKind::Component)
        .map(summarize_component)
        .filter(|s| common::status_is_enabled(&s.status))
        .collect())
}

fn summarize_component(object: &anolisa_core::domain::Installation) -> ComponentSummary {
    ComponentSummary {
        name: object.name.clone(),
        status: common::installation_status_str(object).to_string(),
        installed_version: Some(installed_version(object)),
    }
}

fn collect_recent_logs(
    component: Option<&str>,
    limit: usize,
    layout: &FsLayout,
) -> Result<Vec<RecentLogSummary>, CliError> {
    let log = CentralLog::open(layout.central_log.clone());
    let records = log
        .query(&LogFilter {
            severity_at_least: Some(Severity::Warn),
            object: component.map(|name| name.to_string()),
            limit: None,
            ..Default::default()
        })
        .map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "failed to query central log at {}: {err}",
                layout.central_log.display()
            ),
        })?;

    Ok(take_recent_by_started_at(records, limit)
        .into_iter()
        .map(summarize_log)
        .collect())
}

fn take_recent_by_started_at(mut records: Vec<LogRecord>, limit: usize) -> Vec<LogRecord> {
    records.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    let skip = records.len().saturating_sub(limit);
    records.into_iter().skip(skip).collect()
}

fn summarize_log(record: LogRecord) -> RecentLogSummary {
    RecentLogSummary {
        started_at: redact_sensitive(&record.started_at),
        severity: severity_str(record.severity).to_string(),
        source: redact_sensitive(&record.source),
        component: record.component.map(|v| redact_sensitive(&v)),
        command: redact_sensitive(&record.command),
        message: redact_sensitive(&record.message),
        objects: record
            .objects
            .into_iter()
            .map(|v| redact_sensitive(&v))
            .collect(),
        warnings: record
            .warnings
            .into_iter()
            .map(|v| redact_sensitive(&v))
            .collect(),
        details: redact_details(record.details),
    }
}

/// Carry a record's `details` into the bundle under the same redaction as every
/// other field.
///
/// Without this the structured diagnostics a command attaches — the versions
/// and endpoint a failed self-update records, say — are selected into the
/// bundle and then dropped on the way out, which is the one place they were
/// meant to be read.
fn redact_details(value: serde_json::Value) -> Option<serde_json::Value> {
    let serde_json::Value::Object(fields) = value else {
        return None;
    };
    let kept: serde_json::Map<String, serde_json::Value> = fields
        .into_iter()
        .filter(|(key, _)| EXPORTABLE_DETAIL_KEYS.contains(&key.as_str()))
        .map(|(key, value)| match value {
            serde_json::Value::String(text) => {
                (key, serde_json::Value::String(redact_sensitive(&text)))
            }
            other => (key, other),
        })
        .collect();

    if kept.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(kept))
}

fn render_markdown(
    env: &EnvironmentSummary,
    components: &[ComponentSummary],
    logs: &[RecentLogSummary],
    logs_unavailable: Option<&str>,
    cosh_ng: Option<&cosh_ng::CoshNgDiagnostics>,
) -> String {
    let mut out = String::new();
    out.push_str("## Description\n\n");
    out.push_str("<!-- Please fill in the issue details before submitting. -->\n\n");
    out.push_str("- Problem:\n");
    out.push_str("- Steps to reproduce:\n");
    out.push_str("- Expected behavior:\n");
    out.push_str("- Time observed:\n\n");

    out.push_str("## Environment\n\n");
    push_kv(&mut out, "anolisa", &env.anolisa);
    push_kv(&mut out, "install_mode", &env.install_mode);
    push_kv(&mut out, "os", &env.os);
    push_kv(&mut out, "arch", &env.arch);
    push_opt_kv(&mut out, "libc", env.libc.as_deref());
    push_opt_kv(&mut out, "kernel", env.kernel.as_deref());
    push_opt_kv(&mut out, "pkg_base", env.pkg_base.as_deref());
    push_opt_kv(&mut out, "btf", env.btf.map(bool_label));
    push_opt_kv(&mut out, "cap_bpf", env.cap_bpf.map(bool_label));
    push_opt_kv(&mut out, "container", env.container.as_deref());

    out.push_str("\n## Installed Components\n\n");
    if components.is_empty() {
        out.push_str("- none\n");
    } else {
        for comp in components {
            match comp.installed_version.as_deref() {
                Some(version) => out.push_str(&format!(
                    "- {}: {}, version {}\n",
                    comp.name, comp.status, version
                )),
                None => out.push_str(&format!("- {}: {}\n", comp.name, comp.status)),
            }
        }
    }

    out.push_str("\n## Recent Logs\n\n");
    if let Some(warning) = logs_unavailable {
        out.push_str(&format!("- unavailable: {warning}\n"));
    } else if logs.is_empty() {
        out.push_str("- No warn/error central log records found.\n");
    } else {
        for log in logs {
            out.push_str(&format!(
                "- {} {} {}: {}\n",
                log.started_at, log.severity, log.source, log.message
            ));
            if !log.objects.is_empty() {
                out.push_str(&format!("  - objects: {}\n", log.objects.join(", ")));
            }
            if !log.warnings.is_empty() {
                out.push_str(&format!("  - warnings: {}\n", log.warnings.join("; ")));
            }
            if let Some(details) = &log.details {
                out.push_str(&format!("  - details: {details}\n"));
            }
        }
    }

    if let Some(diagnostics) = cosh_ng {
        render_cosh_ng_section(&mut out, diagnostics);
    }
    out
}

/// cosh-ng section: finding IDs and the bundle manifest are the reviewable
/// references into the sanitized export; the bundle path travels folded to
/// `~` and is always paired with a review-before-upload reminder. Nothing is
/// uploaded or submitted from here.
fn render_cosh_ng_section(out: &mut String, diagnostics: &cosh_ng::CoshNgDiagnostics) {
    out.push_str("\n## cosh-ng Diagnostics\n\n");
    match diagnostics {
        cosh_ng::CoshNgDiagnostics::Available {
            binary_path,
            bundle_path,
            overall_severity,
            findings,
            unavailable_collectors,
            manifest,
        } => {
            push_kv(out, "status", "available");
            push_kv(out, "cosh-shell binary", binary_path);
            push_kv(out, "diagnostic bundle", bundle_path);
            out.push_str("- review the bundle before attaching it; it is local-only (0600) and never uploaded automatically\n");
            if let Some(severity) = overall_severity {
                push_kv(out, "overall_severity", severity);
            }
            if findings.is_empty() {
                out.push_str("- findings: none\n");
            } else {
                out.push_str("- findings:\n");
                for finding in findings {
                    out.push_str(&format!("  - {} ({})\n", finding.id, finding.severity));
                }
            }
            if !unavailable_collectors.is_empty() {
                out.push_str(&format!(
                    "- unavailable collectors: {}\n",
                    unavailable_collectors.join(", ")
                ));
            }
            if !manifest.is_empty() {
                out.push_str("- bundle manifest:\n");
                for entry in manifest {
                    out.push_str(&format!(
                        "  - {}: {} ({} items)\n",
                        entry.source, entry.status, entry.items
                    ));
                }
            }
            out.push_str(
                "\nReproduction checklist (GitHub issue #3055 SOP) — run in the affected Zsh session:\n\n",
            );
            out.push_str("```zsh\n");
            out.push_str("typeset -p _COSH_AI_ENABLED _COSH_HAS_USER_COMMAND_NOT_FOUND\n");
            out.push_str("whence -v command_not_found_handler\n");
            out.push_str("whence -v _cosh_user_command_not_found_handler\n");
            out.push_str("?? 测试\n");
            out.push_str("```\n\n");
            out.push_str("Then, from a separate terminal:\n\n");
            out.push_str("```sh\n");
            out.push_str(&format!("{} doctor\n", cosh_ng::shell_quote(binary_path)));
            out.push_str(&format!(
                "{} diagnostics export --since-hours 1\n",
                cosh_ng::shell_quote(binary_path)
            ));
            out.push_str("```\n");
        }
        cosh_ng::CoshNgDiagnostics::Unavailable {
            reason,
            manual_command,
        } => {
            push_kv(out, "status", "unavailable");
            push_kv(out, "reason", reason);
            out.push_str(&format!(
                "- collect the bundle manually, review it, and attach it to the issue:\n  `{manual_command}`\n"
            ));
        }
        cosh_ng::CoshNgDiagnostics::Skipped => {
            push_kv(out, "status", "skipped (dry run)");
            out.push_str(
                "- diagnostic collection skipped: --dry-run prints the plan without executing; re-run without --dry-run to export the bundle\n",
            );
        }
    }
}

fn push_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(&format!("- {key}: {value}\n"));
}

fn push_opt_kv(out: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        push_kv(out, key, value);
    }
}

fn bool_label(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

fn severity_str(sev: Severity) -> &'static str {
    match sev {
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

fn redact_sensitive(input: &str) -> String {
    // This is intentionally keyword-based and biased toward over-redaction:
    // component logs are free-form, so safety matters more than exact parsing.
    const KEYS: &[&str] = &[
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "api_key",
        "access_key",
        "private_key",
    ];

    let mut out = input.to_string();
    for key in KEYS {
        out = redact_key_values(&out, key);
    }
    out
}

fn redact_key_values(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut pos = 0;

    while let Some(relative) = lower[pos..].find(key) {
        let key_start = pos + relative;
        let key_end = key_start + key.len();
        let mut cursor = key_end;
        while cursor < input.len() && is_space_or_quote(input.as_bytes()[cursor]) {
            cursor += 1;
        }
        if cursor >= input.len() || !matches!(input.as_bytes()[cursor], b'=' | b':') {
            out.push_str(&input[pos..key_end]);
            pos = key_end;
            continue;
        }

        cursor += 1;
        while cursor < input.len() && is_space_or_quote(input.as_bytes()[cursor]) {
            cursor += 1;
        }

        out.push_str(&input[pos..cursor]);
        out.push_str("<redacted>");

        let value_end = input[cursor..]
            .find(is_value_boundary)
            .map(|idx| cursor + idx)
            .unwrap_or(input.len());
        pos = value_end;
    }

    out.push_str(&input[pos..]);
    out
}

fn is_space_or_quote(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\'' | b'"')
}

fn is_value_boundary(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | ',' | ';' | '&' | '"' | '\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_summary(name: &str, status: &str, version: Option<&str>) -> ComponentSummary {
        ComponentSummary {
            name: name.to_string(),
            status: status.to_string(),
            installed_version: version.map(|v| v.to_string()),
        }
    }

    fn filter_summaries(
        all: &[ComponentSummary],
        component: Option<&str>,
    ) -> Result<Vec<ComponentSummary>, CliError> {
        match component {
            Some(name) => {
                let matches: Vec<ComponentSummary> =
                    all.iter().filter(|s| s.name == name).cloned().collect();
                if matches.is_empty() {
                    return Err(CliError::InvalidArgument {
                        command: COMMAND.to_string(),
                        reason: format!("unknown component '{name}'"),
                    });
                }
                Ok(matches)
            }
            None => Ok(all
                .iter()
                .filter(|s| common::status_is_enabled(&s.status))
                .cloned()
                .collect()),
        }
    }

    fn log_record(started_at: &str, message: &str) -> LogRecord {
        LogRecord {
            kind: anolisa_core::LogKind::Operation,
            operation_id: Some("op-1".to_string()),
            command: "enable tokenless".to_string(),
            source: "anolisa-cli".to_string(),
            component: None,
            severity: Severity::Warn,
            message: message.to_string(),
            actor: "cli".to_string(),
            install_mode: Some("user".to_string()),
            started_at: started_at.to_string(),
            finished_at: None,
            status: None,
            objects: vec!["tokenless".to_string()],
            backup_ids: Vec::new(),
            warnings: Vec::new(),
            details: serde_json::Value::Null,
        }
    }

    #[test]
    fn default_component_report_includes_enabled_rows_only() {
        let all = vec![
            make_summary("agent-observability", "installed", Some("0.1.0")),
            make_summary("sandbox", "disabled", Some("0.1.0")),
            make_summary("tokenless", "not_installed", None),
        ];

        let caps = filter_summaries(&all, None).expect("summaries");

        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].name, "agent-observability");
    }

    #[test]
    fn component_filter_keeps_requested_row_even_when_disabled() {
        let all = vec![make_summary("sandbox", "disabled", Some("0.1.0"))];

        let caps = filter_summaries(&all, Some("sandbox")).expect("summaries");

        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].status, "disabled");
    }

    #[test]
    fn component_filter_rejects_unknown_name() {
        let all = vec![make_summary("sandbox", "installed", Some("0.1.0"))];

        let err = filter_summaries(&all, Some("missing")).expect_err("unknown component");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("unknown component"));
    }

    #[test]
    fn take_recent_preserves_last_records_in_order() {
        let records = vec![
            log_record("2026-06-01T10:00:00Z", "one"),
            log_record("2026-06-01T10:00:01Z", "two"),
            log_record("2026-06-01T10:00:02Z", "three"),
        ];

        let recent = take_recent_by_started_at(records, 2);

        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "two");
        assert_eq!(recent[1].message, "three");
    }

    #[test]
    fn take_recent_sorts_by_started_at_before_limiting() {
        let records = vec![
            log_record("2026-06-01T10:00:02Z", "three"),
            log_record("2026-06-01T10:00:00Z", "one"),
            log_record("2026-06-01T10:00:01Z", "two"),
        ];

        let recent = take_recent_by_started_at(records, 2);

        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "two");
        assert_eq!(recent[1].message, "three");
    }

    #[test]
    fn missing_central_log_yields_empty_recent_logs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::context_for_root(
            tmp.path(),
            crate::context::InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        );

        let logs = collect_recent_logs(None, DEFAULT_LIMIT, &common::resolve_layout(&ctx))
            .expect("missing log is ok");

        assert!(logs.is_empty());
        let env = EnvironmentSummary {
            anolisa: "0.1.0".to_string(),
            install_mode: "system".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: None,
            pkg_base: None,
            btf: None,
            cap_bpf: None,
            container: None,
        };
        let markdown = render_markdown(&env, &[], &logs, None, None);
        assert!(markdown.contains("No warn/error central log records found."));
    }

    #[test]
    fn redacts_sensitive_key_values() {
        let input = "token=abc password: hunter2 url=https://x?a=1&access_key=ak";

        let redacted = redact_sensitive(input);

        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("ak"));
        assert!(redacted.contains("token=<redacted>"));
        assert!(redacted.contains("password: <redacted>"));
        assert!(redacted.contains("access_key=<redacted>"));
    }

    #[test]
    fn redaction_preserves_json_style_quotes() {
        let input = r#"{"token":"abc","ok":true}"#;

        let redacted = redact_sensitive(input);

        assert_eq!(redacted, r#"{"token":"<redacted>","ok":true}"#);
    }

    #[test]
    fn markdown_uses_redacted_log_messages() {
        let env = EnvironmentSummary {
            anolisa: "0.1.0".to_string(),
            install_mode: "user".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: None,
            pkg_base: None,
            btf: None,
            cap_bpf: None,
            container: None,
        };
        let logs = vec![summarize_log(log_record(
            "2026-06-01T10:00:00Z",
            "failed with token=super-secret",
        ))];

        let markdown = render_markdown(&env, &[], &logs, None, None);

        assert!(markdown.contains("token=<redacted>"));
        assert!(!markdown.contains("super-secret"));
    }

    /// The structured failure context is the reason a failed operation is worth
    /// recording, and the bundle is where it gets read. Asserted on a real
    /// payload: the record goes through the log file, the query, the summary,
    /// and the renderer.
    #[test]
    fn bug_payload_carries_redacted_log_details() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::context_for_root(
            tmp.path(),
            crate::context::InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        );

        let mut record = log_record("2026-06-01T10:00:00Z", "self-update failed");
        record.details = serde_json::json!({
            "current_version": "0.3.7",
            "latest_version": "0.3.8",
            "apply_mode": "rpm-package",
            "endpoint": "https://mirror.invalid",
            "access_key": "must-not-appear",
        });
        let layout = common::resolve_layout(&ctx);
        CentralLog::open(layout.central_log.clone())
            .append(&record)
            .expect("append");

        let logs =
            collect_recent_logs(None, DEFAULT_LIMIT, &common::resolve_layout(&ctx)).expect("query");
        let details = logs[0]
            .details
            .as_ref()
            .expect("details must reach the bundle");

        assert_eq!(details["latest_version"], "0.3.8");
        assert_eq!(details["apply_mode"], "rpm-package");
        assert_eq!(details["endpoint"], "https://mirror.invalid");
        assert!(
            details.get("access_key").is_none(),
            "a field outside the allowlist must not travel: {details}"
        );

        let markdown = render_markdown(&collect_environment(&ctx), &[], &logs, None, None);
        assert!(markdown.contains("latest_version"), "{markdown}");
        assert!(!markdown.contains("must-not-appear"), "{markdown}");
    }

    /// A failed hook parks raw subprocess stderr in `details.stderr_tail`, and
    /// the keyword scan cannot clean it: a bare `Bearer` value and a PEM block
    /// have no `key=value` shape to catch. Carrying details must not be what
    /// first lets that out of the machine.
    #[test]
    fn free_form_details_stay_out_of_the_bundle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::context_for_root(
            tmp.path(),
            crate::context::InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        );

        let mut record = log_record("2026-06-01T10:00:00Z", "hook failed");
        record.details = serde_json::json!({
            "phase": "post-install",
            "exit_code": 1,
            "stderr_tail": "curl: Authorization: Bearer eyJhbGciOi.LEAKED\n\
                            -----BEGIN OPENSSH PRIVATE KEY-----\nb3BlLEAKED\n",
        });
        let layout = common::resolve_layout(&ctx);
        CentralLog::open(layout.central_log.clone())
            .append(&record)
            .expect("append");

        let logs =
            collect_recent_logs(None, DEFAULT_LIMIT, &common::resolve_layout(&ctx)).expect("query");
        assert!(
            logs[0].details.is_none(),
            "no field of a hook record is exportable: {:?}",
            logs[0].details
        );

        let markdown = render_markdown(&collect_environment(&ctx), &[], &logs, None, None);
        assert!(!markdown.contains("LEAKED"), "{markdown}");
        assert!(!markdown.contains("stderr_tail"), "{markdown}");
    }

    #[test]
    fn validate_limit_rejects_values_above_max() {
        let err = validate_limit(MAX_LIMIT + 1).expect_err("limit should fail");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    fn env_summary() -> EnvironmentSummary {
        EnvironmentSummary {
            anolisa: "0.1.0".to_string(),
            install_mode: "user".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: None,
            pkg_base: None,
            btf: None,
            cap_bpf: None,
            container: None,
        }
    }

    fn available_diagnostics() -> cosh_ng::CoshNgDiagnostics {
        cosh_ng::CoshNgDiagnostics::Available {
            binary_path: "~/.local/lib/anolisa/libexec/cosh-ng/cosh-shell".to_string(),
            bundle_path: "~/.local/state/anolisa/diagnostics/cosh-diagnostic-1-2.json".to_string(),
            overall_severity: Some("warning".to_string()),
            findings: vec![
                cosh_ng::FindingSummary {
                    id: "hooks.user_command_not_found".to_string(),
                    severity: "warning".to_string(),
                },
                cosh_ng::FindingSummary {
                    id: "provider.unreachable".to_string(),
                    severity: "critical".to_string(),
                },
            ],
            unavailable_collectors: vec!["pty (unsupported)".to_string()],
            manifest: vec![
                cosh_ng::ManifestSummary {
                    source: "environment".to_string(),
                    status: "included".to_string(),
                    items: 1,
                },
                cosh_ng::ManifestSummary {
                    source: "logs".to_string(),
                    status: "partial".to_string(),
                    items: 3,
                },
            ],
        }
    }

    #[test]
    fn markdown_includes_cosh_ng_section_when_available() {
        let diagnostics = available_diagnostics();

        let markdown = render_markdown(&env_summary(), &[], &[], None, Some(&diagnostics));

        assert!(markdown.contains("## cosh-ng Diagnostics"), "{markdown}");
        assert!(
            markdown.contains("~/.local/state/anolisa/diagnostics/cosh-diagnostic-1-2.json"),
            "{markdown}"
        );
        assert!(
            markdown.contains("hooks.user_command_not_found"),
            "{markdown}"
        );
        assert!(markdown.contains("provider.unreachable"), "{markdown}");
        assert!(markdown.contains("overall_severity: warning"), "{markdown}");
        assert!(markdown.contains("logs: partial (3 items)"), "{markdown}");
        assert!(markdown.contains("pty (unsupported)"), "{markdown}");
        assert!(
            markdown.contains("review the bundle before attaching"),
            "{markdown}"
        );
        assert!(markdown.contains("#3055"), "{markdown}");
        assert!(
            markdown.contains("~/.local/lib/anolisa/libexec/cosh-ng/cosh-shell doctor"),
            "reproduction checklist must use the resolved binary path: {markdown}"
        );
    }

    #[test]
    fn markdown_never_carries_an_unfolded_home_path() {
        let diagnostics = available_diagnostics();

        let markdown = render_markdown(&env_summary(), &[], &[], None, Some(&diagnostics));

        assert!(
            !markdown.contains("/home/"),
            "bundle paths must be folded to ~ before rendering: {markdown}"
        );
    }

    #[test]
    fn markdown_explains_unavailable_with_manual_guidance() {
        let diagnostics = cosh_ng::CoshNgDiagnostics::Unavailable {
            reason: "cosh-shell binary not found (set COSH_SHELL_BIN or add it to PATH)"
                .to_string(),
            manual_command:
                "cosh-shell diagnostics export --output ~/.local/state/anolisa/diagnostics/cosh-diagnostic-manual.json"
                    .to_string(),
        };

        let markdown = render_markdown(&env_summary(), &[], &[], None, Some(&diagnostics));

        assert!(markdown.contains("status: unavailable"), "{markdown}");
        assert!(
            markdown.contains("cosh-shell binary not found"),
            "{markdown}"
        );
        assert!(
            markdown.contains("cosh-shell diagnostics export --output"),
            "{markdown}"
        );
    }

    #[test]
    fn markdown_is_byte_identical_without_cosh_ng() {
        let env = env_summary();
        let logs = vec![summarize_log(log_record(
            "2026-06-01T10:00:00Z",
            "plain warning",
        ))];

        let with = render_markdown(&env, &[], &logs, None, None);

        // The legacy shape ends after the Recent Logs section.
        assert!(with.contains("- 2026-06-01T10:00:00Z warn anolisa-cli: plain warning\n"));
        assert!(with.ends_with("  - objects: tokenless\n"));
        assert!(!with.contains("cosh-ng Diagnostics"), "{with}");
    }

    #[test]
    fn json_payload_carries_cosh_ng_diagnostics_only_for_cosh_ng() {
        let base = BugReportPayload {
            issue_url: ISSUE_URL.to_string(),
            markdown: String::new(),
            environment: env_summary(),
            installed_components: Vec::new(),
            recent_logs: Vec::new(),
            recent_logs_unavailable: None,
            cosh_ng_diagnostics: None,
        };
        let without = serde_json::to_value(&base).expect("serialize");
        assert!(without.get("cosh_ng_diagnostics").is_none());

        let with = BugReportPayload {
            cosh_ng_diagnostics: Some(available_diagnostics()),
            ..base
        };
        let value = serde_json::to_value(&with).expect("serialize");
        let section = value
            .get("cosh_ng_diagnostics")
            .expect("cosh_ng_diagnostics field");
        assert_eq!(section["status"], "available");
        assert_eq!(section["findings"][0]["id"], "hooks.user_command_not_found");
    }

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, body: &str) {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(path).expect("create executable");
        file.write_all(body.as_bytes()).expect("write executable");
        file.sync_all().expect("sync executable");
        drop(file);
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    /// One cosh-ng component record as an RPM install would write it; the
    /// install mode and prefix must match the layout that loads the state.
    fn write_cosh_ng_state(layout: &FsLayout, rpm: bool) {
        use anolisa_core::{
            InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectStatus,
            Ownership, RpmMetadata, SubscriptionScope,
        };
        let object = InstalledObject {
            kind: ObjectKind::Component,
            name: COSH_NG_COMPONENT.to_string(),
            version: "1.0.0".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some(if rpm { "rpm" } else { "raw" }.to_string()),
            ownership: Some(if rpm {
                Ownership::RpmObserved
            } else {
                Ownership::RawManaged
            }),
            rpm_metadata: rpm.then(|| RpmMetadata {
                package_name: "cosh-ng".to_string(),
                evr: None,
                arch: None,
                source_repo: None,
            }),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            last_operation_id: None,
            managed: !rpm,
            adopted: rpm,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        };
        let state = InstalledState {
            install_mode: match layout.mode {
                anolisa_platform::fs_layout::InstallMode::System => StateInstallMode::System,
                anolisa_platform::fs_layout::InstallMode::User => StateInstallMode::User,
            },
            prefix: layout.prefix.clone(),
            objects: vec![object],
            ..InstalledState::default()
        };
        state
            .save(&layout.state_dir.join("installed.toml"))
            .expect("save state");
    }

    #[cfg(unix)]
    fn user_mode_context(root: &std::path::Path) -> crate::context::CliContext {
        crate::test_support::context_for_root(
            root,
            crate::context::InstallMode::User,
            Some(root.join("sysroot")),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        )
    }

    #[cfg(unix)]
    fn fake_export_script() -> &'static str {
        // Parse --output from the export argv and write a canned bundle.
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         \x20 if [ \"$1\" = \"--output\" ]; then shift; out=\"$1\"; fi\n\
         \x20 shift\n\
         done\n\
         \x20 cat > \"$out\" <<'JSON'\n\
         {\"format\": \"cosh-diagnostic-bundle\", \"version\": 1, \
         \"manifest\": [{\"source\": \"environment\", \"status\": \"included\", \"items\": 1}], \
         \"sources\": {\"health\": {\"overall_severity\": \"ok\", \"findings\": [], \
         \"unavailable\": []}}}\n\
         JSON\n"
    }

    /// Command-level regression for the round-3 P1: a plain user-mode
    /// invocation with an empty user state and a system-scope RPM record
    /// must resolve cosh-ng through the visible system scope and bridge the
    /// staged RPM libexec binary — previously this exited with
    /// "unknown component" before the RPM candidate was ever probed.
    #[cfg(unix)]
    #[test]
    fn build_payload_bridges_system_scope_rpm_install_from_user_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = user_mode_context(tmp.path());
        let system_layout = ctx.visible_system_layout().clone();
        write_cosh_ng_state(&system_layout, true);
        let rpm_bin = system_layout
            .prefix
            .join("usr/libexec/anolisa/cosh-ng/cosh-shell");
        write_executable(&rpm_bin, fake_export_script());

        let home = tmp.path().join("home");
        let diag_dir = home.join("diagnostics");
        let payload = build_payload_with(Some("cosh-ng"), DEFAULT_LIMIT, &ctx, |layout| {
            let binary = cosh_ng::resolve_binary_with(None, layout, None);
            cosh_ng::collect_impl(
                binary,
                Some(diag_dir.clone()),
                &home,
                std::time::Duration::from_secs(10),
            )
        })
        .expect("a system-scope cosh-ng record must resolve from user mode");

        assert_eq!(payload.installed_components.len(), 1);
        assert_eq!(payload.installed_components[0].name, COSH_NG_COMPONENT);
        let Some(cosh_ng::CoshNgDiagnostics::Available {
            binary_path,
            bundle_path,
            ..
        }) = payload.cosh_ng_diagnostics
        else {
            panic!(
                "expected available diagnostics: {:?}",
                payload.cosh_ng_diagnostics
            );
        };
        assert_eq!(binary_path, rpm_bin.display().to_string());
        assert!(bundle_path.starts_with("~/diagnostics/"), "{bundle_path}");
        assert!(
            diag_dir.read_dir().expect("diag dir").count() == 1,
            "the bundle must land in the calling user's directory"
        );
    }

    /// When both scopes record cosh-ng, the active (user-scope) record owns
    /// the layout the bridge probes — a user install must not be displaced
    /// by the shadowed system one.
    #[cfg(unix)]
    #[test]
    fn build_payload_prefers_the_active_user_scope_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = user_mode_context(tmp.path());
        let system_layout = ctx.visible_system_layout().clone();
        write_cosh_ng_state(&system_layout, true);
        let user_layout = common::resolve_layout(&ctx);
        write_cosh_ng_state(&user_layout, false);
        let user_bin = user_layout.libexec_dir.join("cosh-ng/cosh-shell");
        write_executable(&user_bin, fake_export_script());
        let rpm_bin = system_layout
            .prefix
            .join("usr/libexec/anolisa/cosh-ng/cosh-shell");
        write_executable(&rpm_bin, fake_export_script());

        let home = tmp.path().join("home");
        let diag_dir = home.join("diagnostics");
        let payload = build_payload_with(Some("cosh-ng"), DEFAULT_LIMIT, &ctx, |layout| {
            let binary = cosh_ng::resolve_binary_with(None, layout, None);
            cosh_ng::collect_impl(
                binary,
                Some(diag_dir.clone()),
                &home,
                std::time::Duration::from_secs(10),
            )
        })
        .expect("payload");

        let Some(cosh_ng::CoshNgDiagnostics::Available { binary_path, .. }) =
            payload.cosh_ng_diagnostics
        else {
            panic!(
                "expected available diagnostics: {:?}",
                payload.cosh_ng_diagnostics
            );
        };
        // The user binary lives below the injected home, so the display
        // path is home-folded; what matters is it is the user libexec, not
        // the shadowed system RPM path.
        assert_eq!(
            binary_path,
            "~/.local/lib/anolisa/libexec/cosh-ng/cosh-shell"
        );
    }

    /// Command-level regression for the round-4 P2: with the cosh-ng record
    /// in system scope only, the report must read the *system* central log —
    /// a warn record there reaches both the JSON payload and the Markdown,
    /// while the calling user's (empty) log contributes nothing.
    #[cfg(unix)]
    #[test]
    fn build_payload_reads_central_log_of_the_records_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = user_mode_context(tmp.path());
        let system_layout = ctx.visible_system_layout().clone();
        write_cosh_ng_state(&system_layout, true);
        let rpm_bin = system_layout
            .prefix
            .join("usr/libexec/anolisa/cosh-ng/cosh-shell");
        write_executable(&rpm_bin, fake_export_script());
        let mut record = log_record("2026-06-01T10:00:00Z", "system-scope warning from cosh-ng");
        record.objects = vec![COSH_NG_COMPONENT.to_string()];
        CentralLog::open(system_layout.central_log.clone())
            .append(&record)
            .expect("append system record");

        let home = tmp.path().join("home");
        let diag_dir = home.join("diagnostics");
        let payload = build_payload_with(Some("cosh-ng"), DEFAULT_LIMIT, &ctx, |layout| {
            let binary = cosh_ng::resolve_binary_with(None, layout, None);
            cosh_ng::collect_impl(
                binary,
                Some(diag_dir.clone()),
                &home,
                std::time::Duration::from_secs(10),
            )
        })
        .expect("payload");

        assert_eq!(payload.recent_logs.len(), 1);
        assert_eq!(
            payload.recent_logs[0].message,
            "system-scope warning from cosh-ng"
        );
        assert!(
            payload
                .markdown
                .contains("system-scope warning from cosh-ng"),
            "the record must reach the markdown: {}",
            payload.markdown
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            json["recent_logs"][0]["message"].as_str(),
            Some("system-scope warning from cosh-ng"),
            "the record must reach the JSON payload: {json}"
        );
    }

    /// Command-level regression for the round-6 P1: a cross-scope central
    /// log the caller cannot read must degrade to an explicit warning, not
    /// abort the report — the diagnostic bridge still runs and produces the
    /// bundle. A directory at the log path fails the query deterministically
    /// on any user, without relying on permission bits.
    #[cfg(unix)]
    #[test]
    fn build_payload_tolerates_an_unreadable_cross_scope_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = user_mode_context(tmp.path());
        let system_layout = ctx.visible_system_layout().clone();
        write_cosh_ng_state(&system_layout, true);
        let rpm_bin = system_layout
            .prefix
            .join("usr/libexec/anolisa/cosh-ng/cosh-shell");
        write_executable(&rpm_bin, fake_export_script());
        std::fs::create_dir_all(&system_layout.central_log).expect("log blocker dir");

        let home = tmp.path().join("home");
        let diag_dir = home.join("diagnostics");
        let payload = build_payload_with(Some("cosh-ng"), DEFAULT_LIMIT, &ctx, |layout| {
            let binary = cosh_ng::resolve_binary_with(None, layout, None);
            cosh_ng::collect_impl(
                binary,
                Some(diag_dir.clone()),
                &home,
                std::time::Duration::from_secs(10),
            )
        })
        .expect("an unreadable cross-scope log must not abort the report");

        assert!(payload.recent_logs.is_empty());
        let warning = payload
            .recent_logs_unavailable
            .as_ref()
            .expect("log warning");
        assert!(warning.contains("unavailable"), "{warning}");
        assert!(
            payload.markdown.contains("- unavailable:"),
            "{}",
            payload.markdown
        );
        assert!(
            payload.markdown.contains("## cosh-ng Diagnostics"),
            "{}",
            payload.markdown
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(json["recent_logs_unavailable"].as_str().is_some(), "{json}");
        assert!(
            matches!(
                payload.cosh_ng_diagnostics,
                Some(cosh_ng::CoshNgDiagnostics::Available { .. })
            ),
            "the bridge must still produce the bundle: {:?}",
            payload.cosh_ng_diagnostics
        );
        assert_eq!(
            diag_dir.read_dir().expect("diag dir").count(),
            1,
            "exactly one bundle in the user diagnostics dir"
        );
    }

    /// `--dry-run` must not run the collector: no exporter subprocess, no
    /// diagnostics directory or bundle, and the report marks the skip
    /// explicitly instead of disguising it as unavailable.
    #[test]
    fn dry_run_skips_the_cosh_ng_collector() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::context_for_root(
            tmp.path(),
            crate::context::InstallMode::User,
            Some(tmp.path().join("sysroot")),
            crate::test_support::TestContextOptions {
                dry_run: true,
                ..Default::default()
            },
        );
        let system_layout = ctx.visible_system_layout().clone();
        write_cosh_ng_state(&system_layout, true);

        let payload = build_payload_with(Some("cosh-ng"), DEFAULT_LIMIT, &ctx, |_| {
            panic!("the collector must not run under --dry-run")
        })
        .expect("payload");

        assert_eq!(
            payload.cosh_ng_diagnostics,
            Some(cosh_ng::CoshNgDiagnostics::Skipped)
        );
        assert!(
            payload.markdown.contains("status: skipped (dry run)"),
            "{}",
            payload.markdown
        );
        assert!(
            !payload.markdown.contains("status: unavailable"),
            "the skip must not be disguised as unavailable: {}",
            payload.markdown
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            json["cosh_ng_diagnostics"]["status"].as_str(),
            Some("skipped")
        );
        // Nothing was written anywhere below the fixture root besides the
        // state file the test itself created.
        let user_layout = common::resolve_layout(&ctx);
        assert!(
            !user_layout.state_dir.join("diagnostics").exists(),
            "dry run must not create the diagnostics directory"
        );
    }

    /// A name no visible scope and no component index knows must still be
    /// rejected rather than producing an empty report.
    #[test]
    fn build_payload_rejects_a_component_unknown_everywhere() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::context_for_root(
            tmp.path(),
            crate::context::InstallMode::System,
            Some(tmp.path().to_path_buf()),
            crate::test_support::TestContextOptions {
                quiet: false,
                no_color: false,
                ..Default::default()
            },
        );

        let err = build_payload_with(Some("no-such-component"), DEFAULT_LIMIT, &ctx, |_| {
            panic!("the bridge must not run for an unknown component")
        })
        .expect_err("unknown component must fail");

        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("component index is unavailable"),
            "{}",
            err.reason()
        );
    }

    /// The reproduction checklist lines are promises: with a binary path
    /// containing spaces and shell metacharacters the rendered commands
    /// must run verbatim under `sh -c`.
    #[cfg(unix)]
    #[test]
    fn rendered_reproduction_commands_execute_with_metachar_binary_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = tmp.path().join("lib exec;$(rm -rf)").join("cosh-shell");
        write_executable(&bin, "#!/bin/sh\nexit 0\n");
        let base = available_diagnostics();
        let cosh_ng::CoshNgDiagnostics::Available {
            bundle_path,
            overall_severity,
            findings,
            unavailable_collectors,
            manifest,
            ..
        } = base
        else {
            unreachable!("fixture is the available variant");
        };
        let diagnostics = cosh_ng::CoshNgDiagnostics::Available {
            binary_path: bin.display().to_string(),
            bundle_path,
            overall_severity,
            findings,
            unavailable_collectors,
            manifest,
        };

        let markdown = render_markdown(&env_summary(), &[], &[], None, Some(&diagnostics));

        let quoted = format!("'{}'", bin.display());
        assert!(markdown.contains(&format!("{quoted} doctor")), "{markdown}");
        assert!(
            markdown.contains(&format!("{quoted} diagnostics export --since-hours 1")),
            "{markdown}"
        );
        let block = markdown
            .split(
                "```sh
",
            )
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("sh code block");
        let mut executed = 0;
        for line in block.lines().filter(|line| !line.trim().is_empty()) {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(line)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("spawn sh");
            assert!(status.success(), "rendered command failed: {line}");
            executed += 1;
        }
        assert_eq!(executed, 2, "both reproduction commands must be exercised");
    }
}
