use std::env;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::HealthConfig;

use super::builder::HealthReportBuilder;
use super::collectors::{
    collect_configured_services, collect_cpu, collect_disk, collect_host, collect_kernel_signals,
    collect_memory,
};
use super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
    HealthSeverity, HealthUnavailableReason,
};
use super::recommendation::{preview_try_recommendations, record_visible_try_recommendations};
use super::rules::apply_judgement_rules;
use super::suppression::{
    current_time_ms, health_machine_id, health_suppression_store_path, host_id_for_report,
    HealthSuppressionStore,
};

const DEFAULT_SCAN_BUDGET: Duration = Duration::from_secs(3);
const HEALTH_SCAN_ENV: &str = "COSH_SHELL_HEALTH_SCAN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthScanMode {
    Disabled,
    Live,
    Fixture(String),
}

#[derive(Debug, Clone)]
pub(crate) struct HealthScanOptions {
    pub(crate) started_at_ms: u128,
    pub(crate) now_epoch_seconds: u64,
    pub(crate) total_budget: Duration,
    pub(crate) mode: HealthScanMode,
}

impl HealthScanOptions {
    pub(crate) fn from_env() -> Self {
        Self {
            started_at_ms: now_millis(),
            now_epoch_seconds: now_seconds(),
            total_budget: DEFAULT_SCAN_BUDGET,
            mode: health_scan_mode_from_env(),
        }
    }
}

pub(crate) fn health_scan_mode_from_env() -> HealthScanMode {
    match env::var(HEALTH_SCAN_ENV) {
        Ok(value) => health_scan_mode_from_value(Some(value.as_str())),
        _ => HealthScanMode::Live,
    }
}

pub(crate) fn run_health_scan(config: &HealthConfig) -> Option<HealthScanReport> {
    run_health_scan_with_options(config, HealthScanOptions::from_env())
}

pub(crate) fn startup_health_scan_enabled_for_env(config: &HealthConfig) -> bool {
    if !config.enabled {
        return false;
    }
    match health_scan_mode_from_env() {
        HealthScanMode::Disabled => false,
        HealthScanMode::Fixture(_) => true,
        HealthScanMode::Live => cfg!(target_os = "linux"),
    }
}

pub(crate) fn spawn_startup_health_scan(
    config: HealthConfig,
) -> mpsc::Receiver<Option<HealthScanReport>> {
    let (sender, receiver) = mpsc::channel();
    if matches!(health_scan_mode_from_env(), HealthScanMode::Fixture(_)) {
        let report = startup_health_report(&config);
        let _ = sender.send(report);
        return receiver;
    }
    thread::spawn(move || {
        let report = startup_health_report(&config);
        let _ = sender.send(report);
    });
    receiver
}

fn startup_health_report(config: &HealthConfig) -> Option<HealthScanReport> {
    run_health_scan(config).map(|mut report| {
        let mut suppression = HealthSuppressionStore::load_default();
        let machine_id = health_machine_id();
        let host_id = host_id_for_report(&report, machine_id.as_deref());
        preview_try_recommendations(
            &mut report,
            config,
            &mut suppression,
            &host_id,
            current_time_ms(),
        );
        report
    })
}

pub(crate) fn record_startup_health_recommendations(report: &HealthScanReport) {
    if report.try_items.is_empty() {
        return;
    }
    let mut suppression = HealthSuppressionStore::load_default();
    let machine_id = health_machine_id();
    let host_id = host_id_for_report(report, machine_id.as_deref());
    record_visible_try_recommendations(report, &mut suppression, &host_id, current_time_ms());
    if let Some(path) = health_suppression_store_path() {
        let _ = suppression.write_to_path(&path);
    }
}

pub(crate) fn run_health_scan_with_options(
    config: &HealthConfig,
    options: HealthScanOptions,
) -> Option<HealthScanReport> {
    if !config.enabled || options.mode == HealthScanMode::Disabled {
        return None;
    }
    let report = match &options.mode {
        HealthScanMode::Disabled => return None,
        HealthScanMode::Fixture(name) => fixture_report(name, config, &options),
        HealthScanMode::Live => live_report(config, &options),
    };
    Some(report)
}

fn live_report(config: &HealthConfig, options: &HealthScanOptions) -> HealthScanReport {
    let scan_started = Instant::now();
    let mut builder = HealthReportBuilder::for_started_at(options.started_at_ms);
    builder.set_role(config.role.clone());
    let (sender, receiver) = mpsc::channel();
    let deadline = Instant::now() + options.total_budget;

    spawn_collector("host", sender.clone(), options.started_at_ms, collect_host);
    let mut expected = vec![
        ("host", HealthCollector::Host, HealthSeverity::Unavailable),
        ("cpu", HealthCollector::Cpu, HealthSeverity::Degraded),
        ("memory", HealthCollector::Memory, HealthSeverity::Degraded),
        ("disk", HealthCollector::Disk, HealthSeverity::Degraded),
        (
            "kernel_signal",
            HealthCollector::KernelSignal,
            HealthSeverity::Unavailable,
        ),
    ];
    spawn_collector("cpu", sender.clone(), options.started_at_ms, collect_cpu);
    spawn_collector(
        "memory",
        sender.clone(),
        options.started_at_ms,
        collect_memory,
    );

    let disk_config = config.clone();
    spawn_collector(
        "disk",
        sender.clone(),
        options.started_at_ms,
        move |builder| collect_disk(builder, &disk_config),
    );

    let now_epoch_seconds = options.now_epoch_seconds;
    spawn_collector(
        "kernel_signal",
        sender.clone(),
        options.started_at_ms,
        move |builder| collect_kernel_signals(builder, now_epoch_seconds),
    );

    if !config.services.is_empty() {
        expected.push((
            "configured_service",
            HealthCollector::ConfiguredService,
            HealthSeverity::Degraded,
        ));
        let service_config = config.clone();
        spawn_collector(
            "configured_service",
            sender.clone(),
            options.started_at_ms,
            move |builder| collect_configured_services(builder, &service_config),
        );
    }
    drop(sender);

    for _ in 0..expected.len() {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(report) => {
                builder.merge_report(report);
            }
            Err(_) => break,
        }
    }
    mark_missed_collectors_as_timeout(&mut builder, &expected, options.total_budget.as_millis());
    let elapsed_ms = scan_started
        .elapsed()
        .as_millis()
        .min(options.total_budget.as_millis());
    let mut report = builder.finish(options.started_at_ms + elapsed_ms);
    apply_judgement_rules(&mut report, config);
    report
}

fn spawn_collector<F>(
    _name: &'static str,
    sender: mpsc::Sender<HealthScanReport>,
    started_at_ms: u128,
    run: F,
) where
    F: FnOnce(&mut HealthReportBuilder) + Send + 'static,
{
    thread::spawn(move || {
        let mut builder = HealthReportBuilder::for_started_at(started_at_ms);
        run(&mut builder);
        let report = builder.finish(started_at_ms);
        let _ = sender.send(report);
    });
}

fn mark_missed_collectors_as_timeout(
    builder: &mut HealthReportBuilder,
    expected: &[(&'static str, HealthCollector, HealthSeverity)],
    elapsed_ms: u128,
) {
    for (check_name, collector, severity) in expected {
        if builder_has_collector_result(builder.report(), check_name, *collector) {
            continue;
        }
        builder.add_unavailable(
            *collector,
            HealthUnavailableReason::Timeout,
            *severity,
            elapsed_ms,
        );
    }
}

fn builder_has_collector_result(
    report: &HealthScanReport,
    check_name: &str,
    collector: HealthCollector,
) -> bool {
    report.checks_done.iter().any(|check| check == check_name)
        || report
            .unavailable
            .iter()
            .any(|item| item.collector == collector)
}

fn fixture_report(
    name: &str,
    config: &HealthConfig,
    options: &HealthScanOptions,
) -> HealthScanReport {
    let source = fixture_source(name).unwrap_or_else(|| fixture_source("linux-healthy").unwrap());
    let mut builder = HealthReportBuilder::for_started_at(options.started_at_ms);
    builder.set_role(config.role.clone());
    load_fixture_source(source, &mut builder);
    let mut report = builder.finish(options.started_at_ms + 1);
    apply_judgement_rules(&mut report, config);
    report
}

fn fixture_source(name: &str) -> Option<&'static str> {
    match name {
        "linux-healthy" | "healthy" => Some(include_str!("fixtures/linux-healthy.health")),
        "linux-warning" | "warning" => Some(include_str!("fixtures/linux-warning.health")),
        "linux-critical" | "critical" => Some(include_str!("fixtures/linux-critical.health")),
        "linux-degraded" | "degraded" => Some(include_str!("fixtures/linux-degraded.health")),
        "linux-partial" | "partial" => Some(include_str!("fixtures/linux-partial.health")),
        _ => None,
    }
}

fn load_fixture_source(source: &str, builder: &mut HealthReportBuilder) {
    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(host) = line.strip_prefix("host=") {
            builder.set_host(host);
            continue;
        }
        if let Some(score) = line.strip_prefix("score=") {
            builder.set_health_score(score.parse::<u8>().ok());
            continue;
        }
        if let Some(check) = line.strip_prefix("check=") {
            builder.add_check_done(check);
            continue;
        }
        if let Some(rest) = line.strip_prefix("fact ") {
            load_fixture_fact(rest, builder);
            continue;
        }
        if let Some(rest) = line.strip_prefix("unavailable ") {
            load_fixture_unavailable(rest, builder);
        }
    }
}

fn load_fixture_fact(rest: &str, builder: &mut HealthReportBuilder) {
    let mut parts = rest.split_whitespace();
    let Some(category) = parts.next().and_then(parse_fixture_category) else {
        return;
    };
    let Some(key) = parts.next() else {
        return;
    };
    let Some(kind) = parts.next() else {
        return;
    };
    let Some(value) = parts
        .next()
        .and_then(|value| parse_fixture_value(kind, value))
    else {
        return;
    };
    let unit = parts
        .next()
        .filter(|unit| *unit != "-")
        .map(ToString::to_string);
    builder.add_fact(category, key, value, unit, HealthFactSource::Fixture, 0);
}

fn load_fixture_unavailable(rest: &str, builder: &mut HealthReportBuilder) {
    let mut parts = rest.split_whitespace();
    let Some(collector) = parts.next().and_then(parse_fixture_collector) else {
        return;
    };
    let Some(reason) = parts.next().and_then(parse_fixture_unavailable_reason) else {
        return;
    };
    let Some(severity) = parts.next().and_then(HealthSeverity::parse) else {
        return;
    };
    let elapsed_ms = parts
        .next()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or_default();
    builder.add_unavailable(collector, reason, severity, elapsed_ms);
}

fn parse_fixture_category(value: &str) -> Option<HealthFactCategory> {
    match value {
        "host" => Some(HealthFactCategory::Host),
        "cpu" => Some(HealthFactCategory::Cpu),
        "memory" => Some(HealthFactCategory::Memory),
        "disk" => Some(HealthFactCategory::Disk),
        "kernel" => Some(HealthFactCategory::Kernel),
        "service" => Some(HealthFactCategory::Service),
        "provider" => Some(HealthFactCategory::Provider),
        "config" => Some(HealthFactCategory::Config),
        "hooks" => Some(HealthFactCategory::Hooks),
        "pty" => Some(HealthFactCategory::Pty),
        "permissions" => Some(HealthFactCategory::Permissions),
        _ => None,
    }
}

fn parse_fixture_value(kind: &str, value: &str) -> Option<HealthFactValue> {
    match kind {
        "str" => Some(HealthFactValue::String(value.to_string())),
        "i64" => value.parse::<i64>().ok().map(HealthFactValue::Integer),
        "u64" => value.parse::<u64>().ok().map(HealthFactValue::Unsigned),
        "f64" => value.parse::<f64>().ok().map(HealthFactValue::Float),
        "bool" => value.parse::<bool>().ok().map(HealthFactValue::Bool),
        _ => None,
    }
}

fn parse_fixture_collector(value: &str) -> Option<HealthCollector> {
    match value {
        "host" => Some(HealthCollector::Host),
        "cpu" => Some(HealthCollector::Cpu),
        "memory" => Some(HealthCollector::Memory),
        "disk" => Some(HealthCollector::Disk),
        "kernel_signal" => Some(HealthCollector::KernelSignal),
        "configured_service" => Some(HealthCollector::ConfiguredService),
        "provider" => Some(HealthCollector::Provider),
        "config" => Some(HealthCollector::Config),
        "hooks" => Some(HealthCollector::Hooks),
        "pty" => Some(HealthCollector::Pty),
        "permissions" => Some(HealthCollector::Permissions),
        _ => None,
    }
}

fn parse_fixture_unavailable_reason(value: &str) -> Option<HealthUnavailableReason> {
    match value {
        "unsupported" => Some(HealthUnavailableReason::Unsupported),
        "permission_denied" => Some(HealthUnavailableReason::PermissionDenied),
        "command_missing" => Some(HealthUnavailableReason::CommandMissing),
        "timeout" => Some(HealthUnavailableReason::Timeout),
        "parse_error" => Some(HealthUnavailableReason::ParseError),
        _ => None,
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn health_scan_mode_from_value(value: Option<&str>) -> HealthScanMode {
    match value {
        Some("0" | "false" | "off" | "disabled") => HealthScanMode::Disabled,
        Some(value) if value.starts_with("fixture:") => {
            HealthScanMode::Fixture(value["fixture:".len()..].to_string())
        }
        _ => HealthScanMode::Live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_returns_no_report() {
        let config = HealthConfig {
            enabled: false,
            ..HealthConfig::default()
        };
        let report = run_health_scan_with_options(
            &config,
            HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::from_millis(1),
                mode: HealthScanMode::Live,
            },
        );

        assert!(report.is_none());
    }

    #[test]
    fn parses_env_scan_modes() {
        assert_eq!(
            health_scan_mode_from_value(Some("0")),
            HealthScanMode::Disabled
        );
        assert_eq!(
            health_scan_mode_from_value(Some("fixture:linux-warning")),
            HealthScanMode::Fixture("linux-warning".to_string())
        );
        assert_eq!(health_scan_mode_from_value(Some("1")), HealthScanMode::Live);
        assert_eq!(health_scan_mode_from_value(None), HealthScanMode::Live);
    }

    #[test]
    fn fixture_warning_generates_partial_report_and_findings() {
        let report = run_health_scan_with_options(
            &HealthConfig::default(),
            HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::from_millis(1),
                mode: HealthScanMode::Fixture("linux-warning".to_string()),
            },
        )
        .expect("fixture report");

        assert_eq!(report.host.as_deref(), Some("fixture-linux-warning"));
        assert_eq!(report.health_score, Some(68));
        assert!(report.checks_done.contains(&"fixture".to_string()));
        assert!(report.findings.iter().any(|finding| finding.id == "J06"));
    }

    #[test]
    fn named_fixture_files_cover_expected_severities() {
        let cases = [
            ("linux-healthy", HealthSeverity::Ok),
            ("linux-warning", HealthSeverity::Warning),
            ("linux-critical", HealthSeverity::Critical),
            ("linux-degraded", HealthSeverity::Degraded),
        ];
        for (name, severity) in cases {
            let report = run_health_scan_with_options(
                &HealthConfig::default(),
                HealthScanOptions {
                    started_at_ms: 1,
                    now_epoch_seconds: 100,
                    total_budget: Duration::from_millis(1),
                    mode: HealthScanMode::Fixture(name.to_string()),
                },
            )
            .expect("fixture report");

            assert_eq!(report.overall_severity, severity, "{name}: {report:?}");
            assert!(
                report
                    .facts
                    .iter()
                    .all(|fact| matches!(fact.source, HealthFactSource::Fixture)),
                "{name}: {report:?}"
            );
            assert!(report.checks_done.contains(&"fixture".to_string()));
        }
    }

    #[test]
    fn live_scan_returns_partial_result_within_total_budget() {
        let started = Instant::now();
        let report = run_health_scan_with_options(
            &HealthConfig::default(),
            HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::from_millis(1),
                mode: HealthScanMode::Live,
            },
        )
        .expect("live report");

        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(report.elapsed_ms <= 1, "{report:?}");
    }

    #[test]
    fn live_scan_records_collectors_missed_by_total_budget() {
        let report = run_health_scan_with_options(
            &HealthConfig::default(),
            HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::ZERO,
                mode: HealthScanMode::Live,
            },
        )
        .expect("live report");

        assert_eq!(report.elapsed_ms, 0);
        for collector in [
            HealthCollector::Host,
            HealthCollector::Cpu,
            HealthCollector::Memory,
            HealthCollector::Disk,
            HealthCollector::KernelSignal,
        ] {
            assert!(
                report.unavailable.iter().any(|item| {
                    item.collector == collector && item.reason == HealthUnavailableReason::Timeout
                }),
                "{collector:?}: {report:?}"
            );
        }
    }
}
