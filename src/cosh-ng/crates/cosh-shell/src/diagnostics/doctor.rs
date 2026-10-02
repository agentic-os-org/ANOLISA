//! On-demand doctor orchestration shared by the `cosh-shell doctor` CLI, the
//! `/health` slash command, and the diagnostics bundle export.
//!
//! All entry points call [`run_doctor_report`], which assembles a single
//! [`HealthScanReport`] from the existing resource collectors plus the
//! environment collectors (provider/config/hooks/PTY/permissions). The CLI
//! additionally renders it with [`format_doctor_report_plain`]; the slash
//! command renders the same report as an inline card; the diagnostics bundle
//! projects it into the stable bundle schema.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::CoshConfig;
use crate::diagnostics::health::{
    doctor_status, finding_remediation, run_env_collectors, run_health_scan_with_options,
    HealthCollector, HealthReportBuilder, HealthScanMode, HealthScanOptions, HealthScanReport,
    HealthSeverity, HealthUnavailableReason,
};
use crate::{I18n, MessageId};

/// Assemble the unified on-demand health report.
///
/// - Fixture mode (`COSH_SHELL_HEALTH_SCAN=fixture:*`): the fixture fully
///   determines the report; environment collectors are skipped so results are
///   deterministic.
/// - Otherwise: run the resource scan (when enabled) and always run the
///   environment collectors, merging both into one report.
pub(crate) fn run_doctor_report(config: &CoshConfig, cwd: &Path) -> HealthScanReport {
    let options = HealthScanOptions::from_env();
    let started_at_ms = options.started_at_ms;
    let fixture_mode = matches!(options.mode, HealthScanMode::Fixture(_));

    let mut builder = HealthReportBuilder::for_started_at(started_at_ms);
    if let Some(base) = run_health_scan_with_options(&config.health, options) {
        builder.merge_report(base);
    }
    if !fixture_mode {
        run_env_collectors(&mut builder, config, cwd, 0);
    }

    builder.finish(now_millis().max(started_at_ms))
}

/// Run the doctor report against the process working directory, falling back
/// to `.` when it is unavailable.
///
/// Shared by the `cosh-shell doctor` CLI and the diagnostics bundle export so
/// both process-launched entry points keep identical cwd semantics; the
/// `/health` slash command instead passes the child shell cwd explicitly to
/// [`run_doctor_report`].
pub(crate) fn run_doctor_report_from_process_cwd(config: &CoshConfig) -> HealthScanReport {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_doctor_report(config, &cwd)
}

/// Render a report as human-readable plain-text lines for `cosh-shell doctor`.
///
/// Layout: title, `status: <token>`, `checks: <names>`, one line per finding
/// (with an indented remediation line when available), one line per check that
/// could not run, and an "all checks passed" line when nothing needs attention.
pub(crate) fn format_doctor_report_plain(report: &HealthScanReport, i18n: I18n) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(i18n.t(MessageId::DoctorTitle).to_string());
    lines.push(format!(
        "{}: {}",
        i18n.t(MessageId::DoctorStatusLabel),
        doctor_status(report).token()
    ));

    if !report.checks_done.is_empty() {
        let mut checks = report.checks_done.clone();
        checks.sort();
        checks.dedup();
        lines.push(format!(
            "{}: {}",
            i18n.t(MessageId::DoctorChecksLabel),
            checks.join(", ")
        ));
    }

    let mut findings: Vec<_> = report.findings.iter().collect();
    findings.sort_by_key(|finding| {
        (
            std::cmp::Reverse(finding.severity.precedence()),
            finding.id.clone(),
        )
    });
    for finding in findings {
        let args: Vec<(&str, &str)> = finding
            .detail_args
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        lines.push(format!(
            "[{}] {}",
            finding.severity.label(),
            i18n.format(finding.title_id.to_i18n(), &args)
        ));
        if let Some(remediation) = finding_remediation(finding, i18n) {
            lines.push(format!(
                "  {}: {}",
                i18n.t(MessageId::DoctorRemediationLabel),
                remediation
            ));
        }
    }

    for item in &report.unavailable {
        lines.push(format!(
            "[{}] {}: {}",
            item.severity.label(),
            collector_token(item.collector),
            i18n.t(unavailable_reason_message(item.reason))
        ));
    }

    if report.findings.is_empty() && report.unavailable.is_empty() {
        lines.push(i18n.t(MessageId::DoctorAllHealthy).to_string());
    }

    lines
}

fn collector_token(collector: HealthCollector) -> &'static str {
    match collector {
        HealthCollector::Host => "host",
        HealthCollector::Cpu => "cpu",
        HealthCollector::Memory => "memory",
        HealthCollector::Disk => "disk",
        HealthCollector::KernelSignal => "kernel_signal",
        HealthCollector::ConfiguredService => "configured_service",
        HealthCollector::Provider => "provider",
        HealthCollector::Config => "config",
        HealthCollector::Hooks => "hooks",
        HealthCollector::Pty => "pty",
        HealthCollector::Permissions => "permissions",
    }
}

fn unavailable_reason_message(reason: HealthUnavailableReason) -> MessageId {
    match reason {
        HealthUnavailableReason::PermissionDenied => MessageId::HealthUnavailablePermissionDenied,
        HealthUnavailableReason::CommandMissing => MessageId::HealthUnavailableCommandMissing,
        HealthUnavailableReason::Timeout => MessageId::HealthUnavailableTimeout,
        HealthUnavailableReason::Unsupported => MessageId::HealthUnavailableUnsupported,
        HealthUnavailableReason::ParseError => MessageId::HealthUnavailableParseError,
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::Language;
    use crate::diagnostics::health::{
        report_exit_code, HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport,
        UnavailableCollector,
    };

    fn en() -> I18n {
        I18n::new(Language::EnUs)
    }

    fn warning_finding() -> HealthFinding {
        let mut args = BTreeMap::new();
        args.insert("adapter".to_string(), "cosh-core".to_string());
        HealthFinding {
            id: "env-provider".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Observation,
            title_id: HealthMessageId::HealthFindingProviderUnconfigured,
            detail_id: Some(HealthMessageId::HealthRemediationProvider),
            detail_args: args,
            evidence_fact_ids: vec!["provider.adapter".to_string()],
            suggested_try_ids: Vec::new(),
        }
    }

    #[test]
    fn healthy_report_renders_all_passed_and_exit_zero() {
        let mut report = HealthScanReport::new("health-1", 0);
        report.checks_done.push("provider".to_string());
        report.recompute_overall_severity();
        assert_eq!(report_exit_code(&report), 0);

        let lines = format_doctor_report_plain(&report, en());
        let joined = lines.join("\n");
        assert!(joined.contains("status: healthy"), "{joined}");
        assert!(joined.contains("all checks passed"), "{joined}");
        assert!(joined.contains("checks: provider"), "{joined}");
    }

    #[test]
    fn warning_finding_renders_remediation_and_exit_one() {
        let mut report = HealthScanReport::new("health-2", 0);
        report.findings.push(warning_finding());
        report.recompute_overall_severity();
        assert_eq!(report_exit_code(&report), 1);

        let lines = format_doctor_report_plain(&report, en());
        let joined = lines.join("\n");
        assert!(joined.contains("status: warning"), "{joined}");
        assert!(joined.contains("[warning]"), "{joined}");
        assert!(joined.contains("remediation:"), "{joined}");
        assert!(joined.contains("cosh-core"), "{joined}");
    }

    #[test]
    fn critical_finding_renders_error_status_and_exit_two() {
        let mut report = HealthScanReport::new("health-3", 0);
        report.findings.push(HealthFinding {
            id: "kernel".to_string(),
            severity: HealthSeverity::Critical,
            category: HealthFindingCategory::RootCause,
            title_id: HealthMessageId::HealthFindingKernelPanic,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: Vec::new(),
            suggested_try_ids: Vec::new(),
        });
        report.recompute_overall_severity();
        assert_eq!(report_exit_code(&report), 2);

        let joined = format_doctor_report_plain(&report, en()).join("\n");
        assert!(joined.contains("status: error"), "{joined}");
        assert!(joined.contains("[critical]"), "{joined}");
    }

    #[test]
    fn partially_unavailable_report_renders_warning_status() {
        let mut report = HealthScanReport::new("health-4", 0);
        report.unavailable.push(UnavailableCollector {
            collector: HealthCollector::Provider,
            reason: HealthUnavailableReason::CommandMissing,
            severity: HealthSeverity::Unavailable,
            elapsed_ms: 1,
        });
        report.recompute_overall_severity();
        assert_eq!(report_exit_code(&report), 1);

        let joined = format_doctor_report_plain(&report, en()).join("\n");
        assert!(joined.contains("status: warning"), "{joined}");
        assert!(joined.contains("provider:"), "{joined}");
        assert!(joined.contains("command missing"), "{joined}");
    }
}
