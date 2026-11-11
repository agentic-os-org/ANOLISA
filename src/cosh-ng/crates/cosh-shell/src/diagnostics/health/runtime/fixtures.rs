use super::*;

#[cfg(test)]
mod tests;

pub(super) fn fixture_report(
    name: &str,
    config: &HealthConfig,
    options: &HealthScanOptions,
) -> HealthScanReport {
    fixture_report_for_context(name, config, options, plan::ScanContext::Resources, &[])
}

pub(super) fn fixture_report_for_context(
    name: &str,
    config: &HealthConfig,
    options: &HealthScanOptions,
    context: plan::ScanContext,
    disabled: &[String],
) -> HealthScanReport {
    let started = Instant::now();
    let source = fixture_source(name).unwrap_or_else(|| fixture_source("linux-healthy").unwrap());
    let mut raw = HealthReportBuilder::for_started_at(options.started_at_ms);
    load_fixture_source(source, &mut raw);
    let raw = raw.finish(options.started_at_ms);
    let request = plan::WorkerRequest {
        scan_id: raw.scan_id.clone(),
        started_at_ms: options.started_at_ms,
        now_epoch_seconds: options.now_epoch_seconds,
        deadline: job::JobDeadline::after(DEFAULT_SCAN_BUDGET).expect("fixture clock"),
        context,
        config: config.clone(),
        environment: None,
        disabled: disabled.to_vec(),
        config_source: plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
    };
    let planned = plan::checks(&request);
    let mut builder = HealthReportBuilder::for_started_at(options.started_at_ms);
    builder.set_role(if config.enabled {
        config.role.clone()
    } else {
        None
    });
    if config.enabled {
        builder.set_health_score(raw.health_score);
    }
    for check in &planned {
        builder.declare_check_id(check.id, check.collector, check.enabled);
    }
    if planned
        .iter()
        .any(|check| check.id == "host.uptime" && check.enabled)
    {
        if let Some(host) = &raw.host {
            builder.set_host(host);
        }
    }
    let memory_pressure = super::super::rules::number(&raw.facts, "memory.available_mib")
        .zip(super::super::rules::number(
            &raw.facts,
            "memory.available_ratio",
        ))
        .map(|(available, ratio)| {
            super::super::rules::memory_pressure_level(available, ratio).is_some()
        });
    let summary_is_root = raw.facts.iter().any(|fact| {
        fact.key == "filesystem.riskiest_mount"
            && matches!(&fact.value, HealthFactValue::String(path) if path == "/")
    });
    for check in planned.into_iter().filter(|check| check.enabled) {
        builder.start_check_id(
            check.id,
            check.collector,
            check.scope,
            check.coverage,
            options.started_at_ms,
        );
        for fact in &raw.facts {
            let owner = super::super::plan::CHECKS
                .iter()
                .find(|definition| super::super::plan::owns_fact(definition.id, &fact.key));
            if owner.is_some_and(|owner| owner.id == check.id)
                && !(check.id == "filesystem.mounts" && summary_is_root)
            {
                builder.add_fact(
                    fact.category,
                    &fact.key,
                    fact.value.clone(),
                    fact.unit.clone(),
                    HealthFactSource::Fixture,
                    fact.elapsed_ms,
                );
            }
        }
        if check.id == "resource.swap" {
            if let Some(pressure) = memory_pressure {
                builder.add_fact(
                    HealthFactCategory::Memory,
                    "memory.swap_memory_pressure",
                    HealthFactValue::Bool(pressure),
                    None,
                    HealthFactSource::Fixture,
                    0,
                );
            }
        }
        for gap in raw
            .unavailable
            .iter()
            .filter(|gap| gap.collector == check.collector)
        {
            builder.add_unavailable(gap.collector, gap.reason, gap.severity, gap.elapsed_ms);
        }
        if check.id == "resource.swap"
            && !super::super::rules::swap_context_complete(builder.report(), config)
        {
            builder.add_unavailable(
                check.collector,
                HealthUnavailableReason::MissingContext,
                HealthSeverity::Unavailable,
                0,
            );
        }
        builder.add_check_done(check.id).complete_check();
    }
    if config.enabled {
        builder.add_check_done("fixture");
    }
    let mut report = builder.finish(
        options
            .started_at_ms
            .saturating_add(started.elapsed().as_millis()),
    );
    apply_judgement_rules(&mut report, config);
    super::super::recommendation::apply_try_candidates(&mut report, config);
    report.elapsed_ms = started.elapsed().as_millis();
    report
}

fn fixture_source(name: &str) -> Option<&'static str> {
    match name {
        "linux-healthy" | "healthy" => Some(include_str!("../fixtures/linux-healthy.health")),
        "linux-warning" | "warning" => Some(include_str!("../fixtures/linux-warning.health")),
        "linux-critical" | "critical" => Some(include_str!("../fixtures/linux-critical.health")),
        "linux-degraded" | "degraded" => Some(include_str!("../fixtures/linux-degraded.health")),
        "linux-partial" | "partial" => Some(include_str!("../fixtures/linux-partial.health")),
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
