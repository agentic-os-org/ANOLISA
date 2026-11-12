//! Live audit-chain demo: prove a real BPF-LSM file-delete denial reaches the
//! System Audit evidence chain and that the violation resolves to its case.
//!
//! `enforce_unlink_demo` shows the kernel block; this is its audit counterpart.
//! A guard hit used to stop at the enforcement stream, leaving System Audit empty
//! while the dashboard promised an evidence chain (#9240). The demo drives the
//! production publisher with a kernel-generated denial and asserts the whole
//! chain: violation id == file evidence id, the decision opens a case, the case
//! keeps the file event as evidence, and the denial is queryable as `blocked`.
//!
//! Run as root on a BPF-LSM host (`cat /sys/kernel/security/lsm` contains `bpf`):
//!   ulimit -l unlimited
//!   ACTPLANE_PINNED_PROFILE=agent-file-guard \
//!     cargo run -p agentsight-enforcer --features actplane \
//!       --example file_delete_audit_chain_demo

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use agentsight_audit::{AuditEventFilter, AuditService, AuditStore};
use agentsight_enforcement_protocol::{ApplyPolicy, PolicyMode, SecurityEventKind};
use agentsight_enforcer::{ActPlaneBackend, EnforcementBackend, SubscriberClass};
use uuid::Uuid;

const PROTECTED: &str = "/tmp/asc-audit-chain";
const DSL: &str = "source AGENT = exec \"**\"\n\nrule canonical-namespace-mutation-0-0:\nblock unlink file \"/tmp/asc-audit-chain\" if AGENT\nbecause \"Canonical namespace-mutation policy\"\n";

fn start_time(pid: i32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("read stat");
    let tail = stat.rsplit_once(") ").expect("comm terminator").1;
    tail.split_whitespace()
        .nth(19)
        .expect("field 22")
        .parse()
        .expect("numeric start time")
}

fn main() {
    fs::write(PROTECTED, b"protect me\n").expect("seed protected file");

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 5; exec rm -vf {PROTECTED}"))
        .spawn()
        .expect("spawn agent");
    let child_pid = child.id() as i32;
    let child_start = start_time(child_pid);

    let backend = ActPlaneBackend::open().expect("open pinned ActPlane engine");
    let health = backend.health().expect("backend health");
    println!(
        "[caps] file_delete_guard={} backend={}",
        health.capabilities.file_delete_guard, health.backend
    );

    // Subscribe before applying so no published event is missed.
    let violations = backend.subscribe(Uuid::new_v4(), SubscriberClass::BestEffort);
    let evidence = backend.subscribe_security_events();

    let request = ApplyPolicy {
        binding_id: Uuid::new_v4(),
        agent_id: "audit-chain-agent".into(),
        session_id: Some("audit-chain-session".into()),
        root_pid: child_pid,
        process_start_time: child_start,
        policy_id: "canonical-namespace-mutation".into(),
        policy_revision: "0".into(),
        policy_dsl: DSL.into(),
        policy_mode: Some(PolicyMode::Enforce),
        file_protection_policy: None,
    };
    let binding = backend.apply(request).expect("apply enforce policy");
    println!("[bind] state={:?}", binding.state);

    let status = child.wait().expect("await agent");
    println!("[agent] exited: {status}");
    sleep(Duration::from_millis(500));

    let survived = Path::new(PROTECTED).exists();
    println!("[kernel] protected file survived: {survived}");

    let violation = violations
        .try_iter()
        .find(|v| v.operation == "unlink")
        .expect("a real blocked unlink violation must be published");
    println!(
        "[violation] event_id={} op={} blocked={}",
        violation.event_id, violation.operation, violation.blocked
    );

    let pair: Vec<_> = evidence.try_iter().collect();
    println!("[evidence] security events published: {}", pair.len());
    for event in &pair {
        println!("  - {:?} event_id={}", event.kind, event.event_id);
    }

    let store = Arc::new(AuditStore::open_in_memory().expect("open audit store"));
    let service = AuditService::new(Arc::clone(&store));
    for event in &pair {
        service
            .ingest(event.clone())
            .expect("real kernel evidence must ingest");
    }

    let summary = service
        .summary(&AuditEventFilter::default())
        .expect("audit summary");
    println!(
        "[audit] total_events={} blocked_events={}",
        summary.total_events, summary.blocked_events
    );

    let links = service
        .case_ids_for_events(&[violation.event_id])
        .expect("evidence links");
    let case_id = links
        .get(&violation.event_id)
        .copied()
        .expect("the violation's event id must resolve to a case");
    println!("[correlation] violation -> case_id={case_id}");

    let detail = service.case(case_id).expect("case detail");
    let evidence_ids: Vec<Uuid> = detail.evidence.iter().map(|item| item.event_id).collect();
    println!("[case] evidence events: {evidence_ids:?}");

    // #9567: a policy denial on a file operation must be queryable as blocked.
    let blocked = service
        .events(&AuditEventFilter {
            result: Some("blocked".into()),
            ..AuditEventFilter::default()
        })
        .expect("blocked filter query");
    let blocked_file = blocked.items.iter().any(|event| {
        matches!(&event.kind, SecurityEventKind::FileAction(action) if action.operation == "unlink")
    });
    println!(
        "[blocked-filter] total={} contains_denied_unlink={}",
        blocked.total, blocked_file
    );

    let _ = backend.detach(binding.request.binding_id);
    let _ = fs::remove_file(PROTECTED);

    let ok = survived
        && summary.total_events > 0
        && summary.blocked_events > 0
        && evidence_ids.contains(&violation.event_id)
        && blocked_file;
    println!("\n==== RESULT ====");
    if ok {
        println!(
            "PASS: real kernel denial reached System Audit and the violation resolves to its case"
        );
    } else {
        println!("FAIL: evidence chain incomplete");
    }
    std::process::exit(if ok { 0 } else { 1 });
}
