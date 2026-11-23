use super::*;
use crate::diagnostics::health::model::{HealthCheckStatus, HealthScanReport};
use serde::Deserialize;

#[derive(Deserialize, serde::Serialize)]
struct FrozenSource {
    source: String,
    checks: Vec<String>,
    accessible: bool,
    evidence: String,
}

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Environment {
    fn set(values: &[(&'static str, &std::ffi::OsStr)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, value)| {
                let previous = std::env::var_os(key);
                std::env::set_var(key, value);
                (*key, previous)
            })
            .collect();
        Self(previous)
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn usage() -> serde_json::Value {
    let mut value: nix::libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { nix::libc::getrusage(nix::libc::RUSAGE_CHILDREN, &mut value) },
        0
    );
    serde_json::json!({
        "user_us": value.ru_utime.tv_sec * 1_000_000 + value.ru_utime.tv_usec,
        "system_us": value.ru_stime.tv_sec * 1_000_000 + value.ru_stime.tv_usec,
        "max_rss_kib": value.ru_maxrss,
        "input_blocks": value.ru_inblock,
        "output_blocks": value.ru_oublock,
        "voluntary_switches": value.ru_nvcsw,
        "involuntary_switches": value.ru_nivcsw,
    })
}

fn sampled(report: &HealthScanReport, id: &str) -> bool {
    report.check_results.iter().any(|check| {
        check.id == id
            && (check.status == HealthCheckStatus::Ok
                || (id == "resource.swap"
                    && check.status == HealthCheckStatus::Partial
                    && !check.fact_ids.is_empty()
                    && check
                        .gaps
                        .iter()
                        .all(|gap| gap.reason == HealthUnavailableReason::MissingContext)))
    })
}

fn positive_checks<'a>(
    frozen: &'a [FrozenSource],
    fault_checks: &[String],
) -> Result<Vec<&'a str>, String> {
    if frozen.is_empty() {
        return Err("frozen sources must not be empty".into());
    }
    for source in frozen {
        if source.checks.is_empty() {
            return Err(format!("frozen source has no checks: {}", source.source));
        }
        validate_health_check_ids(&source.checks)?;
    }
    validate_health_check_ids(fault_checks)?;
    for id in fault_checks {
        if !frozen.iter().any(|source| source.checks.contains(id)) {
            return Err(format!("fault check is not frozen: {id}"));
        }
    }
    let checks: Vec<&str> = frozen
        .iter()
        .filter(|source| source.accessible)
        .flat_map(|source| source.checks.iter())
        .filter(|id| !fault_checks.contains(id))
        .map(String::as_str)
        .collect();
    if checks.is_empty() {
        return Err("frozen sources need an accessible non-fault check".into());
    }
    Ok(checks)
}

fn frozen_source(checks: &[&str], accessible: bool) -> FrozenSource {
    FrozenSource {
        source: "independent probe".into(),
        checks: checks.iter().map(|id| (*id).to_string()).collect(),
        accessible,
        evidence: "probe result".into(),
    }
}

#[test]
fn frozen_baseline_rejects_empty_sources() {
    assert!(positive_checks(&[], &[]).is_err());
}

#[test]
fn frozen_baseline_rejects_empty_checks() {
    assert!(positive_checks(&[frozen_source(&[], true)], &[]).is_err());
    assert!(positive_checks(
        &[
            frozen_source(&["resource.load"], true),
            frozen_source(&[], false),
        ],
        &[],
    )
    .is_err());
}

#[test]
fn frozen_baseline_rejects_all_inaccessible_sources() {
    assert!(positive_checks(&[frozen_source(&["resource.load"], false)], &[]).is_err());
}

#[test]
fn frozen_baseline_rejects_faults_covering_all_positive_checks() {
    assert!(positive_checks(
        &[frozen_source(&["resource.load"], true)],
        &["resource.load".into()],
    )
    .is_err());
}

#[test]
fn frozen_baseline_rejects_unregistered_check() {
    assert!(positive_checks(&[frozen_source(&["unknown.check"], true)], &[]).is_err());
}

#[test]
fn frozen_baseline_rejects_unregistered_fault() {
    assert!(positive_checks(
        &[frozen_source(&["resource.load"], true)],
        &["unknown.check".into()],
    )
    .is_err());
}

#[test]
fn frozen_baseline_rejects_unfrozen_fault() {
    assert!(positive_checks(
        &[frozen_source(&["resource.load"], true)],
        &["resource.memory".into()],
    )
    .is_err());
}

#[test]
fn frozen_baseline_accepts_one_registered_positive_check() {
    assert_eq!(
        positive_checks(&[frozen_source(&["resource.load"], true)], &[]).unwrap(),
        ["resource.load"]
    );
}

#[test]
fn frozen_baseline_preserves_positive_check_with_fault_and_inaccessible_source() {
    assert_eq!(
        positive_checks(
            &[
                frozen_source(&["resource.memory", "resource.swap"], true),
                frozen_source(&["kernel.oom"], false),
            ],
            &["resource.swap".into()],
        )
        .unwrap(),
        ["resource.memory"]
    );
}

#[test]
#[ignore = "isolated ALinux4 acceptance; requires independently frozen source probes"]
fn live_report_matrix() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let frozen = std::env::var("COSH_HEALTH_FROZEN_SOURCES").expect("frozen probe path");
    let frozen: Vec<FrozenSource> =
        serde_json::from_slice(&std::fs::read(frozen).unwrap()).unwrap();
    let count = std::env::var("COSH_HEALTH_SAMPLE_COUNT")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(200);
    let label = std::env::var("COSH_HEALTH_SAMPLE_LABEL").unwrap_or_else(|_| "normal".into());
    assert!((30..=1000).contains(&count));
    if label == "normal" {
        assert!(count >= 200);
    }
    let fault_checks: Vec<String> = std::env::var("COSH_HEALTH_FAULT_CHECKS")
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    let positive_checks = positive_checks(&frozen, &fault_checks).expect("valid frozen baseline");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".copilot-shell")).unwrap();
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        "[ui]\nlanguage='en-US'\n",
    )
    .unwrap();
    let _environment = Environment::set(&[
        ("HOME", home.path().as_os_str()),
        (HEALTH_SCAN_ENV, std::ffi::OsStr::new("1")),
    ]);
    println!("HEALTH_FROZEN {}", serde_json::to_string(&frozen).unwrap());
    let mut completed = 0usize;
    let mut fault_observed = 0usize;
    let mut over_budget = 0usize;
    let mut unexpected_checks = 0usize;
    let frozen_ids: Vec<&str> = frozen
        .iter()
        .flat_map(|source| source.checks.iter().map(String::as_str))
        .collect();
    let registered_ids = health_check_ids();
    for sample in 0..count {
        let before = usage();
        let started = Instant::now();
        let expires_ns = clock::boottime_ns().unwrap() + 230_000_000;
        let report = start_login_health_scan_config_entry_until(
            started + Duration::from_millis(230),
            Some(expires_ns / 1000),
            &[],
        )
        .unwrap()
        .unwrap()
        .finish_within(Duration::from_millis(230))
        .unwrap();
        let elapsed_us = started.elapsed().as_micros();
        let complete = positive_checks.iter().all(|id| sampled(&report, id));
        let observed = fault_checks.iter().all(|id| {
            report.check_results.iter().any(|check| {
                check.id == *id
                    && matches!(
                        check.status,
                        HealthCheckStatus::Partial | HealthCheckStatus::Unavailable
                    )
            })
        });
        completed += usize::from(complete);
        fault_observed += usize::from(observed);
        over_budget += usize::from(elapsed_us > 300_000);
        let unexpected: Vec<&str> = report
            .check_results
            .iter()
            .filter(|check| {
                !registered_ids.contains(&check.id.as_str())
                    || (check.status != HealthCheckStatus::Disabled
                        && !frozen_ids.contains(&check.id.as_str()))
            })
            .map(|check| check.id.as_str())
            .collect();
        unexpected_checks += unexpected.len();
        println!(
            "HEALTH_SAMPLE {}",
            serde_json::json!({
                "label": label, "sample": sample, "elapsed_us": elapsed_us,
                "complete_accessible_sources": complete, "expected_fault_observed": observed,
                "unexpected_checks": unexpected,
                "usage_before": before, "usage_after": usage(), "report": report,
            })
        );
    }
    println!(
        "HEALTH_SUMMARY {}",
        serde_json::json!({
            "label": label, "count": count, "joint_complete": completed,
            "expected_fault_observed": fault_observed, "over_budget": over_budget,
            "unexpected_checks": unexpected_checks,
        })
    );
    assert_eq!(
        over_budget, 0,
        "all samples retained, including any overrun"
    );
    assert!(
        completed * 100 >= count * 95,
        "joint source completion below 95%"
    );
    assert_eq!(
        unexpected_checks, 0,
        "unregistered or unfrozen active checks"
    );
    assert_eq!(
        fault_observed, count,
        "fault injection did not reach every sample"
    );
}
