//! v3.7 R11 service source: one bounded `systemctl show` batch, canonical Id
//! binding, persistent-online qualification with a same-boot monotonic state age.

use std::time::Instant;

use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthSeverity,
    HealthUnavailableReason,
};
use super::{run_system_command, unavailable_from_stderr, HealthCommandConfig};
use crate::config::{HealthConfig, HealthServiceExpectedState};

const MAX_SERVICES: usize = 3;
const MAX_NAME_BYTES: usize = 255;
const STATE_AGE_MIN_US: u64 = 5_000_000;
const SHOW_PROPERTIES: &str = "--property=Id,LoadState,ActiveState,SubState,Type,Result,Job,TriggeredBy,StateChangeTimestampMonotonic";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ServiceShowEntry {
    pub id: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub service_type: String,
    pub result: String,
    pub job: String,
    pub triggered_by: String,
    pub state_change_us: Option<u64>,
}

pub(crate) fn canonical_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.ends_with(".service")
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'@' | b':' | b'\\')
        })
        && !name.contains("..")
}

pub(crate) fn parse_systemctl_show(content: &str) -> Vec<ServiceShowEntry> {
    let mut entries = Vec::new();
    for block in content.split("\n\n") {
        let mut entry = ServiceShowEntry::default();
        let mut any = false;
        for line in block.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            any = true;
            match key {
                "Id" => entry.id = value.to_string(),
                "LoadState" => entry.load_state = value.to_string(),
                "ActiveState" => entry.active_state = value.to_string(),
                "SubState" => entry.sub_state = value.to_string(),
                "Type" => entry.service_type = value.to_string(),
                "Result" => entry.result = value.to_string(),
                "Job" => entry.job = value.to_string(),
                "TriggeredBy" => entry.triggered_by = value.to_string(),
                "StateChangeTimestampMonotonic" => {
                    entry.state_change_us = value.parse().ok();
                }
                _ => {}
            }
        }
        if any && !entry.id.is_empty() {
            entries.push(entry);
        }
    }
    entries
}

pub(crate) fn now_monotonic_us() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut value = nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, &mut value) } < 0 {
            return None;
        }
        Some(value.tv_sec as u64 * 1_000_000 + value.tv_nsec as u64 / 1000)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(crate) fn state_age_us(entry: &ServiceShowEntry, now_us: Option<u64>) -> Option<u64> {
    let now = now_us?;
    let change = entry.state_change_us?;
    if change == 0 || change > now {
        return None;
    }
    Some(now - change)
}

pub(crate) fn persistent_qualified(
    entry: &ServiceShowEntry,
    activation: &str,
    age_us: Option<u64>,
) -> bool {
    activation == "persistent"
        && entry.load_state == "loaded"
        && entry.service_type != "oneshot"
        && entry.triggered_by.is_empty()
        && entry.job.is_empty()
        && age_us.is_some_and(|age| age >= STATE_AGE_MIN_US)
}

pub(crate) fn legacy_status(entry: &ServiceShowEntry) -> &'static str {
    match entry.active_state.as_str() {
        "active" => "active",
        "failed" => "failed",
        _ => "inactive",
    }
}

pub(crate) fn collect_configured_services(
    builder: &mut HealthReportBuilder,
    config: &HealthConfig,
) {
    if config.services.is_empty() {
        return;
    }
    let started = Instant::now();
    let valid: Vec<&crate::config::HealthServiceConfig> = config
        .services
        .iter()
        .filter(|service| canonical_service_name(&service.name))
        .take(MAX_SERVICES)
        .collect();
    let over_limit = config
        .services
        .iter()
        .filter(|service| canonical_service_name(&service.name))
        .count()
        .saturating_sub(MAX_SERVICES);
    if over_limit > 0 {
        builder.add_fact(
            HealthFactCategory::Service,
            "service.over_limit",
            HealthFactValue::Unsigned(over_limit as u64),
            None,
            HealthFactSource::Systemctl,
            started.elapsed().as_millis(),
        );
    }
    if valid.is_empty() {
        return;
    }
    let mut args: Vec<&str> = vec!["show"];
    for service in &valid {
        args.push(service.name.as_str());
    }
    args.push(SHOW_PROPERTIES);
    let output = match run_system_command("systemctl", &args, HealthCommandConfig::default()) {
        Ok(output) => output,
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::ConfiguredService,
                err.unavailable_reason(),
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
            return;
        }
    };
    if output.exit_code != Some(0) && output.stdout.trim().is_empty() {
        builder.add_unavailable(
            HealthCollector::ConfiguredService,
            unavailable_from_stderr(&output.stderr),
            HealthSeverity::Degraded,
            output.elapsed_ms,
        );
        return;
    }
    let entries = parse_systemctl_show(&output.stdout);
    let now_us = now_monotonic_us();
    let mut collected = false;
    for service in &valid {
        let Some(entry) = entries.iter().find(|entry| entry.id == service.name) else {
            builder.add_fact(
                HealthFactCategory::Service,
                format!("service.{}.loaded", service.name),
                HealthFactValue::Bool(false),
                None,
                HealthFactSource::Systemctl,
                output.elapsed_ms,
            );
            continue;
        };
        let age = state_age_us(entry, now_us);
        let qualified = persistent_qualified(entry, &service.activation, age);
        let expected = match service.expected {
            HealthServiceExpectedState::Active => "active",
            HealthServiceExpectedState::Inactive => "inactive",
        };
        let record = |builder: &mut HealthReportBuilder, key: &str, value: HealthFactValue| {
            builder.add_fact(
                HealthFactCategory::Service,
                format!("service.{}.{key}", service.name),
                value,
                None,
                HealthFactSource::Systemctl,
                output.elapsed_ms,
            );
        };
        record(
            builder,
            "status",
            HealthFactValue::String(legacy_status(entry).to_string()),
        );
        record(
            builder,
            "load_state",
            HealthFactValue::String(entry.load_state.clone()),
        );
        record(
            builder,
            "active_state",
            HealthFactValue::String(entry.active_state.clone()),
        );
        record(
            builder,
            "sub_state",
            HealthFactValue::String(entry.sub_state.clone()),
        );
        record(
            builder,
            "type",
            HealthFactValue::String(entry.service_type.clone()),
        );
        record(
            builder,
            "result",
            HealthFactValue::String(entry.result.clone()),
        );
        record(
            builder,
            "triggered_by",
            HealthFactValue::String(entry.triggered_by.clone()),
        );
        record(builder, "job", HealthFactValue::String(entry.job.clone()));
        record(
            builder,
            "expected",
            HealthFactValue::String(expected.to_string()),
        );
        record(
            builder,
            "activation",
            HealthFactValue::String(service.activation.clone()),
        );
        record(
            builder,
            "persistent_qualified",
            HealthFactValue::Bool(qualified),
        );
        if let Some(age) = age {
            record(builder, "state_age_us", HealthFactValue::Unsigned(age));
        }
        collected = true;
    }
    if collected {
        builder.add_check_done("configured_service");
    }
}

#[cfg(test)]
mod tests;
