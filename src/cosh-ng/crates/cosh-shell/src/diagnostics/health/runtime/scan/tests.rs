use super::*;
use std::io::Write;

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
        deadline: JobDeadline::after(budget).expect("deadline"),
        context: ScanContext::Resources,
        config: HealthConfig::default(),
        environment: None,
        disabled: Vec::new(),
        config_source: crate::diagnostics::health::runtime::plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
    };
    let running = RunningScan {
        request,
        builder,
        child: None,
        stream: Some(stream),
        decoder: Decoder::default(),
        nonce: "0123456789abcdef0123456789abcdef".to_string(),
        sequence: 1,
        configured: true,
        enabled: BTreeSet::from(["resource.load".to_string()]),
        started: Instant::now(),
        started_ns: monotonic_ns().expect("clock"),
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
        Payload::Progress(progress),
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
fn completed_check_closes_eligibility_without_waiting_for_eof() {
    let (mut handle, mut peer) = staged(Duration::from_secs(1));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
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
    configure_isolated_worker(&mut command, running.request.deadline);
    let child = command.spawn().expect("isolated job");
    running.child = Some(permit.register(child, CleanupScope::OwnedGroup, Instant::now()));
    send(&mut peer, 1, fact());
    send(&mut peer, 2, completed());
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
        peer.write_all(&tail).unwrap();
        let report = handle.poll_ready().expect("completed scan seals");
        assert!(
            !report.facts.is_empty(),
            "prior complete facts remain usable"
        );
        assert!(
            !report.unavailable.is_empty(),
            "incomplete tail {tail:?} cannot be healthy"
        );
    }
}
