use super::*;

#[test]
fn kernel_record_scope_and_coverage_belong_to_the_observed_event() {
    use crate::diagnostics::health::model::{HealthFactCoverage, HealthFactScope};
    let facts = parse_kernel_signals("1000.0 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/workload,task=worker,pid=12\n", "journalctl", 1100);
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder.start_check_id(
        "kernel.oom",
        HealthCollector::KernelSignal,
        HealthFactScope::Unknown,
        HealthFactCoverage::Unknown,
        1,
    );
    facts.record(&mut builder, 0);
    builder.add_check_done("kernel.oom").complete_check();
    let report = builder.finish(2);
    let metadata = report
        .fact_metadata
        .get("kernel.oom_latest_age_seconds")
        .unwrap();
    assert_eq!(metadata.scope, HealthFactScope::Cgroup("/workload".into()));
    assert!(matches!(
        metadata.coverage,
        HealthFactCoverage::ReadWindow {
            since_ms: Some(1_000_000),
            max_records: 200,
            complete: false,
            ..
        }
    ));
}

#[test]
fn passive_mount_scope_does_not_report_rejected_paths_as_checked() {
    let _guard = crate::diagnostics::test_env::env_guard();
    use crate::diagnostics::health::model::{
        HealthCheckStatus, HealthFactCoverage, HealthFactScope,
    };
    let config = HealthConfig {
        scan_mounts: Some(vec!["/not-a-verified-local-mount".into()]),
        ..HealthConfig::default()
    };
    let sample = read_disk_sample(&config, true, true, true).unwrap();
    let mut builder = HealthReportBuilder::for_started_at(1);
    for id in ["filesystem.root", "filesystem.mounts"] {
        builder.start_check_id(
            id,
            HealthCollector::Disk,
            HealthFactScope::Unknown,
            HealthFactCoverage::Snapshot,
            1,
        );
        sample.record(&mut builder, 0);
        builder.add_check_done(id).complete_check();
    }
    let report = builder.finish(2);
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "filesystem.root")
            .unwrap()
            .status,
        HealthCheckStatus::Ok
    );
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "filesystem.mounts")
            .unwrap()
            .status,
        HealthCheckStatus::Unavailable
    );
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::Unsupported));
}

#[test]
fn disk_sample_keeps_each_mount_instead_of_only_the_maximum() {
    let facts = parse_df_p("Filesystem Type 1024-blocks Used Available Capacity Mounted on\n/dev/a xfs 100000000 20000000 80000000 20% /\n/dev/b xfs 100000000 97000000 3000000 97% /data\n", &["/".into()]).unwrap();
    let mut builder = HealthReportBuilder::for_started_at(1);
    facts.record(&mut builder, 0);
    let report = builder.finish(2);
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "filesystem.mount./data.used_ratio"
            && fact.value == HealthFactValue::Float(0.97)));
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "filesystem.root_used_ratio"
            && fact.value == HealthFactValue::Float(0.20)));
}

#[test]
fn system_collectors_do_not_execute_path_wrappers() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("wrapper-ran");
    for program in ["df", "hostname", "journalctl", "dmesg", "bash"] {
        let path = directory.path().join(program);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf reached > '{}'\nexit 1\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let previous = std::env::var_os("PATH");
    std::env::set_var("PATH", directory.path());
    let mut builder = HealthReportBuilder::for_started_at(1);
    let config = HealthConfig {
        scan_mounts: Some(vec!["/".into()]),
        ..HealthConfig::default()
    };
    collect_disk(&mut builder, &config);
    collect_kernel_signals(&mut builder, 100);
    collect_host(&mut builder);
    match previous {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    assert!(
        !marker.exists(),
        "user PATH code ran during system collection"
    );
}

#[test]
fn swap_check_does_not_publish_disabled_memory_facts() {
    use crate::diagnostics::health::model::{HealthFactCoverage, HealthFactScope};
    let facts = parse_proc_meminfo("MemTotal: 8388608 kB\nMemAvailable: 2097152 kB\nSwapTotal: 4194304 kB\nSwapFree: 1048576 kB\n").unwrap();
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder.start_check_id(
        "resource.swap",
        HealthCollector::Memory,
        HealthFactScope::Host,
        HealthFactCoverage::Snapshot,
        1,
    );
    facts.record(&mut builder, 0);
    builder.add_check_done("resource.swap").complete_check();
    let report = builder.finish(2);
    assert!(!report.facts.is_empty());
    assert!(report
        .facts
        .iter()
        .all(|fact| fact.key.starts_with("memory.swap_")));
}

#[test]
fn parses_host_text_facts() {
    let os_release = r#"
NAME="Anolis OS"
PRETTY_NAME="Anolis OS 23"
"#;

    assert_eq!(
        parse_os_release_pretty_name(os_release).as_deref(),
        Some("Anolis OS 23")
    );
    assert_eq!(parse_proc_uptime_seconds("123.45 678.90"), Some(123));
}

#[test]
fn parses_loadavg_and_records_cpu_facts() {
    let facts = parse_proc_loadavg("8.00 4.00 2.00 1/100 42", 4).expect("loadavg");
    let mut builder = HealthReportBuilder::for_started_at(1);

    facts.record(&mut builder, 3);
    let report = builder.finish(5);

    assert_eq!(facts.load_per_core_1m, 2.0);
    assert_eq!(facts.load_per_core_5m, 1.0);
    assert_eq!(report.facts.len(), 6);
    assert_eq!(report.facts[0].key, "cpu.cores");
    assert_eq!(report.facts[5].key, "cpu.load_per_core_5m");
}

#[test]
fn parses_proc_stat_cpu_cores() {
    let cores = parse_proc_stat_cpu_cores(
        r#"
cpu  1 2 3 4 5 6 7 8 9 10
cpu0 1 2 3 4 5 6 7 8 9 10
cpu1 1 2 3 4 5 6 7 8 9 10
intr 0
"#,
    );

    assert_eq!(cores, Some(2));
    assert_eq!(parse_proc_stat_cpu_cores("cpu 1 2 3\nintr 0"), None);
}

#[test]
fn parses_meminfo_using_memavailable_and_swap_free() {
    let facts = parse_proc_meminfo(
        r#"
MemTotal:       8388608 kB
MemFree:         100000 kB
MemAvailable:    786432 kB
SwapTotal:      2097152 kB
SwapFree:        524288 kB
"#,
    )
    .expect("meminfo");

    assert_eq!(facts.total_mib, 8192);
    assert_eq!(facts.available_mib, 768);
    assert_eq!(facts.swap_total_mib, 2048);
    assert_eq!(facts.swap_used_mib, 1536);
    assert!((facts.available_ratio - 0.09375).abs() < f64::EPSILON);
    assert!((facts.swap_used_ratio - 0.75).abs() < f64::EPSILON);
}

#[test]
fn parses_df_with_type_and_keeps_critical_overlay_mount() {
    let facts = parse_df_p(
        r#"
Filesystem     Type     1024-blocks    Used Available Capacity Mounted on
overlay        overlay     10485760 9437184   1048576      90% /
/dev/vdb1      xfs        104857600 83886080 20971520      80% /data
tmpfs          tmpfs        1048576       0   1048576       0% /run
"#,
        &["/".to_string()],
    )
    .expect("df");

    assert_eq!(facts.riskiest_mount, "/");
    assert_eq!(facts.riskiest_type, "overlay");
    assert_eq!(facts.max_used_ratio, 0.90);
    assert_eq!(facts.available_gib, 1.0);
    assert_eq!(facts.root_used_ratio, Some(0.90));
    assert_eq!(facts.root_available_gib, Some(1.0));
}

#[test]
fn parses_df_without_type_and_ignores_tmpfs_when_type_is_available() {
    let facts = parse_df_p(
        r#"
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1        10485760 9437184   1048576      90% /
/dev/vdb1       104857600 99614720  5242880      95% /data
"#,
        &["/".to_string()],
    )
    .expect("df");

    assert_eq!(facts.riskiest_mount, "/data");
    assert_eq!(facts.riskiest_type, "unknown");
    assert_eq!(facts.max_used_ratio, 0.95);
    assert_eq!(facts.available_gib, 5.0);
    assert_eq!(facts.root_used_ratio, Some(0.90));
    assert_eq!(facts.root_available_gib, Some(1.0));
}

#[test]
fn parses_df_with_type_ignores_read_only_image_mounts() {
    let facts = parse_df_p(
        r#"
Filesystem     Type     1024-blocks    Used Available Capacity Mounted on
/dev/vda2      ext4        10485760 1048576   9437184      10% /
/dev/vdb       iso9660        54352   54352         0     100% /mnt/lima-cidata
/dev/loop0     squashfs       64000   64000         0     100% /snap/core
"#,
        &["/".to_string()],
    )
    .expect("df");

    assert_eq!(facts.riskiest_mount, "/");
    assert_eq!(facts.riskiest_type, "ext4");
    assert_eq!(facts.max_used_ratio, 0.10);
    assert_eq!(facts.available_gib, 9.0);
    assert_eq!(facts.root_used_ratio, Some(0.10));
    assert_eq!(facts.root_available_gib, Some(9.0));
}

#[test]
fn command_runner_preserves_bounded_output_and_records_exit_code() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let output = run_health_command(
        "sh",
        &["-c", "printf abcdef; printf err >&2; exit 7"],
        HealthCommandConfig {
            timeout: Duration::from_secs(2),
            output_limit_bytes: 6,
        },
    )
    .expect("run command");

    assert_eq!(output.exit_code, Some(7));
    assert_eq!(output.stdout, "abcdef");
    assert_eq!(output.stderr, "err");
    assert!(!output.stdout_truncated);
    assert!(!output.stderr_truncated);
}

#[test]
fn command_runner_classifies_missing_and_timeout() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let missing = run_health_command(
        "cosh-health-command-missing-test",
        &[],
        HealthCommandConfig::default(),
    )
    .expect_err("missing command");
    assert_eq!(
        missing.unavailable_reason(),
        HealthUnavailableReason::CommandMissing
    );

    let timeout = run_health_command(
        "sh",
        &["-c", "sleep 2"],
        HealthCommandConfig {
            timeout: Duration::from_millis(20),
            output_limit_bytes: 1024,
        },
    )
    .expect_err("timeout");
    assert_eq!(
        timeout.unavailable_reason(),
        HealthUnavailableReason::Timeout
    );
}

#[test]
fn service_timeout_caps_per_command_and_total_budget() {
    assert_eq!(
        service_timeout_for_remaining(Duration::from_millis(2_000)),
        Some(Duration::from_millis(300))
    );
    assert_eq!(
        service_timeout_for_remaining(Duration::from_millis(100)),
        Some(Duration::from_millis(100))
    );
    assert_eq!(service_timeout_for_remaining(Duration::ZERO), None);
}

#[test]
fn parses_kernel_signals_from_short_unix_logs() {
    let facts = parse_kernel_signals(
        r#"
1999900.000 kernel: oom-kill: constraint=CONSTRAINT_NONE
1999950.000 kernel: Out of memory: Killed process 1234 (mysql) total-vm:1024kB
1999960.000 kernel: Kernel panic - not syncing: fatal exception
"#,
        "journalctl",
        2_000_000,
    );
    let mut builder = HealthReportBuilder::for_started_at(1);

    facts.record(&mut builder, 4);
    let report = builder.finish(8);

    assert_eq!(facts.oom_latest_age_seconds, Some(50));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("mysql"));
    assert_eq!(facts.oom_latest_pid, Some(1234));
    assert_eq!(facts.oom_event_count_last_1h, 2);
    assert_eq!(facts.oom_event_count_last_24h, 2);
    assert_eq!(facts.oom_latest_confidence.as_deref(), Some("high"));
    assert!(facts.panic_recent);
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.oom_latest_age_seconds"));
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.log_source"));
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.oom_latest_pid"));
}

#[test]
fn parses_kernel_signals_from_dmesg_uptime_prefix() {
    let facts = parse_kernel_signals_with_boot_epoch(
        r#"
[ 1990.100000] python3 invoked oom-killer: gfp_mask=0xcc0(GFP_KERNEL), order=0
[ 1995.200000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/cosh_kernel_oom,task=python3,pid=20495,uid=0
"#,
        "dmesg",
        2_000_000,
        Some(1_998_000),
    );

    assert_eq!(facts.oom_latest_age_seconds, Some(5));
    assert_eq!(facts.oom_latest_pid, Some(20495));
    assert_eq!(
        facts.oom_latest_constraint.as_deref(),
        Some("CONSTRAINT_MEMCG")
    );
    assert_eq!(
        facts.oom_latest_oom_cgroup.as_deref(),
        Some("/cosh_kernel_oom")
    );
    let mut builder = HealthReportBuilder::for_started_at(1);
    facts.record(&mut builder, 4);
    let report = builder.finish(8);
    assert!(report.facts.iter().any(|fact| {
        fact.key == "kernel.oom_latest_scope_label_id"
            && fact.value == HealthFactValue::String("memcg".to_string())
    }));
}

#[test]
fn parses_kernel_signals_keep_latest_oom_details_together() {
    let facts = parse_kernel_signals(
        r#"
1999000.000 kernel: oom-kill:constraint=CONSTRAINT_NONE,task=old,pid=111
1999001.000 kernel: Out of memory: Killed process 111 (old) total-vm:1024kB
1999900.000 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/cosh,task_memcg=/cosh/session,task=python3,pid=222
1999901.000 kernel: Memory cgroup out of memory: Killed process 222 (python3) total-vm:2048kB
"#,
        "journalctl",
        2_000_000,
    );

    assert_eq!(facts.oom_latest_age_seconds, Some(99));
    assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
    assert_eq!(facts.oom_latest_pid, Some(222));
    assert_eq!(
        facts.oom_latest_constraint.as_deref(),
        Some("CONSTRAINT_MEMCG")
    );
    assert_eq!(
        facts.oom_latest_task_cgroup.as_deref(),
        Some("/cosh/session")
    );
    assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/cosh"));
    assert_eq!(facts.oom_event_count_last_1h, 2);
    assert_eq!(facts.oom_event_count_last_24h, 2);
    assert_eq!(facts.oom_latest_source_line_count, 2);
}

#[test]
fn parses_kernel_signals_unclocked_oom_as_low_confidence_only() {
    let facts = parse_kernel_signals(
        "kernel: Out of memory: Killed process 333 (worker) total-vm:1024kB",
        "dmesg",
        2_000_000,
    );

    assert_eq!(facts.oom_latest_age_seconds, None);
    assert_eq!(facts.oom_killed_process, None);
    assert_eq!(facts.oom_event_count_last_1h, 0);
    assert_eq!(facts.oom_latest_source_line_count, 1);
    assert_eq!(facts.oom_latest_confidence.as_deref(), Some("low"));
}

#[test]
fn parses_systemctl_is_active_status() {
    let output = HealthCommandOutput {
        exit_code: Some(3),
        stdout: "inactive\n".to_string(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        elapsed_ms: 2,
    };
    let fact = parse_systemctl_is_active("redis", &output).expect("service status");
    let mut builder = HealthReportBuilder::for_started_at(1);

    fact.record(&mut builder, output.elapsed_ms);
    let report = builder.finish(3);

    assert_eq!(fact.status, "inactive");
    assert_eq!(report.facts[0].key, "service.redis.status");
    assert_eq!(
        report.facts[0].value,
        HealthFactValue::String("inactive".to_string())
    );
}

#[test]
fn p6_classifies_insufficient_journal_permissions_without_parse_error() {
    for stderr in [
        "No journal files were opened due to insufficient permissions.",
        "Failed to open journal: Permission denied",
        "dmesg: read kernel buffer failed: Operation not permitted",
    ] {
        assert_eq!(
            unavailable_from_stderr(stderr),
            HealthUnavailableReason::PermissionDenied,
            "{stderr}"
        );
    }
    assert_eq!(
        unavailable_from_stderr("Invalid journal header"),
        HealthUnavailableReason::ParseError
    );
}

#[test]
fn classifies_systemd_offline_as_service_unavailable() {
    let reason = unavailable_from_stderr(
        "System has not been booted with systemd as init system (PID 1). Can't operate.\n\
             Failed to connect to bus: Host is down",
    );

    assert_eq!(reason, HealthUnavailableReason::Unsupported);
}

#[test]
fn configured_service_output_degrades_without_systemd_runtime() {
    let output = HealthCommandOutput {
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "System has not been booted with systemd as init system (PID 1). Can't operate.\n\
                     Failed to connect to bus: Host is down"
            .to_string(),
        stdout_truncated: false,
        stderr_truncated: false,
        elapsed_ms: 4,
    };
    let mut builder = HealthReportBuilder::for_started_at(1);

    let result = record_configured_service_output(
        &mut builder,
        "definitely-not-a-real-unit.service",
        &output,
    );
    let report = builder.finish(2);

    assert!(result.is_err());
    assert!(!report
        .checks_done
        .contains(&"configured_service".to_string()));
    assert!(report.unavailable.iter().any(|item| {
        item.collector == HealthCollector::ConfiguredService
            && item.severity == HealthSeverity::Degraded
            && item.reason == HealthUnavailableReason::Unsupported
    }));
}

#[test]
fn service_status_rejects_truncated_output() {
    let output = HealthCommandOutput {
        exit_code: Some(0),
        stdout: "active\npartial".to_string(),
        stderr: String::new(),
        stdout_truncated: true,
        stderr_truncated: false,
        elapsed_ms: 1,
    };
    assert!(parse_systemctl_is_active("example", &output).is_none());
}
