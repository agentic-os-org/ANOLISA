//! Bounded kernel signal collection: structured journal first, legacy text and
//! dmesg fallbacks kept diagnostics-only for recent eligibility.

use std::fs;
use std::time::Instant;

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{
    HealthCheckStatus, HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue,
    HealthSeverity, HealthUnavailableReason,
};
use super::super::system::system_program;
use super::super::{
    run_health_command_filtered, unavailable_from_stderr, HealthCommandConfig, HealthCommandError,
    HealthCommandOutput, HealthFilteredCommandOutput, HealthLineFilter,
};
use super::oom::is_oom_line;
use super::{attribution, cgroup, finish_signals, parse_kernel_signals_attributed};

const JOURNAL_SIGNAL_PATTERN: &str = "(?i)oom-kill|out of memory|killed process|oom_reaper|killed as a result of limit of|kernel panic|panic:|vfs: file-max limit|nf_conntrack: table full|ext4-fs error|remounting filesystem read-only|aborting journal|detected io errors while flushing|jbd2:|xfs \\(|blk_update_request:";

pub(crate) fn collect_kernel_signals(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
    let mut boot_id = None;
    if let Ok(boot) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let boot = boot.trim();
        if boot.len() == 36
            && boot
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            boot_id = Some(boot.to_string());
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.boot_id",
                HealthFactValue::String(boot.into()),
                None,
                HealthFactSource::Derived,
                0,
            );
        }
    }
    cgroup::collect_cgroup(builder, now_epoch_seconds);
    let current_cgroup = builder.report().facts.iter().find_map(|fact| {
        (fact.key == "kernel.current_cgroup_path").then(|| match &fact.value {
            HealthFactValue::String(path) => path.clone(),
            _ => String::new(),
        })
    });
    let host_qualified = builder.report().check_results.iter().any(|check| {
        check.collector == Some(HealthCollector::Host) && check.status == HealthCheckStatus::Ok
    });
    let ctx = attribution::AttributionContext {
        current_cgroup,
        now_monotonic_ns: attribution::now_monotonic_ns(),
        host_qualified,
        boot_id,
        source_verified: false,
    };
    let started = Instant::now();
    match system_program("journalctl") {
        Ok(program) => {
            let run_json = |args: &[&str], config: HealthCommandConfig| {
                run_health_command_filtered(
                    program,
                    args,
                    config,
                    HealthLineFilter {
                        newest_first: true,
                        ..HealthLineFilter::default()
                    },
                    |_| true,
                )
            };
            let run_text = |args: &[&str], config: HealthCommandConfig| {
                run_health_command_filtered(
                    program,
                    args,
                    config,
                    HealthLineFilter {
                        newest_first: true,
                        ..HealthLineFilter::default()
                    },
                    is_kernel_signal_line,
                )
            };
            match read_journal_signals(HealthCommandConfig::default(), "json", run_json) {
                Ok(output)
                    if output.exit_code == Some(0)
                        || !output.stdout.trim().is_empty()
                        || (output.exit_code == Some(1) && output.stderr.trim().is_empty()) =>
                {
                    let mut verified_ctx = ctx.clone();
                    verified_ctx.source_verified = true;
                    match attribution::parse_journal_json_signals(&output.stdout, &verified_ctx) {
                        Some((events, unclocked, panic_recent)) => {
                            let coverage = events.iter().map(|event| event.event_epoch()).min();
                            let mut facts = finish_signals(
                                &events,
                                unclocked,
                                panic_recent,
                                "journalctl".to_string(),
                                now_epoch_seconds,
                                coverage,
                                Some(&verified_ctx),
                            );
                            if output.stdout_truncated {
                                if facts.oom_latest_confidence.as_deref() == Some("high") {
                                    facts.oom_latest_confidence = Some("low".to_string());
                                }
                                if facts.oom_attributed_confidence.as_deref() == Some("high") {
                                    facts.oom_attributed_confidence = Some("low".to_string());
                                }
                            }
                            facts.record(builder, output.elapsed_ms);
                            let capacity = super::capacity::parse_journal_capacity_events(
                                &output.stdout,
                                &verified_ctx,
                                now_epoch_seconds,
                            );
                            super::capacity::record_capacity_events(
                                builder,
                                &capacity,
                                output.elapsed_ms,
                            );
                            let fs_lines = super::fs_events::parse_journal_fs_lines(
                                &output.stdout,
                                &verified_ctx,
                            );
                            let fs_chains = super::fs_events::build_chains(
                                &fs_lines,
                                &verified_ctx,
                                now_epoch_seconds,
                            );
                            super::fs_events::record_fs_chains(
                                builder,
                                &fs_chains,
                                &fs_lines,
                                output.elapsed_ms,
                            );
                            builder.add_check_done("kernel_signal");
                        }
                        None => {
                            let remaining = HealthCommandConfig::default()
                                .timeout
                                .saturating_sub(started.elapsed());
                            if remaining.is_zero() {
                                builder.add_unavailable(
                                    HealthCollector::KernelSignal,
                                    HealthUnavailableReason::Timeout,
                                    HealthSeverity::Unavailable,
                                    started.elapsed().as_millis(),
                                );
                                return;
                            }
                            match read_journal_signals(
                                HealthCommandConfig {
                                    timeout: remaining,
                                    ..HealthCommandConfig::default()
                                },
                                "short-unix",
                                run_text,
                            ) {
                                Ok(text) => {
                                    let mut facts = parse_kernel_signals_attributed(
                                        &text.stdout,
                                        "journalctl",
                                        now_epoch_seconds,
                                        None,
                                        &ctx,
                                    );
                                    if text.stdout_truncated
                                        && facts.oom_latest_confidence.as_deref() == Some("high")
                                    {
                                        facts.oom_latest_confidence = Some("low".to_string());
                                    }
                                    facts.record(builder, text.elapsed_ms);
                                    let capacity = super::capacity::parse_text_capacity_events(
                                        &text.stdout,
                                        "journalctl-text",
                                    );
                                    super::capacity::record_capacity_events(
                                        builder,
                                        &capacity,
                                        text.elapsed_ms,
                                    );
                                    let fs_lines = super::fs_events::parse_text_fs_lines(
                                        &text.stdout,
                                        "journalctl-text",
                                    );
                                    let fs_chains = super::fs_events::build_chains(
                                        &fs_lines,
                                        &ctx,
                                        now_epoch_seconds,
                                    );
                                    super::fs_events::record_fs_chains(
                                        builder,
                                        &fs_chains,
                                        &fs_lines,
                                        text.elapsed_ms,
                                    );
                                    builder.add_check_done("kernel_signal");
                                }
                                Err(error) => {
                                    builder.add_unavailable(
                                        HealthCollector::KernelSignal,
                                        error.unavailable_reason(),
                                        HealthSeverity::Unavailable,
                                        started.elapsed().as_millis(),
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(output) => {
                    builder.add_unavailable(
                        HealthCollector::KernelSignal,
                        kernel_unavailable_reason(output.exit_code, &output.stderr),
                        HealthSeverity::Unavailable,
                        output.elapsed_ms,
                    );
                }
                Err(error) => {
                    builder.add_unavailable(
                        HealthCollector::KernelSignal,
                        error.unavailable_reason(),
                        HealthSeverity::Unavailable,
                        started.elapsed().as_millis(),
                    );
                }
            }
        }
        Err(HealthCommandError::CommandMissing) => {
            collect_kernel_from_dmesg(builder, now_epoch_seconds, started, &ctx)
        }
        Err(error) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                error.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(super) fn read_journal_signals(
    config: HealthCommandConfig,
    output_format: &str,
    mut run: impl FnMut(
        &[&str],
        HealthCommandConfig,
    ) -> Result<HealthFilteredCommandOutput, HealthCommandError>,
) -> Result<HealthCommandOutput, HealthCommandError> {
    let started = Instant::now();
    let args = [
        "--quiet",
        "-k",
        "--since",
        "24 hours ago",
        "--no-pager",
        "--reverse",
        "-n",
        "200",
        "-o",
        output_format,
        "--grep",
        JOURNAL_SIGNAL_PATTERN,
    ];
    let output = run(&args, config)?;
    // Localized option errors still name --grep; permission failures do not.
    if !matches!(output.exit_code, Some(code) if code != 0)
        || !output.filtered.lines.is_empty()
        || !output.stderr.contains("--grep")
    {
        return Ok(journal_lines_output(output, 0));
    }
    let remaining = config.timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(HealthCommandError::Timeout);
    }
    let compatible = run(
        &args[..args.len() - 2],
        HealthCommandConfig {
            timeout: remaining,
            ..config
        },
    )?;
    Ok(journal_lines_output(compatible, output.elapsed_ms))
}

fn journal_lines_output(
    output: HealthFilteredCommandOutput,
    prior_elapsed_ms: u128,
) -> HealthCommandOutput {
    HealthCommandOutput {
        exit_code: output.exit_code,
        stdout: output.filtered.lines.join("\n"),
        stderr: output.stderr,
        stdout_truncated: !output.filtered.complete || output.filtered.truncated_lines > 0,
        stderr_truncated: false,
        elapsed_ms: output.elapsed_ms + prior_elapsed_ms,
    }
}

pub(super) fn is_kernel_signal_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    is_oom_line(&lower)
        || lower.contains("kernel panic")
        || lower.contains("panic:")
        || super::capacity::is_vfs_filemax_line(&lower)
        || super::capacity::is_conntrack_full_line(&lower)
        || super::fs_events::classify_fs_line(&lower).is_some()
}

pub(super) fn kernel_unavailable_reason(
    exit_code: Option<i32>,
    stderr: &str,
) -> HealthUnavailableReason {
    if exit_code.is_none() {
        // No observed exit means a budget cut, not a failure to parse the log.
        return HealthUnavailableReason::Timeout;
    }
    unavailable_from_stderr(stderr)
}

fn collect_kernel_from_dmesg(
    builder: &mut HealthReportBuilder,
    now_epoch_seconds: u64,
    started: Instant,
    ctx: &attribution::AttributionContext,
) {
    let boot_epoch_seconds =
        read_proc_uptime_seconds(builder).map(|uptime| now_epoch_seconds.saturating_sub(uptime));
    let program = match system_program("dmesg") {
        Ok(program) => program,
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                err.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
            return;
        }
    };
    match run_health_command_filtered(
        program,
        &[],
        HealthCommandConfig::default(),
        HealthLineFilter::default(),
        is_kernel_signal_line,
    ) {
        Ok(output) => {
            if output.exit_code != Some(0) && output.filtered.lines.is_empty() {
                builder.add_unavailable(
                    HealthCollector::KernelSignal,
                    kernel_unavailable_reason(output.exit_code, &output.stderr),
                    HealthSeverity::Unavailable,
                    output.elapsed_ms,
                );
                return;
            }
            let mut facts = parse_kernel_signals_attributed(
                &output.filtered.lines.join("\n"),
                "dmesg",
                now_epoch_seconds,
                boot_epoch_seconds,
                ctx,
            );
            // A partial read cannot prove which event was the most recent one.
            if !output.filtered.complete || output.filtered.truncated_lines > 0 {
                if facts.oom_latest_confidence.as_deref() == Some("high") {
                    facts.oom_latest_confidence = Some("low".to_string());
                }
                if facts.oom_attributed_confidence.as_deref() == Some("high") {
                    facts.oom_attributed_confidence = Some("low".to_string());
                }
            }
            facts.record(builder, output.elapsed_ms);
            let capacity = super::capacity::parse_text_capacity_events(
                &output.filtered.lines.join("\n"),
                "dmesg",
            );
            super::capacity::record_capacity_events(builder, &capacity, output.elapsed_ms);
            let fs_lines =
                super::fs_events::parse_text_fs_lines(&output.filtered.lines.join("\n"), "dmesg");
            let fs_chains = super::fs_events::build_chains(&fs_lines, ctx, now_epoch_seconds);
            super::fs_events::record_fs_chains(builder, &fs_chains, &fs_lines, output.elapsed_ms);
            builder.add_check_done("kernel_signal");
        }
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                err.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(super) fn read_proc_uptime_seconds(builder: &HealthReportBuilder) -> Option<u64> {
    let report = builder.report();
    if let Some(fact) = report
        .facts
        .iter()
        .find(|fact| fact.key == "host.uptime_seconds")
    {
        return match fact.value {
            HealthFactValue::Unsigned(seconds) => Some(seconds),
            _ => None,
        };
    }
    if report.check_results.iter().any(|check| {
        check.collector == Some(HealthCollector::Host)
            && check.status != HealthCheckStatus::Disabled
    }) {
        return None;
    }
    let content = fs::read_to_string("/proc/uptime").ok()?;
    super::super::parse_proc_uptime_seconds(&content)
}
