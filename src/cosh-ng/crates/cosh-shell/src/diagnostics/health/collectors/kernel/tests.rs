use super::*;
use std::time::Duration;

fn journal_output(code: i32, stdout: &str, stderr: &str) -> super::super::HealthCommandOutput {
    super::super::HealthCommandOutput {
        exit_code: Some(code),
        stdout: stdout.into(),
        stderr: stderr.into(),
        stdout_truncated: false,
        stderr_truncated: false,
        elapsed_ms: 1,
    }
}

fn journal_filtered(
    code: i32,
    lines: &[&str],
    stderr: &str,
) -> super::super::HealthFilteredCommandOutput {
    super::super::HealthFilteredCommandOutput {
        filtered: super::super::command::HealthFilteredLines {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            raw_bytes_read: 0,
            lines_scanned: lines.len() as u64,
            truncated_lines: 0,
            complete: true,
        },
        stderr: stderr.into(),
        exit_code: Some(code),
        elapsed_ms: 1,
    }
}

fn newest_first_filter() -> HealthLineFilter {
    HealthLineFilter {
        newest_first: true,
        ..HealthLineFilter::default()
    }
}

fn journal_filtered_fixture(
    content: &str,
    filter: HealthLineFilter,
) -> super::super::HealthFilteredCommandOutput {
    let mut state = super::super::command::HealthLineFilterState::new(filter);
    for chunk in content.as_bytes().chunks(4096) {
        state.push(chunk, &is_kernel_signal_line);
    }
    state.mark_eof(&is_kernel_signal_line);
    super::super::HealthFilteredCommandOutput {
        filtered: state.finish(),
        stderr: String::new(),
        exit_code: Some(0),
        elapsed_ms: 1,
    }
}

#[test]
fn journal_grep_compatibility_recovers_legacy_oom_evidence() {
    for error in [
        "journalctl: unrecognized option '--grep'",
        "journalctl: unknown option --grep",
        "journalctl: unbekannte Option »--grep«",
        "Compiled without PCRE2 support, cannot use --grep",
    ] {
        let mut calls = 0;
        let output = read_journal_signals(HealthCommandConfig::default(), |args, config| {
            calls += 1;
            assert!(config.timeout <= HealthCommandConfig::default().timeout);
            assert!(args.windows(2).any(|pair| pair == ["-n", "200"]));
            assert!(args.contains(&"--reverse"));
            if args.contains(&"--grep") {
                return Ok(journal_filtered(1, &[], error));
            }
            assert!(args.windows(2).any(|pair| pair == ["--since", "24 hours ago"]));
            Ok(journal_filtered(0, &[
                "1991.0 kernel: Killed process 71 (worker)",
                "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=worker,pid=71",
            ], ""))
        }).unwrap();
        assert_eq!(calls, 2);
        let facts = parse_reverse_journal_signals(&output.stdout, 2000);
        assert_eq!(facts.oom_event_count_last_1h, 1);
        assert_eq!(facts.oom_latest_pid, Some(71));
        assert_eq!(facts.oom_killed_process.as_deref(), Some("worker"));
        assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/w"));
    }
}

#[test]
fn journal_grep_compatibility_keeps_modern_results_without_retry() {
    for (filtered, expected) in [
        (
            journal_filtered(0, &["1990.0 kernel: Killed process 71 (worker)"], ""),
            journal_output(0, "1990.0 kernel: Killed process 71 (worker)", ""),
        ),
        (journal_filtered(1, &[], ""), journal_output(1, "", "")),
    ] {
        let mut calls = 0;
        let actual = read_journal_signals(HealthCommandConfig::default(), |args, _| {
            calls += 1;
            assert!(args.contains(&"--grep"));
            Ok(filtered.clone())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(actual, expected);
    }
}

#[test]
fn journal_grep_compatibility_preserves_other_errors_without_retry() {
    for (filtered, expected) in [
        (
            journal_filtered(1, &[], "Failed to open journal: Permission denied"),
            journal_output(1, "", "Failed to open journal: Permission denied"),
        ),
        (
            journal_filtered(1, &[], "journalctl: unrecognized option '--other'"),
            journal_output(1, "", "journalctl: unrecognized option '--other'"),
        ),
        (
            journal_filtered(1, &[], "Failed to compile pattern: invalid expression"),
            journal_output(1, "", "Failed to compile pattern: invalid expression"),
        ),
    ] {
        let mut calls = 0;
        let actual = read_journal_signals(HealthCommandConfig::default(), |_, _| {
            calls += 1;
            Ok(filtered.clone())
        });
        assert_eq!(calls, 1);
        assert_eq!(actual, Ok(expected));
    }
    for error in [
        HealthCommandError::Timeout,
        HealthCommandError::OutputLimit,
        HealthCommandError::PermissionDenied,
        HealthCommandError::Io,
    ] {
        let mut calls = 0;
        let actual = read_journal_signals(HealthCommandConfig::default(), |_, _| {
            calls += 1;
            Err(error.clone())
        });
        assert_eq!(calls, 1);
        assert_eq!(actual, Err(error));
    }
}

#[test]
fn journal_grep_compatibility_does_not_reset_an_exhausted_budget() {
    let mut calls = 0;
    let result = read_journal_signals(
        HealthCommandConfig {
            timeout: Duration::ZERO,
            ..HealthCommandConfig::default()
        },
        |_, _| {
            calls += 1;
            Ok(journal_filtered(
                1,
                &[],
                "journalctl: unrecognized option '--grep'",
            ))
        },
    );
    assert_eq!(calls, 1);
    assert_eq!(result, Err(HealthCommandError::Timeout));
}

#[test]
fn journal_grep_fallback_keeps_oom_evidence_beyond_the_output_buffer() {
    let mut legacy = String::from(
        "1992.0 kernel: Killed process 71 (worker)\n1991.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=worker,pid=71\n",
    );
    while legacy.len() <= HealthCommandConfig::default().output_limit_bytes {
        legacy.push_str("1990.0 kernel: audit: unrelated noise record for the journal\n");
    }
    let result = read_journal_signals(HealthCommandConfig::default(), |args, _| {
        if args.contains(&"--grep") {
            return Ok(journal_filtered(
                1,
                &[],
                "journalctl: unrecognized option '--grep'",
            ));
        }
        Ok(journal_filtered_fixture(&legacy, newest_first_filter()))
    });
    let output = result.expect("legacy journal keeps bounded evidence");
    assert!(!output.stdout_truncated);
    let facts = parse_reverse_journal_signals(&output.stdout, 2000);
    assert_eq!(facts.oom_latest_pid, Some(71));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("worker"));
    assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/w"));
}

#[test]
fn journal_grep_fallback_keeps_the_newest_oom_during_a_storm() {
    let mut storm = String::new();
    for index in (0..600).rev() {
        storm.push_str(&format!(
            "{}.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/slice/task-{index},task=java,pid={}\n",
            1000 + index,
            10000 + index
        ));
    }
    let output = read_journal_signals(HealthCommandConfig::default(), |args, _| {
        if args.contains(&"--grep") {
            return Ok(journal_filtered(
                1,
                &[],
                "Compiled without PCRE2 support, cannot use --grep",
            ));
        }
        Ok(journal_filtered_fixture(&storm, newest_first_filter()))
    })
    .unwrap();
    assert!(output.stdout_truncated);
    let facts = parse_reverse_journal_signals(&output.stdout, 2000);
    assert_eq!(facts.oom_latest_pid, Some(10599));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("java"));
}

#[test]
fn journal_grep_marks_a_partial_read_as_incomplete() {
    let mut content = String::new();
    for index in (0..600).rev() {
        content.push_str(&format!(
            "{}.0 kernel: oom-kill:constraint=CONSTRAINT_NONE,task=java,pid={}\n",
            1000 + index,
            10000 + index
        ));
    }
    let output = read_journal_signals(HealthCommandConfig::default(), |args, _| {
        assert!(args.contains(&"--grep"));
        Ok(journal_filtered_fixture(
            &content,
            HealthLineFilter {
                raw_read_limit_bytes: 4096,
                ..newest_first_filter()
            },
        ))
    })
    .unwrap();
    assert!(output.stdout_truncated);
    assert!(!output.stdout.is_empty());
}

#[test]
fn journal_local_filter_matches_the_grep_pattern() {
    for line in [
        "oom-kill:constraint=CONSTRAINT_NONE",
        "Out of memory: Killed process 71 (java)",
        "oom_reaper: reaped process 71 (java)",
        "Task in /w killed as a result of limit of /w",
        "Kernel panic - not syncing",
        "panic: fatal exception",
    ] {
        assert!(is_kernel_signal_line(line), "{line}");
    }
    assert!(!is_kernel_signal_line("audit: unrelated noise record"));
}

#[test]
fn kernel_budget_cut_without_evidence_is_not_a_parse_error() {
    assert_eq!(
        kernel_unavailable_reason(None, ""),
        HealthUnavailableReason::Timeout
    );
    assert_eq!(
        kernel_unavailable_reason(Some(1), "Failed to open journal: Permission denied"),
        HealthUnavailableReason::PermissionDenied
    );
    assert_eq!(
        kernel_unavailable_reason(Some(1), ""),
        HealthUnavailableReason::ParseError
    );
}

#[test]
fn dmesg_reuses_the_already_sampled_host_uptime() {
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder.start_check_id(
        "host.uptime",
        HealthCollector::Host,
        HealthFactScope::Host,
        HealthFactCoverage::Snapshot,
        1,
    );
    builder.add_fact(
        HealthFactCategory::Host,
        "host.uptime_seconds",
        HealthFactValue::Unsigned(123),
        Some("seconds".into()),
        HealthFactSource::ProcUptime,
        0,
    );
    builder.add_check_done("host.uptime").complete_check();
    assert_eq!(read_proc_uptime_seconds(&builder), Some(123));
}

#[test]
fn legacy_memcg_limit_line_yields_task_and_limit_cgroups() {
    let content = "[ 1990.100000] Task in /workload killed as a result of limit of /workload\n\
                   [ 1990.200000] Out of memory: Killed process 20495 (python3) total-vm:1024kB\n";
    let facts = parse_kernel_signals_with_boot_epoch(content, "dmesg", 2000, Some(0));
    assert_eq!(facts.oom_latest_task_cgroup.as_deref(), Some("/workload"));
    assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/workload"));
    assert_eq!(facts.oom_latest_pid, Some(20495));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_event_count_last_1h, 1);
}

#[test]
fn oom_reaper_line_joins_the_existing_event() {
    let content =
        "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=python3,pid=20495\n\
                   1990.0 kernel: Out of memory: Killed process 20495 (python3) total-vm:1024kB\n\
                   1991.0 kernel: oom_reaper: reaped process 20495 (python3), now anon-rss:0kB\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_source_line_count, 3);
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_latest_pid, Some(20495));
}

#[test]
fn same_second_kill_records_without_pid_stay_separate() {
    let content = "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/a\n\
                   1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/a\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 2);
    assert_eq!(facts.oom_event_count_last_24h, 2);
}

#[test]
fn kill_record_and_its_own_killed_process_stay_one_event() {
    let content =
        "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=python3,pid=20495\n\
                   1990.0 kernel: Out of memory: Killed process 20495 (python3) total-vm:1024kB\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_source_line_count, 2);
    assert_eq!(facts.oom_latest_pid, Some(20495));
}

#[test]
fn oom_killer_invoker_is_not_reported_as_the_victim() {
    let content =
        "1990.0 kernel: python3 invoked oom-killer: gfp_mask=0xcc0(GFP_KERNEL), order=0\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_killed_process, None);
    assert_eq!(facts.oom_latest_pid, None);
    assert_eq!(facts.oom_event_count_last_1h, 1);
}

#[test]
fn oom_storm_in_the_same_second_counts_every_kill() {
    let mut content = String::new();
    for index in 0..40 {
        content.push_str(&format!(
            "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/storm,task=java,pid={index}\n\
             1990.0 kernel: Out of memory: Killed process {index} (java) total-vm:1024kB\n"
        ));
    }
    let facts = parse_kernel_signals(&content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 40);
    assert_eq!(facts.oom_event_count_last_24h, 40);
    assert_eq!(facts.oom_latest_pid, Some(39));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("java"));
}

#[test]
fn oom_storm_without_pids_counts_every_kill_record() {
    let mut content = String::new();
    for _ in 0..40 {
        content.push_str("1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/storm\n");
    }
    let facts = parse_kernel_signals(&content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 40);
}

#[test]
fn interleaved_cgroup_kills_in_the_same_second_stay_two_events() {
    let content = "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/a,task=java,pid=1\n\
                   1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/b,task=java,pid=2\n\
                   1990.0 kernel: Out of memory: Killed process 1 (java)\n\
                   1990.0 kernel: Out of memory: Killed process 2 (java)\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 2);
    assert_eq!(facts.oom_latest_pid, Some(2));
}

#[test]
fn kernel_310_legacy_memcg_sequence_reports_scope_and_victim() {
    let content = "[ 1990.100000] python3 invoked oom-killer: gfp_mask=0x201da, order=0\n\
                   [ 1990.100000] Task in /workload killed as a result of limit of /workload\n\
                   [ 1990.100000] memory: usage 524288kB, limit 524288kB, failcnt 12\n\
                   [ 1990.200000] Out of memory: Kill process 20495 (python3) score 500\n\
                   [ 1990.200000] Killed process 20495 (python3) total-vm:1024kB, anon-rss:512kB\n";
    let facts = parse_kernel_signals_with_boot_epoch(content, "dmesg", 2000, Some(0));
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_pid, Some(20495));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_latest_task_cgroup.as_deref(), Some("/workload"));
    assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/workload"));
}

#[test]
fn kernel_419_memcg_sequence_keeps_task_and_oom_cgroups_distinct() {
    let content = "[ 1990.100000] oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=/,mems_allowed=0-1,oom_memcg=/parent,task_memcg=/parent/child,task=python3,pid=20495,uid=0\n\
                   [ 1990.200000] Out of memory: Killed process 20495 (python3) total-vm:1024kB\n\
                   [ 1991.000000] oom_reaper: reaped process 20495 (python3), now anon-rss:0kB\n";
    let facts = parse_kernel_signals_with_boot_epoch(content, "dmesg", 2000, Some(0));
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(
        facts.oom_latest_constraint.as_deref(),
        Some("CONSTRAINT_MEMCG")
    );
    assert_eq!(
        facts.oom_latest_task_cgroup.as_deref(),
        Some("/parent/child")
    );
    assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/parent"));
    assert_eq!(facts.oom_latest_source_line_count, 3);
}

#[test]
fn kernel_510_host_oom_reports_host_constraint() {
    let content = "[ 1990.100000] python3 invoked oom-killer: gfp_mask=0xcc0(GFP_KERNEL), order=0\n\
                   [ 1990.100000] oom-kill:constraint=CONSTRAINT_NONE,nodemask=(null),cpuset=/,mems_allowed=0-1,global_oom,task_memcg=/,task=python3,pid=20495,uid=0\n\
                   [ 1990.200000] Out of memory: Killed process 20495 (python3) total-vm:1024kB\n";
    let facts = parse_kernel_signals_with_boot_epoch(content, "dmesg", 2000, Some(0));
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(
        facts.oom_latest_constraint.as_deref(),
        Some("CONSTRAINT_NONE")
    );
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_latest_pid, Some(20495));
}

#[test]
fn alinux4_6x_cgroup_oom_reports_victim_and_scope() {
    let content = "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=/,mems_allowed=0-3,oom_memcg=/kubepods/burstable/podabc,task_memcg=/kubepods/burstable/podabc/cri-containerd-def.scope,task=java,pid=31337,uid=0\n\
                   1990.0 kernel: Out of memory: Killed process 31337 (java) total-vm:8388608kB, anon-rss:4194304kB, file-rss:0kB, shmem-rss:0kB, UID:0 pgtables:8192kB oom_score_adj:0\n\
                   1991.0 kernel: oom_reaper: reaped process 31337 (java), now anon-rss:0kB, file-rss:0kB, shmem-rss:0kB\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_pid, Some(31337));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("java"));
    assert_eq!(
        facts.oom_latest_oom_cgroup.as_deref(),
        Some("/kubepods/burstable/podabc")
    );
    assert_eq!(facts.oom_latest_source_line_count, 3);
}

#[test]
fn kill_fragment_without_an_invoker_line_is_kept() {
    let content = "1990.0 kernel: Out of memory: Killed process 20495 (python3) total-vm:1024kB\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_pid, Some(20495));
    assert_eq!(facts.oom_latest_confidence.as_deref(), Some("high"));
}

#[test]
fn reaper_only_fragment_is_kept_without_inventing_a_constraint() {
    let content = "1990.0 kernel: oom_reaper: reaped process 20495 (python3), now anon-rss:0kB\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_pid, Some(20495));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_latest_constraint, None);
}

#[test]
fn pid_reuse_outside_the_association_window_counts_separate_events() {
    let content = "1900.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=java,pid=71\n\
                   1900.0 kernel: Out of memory: Killed process 71 (java)\n\
                   1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=java,pid=71\n\
                   1990.0 kernel: Out of memory: Killed process 71 (java)\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 2);
    assert_eq!(facts.oom_latest_pid, Some(71));
}

#[test]
fn unclocked_oom_lines_lower_confidence_instead_of_claiming_recency() {
    let content = "kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=java,pid=71\n\
                   kernel: Out of memory: Killed process 71 (java)\n";
    let facts = parse_kernel_signals(content, "journalctl", 2000);
    assert_eq!(facts.oom_event_count_last_1h, 0);
    assert_eq!(facts.oom_latest_age_seconds, None);
    assert_eq!(facts.oom_latest_confidence.as_deref(), Some("low"));
}

#[test]
fn oom_storm_stress_thousands_of_events_parse_within_check_budget() {
    let mut content = String::new();
    for index in 0..5000 {
        content.push_str(&format!(
            "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/storm,task=java,pid={index}\n\
             1990.0 kernel: Out of memory: Killed process {index} (java) total-vm:1024kB\n"
        ));
    }
    let started = Instant::now();
    let facts = parse_kernel_signals(&content, "journalctl", 2000);
    let elapsed = started.elapsed();
    eprintln!(
        "stress parse_storm: bytes={} events={} elapsed_ms={}",
        content.len(),
        facts.oom_event_count_last_1h,
        elapsed.as_millis()
    );
    assert_eq!(facts.oom_event_count_last_1h, 5000);
    assert_eq!(facts.oom_event_count_last_24h, 5000);
    assert_eq!(facts.oom_latest_pid, Some(4999));
    assert!(elapsed < Duration::from_millis(120));
}

#[test]
fn oom_storm_stress_interleaved_cgroups_count_every_event() {
    let mut content = String::new();
    for batch in 0..50 {
        for slot in 0..4 {
            let pid = batch * 4 + slot;
            content.push_str(&format!(
                "1990.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/cg{slot},task=java,pid={pid}\n"
            ));
        }
        for slot in 0..4 {
            let pid = batch * 4 + slot;
            content.push_str(&format!(
                "1990.0 kernel: Out of memory: Killed process {pid} (java)\n"
            ));
        }
    }
    let started = Instant::now();
    let facts = parse_kernel_signals(&content, "journalctl", 2000);
    let elapsed = started.elapsed();
    eprintln!(
        "stress parse_interleaved: bytes={} events={} elapsed_ms={}",
        content.len(),
        facts.oom_event_count_last_1h,
        elapsed.as_millis()
    );
    assert_eq!(facts.oom_event_count_last_1h, 200);
    assert_eq!(facts.oom_latest_pid, Some(199));
    assert!(elapsed < Duration::from_millis(120));
}
