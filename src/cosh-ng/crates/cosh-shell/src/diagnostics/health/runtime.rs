mod fixtures;
pub(super) mod job;
mod plan;
pub(super) mod process;
mod protocol;
mod scan;
mod worker;
use fixtures::{fixture_report, fixture_report_for_context};

pub(crate) use scan::HealthScanHandle;
pub(crate) use worker::{
    run_private_health_worker, PRIVATE_WORKER_FLAG as PRIVATE_HEALTH_WORKER_FLAG,
};

use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::HealthConfig;

use super::builder::HealthReportBuilder;
use super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
    HealthSeverity, HealthUnavailableReason,
};
use super::rules::apply_judgement_rules;

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

pub(crate) fn start_startup_health_scan(config: HealthConfig) -> Option<HealthScanHandle> {
    let mut options = HealthScanOptions::from_env();
    options.total_budget = Duration::from_millis(500);
    start_scan_with_context(&config, options, plan::ScanContext::Startup, None)
}

pub(crate) fn validate_health_check_ids(disabled: &[String]) -> Result<(), String> {
    for id in disabled {
        if super::plan::definition(id).is_none() {
            return Err(format!("unknown health check: {id}"));
        }
    }
    Ok(())
}

pub(crate) fn health_check_ids() -> Vec<&'static str> {
    super::plan::CHECKS.iter().map(|check| check.id).collect()
}

pub(crate) fn bash_budget_remaining(
    deadline_boottime_us: u64,
    maximum: Duration,
) -> Option<Duration> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (deadline_boottime_us, maximum);
        None
    }
    #[cfg(target_os = "linux")]
    {
        let mut now: nix::libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_BOOTTIME, &mut now) } < 0
            || now.tv_nsec < 0
            || now.tv_nsec >= 1_000_000_000
        {
            return None;
        }
        let now_us = u64::try_from(now.tv_sec)
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add((now.tv_nsec as u64).checked_add(999)? / 1000)?;
        let remaining = Duration::from_micros(deadline_boottime_us.checked_sub(now_us)?);
        (!remaining.is_zero() && remaining <= maximum).then_some(remaining)
    }
}

pub(crate) fn start_login_health_scan_config_entry_until(
    absolute: Instant,
    disabled: &[String],
) -> Result<Option<HealthScanHandle>, String> {
    validate_health_check_ids(disabled)?;
    let mut options = HealthScanOptions::from_env();
    options.total_budget = absolute
        .saturating_duration_since(Instant::now())
        .min(Duration::from_millis(350));
    if options.mode == HealthScanMode::Disabled || options.total_budget.is_zero() {
        return Ok(None);
    }
    let deadline = job::JobDeadline::at_instant(absolute, options.total_budget)
        .map_err(|error| error.to_string())?;
    if deadline.remaining().unwrap_or_default().is_zero() {
        return Ok(None);
    }
    Ok(Some(HealthScanHandle::start(plan::WorkerRequest {
        scan_id: super::builder::health_scan_id(options.started_at_ms),
        started_at_ms: options.started_at_ms,
        now_epoch_seconds: options.now_epoch_seconds,
        deadline,
        context: plan::ScanContext::LoginBanner,
        config: HealthConfig::default(),
        environment: None,
        disabled: disabled.to_vec(),
        config_source: plan::ConfigSource::LoginConfigProjection,
        fixture_name: match options.mode {
            HealthScanMode::Fixture(name) => Some(name),
            _ => None,
        },
    })))
}

fn start_scan_with_context(
    config: &HealthConfig,
    options: HealthScanOptions,
    context: plan::ScanContext,
    environment: Option<(&crate::config::CoshConfig, &std::path::Path)>,
) -> Option<HealthScanHandle> {
    if (!config.enabled && context != plan::ScanContext::OnDemand)
        || options.mode == HealthScanMode::Disabled
    {
        return None;
    }
    if let HealthScanMode::Fixture(name) = &options.mode {
        return Some(HealthScanHandle::ready(fixture_report_for_context(
            name,
            config,
            &options,
            context,
            &[],
        )));
    }
    let deadline =
        match job::JobDeadline::after(options.total_budget.min(worker::MAX_WORKER_BUDGET)) {
            Ok(deadline) => deadline,
            Err(_) => return None,
        };
    Some(HealthScanHandle::start(plan::WorkerRequest {
        scan_id: super::builder::health_scan_id(options.started_at_ms),
        started_at_ms: options.started_at_ms,
        now_epoch_seconds: options.now_epoch_seconds,
        deadline,
        context,
        config: config.clone(),
        disabled: Vec::new(),
        config_source: crate::diagnostics::health::runtime::plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
        environment: environment
            .map(|(config, cwd)| plan::EnvironmentSnapshot::capture(config, cwd)),
    }))
}

pub(crate) fn run_supervised_doctor_report(
    config: &crate::config::CoshConfig,
    cwd: &std::path::Path,
) -> HealthScanReport {
    let started = Instant::now();
    let mut options = HealthScanOptions::from_env();
    let started_at_ms = options.started_at_ms;
    if let HealthScanMode::Fixture(name) = &options.mode {
        return fixture_report(name, &config.health, &options);
    }
    let mut health = config.health.clone();
    if options.mode == HealthScanMode::Disabled {
        health.enabled = false;
    }
    options.mode = HealthScanMode::Live;
    let budget = options.total_budget;
    if let Some(mut handle) = start_scan_with_context(
        &health,
        options,
        plan::ScanContext::OnDemand,
        Some((config, cwd)),
    ) {
        if let Some(report) = handle.finish_within(budget) {
            return report;
        }
    }
    let mut builder = HealthReportBuilder::for_started_at(started_at_ms);
    builder.add_unavailable(
        HealthCollector::Config,
        HealthUnavailableReason::ParseError,
        HealthSeverity::Unavailable,
        started.elapsed().as_millis(),
    );
    builder.finish(started_at_ms.saturating_add(started.elapsed().as_millis()))
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

#[cfg(test)]
fn run_health_scan_with_options(
    config: &HealthConfig,
    options: HealthScanOptions,
) -> Option<HealthScanReport> {
    let budget = options.total_budget;
    start_scan_with_context(config, options, plan::ScanContext::Resources, None)?
        .finish_within(budget)
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
    fn disabling_memory_keeps_only_swap_owned_projection() {
        let mut config = HealthConfig::default();
        config.checks.insert("resource.memory".into(), false);
        let report = run_health_scan_with_options(
            &config,
            HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::from_secs(1),
                mode: HealthScanMode::Fixture("linux-critical".into()),
            },
        )
        .unwrap();
        assert!(!report
            .facts
            .iter()
            .any(|fact| fact.key == "memory.available_mib"));
        assert!(report
            .facts
            .iter()
            .any(|fact| fact.key == "memory.swap_used_ratio"));
        assert!(!report
            .findings
            .iter()
            .any(|finding| matches!(finding.id.as_str(), "J05" | "J06")));
        assert!(!report.checks_done.iter().any(|id| id == "memory"));
        assert!(report.checks_done.iter().any(|id| id == "swap"));
        for finding in &report.findings {
            assert!(finding
                .evidence_fact_ids
                .iter()
                .all(|id| report.facts.iter().any(|fact| fact.id == *id)));
        }
    }

    #[test]
    fn login_worker_respects_user_config_before_collecting() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".copilot-shell")).unwrap();
        std::fs::write(
            directory.path().join(".copilot-shell/config.toml"),
            "[health]\nenabled=false\n[ui]\nlanguage='en-US'\n",
        )
        .unwrap();
        let previous = std::env::var_os("HOME");
        let previous_mode = std::env::var_os(HEALTH_SCAN_ENV);
        std::env::set_var("HOME", directory.path());
        std::env::set_var(HEALTH_SCAN_ENV, "1");
        let report = HealthScanHandle::start(plan::WorkerRequest {
            scan_id: "health-1".into(),
            started_at_ms: 1,
            now_epoch_seconds: 100,
            deadline: job::JobDeadline::after(Duration::from_secs(3)).unwrap(),
            context: plan::ScanContext::LoginBanner,
            config: HealthConfig::default(),
            environment: None,
            disabled: Vec::new(),
            config_source: plan::ConfigSource::LoginConfigProjection,
            fixture_name: None,
        })
        .finish_within(Duration::from_secs(3))
        .unwrap();
        match previous {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match previous_mode {
            Some(value) => std::env::set_var(HEALTH_SCAN_ENV, value),
            None => std::env::remove_var(HEALTH_SCAN_ENV),
        }
        let config = report
            .login_config
            .as_ref()
            .unwrap_or_else(|| panic!("actual worker config frame: {report:?}"));
        assert!(!config.health.enabled);
        assert_eq!(config.language, "en-US");
        assert!(report.facts.is_empty());
        assert!(report
            .check_results
            .iter()
            .all(|check| check.status == super::super::model::HealthCheckStatus::Disabled));
    }

    #[test]
    fn scan_finish_builds_candidates_without_entry_suppression() {
        let report = fixture_report(
            "linux-critical",
            &HealthConfig::default(),
            &HealthScanOptions {
                started_at_ms: 1,
                now_epoch_seconds: 100,
                total_budget: Duration::from_millis(100),
                mode: HealthScanMode::Fixture("linux-critical".to_string()),
            },
        );
        assert!(!report.try_items.is_empty());
        for item in &report.try_items {
            assert!(report.findings.iter().any(|finding| {
                finding.id == item.finding_id && finding.suggested_try_ids.contains(&item.id)
            }));
        }
    }

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
        let observed_ms = started.elapsed().as_millis();
        assert!(report.elapsed_ms <= observed_ms.saturating_add(1));
        assert!(
            observed_ms.saturating_sub(report.elapsed_ms) <= 3,
            "elapsed was truncated: {report:?}"
        );
    }

    #[test]
    fn live_scan_records_collectors_missed_by_total_budget() {
        let started = Instant::now();
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

        let observed_ms = started.elapsed().as_millis();
        assert!(report.elapsed_ms <= observed_ms.saturating_add(1));
        assert!(observed_ms.saturating_sub(report.elapsed_ms) <= 3);
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

    #[test]
    fn supervised_doctor_receives_actual_worker_facts() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let previous = std::env::var_os(HEALTH_SCAN_ENV);
        std::env::set_var(HEALTH_SCAN_ENV, "1");
        let mut config = crate::config::CoshConfig::default();
        config.adapter_default = "fake".to_string();
        let report = run_supervised_doctor_report(&config, std::path::Path::new("/tmp"));
        match previous {
            Some(value) => std::env::set_var(HEALTH_SCAN_ENV, value),
            None => std::env::remove_var(HEALTH_SCAN_ENV),
        }
        assert!(
            report
                .facts
                .iter()
                .any(|fact| fact.key == "provider.adapter"
                    && fact.value == HealthFactValue::String("fake".to_string())),
            "actual worker did not return provider facts: {report:?}"
        );
        assert!(report
            .check_results
            .iter()
            .any(|check| check.id == "environment.provider"
                && check.status == super::super::model::HealthCheckStatus::Ok));
    }
}
