use super::*;

const DMESG_OOM: &str = "[ 100.200000] Out of memory: Killed process 10 (worker) total-vm:1024kB";

#[test]
fn legacy_journal_window_can_retain_only_a_reaper() {
    let mut lines = vec![
        "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB".to_string(),
        "1991.200000 kernel: oom_reaper: reaped process 10 (worker), now anon-rss:0kB".to_string(),
    ];
    lines.extend((0..199).map(|index| format!("1992.0 kernel: ordinary message {index}")));
    lines.reverse();
    let filtered = |code, stderr: &str, content: &str| {
        let mut state = super::super::command::HealthLineFilterState::new(HealthLineFilter {
            newest_first: true,
            ..HealthLineFilter::default()
        });
        state.push(content.as_bytes(), &is_kernel_signal_line);
        state.mark_eof(&is_kernel_signal_line);
        super::super::HealthFilteredCommandOutput {
            filtered: state.finish(),
            stderr: stderr.into(),
            exit_code: Some(code),
            elapsed_ms: 0,
        }
    };
    let mut calls = 0;
    let output = read_journal_signals(HealthCommandConfig::default(), |args, _| {
        calls += 1;
        assert!(args.contains(&"--reverse"));
        if args.contains(&"--grep") {
            return Ok(filtered(1, "unknown option --grep", ""));
        }
        let count = args.windows(2).find(|pair| pair[0] == "-n").unwrap()[1]
            .parse::<usize>()
            .unwrap();
        assert_eq!(count, 200);
        Ok(filtered(
            0,
            "",
            &lines
                .iter()
                .take(count)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    })
    .unwrap();
    assert_eq!(calls, 2);
    assert!(!output.stdout_truncated);
    assert!(output.stdout.contains("oom_reaper"));
    assert!(!output.stdout.contains("Killed process"));
    let facts = parse_reverse_journal_signals(&output.stdout, 2000);
    assert_eq!(facts.oom_latest_pid, Some(10));
    assert_eq!(facts.oom_event_count_last_1h, 1);
    assert_eq!(facts.oom_latest_event_identity, None);
}

#[test]
fn dmesg_event_identity_survives_wall_epoch_estimation_drift() {
    let first = parse_kernel_signals_with_boot_epoch(DMESG_OOM, "dmesg", 1000, Some(799));
    let later = parse_kernel_signals_with_boot_epoch(DMESG_OOM, "dmesg", 1001, Some(800));
    assert_ne!(first.oom_latest_event_epoch, later.oom_latest_event_epoch);
    assert!(first.oom_latest_event_identity.is_some());
    assert_eq!(
        first.oom_latest_event_identity,
        later.oom_latest_event_identity
    );
    assert_eq!(first.oom_latest_pid, later.oom_latest_pid);
}

#[test]
fn dmesg_distinct_subsecond_records_keep_distinct_event_identities() {
    let first = parse_kernel_signals_with_boot_epoch(DMESG_OOM, "dmesg", 1000, Some(800));
    let later_line = DMESG_OOM.replace("100.200000", "100.800000");
    let later = parse_kernel_signals_with_boot_epoch(&later_line, "dmesg", 1000, Some(800));
    assert_eq!(first.oom_latest_event_epoch, later.oom_latest_event_epoch);
    assert!(first.oom_latest_event_identity.is_some());
    assert_ne!(
        first.oom_latest_event_identity,
        later.oom_latest_event_identity
    );
}

#[test]
fn journal_event_identity_uses_the_logged_event_time() {
    let line = "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let first = parse_kernel_signals_with_boot_epoch(line, "journalctl", 2000, None);
    let later = parse_kernel_signals_with_boot_epoch(line, "journalctl", 2500, None);
    assert_eq!(first.oom_latest_event_epoch, Some(1990));
    assert!(first.oom_latest_event_identity.is_some());
    assert_eq!(
        first.oom_latest_event_identity,
        later.oom_latest_event_identity
    );
    let new_line = line.replace("1990.200000", "1991.200000");
    let new_event = parse_kernel_signals_with_boot_epoch(&new_line, "journalctl", 2500, None);
    assert_ne!(
        first.oom_latest_event_identity,
        new_event.oom_latest_event_identity
    );
}

#[test]
fn appended_reaper_preserves_journal_and_dmesg_event_identity() {
    for (source, kill_time, reaper_time, boot_epoch) in [
        (
            "journalctl",
            "1990.200000 kernel:",
            "1991.200000 kernel:",
            None,
        ),
        ("dmesg", "[ 100.200000]", "[ 100.800000]", Some(1890)),
        ("dmesg", "[ 100.200000]", "[ 101.200000]", Some(1890)),
    ] {
        let killed =
            format!("{kill_time} Out of memory: Killed process 10 (worker) total-vm:1024kB");
        let appended = format!(
            "{killed}\n{reaper_time} oom_reaper: reaped process 10 (worker), now anon-rss:0kB"
        );
        let first = parse_kernel_signals_with_boot_epoch(&killed, source, 2000, boot_epoch);
        let later = parse_kernel_signals_with_boot_epoch(&appended, source, 2000, boot_epoch);
        assert_eq!(later.oom_event_count_last_1h, 1, "{source}: {appended}");
        assert_eq!(first.oom_latest_pid, later.oom_latest_pid);
        assert_eq!(
            first.oom_latest_event_identity, later.oom_latest_event_identity,
            "{source}: {appended}"
        );
        assert_eq!(later.oom_latest_age_seconds, first.oom_latest_age_seconds);
    }
}

#[test]
fn appended_reaper_does_not_reexpose_actual_login_health_candidate() {
    let killed = "1990.200000 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=worker,pid=10\n1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let appended = format!(
        "{killed}\n1991.200000 kernel: oom_reaper: reaped process 10 (worker), now anon-rss:0kB"
    );
    let new_event = killed
        .replace("1990.200000", "1992.200000")
        .replace("pid=10", "pid=11")
        .replace("process 10", "process 11");
    let claims = oom_claims(&[killed, &appended, &new_event], "journalctl", None);
    assert_claimed_suppressed_claimed(&claims);
    assert_eq!(claims[0].0.text, claims[1].0.text);
    assert_eq!(claims[0].0.severity, claims[1].0.severity);
}

#[test]
fn window_losing_kill_record_does_not_reexpose_orphan_reaper() {
    for (source, killed, reaper, later, boot) in [
        (
            "journalctl",
            "1990.200000 kernel:",
            "1991.200000 kernel:",
            "1992.200000 kernel:",
            None,
        ),
        (
            "dmesg",
            "[ 100.200000]",
            "[ 101.200000]",
            "[ 102.200000]",
            Some(1890),
        ),
    ] {
        let killed = format!("{killed} Out of memory: Killed process 10 (worker) total-vm:1024kB");
        let reaper = format!("{reaper} oom_reaper: reaped process 10 (worker), now anon-rss:0kB");
        let full = format!("{killed}\n{reaper}");
        let new_event =
            format!("{later} Out of memory: Killed process 11 (worker) total-vm:1024kB");
        let claims = oom_claims(&[&full, &reaper, &new_event], source, boot);
        assert_claimed_suppressed_claimed(&claims);
        assert_eq!(claims[0].0.text, claims[1].0.text);
        assert_eq!(claims[0].0.severity, claims[1].0.severity);
    }
}

#[test]
fn window_losing_oom_header_keeps_the_same_confirmed_event() {
    let killed = "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let header =
        "1989.900000 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=worker,pid=10";
    let full = format!("{header}\n{killed}");
    let new_event = killed
        .replace("1990.200000", "1992.200000")
        .replace("process 10", "process 11");
    let claims = oom_claims(&[&full, killed, &new_event], "journalctl", None);
    assert_claimed_suppressed_claimed(&claims);
}

#[test]
fn first_orphan_claim_keeps_its_window_when_confirmed_logs_arrive() {
    use crate::config::guide::ClaimDecision;
    let reaper = "1991.200000 kernel: oom_reaper: reaped process 10 (worker), now anon-rss:0kB";
    let full = format!(
        "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB\n{reaper}"
    );
    let claims = oom_claims(&[reaper, &full], "journalctl", None);
    assert!(matches!(claims[0].1, ClaimDecision::Claimed(_)));
    assert!(matches!(claims[1].1, ClaimDecision::Suppressed));
}

#[test]
fn source_switch_cannot_prove_a_new_oom_event() {
    let journal = "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let dmesg = "[ 100.200000] Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let new_event = journal
        .replace("1990.200000", "1992.200000")
        .replace("process 10", "process 11");
    let claims = oom_claims_with_sources(&[
        (journal, "journalctl", None, Some("test-boot")),
        (dmesg, "dmesg", Some(1890), Some("test-boot")),
        (&new_event, "journalctl", None, Some("test-boot")),
    ]);
    assert_claimed_suppressed_claimed(&claims);
}

#[test]
fn boottime_event_needs_boot_identity_before_reexposure() {
    let first = "[ 100.200000] Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let new_event = first
        .replace("100.200000", "102.200000")
        .replace("process 10", "process 11");
    let claims = oom_claims_with_sources(&[
        (first, "dmesg", Some(1890), Some("test-boot")),
        (&new_event, "dmesg", Some(1890), None),
        (&new_event, "dmesg", Some(1890), Some("test-boot")),
    ]);
    assert_claimed_suppressed_claimed(&claims);
}

#[test]
fn epoch_event_ignores_optional_boot_metadata() {
    use crate::config::guide::ClaimDecision;
    let killed = "1990.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let claims = oom_claims_with_sources(&[
        (killed, "journalctl", None, Some("test-boot")),
        (killed, "journalctl", None, None),
    ]);
    assert!(matches!(claims[0].1, ClaimDecision::Claimed(_)));
    assert!(matches!(claims[1].1, ClaimDecision::Suppressed));
}

#[test]
fn reaper_does_not_upgrade_the_same_oom_at_the_recency_boundary() {
    let killed = "1699.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let appended = format!(
        "{killed}\n1701.200000 kernel: oom_reaper: reaped process 10 (worker), now anon-rss:0kB"
    );
    let new_event = "1992.200000 kernel: Out of memory: Killed process 11 (worker) total-vm:1024kB";
    let claims = oom_claims(&[killed, &appended, new_event], "journalctl", None);
    assert_claimed_suppressed_claimed(&claims);
    assert_eq!(claims[0].0.severity, claims[1].0.severity);
}

#[test]
fn reaper_does_not_refresh_an_expired_oom_count_window() {
    let killed = "1000.200000 kernel: Out of memory: Killed process 10 (worker) total-vm:1024kB";
    let appended = format!(
        "{killed}\n1001.200000 kernel: oom_reaper: reaped process 10 (worker), now anon-rss:0kB"
    );
    let facts = parse_kernel_signals_with_boot_epoch(&appended, "journalctl", 4601, None);
    assert_eq!(facts.oom_latest_event_epoch, Some(1000));
    assert_eq!(facts.oom_latest_age_seconds, Some(3601));
    assert_eq!(facts.oom_event_count_last_1h, 0);
}

#[test]
fn old_reaper_does_not_replace_a_newer_oom_victim() {
    let killed = "1990.200000 kernel: Out of memory: Killed process 10 (first) total-vm:1024kB\n1991.200000 kernel: Out of memory: Killed process 11 (second) total-vm:1024kB";
    let appended = format!(
        "{killed}\n1992.200000 kernel: oom_reaper: reaped process 10 (first), now anon-rss:0kB"
    );
    let facts = parse_kernel_signals_with_boot_epoch(&appended, "journalctl", 2000, None);
    assert_eq!(facts.oom_latest_pid, Some(11));
    assert_eq!(facts.oom_latest_event_epoch, Some(1991));
}

#[test]
fn process_name_does_not_turn_a_reaper_into_a_kill_anchor() {
    let killed =
        "1990.200000 kernel: Out of memory: Killed process 10 (Killed process) total-vm:1024kB";
    let reaper =
        "1991.200000 kernel: oom_reaper: reaped process 10 (Killed process), now anon-rss:0kB";
    let full = format!("{killed}\n{reaper}");
    let new_event = killed
        .replace("1990.200000", "1992.200000")
        .replace("process 10", "process 11");
    assert_claimed_suppressed_claimed(&oom_claims(
        &[&full, reaper, &new_event],
        "journalctl",
        None,
    ));
}

#[test]
fn only_message_kill_records_can_anchor_an_event() {
    for message in [
        "oom_reaper: reaped process 10 (Killed process), now anon-rss:0kB",
        "oom_reaper: reaped process 10 (Killed process ), now anon-rss:0kB",
        "oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/Killed process 10 group,task=worker,pid=10",
        "diagnostic text says Killed process 10 (worker)",
    ] {
        let line = format!("1990.200000 kernel: {message}");
        let facts = parse_kernel_signals_with_boot_epoch(&line, "journalctl", 2000, None);
        assert_eq!(facts.oom_latest_event_identity, None, "{line}");
    }
    for line in [
        "1990.200000 kernel: Killed process 10 (worker)",
        "1990.200000 host kernel: Out of memory: Killed process 10 (worker)",
        "[ 100.200000] Memory cgroup out of memory: Killed process 10 (worker)",
        "[ 100.200000] [ T50] Out of memory: Killed process 10 (worker)",
    ] {
        let source = if line.starts_with('[') {
            "dmesg"
        } else {
            "journalctl"
        };
        let facts = parse_kernel_signals_with_boot_epoch(line, source, 2000, Some(1890));
        assert!(facts.oom_latest_event_identity.is_some(), "{line}");
        assert_eq!(facts.oom_latest_pid, Some(10));
    }
}

fn assert_claimed_suppressed_claimed(
    claims: &[(
        crate::ui::guidance::Invitation,
        crate::config::guide::ClaimDecision,
    )],
) {
    use crate::config::guide::ClaimDecision;
    assert!(matches!(claims[0].1, ClaimDecision::Claimed(_)));
    assert!(
        matches!(claims[1].1, ClaimDecision::Suppressed),
        "{:?}",
        claims[1].1
    );
    assert!(
        matches!(claims[2].1, ClaimDecision::Claimed(_)),
        "{:?}",
        claims[2].1
    );
    assert_eq!(claims[0].0.problem, claims[1].0.problem);
}

fn oom_claims(
    logs: &[&str],
    source: &str,
    boot_epoch: Option<u64>,
) -> Vec<(
    crate::ui::guidance::Invitation,
    crate::config::guide::ClaimDecision,
)> {
    let readings = logs
        .iter()
        .map(|log| (*log, source, boot_epoch, Some("test-boot")))
        .collect::<Vec<_>>();
    oom_claims_with_sources(&readings)
}

fn oom_claims_with_sources(
    readings: &[(&str, &str, Option<u64>, Option<&str>)],
) -> Vec<(
    crate::ui::guidance::Invitation,
    crate::config::guide::ClaimDecision,
)> {
    use crate::config::guide::{
        ClaimRequest, GuideStore, HealthExposure, InviteKind, RuntimeState,
    };
    use crate::config::HealthConfig;
    use crate::diagnostics::health::{apply_judgement_rules, HealthReportBuilder};
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "linux")]
    let directory = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = GuideStore::open(root.join("persistent")).unwrap();
    let runtime = RuntimeState::open(&root).unwrap();
    store.initialize_runtime(&runtime).unwrap();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let config = HealthConfig::default();
    let mut claims = Vec::new();
    for (index, (log, source, boot_epoch, boot_id)) in readings.iter().copied().enumerate() {
        let facts = parse_kernel_signals_with_boot_epoch(log, source, 2000, boot_epoch);
        let mut builder = HealthReportBuilder::for_started_at(2_000_000 + index as u128);
        facts.record(&mut builder, 0);
        if let Some(boot_id) = boot_id {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.boot_id",
                HealthFactValue::String(boot_id.into()),
                None,
                HealthFactSource::Fixture,
                0,
            );
        }
        let mut report = builder.finish(2_000_000 + index as u128);
        apply_judgement_rules(&mut report, &config);
        crate::diagnostics::health::recommendation::apply_try_candidates(&mut report, &config);
        let candidate =
            crate::ui::guidance::health_candidate(&report, crate::Language::EnUs).unwrap();
        let decision = store
            .claim_health_version(
                Some(&runtime),
                &snapshot,
                ClaimRequest {
                    kind: InviteKind::LoginHealth,
                    host: "host",
                    problem: &candidate.problem,
                    session: &report.scan_id,
                },
                1000 + index as u64 * 1000,
                HealthExposure {
                    evidence: candidate.evidence.as_deref().unwrap(),
                    event: candidate
                        .event
                        .as_ref()
                        .map(|(domain, value)| (*domain, value.as_str())),
                    severity: candidate.severity,
                    legacy_problems: &candidate.legacy_problems,
                },
            )
            .unwrap();
        claims.push((candidate, decision));
    }
    claims
}
