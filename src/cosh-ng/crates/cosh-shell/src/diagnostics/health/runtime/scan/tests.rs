use super::super::clock::boottime_ns;
use super::super::clock::HealthDeadline;
use super::*;
use std::io::Write;

#[test]
fn review_source_deadline_is_enforced_by_parent() {
    let (mut handle, _peer) = staged(Duration::from_millis(250));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    static OWNER: std::sync::OnceLock<ReapOwner> = std::sync::OnceLock::new();
    let owner = OWNER.get_or_init(ReapOwner::new);
    let mut command = Command::new("/bin/sleep");
    command
        .arg("2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(
        &mut command,
        running.request.deadline.job_deadline().unwrap(),
    );
    let child = command.spawn().unwrap();
    running.child_pgid = Some(child.id());
    running.child = Some(owner.reserve().unwrap().register(
        child,
        CleanupScope::OwnedGroup,
        Instant::now() + Duration::from_millis(250),
    ));
    running.plan = vec![SourcePlan {
        source: super::super::super::plan::SampleSource::Load,
        check_ids: vec!["resource.load".into()],
        budget: Duration::from_millis(10),
        finish: FinishPolicy::ReuseOnSuccess,
    }];
    running.awaiting = None;
    running.next_source = 0;
    running.dispatch_next();
    thread::sleep(Duration::from_millis(60));
    let report = handle.poll_ready();
    assert!(
        report.is_some(),
        "parent still waits after a 10ms source deadline, with 190ms global budget remaining"
    );
}

#[test]
fn review_wrong_source_fact_is_rejected() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running
        .builder
        .declare_check_id("resource.memory", HealthCollector::Memory, true);
    running.enabled.insert("resource.memory".into());
    running.plan = vec![SourcePlan {
        source: super::super::super::plan::SampleSource::Meminfo,
        check_ids: vec!["resource.memory".into()],
        budget: Duration::from_millis(30),
        finish: FinishPolicy::ReuseOnSuccess,
    }];
    running.next_source = 1;
    running.awaiting = Some(awaited(running.request.deadline));
    send(&mut peer, 1, fact());
    let _ = handle.poll_ready();
    let ScanState::Running(running) = &handle.state else {
        return;
    };
    assert!(
        running.builder.report().facts.is_empty(),
        "Load fact accepted while only Meminfo is active"
    );
}

#[test]
fn late_parent_poll_keeps_ontime_frames_but_source_remains_timed_out() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    source_done(&mut peer, 3);
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running
        .awaiting
        .as_mut()
        .unwrap()
        .work_deadline
        .shorten_within(Duration::from_millis(5))
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    let report = handle.poll_ready().expect("expired source seals");
    assert_eq!(report.facts.len(), 1);
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "resource.load")
            .unwrap()
            .status,
        HealthCheckStatus::Partial
    );
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::Timeout));
}

#[test]
fn late_source_done_does_not_discard_ontime_facts() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    let task = running.awaiting.as_mut().unwrap();
    task.work_deadline
        .shorten_within(Duration::from_millis(5))
        .unwrap();
    let encoded = protocol::encode(&running.nonce, 3, Payload::SourceDone { task_id: 1 }).unwrap();
    let mut frame: serde_json::Value = serde_json::from_slice(&encoded[4..]).unwrap();
    frame["emitted_ns"] = (task.work_deadline.expires_ns + 1).into();
    let bytes = serde_json::to_vec(&frame).unwrap();
    peer.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
    peer.write_all(&bytes).unwrap();
    thread::sleep(Duration::from_millis(10));
    let report = handle.poll_ready().unwrap();
    assert_eq!(report.facts.len(), 1);
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::Timeout));
}

#[test]
fn expired_source_with_timely_done_and_incomplete_tail_discards_only_that_source() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    source_done(&mut peer, 3);
    peer.write_all(&[0]).unwrap();
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running
        .awaiting
        .as_mut()
        .unwrap()
        .work_deadline
        .shorten_within(Duration::from_millis(5))
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    let report = handle.poll_ready().unwrap();
    assert!(report.facts.is_empty());
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

#[test]
fn replacement_is_not_started_when_the_original_balance_cannot_fund_it() {
    let (mut handle, _peer) = staged(Duration::from_millis(10));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.awaiting = None;
    running.next_source = 0;
    running.generation = 7;
    running.task_id = 9;
    assert_eq!(
        running.dispatch_next(),
        Some(HealthUnavailableReason::Timeout)
    );
    assert!(running.child.is_none());
    assert!(running.awaiting.is_none());
    assert_eq!(running.next_source, 0);
    assert_eq!(running.generation, 7);
    assert_eq!(running.task_id, 9);
    assert!(running.builder.is_pending("resource.load"));
}

#[test]
fn collect_send_failure_still_records_the_source_attempt_and_gap() {
    let (mut handle, peer) = staged(Duration::from_secs(1));
    drop(peer);
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.awaiting = None;
    running.next_source = 0;
    let owner = ReapOwner::global().unwrap();
    let mut command = Command::new("/bin/sleep");
    command
        .arg("2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(
        &mut command,
        running.request.deadline.job_deadline().unwrap(),
    );
    let child = command.spawn().unwrap();
    running.child_pgid = Some(child.id());
    running.child = Some(owner.reserve().unwrap().register(
        child,
        CleanupScope::OwnedGroup,
        running.request.deadline.local_instant().unwrap(),
    ));
    assert!(running.dispatch_next().is_some());
    assert_eq!(
        running.next_source, 1,
        "failed send consumes this source's single attempt"
    );
    assert!(running
        .builder
        .report()
        .unavailable
        .iter()
        .any(|gap| gap.collector == HealthCollector::Cpu));
}

#[test]
fn source_protocol_failure_preserves_prior_facts_and_trusted_login_config() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.builder.apply_progress(fact()).unwrap();
    running.builder.apply_progress(completed()).unwrap();
    running
        .builder
        .set_login_config(crate::config::HealthLoginConfig {
            health: HealthConfig::default(),
            language: "en-US".into(),
            ai_enabled: true,
        });
    running.request.config_source = super::super::plan::ConfigSource::LoginConfigProjection;
    running
        .builder
        .declare_check_id("resource.memory", HealthCollector::Memory, true);
    running.enabled.insert("resource.memory".into());
    running.plan.push(SourcePlan {
        source: super::super::super::plan::SampleSource::Meminfo,
        check_ids: vec!["resource.memory".into()],
        budget: Duration::from_millis(30),
        finish: FinishPolicy::ReuseOnSuccess,
    });
    running.next_source = 2;
    running.awaiting.as_mut().unwrap().task_id = 2;
    peer.write_all(&((protocol::FRAME_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    let report = handle.poll_ready().unwrap();
    assert!(report.login_config.is_some());
    assert_eq!(report.facts.len(), 1);
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.collector == HealthCollector::Memory));
}

#[test]
fn environment_findings_cannot_borrow_evidence_from_another_source() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.builder.apply_progress(fact()).unwrap();
    running.builder.apply_progress(completed()).unwrap();
    running
        .builder
        .declare_check_id("environment.provider", HealthCollector::Provider, true);
    running.enabled.insert("environment.provider".into());
    running.plan.push(SourcePlan {
        source: super::super::super::plan::SampleSource::Environment,
        check_ids: vec!["environment.provider".into()],
        budget: Duration::from_millis(750),
        finish: FinishPolicy::ReuseOnSuccess,
    });
    running.next_source = 2;
    send(
        &mut peer,
        1,
        HealthProgress::Finding(crate::diagnostics::health::HealthFinding {
            id: "env-provider".into(),
            severity: HealthSeverity::Warning,
            category: crate::diagnostics::health::HealthFindingCategory::Anomaly,
            title_id:
                crate::diagnostics::health::HealthMessageId::HealthFindingProviderUnconfigured,
            detail_id: None,
            detail_args: Default::default(),
            evidence_fact_ids: vec!["cpu.cores".into()],
            suggested_try_ids: vec![],
        }),
    );
    let report = handle.poll_ready().unwrap();
    assert_eq!(report.facts.len(), 1);
    assert!(report
        .findings
        .iter()
        .all(|finding| finding.id != "env-provider"));
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.collector == HealthCollector::Provider));
}

#[test]
fn progress_from_another_task_is_rejected_before_report_mutation() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    peer.write_all(
        &protocol::encode(
            "0123456789abcdef0123456789abcdef",
            1,
            Payload::Progress {
                task_id: 2,
                progress: fact(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    let report = handle.poll_ready().unwrap();
    assert!(report.facts.is_empty());
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

#[test]
fn missing_swap_context_never_creates_an_unregistered_memory_check() {
    let (mut handle, _peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.builder.apply_progress(fact()).unwrap();
    running.builder.apply_progress(completed()).unwrap();
    running
        .builder
        .declare_check_id("resource.swap", HealthCollector::Memory, true);
    running.enabled.insert("resource.swap".into());
    let mut swap_fact = fact();
    if let HealthProgress::Fact { check_id, fact, .. } = &mut swap_fact {
        *check_id = "resource.swap".into();
        fact.id = "swap".into();
        fact.key = "memory.swap_used_ratio".into();
        fact.value = HealthFactValue::Float(0.9);
    }
    running.builder.apply_progress(swap_fact).unwrap();
    let mut used_fact = fact();
    if let HealthProgress::Fact { check_id, fact, .. } = &mut used_fact {
        *check_id = "resource.swap".into();
        fact.id = "swap-used".into();
        fact.key = "memory.swap_used_mib".into();
        fact.value = HealthFactValue::Float(2048.0);
    }
    running.builder.apply_progress(used_fact).unwrap();
    running
        .builder
        .apply_progress(HealthProgress::Check(HealthCheckResult {
            id: "resource.swap".into(),
            collector: Some(HealthCollector::Memory),
            status: HealthCheckStatus::Ok,
            fact_ids: vec!["swap".into(), "swap-used".into()],
            gaps: vec![],
            elapsed_ms: 1,
        }))
        .unwrap();
    running.awaiting = None;
    let report = handle.poll_ready().unwrap();
    assert!(!report
        .check_results
        .iter()
        .any(|check| check.id == "memory"));
    let swap = report
        .check_results
        .iter()
        .find(|check| check.id == "resource.swap")
        .unwrap();
    assert_eq!(swap.status, HealthCheckStatus::Partial);
    assert!(swap
        .gaps
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::MissingContext));
}

#[test]
fn malformed_login_tail_invalidates_configuration_even_with_all_checks_disabled() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.configured = false;
    running.request.config_source = super::super::plan::ConfigSource::LoginConfigProjection;
    let bytes = protocol::encode(
        &running.nonce,
        1,
        Payload::Configuration(crate::config::HealthLoginConfig {
            health: HealthConfig {
                enabled: false,
                ..HealthConfig::default()
            },
            language: "en-US".into(),
            ai_enabled: true,
        }),
    )
    .unwrap();
    peer.write_all(&bytes).unwrap();
    peer.write_all(&((protocol::FRAME_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    let report = handle.poll_ready().unwrap();
    assert!(report.login_config.is_none());
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

use super::super::plan::ScanContext;
use crate::config::HealthConfig;
use crate::diagnostics::health::model::{
    HealthCheckResult, HealthFact, HealthFactCategory, HealthFactCoverage, HealthFactMetadata,
    HealthFactScope, HealthFactSource, HealthFactValue,
};

fn staged(budget: Duration) -> (HealthScanHandle, UnixStream) {
    let (stream, peer) = UnixStream::pair().expect("test stream");
    stream.set_nonblocking(true).expect("nonblocking");
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder
        .declare_check_id("resource.load", HealthCollector::Cpu, true)
        .declare_check_id("resource.memory", HealthCollector::Memory, false);
    let request = WorkerRequest {
        scan_id: "health-1".to_string(),
        started_at_ms: 1,
        now_epoch_seconds: 100,
        deadline: HealthDeadline::after(budget).expect("deadline"),
        context: ScanContext::Resources,
        config: HealthConfig::default(),
        environment: None,
        disabled: Vec::new(),
        config_source: crate::diagnostics::health::runtime::plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
    };
    let deadline = request.deadline;
    let running = RunningScan {
        request,
        builder,
        child: None,
        child_pgid: None,
        stream: Some(stream),
        decoder: Decoder::default(),
        nonce: "0123456789abcdef0123456789abcdef".to_string(),
        sequence: 1,
        configured: true,
        enabled: BTreeSet::from(["resource.load".to_string()]),
        started: Instant::now(),
        started_ns: boottime_ns().expect("clock"),
        plan: vec![SourcePlan {
            source: super::super::super::plan::SampleSource::Load,
            check_ids: vec!["resource.load".into()],
            budget,
            finish: FinishPolicy::ReuseOnSuccess,
        }],
        next_source: 1,
        task_id: 0,
        generation: 1,
        awaiting: Some(awaited(deadline)),
        source_done: false,
        bootstrap_deadline: deadline,
    };
    (
        HealthScanHandle {
            state: ScanState::Running(Box::new(running)),
            cancelled: false,
        },
        peer,
    )
}

fn fact() -> HealthProgress {
    HealthProgress::Fact {
        check_id: "resource.load".to_string(),
        fact: HealthFact {
            id: "cpu.cores".to_string(),
            key: "cpu.cores".to_string(),
            category: HealthFactCategory::Cpu,
            value: HealthFactValue::Unsigned(4),
            unit: None,
            source: HealthFactSource::ProcLoadavg,
            elapsed_ms: 1,
        },
        metadata: HealthFactMetadata {
            scope: HealthFactScope::Host,
            sampled_at_ms: 2,
            coverage: HealthFactCoverage::Snapshot,
        },
    }
}

fn completed() -> HealthProgress {
    HealthProgress::Check(HealthCheckResult {
        id: "resource.load".to_string(),
        collector: Some(HealthCollector::Cpu),
        status: HealthCheckStatus::Ok,
        fact_ids: vec!["cpu.cores".to_string()],
        gaps: Vec::new(),
        elapsed_ms: 1,
    })
}

fn send(peer: &mut UnixStream, sequence: u64, progress: HealthProgress) {
    let bytes = protocol::encode(
        "0123456789abcdef0123456789abcdef",
        sequence,
        Payload::Progress {
            task_id: 1,
            progress,
        },
    )
    .expect("bounded frame");
    peer.write_all(&bytes).expect("test frame");
}

#[test]
fn deadline_can_only_shorten_and_report_can_only_be_taken_once() {
    let (mut handle, _peer) = staged(Duration::from_millis(20));
    let started = Instant::now();
    let report = handle
        .finish_within(Duration::from_secs(1))
        .expect("partial report");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(report.elapsed_ms >= 10);
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.collector == HealthCollector::Cpu
            && gap.reason == HealthUnavailableReason::Timeout));
    assert!(handle.poll_ready().is_none());
    assert!(handle.finish_within(Duration::from_secs(1)).is_none());
}

#[test]
fn sealing_keeps_completed_frames_even_when_the_parent_has_not_polled() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    let report = handle
        .finish_within(Duration::ZERO)
        .expect("sealed partial");
    assert_eq!(report.facts.len(), 1);
    assert_eq!(
        report.fact_metadata["cpu.cores"].scope,
        HealthFactScope::Host
    );
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "resource.load")
            .expect("cpu")
            .status,
        HealthCheckStatus::Partial
    );
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "resource.memory")
            .expect("disabled memory")
            .status,
        HealthCheckStatus::Disabled
    );
    assert!(handle.poll_ready().is_none());
}

#[test]
fn sealing_rejects_records_emitted_after_the_deadline() {
    let (mut handle, mut peer) = staged(Duration::from_millis(1));
    thread::sleep(Duration::from_millis(5));
    send(&mut peer, 1, fact());
    let report = handle
        .finish_within(Duration::from_secs(1))
        .expect("expired report");
    assert!(report.facts.is_empty());
    assert!(handle.poll_ready().is_none());
}

#[test]
fn completed_check_waits_for_source_done_without_waiting_for_eof() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    assert!(
        handle.poll_ready().is_none(),
        "check completion alone cannot seal a source"
    );
    source_done(&mut peer, 3);
    let report = handle
        .poll_ready()
        .expect("completed scan despite open peer");
    assert_eq!(report.checks_done, ["cpu"]);
    assert!(report.unavailable.is_empty());
    assert!(handle.poll_ready().is_none());
}

#[test]
fn cancelled_scan_never_reopens_its_report() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    assert!(handle.poll_ready().is_none());
    handle.cancel();
    assert!(handle.poll_ready().is_none());
    assert!(handle.finish_within(Duration::from_secs(1)).is_none());
    let ScanState::Sealed(Some(report)) = &handle.state else {
        panic!("cancelled report must remain sealed");
    };
    assert_eq!(report.facts.len(), 1);
}

#[test]
fn malformed_sequence_cannot_mutate_the_report() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 2, fact());
    let report = handle.poll_ready().expect("invalid stream closes scan");
    assert!(report.facts.is_empty());
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

#[test]
fn unconfirmed_process_cleanup_cannot_project_a_healthy_scan() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    static OWNER: std::sync::OnceLock<ReapOwner> = std::sync::OnceLock::new();
    let owner = OWNER.get_or_init(ReapOwner::new);
    let permit = owner.reserve().expect("child capacity");
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "/bin/sleep 1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(
        &mut command,
        running
            .request
            .deadline
            .job_deadline()
            .expect("job deadline"),
    );
    let child = command.spawn().expect("isolated job");
    running.child = Some(permit.register(child, CleanupScope::OwnedGroup, Instant::now()));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    source_done(&mut peer, 3);
    let report = handle.poll_ready().expect("scan sealed");
    assert!(
        !report.unavailable.is_empty(),
        "unconfirmed cleanup is not a successful completed scan"
    );
    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(2);
    while owner.reap_pending() {
        assert!(Instant::now() < deadline, "test child remained unreaped");
        std::thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

#[test]
fn malformed_tail_after_completed_checks_cannot_project_healthy() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    peer.write_all(&((protocol::FRAME_LIMIT + 1) as u32).to_be_bytes())
        .expect("bad tail");
    let report = handle.poll_ready().expect("invalid tail seals scan");
    assert!(!report.unavailable.is_empty());
}

#[test]
fn incomplete_tail_after_completed_checks_cannot_project_healthy() {
    for tail in [
        vec![0],
        vec![0, 0],
        vec![0, 0, 0],
        vec![0, 0, 0, 8, b'{', b'"'],
    ] {
        let (mut handle, mut peer) = staged(Duration::from_secs(1));
        send(&mut peer, 1, fact());
        send(&mut peer, 2, completed());
        source_done(&mut peer, 3);
        peer.write_all(&tail).unwrap();
        let report = handle.poll_ready().expect("completed scan seals");
        assert!(
            report.facts.is_empty(),
            "protocol failure discards facts from the unsealed source"
        );
        assert!(
            !report.unavailable.is_empty(),
            "incomplete tail {tail:?} cannot be healthy"
        );
    }
}

#[test]
fn source_done_with_mismatched_task_id_is_rejected() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.awaiting = Some(awaited(running.request.deadline));
    let bytes = protocol::encode(&running.nonce, 1, Payload::SourceDone { task_id: 2 }).unwrap();
    peer.write_all(&bytes).unwrap();
    let report = handle.poll_ready().expect("mismatch seals");
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

#[test]
fn collect_payload_from_worker_is_rejected() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    let bytes = protocol::encode(
        &running.nonce,
        1,
        Payload::Collect(CollectTask {
            task_id: 1,
            source: super::super::super::plan::SampleSource::Load,
            check_ids: vec!["resource.load".to_string()],
            deadline: HealthDeadline::after(Duration::from_millis(30)).unwrap(),
            work_deadline: HealthDeadline::after(Duration::from_millis(20)).unwrap(),
        }),
    )
    .unwrap();
    peer.write_all(&bytes).unwrap();
    let report = handle.poll_ready().expect("collect seals");
    assert!(report
        .unavailable
        .iter()
        .any(|gap| gap.reason == HealthUnavailableReason::ParseError));
}

#[test]
fn matching_source_done_consumes_the_awaited_task() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    running.awaiting = Some(awaited(running.request.deadline));
    let bytes = protocol::encode(&running.nonce, 3, Payload::SourceDone { task_id: 1 }).unwrap();
    peer.write_all(&bytes).unwrap();
    assert!(
        handle.poll_ready().is_some(),
        "completed source seals the completed scan"
    );
}

#[test]
fn completed_scan_seals_healthy_after_last_source_retires_the_stream() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
    let ScanState::Running(running) = &mut handle.state else {
        unreachable!()
    };
    if let Some(stream) = running.stream.as_mut() {
        let _ = running.decoder.receive(stream, running.request.deadline);
    }
    running.apply_frames().expect("frames apply");
    running.stream = None;
    running.child = None;
    running.awaiting = None;
    let report = handle.poll_ready().expect("completed scan seals");
    assert!(!report.facts.is_empty(), "sealed facts survive retirement");
    assert!(
        !report
            .unavailable
            .iter()
            .any(|gap| gap.reason == HealthUnavailableReason::ParseError),
        "retired stream cannot poison a completed scan"
    );
}

fn awaited(deadline: HealthDeadline) -> AwaitedTask {
    AwaitedTask {
        task_id: 1,
        finish: FinishPolicy::ReuseOnSuccess,
        deadline,
        work_deadline: deadline,
    }
}

fn source_done(peer: &mut UnixStream, sequence: u64) {
    peer.write_all(
        &protocol::encode(
            "0123456789abcdef0123456789abcdef",
            sequence,
            Payload::SourceDone { task_id: 1 },
        )
        .unwrap(),
    )
    .unwrap();
}
