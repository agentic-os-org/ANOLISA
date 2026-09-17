//! On-demand doctor orchestration shared by the `cosh-shell doctor` CLI, the
//! `/health` slash command, and the diagnostics bundle export.
//!
//! All entry points call [`run_doctor_report`], which assembles a single
//! [`HealthScanReport`] from the existing resource collectors plus the
//! environment collectors (provider/config/hooks/PTY/permissions). The CLI
//! additionally renders it with [`format_doctor_report_plain`]; the slash
//! command renders the same report as an inline card; the diagnostics bundle
//! projects it into the stable bundle schema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::CoshConfig;
use crate::diagnostics::health::env_collectors::{
    log_file_paths, parse_log_header, read_log_tail, recent_crash_records, ts_label,
    LOG_TAIL_MAX_BYTES,
};
use crate::diagnostics::health::{
    doctor_status, finding_remediation, run_env_collectors, run_health_scan_with_options,
    HealthCollector, HealthReportBuilder, HealthScanMode, HealthScanOptions, HealthScanReport,
    HealthSeverity, HealthUnavailableReason,
};
use crate::diagnostics::run_registry::{pid_alive, read_entries};
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
/// Layout: title, `status: <token>`, the static fact header (version, host,
/// runtime, routing, logs, crashes), `checks: <names>`, one line per finding
/// (with an indented remediation line when available), one line per check that
/// could not run, and an "all checks passed" line when nothing needs
/// attention. When findings or unavailable checks exist, a final line points
/// at the diagnostics export for evidence collection.
///
/// The fact header is assembled here rather than in collectors on purpose:
/// the machine contract (`status:`/`checks:` lines, finding labels, exit
/// codes) and the bundle schema stay untouched, while the header summarizes
/// the same files the collectors read (run registry, daily logs, crash logs)
/// without changing `checks_done` semantics.
pub(crate) fn format_doctor_report_plain(
    config: &CoshConfig,
    report: &HealthScanReport,
    i18n: I18n,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(i18n.t(MessageId::DoctorTitle).to_string());
    lines.push(format!(
        "{}: {}",
        i18n.t(MessageId::DoctorStatusLabel),
        doctor_status(report).token()
    ));
    lines.extend(render_static_facts(config, i18n));
    lines.push(String::new());

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
    } else {
        lines.push(String::new());
        lines.push(i18n.t(MessageId::DoctorExportHint).to_string());
    }

    lines
}

/// The fact header between the status line and the checks list. Every read
/// here is local and side-effect free: version/host come from the binary,
/// runtime/routing from the run registry, logs from the daily log files, and
/// crashes from the crash logs. A missing HOME or missing files degrade to
/// the corresponding "none"/"unavailable"/"no files" wording.
fn render_static_facts(config: &CoshConfig, i18n: I18n) -> Vec<String> {
    let version = env!("CARGO_PKG_VERSION");
    let host = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
    let runtime = runtime_summary();
    let summary: &str = if runtime.is_empty() {
        i18n.t(MessageId::DoctorRuntimeSummaryNone)
    } else {
        &runtime
    };
    vec![
        i18n.format(
            MessageId::DoctorVersionLine,
            &[("shell_version", version), ("core_version", version)],
        ),
        i18n.format(MessageId::DoctorHostLine, &[("host", host.as_str())]),
        i18n.format(MessageId::DoctorRuntimeLine, &[("summary", summary)]),
        routing_line(i18n),
        logs_line(config, i18n),
        crashes_line(i18n),
    ]
}

/// Summarizes live (pid-alive) run-registry entries as e.g.
/// `1 shell (pid 12345) + core (pid 12346, session a1b2c3d4…)`. Empty when
/// nothing is alive.
fn runtime_summary() -> String {
    let mut shell_pids: Vec<u32> = Vec::new();
    let mut cores: Vec<(u32, Option<String>)> = Vec::new();
    for entry in read_entries() {
        if !pid_alive(entry.pid) {
            continue;
        }
        match entry.kind.as_str() {
            "shell" => shell_pids.push(entry.pid),
            "core" => cores.push((entry.pid, entry.session_id.clone())),
            _ => {}
        }
    }
    shell_pids.sort_unstable();
    cores.sort_unstable_by_key(|(pid, _)| *pid);

    let mut parts = Vec::new();
    if !shell_pids.is_empty() {
        let plural = if shell_pids.len() == 1 { "" } else { "s" };
        let pids = shell_pids
            .iter()
            .map(|pid| format!("pid {pid}"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("{} shell{plural} ({pids})", shell_pids.len()));
    }
    if !cores.is_empty() {
        let list = cores
            .iter()
            .map(|(pid, session)| match session {
                Some(session) => format!("pid {pid}, session {}", short_session(session)),
                None => format!("pid {pid}"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        if cores.len() == 1 {
            parts.push(format!("core ({list})"));
        } else {
            parts.push(format!("{} cores ({list})", cores.len()));
        }
    }
    parts.join(" + ")
}

/// Truncates a session id for the summary line (`a1b2c3d4e5…`).
fn short_session(session: &str) -> String {
    if session.chars().count() > 8 {
        let head: String = session.chars().take(8).collect();
        format!("{head}…")
    } else {
        session.to_string()
    }
}

/// Routing facts of the first live shell entry, or the "live probe
/// unavailable" wording when no live shell is registered.
fn routing_line(i18n: I18n) -> String {
    let mut shells: Vec<_> = read_entries()
        .into_iter()
        .filter(|entry| entry.kind == "shell" && pid_alive(entry.pid))
        .collect();
    shells.sort_by_key(|entry| entry.pid);
    let Some(shell) = shells.first() else {
        return i18n.t(MessageId::DoctorRoutingUnavailable).to_string();
    };
    let facts = shell.routing.clone().unwrap_or_default();
    let ai = if facts.ai_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let integration = if facts.integration.is_empty() {
        "n/a"
    } else {
        facts.integration.as_str()
    };
    let cnf = facts.cnf_handler.as_deref().unwrap_or("n/a");
    let route = facts.last_route.as_deref().unwrap_or("n/a");
    i18n.format(
        MessageId::DoctorRoutingLine,
        &[
            ("ai", ai),
            ("integration", integration),
            ("cnf", cnf),
            ("route", route),
        ],
    )
}

/// Log facts: the effective level, the most recent write across the daily
/// files, and 24h ERROR counts per component (`3 (core, last 09:32), 0
/// (shell)`).
fn logs_line(config: &CoshConfig, i18n: I18n) -> String {
    let now_ms = now_millis();
    let files: Vec<_> = log_file_paths()
        .into_iter()
        .filter(|(_, path)| path.exists())
        .collect();
    if files.is_empty() {
        return i18n.t(MessageId::DoctorLogsNoFiles).to_string();
    }

    let mut last_write_ms: Option<u128> = None;
    let mut errors_by_kind: BTreeMap<&str, (u64, Option<u128>)> = BTreeMap::new();
    for (kind, path) in &files {
        if let Ok(modified) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
            let ms = modified
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default();
            last_write_ms = Some(last_write_ms.map_or(ms, |current| current.max(ms)));
        }
        let Some(tail) = read_log_tail(path, LOG_TAIL_MAX_BYTES) else {
            continue;
        };
        let slot = errors_by_kind.entry(short_kind(kind)).or_default();
        for line in tail.lines() {
            if let Some(("ERROR", ts_ms)) = parse_log_header(line, now_ms) {
                slot.0 += 1;
                slot.1 = Some(slot.1.map_or(ts_ms, |current| current.max(ts_ms)));
            }
        }
    }

    let last = last_write_ms.map_or_else(|| "n/a".to_string(), ts_label);
    let errors = errors_by_kind
        .iter()
        .map(|(kind, (count, last_ts))| match last_ts {
            Some(ts) => format!("{count} ({kind}, last {})", short_time(*ts)),
            None => format!("{count} ({kind})"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    i18n.format(
        MessageId::DoctorLogsLine,
        &[
            ("level", config.log_level.as_str()),
            ("last", last.as_str()),
            ("errors", errors.as_str()),
        ],
    )
}

/// Crash summary from the 24h window, e.g. `crashes: 1 in the last 24h
/// (cosh-shell, 09:31)`.
fn crashes_line(i18n: I18n) -> String {
    let records = recent_crash_records(now_millis());
    let count = records.len().to_string();
    let detail = if records.is_empty() {
        String::new()
    } else {
        let parts = records
            .iter()
            .map(|record| format!("{}, {}", record.kind, short_time(record.ts_ms)))
            .collect::<Vec<_>>()
            .join("; ");
        format!(" ({parts})")
    };
    i18n.format(
        MessageId::DoctorCrashesLine,
        &[("count", count.as_str()), ("detail", detail.as_str())],
    )
}

/// `cosh-shell.log`/`cosh-core.log` → `shell`/`core` for the summary line.
fn short_kind(file_kind: &str) -> &str {
    file_kind
        .strip_prefix("cosh-")
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or(file_kind)
}

/// Time-of-day (`09:32`) for compact summary timestamps.
fn short_time(ts_ms: u128) -> String {
    let label = ts_label(ts_ms);
    match label.split_once(' ') {
        Some((_, time)) => time.chars().take(5).collect(),
        None => label,
    }
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
        HealthCollector::Runtime => "runtime",
        HealthCollector::Logs => "logs",
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

        let lines = format_doctor_report_plain(&CoshConfig::default(), &report, en());
        let joined = lines.join("\n");
        assert!(joined.contains("status: healthy"), "{joined}");
        assert!(joined.contains("all checks passed"), "{joined}");
        assert!(joined.contains("checks: provider"), "{joined}");
        assert!(!joined.contains("next step:"), "{joined}");
    }

    #[test]
    fn warning_finding_renders_remediation_and_exit_one() {
        let mut report = HealthScanReport::new("health-2", 0);
        report.findings.push(warning_finding());
        report.recompute_overall_severity();
        assert_eq!(report_exit_code(&report), 1);

        let lines = format_doctor_report_plain(&CoshConfig::default(), &report, en());
        let joined = lines.join("\n");
        assert!(joined.contains("status: warning"), "{joined}");
        assert!(joined.contains("[warning]"), "{joined}");
        assert!(joined.contains("remediation:"), "{joined}");
        assert!(joined.contains("cosh-core"), "{joined}");
        assert!(joined.contains("next step:"), "{joined}");
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

        let joined = format_doctor_report_plain(&CoshConfig::default(), &report, en()).join("\n");
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

        let joined = format_doctor_report_plain(&CoshConfig::default(), &report, en()).join("\n");
        assert!(joined.contains("status: warning"), "{joined}");
        assert!(joined.contains("provider:"), "{joined}");
        assert!(joined.contains("command missing"), "{joined}");
    }

    #[test]
    fn static_facts_render_from_run_registry_logs_and_crashes() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let original_home = std::env::var_os("HOME");
        let home =
            std::env::temp_dir().join(format!("cosh-doctor-static-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        // Live shell + core entries (both carry this process's pid so the
        // zero-signal probe reports them alive).
        let pid = std::process::id();
        let run_dir = home.join(".copilot-shell/run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let shell = crate::diagnostics::run_registry::RunEntry {
            kind: "shell".to_string(),
            pid,
            ppid: None,
            version: "test".to_string(),
            start_ts_ms: 0,
            shell_kind: Some("zsh".to_string()),
            mode: None,
            session_id: Some("abcd1234".to_string()),
            core_pid: Some(pid),
            owner_shell_pid: None,
            routing: Some(crate::diagnostics::run_registry::RoutingFacts {
                ai_enabled: true,
                integration: "enhanced".to_string(),
                ..Default::default()
            }),
        };
        std::fs::write(
            run_dir.join(format!("shell-{pid}.json")),
            serde_json::to_string(&shell).unwrap(),
        )
        .unwrap();
        let core = crate::diagnostics::run_registry::RunEntry {
            kind: "core".to_string(),
            pid,
            ppid: None,
            version: "test".to_string(),
            start_ts_ms: 0,
            shell_kind: None,
            mode: Some("persistent".to_string()),
            session_id: Some("session-abcdefgh".to_string()),
            core_pid: None,
            owner_shell_pid: Some(pid),
            routing: None,
        };
        std::fs::write(
            run_dir.join(format!("core-{pid}.json")),
            serde_json::to_string(&core).unwrap(),
        )
        .unwrap();

        // One recent ERROR line in today's cosh-core log and one crash record.
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let log_dir = home.join(".copilot-shell/logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        std::fs::write(
            log_dir.join(format!("cosh-core.log.{today}")),
            format!("{ts} ERROR provider timeout\n"),
        )
        .unwrap();
        std::fs::write(
            home.join(".copilot-shell/cosh-shell-crash.log"),
            format!(
                "{{\"ts\": {}, \"panic\": \"boom\"}}\n",
                now.timestamp_millis()
            ),
        )
        .unwrap();

        let report = HealthScanReport::new("health-facts", 0);
        let joined = format_doctor_report_plain(&CoshConfig::default(), &report, en()).join("\n");
        assert!(joined.contains("version: cosh-shell"), "{joined}");
        assert!(joined.contains("host: "), "{joined}");
        assert!(
            joined.contains(&format!(
                "1 shell (pid {pid}) + core (pid {pid}, session session-…"
            )),
            "{joined}"
        );
        assert!(joined.contains("ai=enabled"), "{joined}");
        assert!(joined.contains("integration=enhanced"), "{joined}");
        assert!(joined.contains("24h errors: 1 (core, last"), "{joined}");
        assert!(
            joined.contains("1 in the last 24h (cosh-shell,"),
            "{joined}"
        );
        assert!(!joined.contains("next step:"), "{joined}");

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
