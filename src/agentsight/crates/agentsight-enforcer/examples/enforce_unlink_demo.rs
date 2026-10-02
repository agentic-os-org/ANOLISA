//! Live enforce demo: prove the user's ActPlane DSL blocks `unlink` of a file
//! through the real product path (ActPlaneBackend), using the LSM-only
//! `agent-file-guard` pinned profile.
//!
//! DSL under test:
//!   source AGENT = exec "**"
//!   rule canonical-namespace-mutation-0-0:
//!   block unlink file "/tmp/asc-p" if AGENT
//!   because "Canonical namespace-mutation policy"
//!
//! Run on the 7.1.8 box (BPF-LSM active), as root:
//!   ulimit -l unlimited
//!   ACTPLANE_PINNED_PROFILE=agent-file-guard \
//!     cargo run -p agentsight-enforcer --features actplane --example enforce_unlink_demo

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use agentsight_enforcement_protocol::{ApplyPolicy, PolicyMode};
use agentsight_enforcer::{ActPlaneBackend, EnforcementBackend};
use uuid::Uuid;

const PROTECTED: &str = "/tmp/asc-p";
const DSL: &str = "source AGENT = exec \"**\"\n\nrule canonical-namespace-mutation-0-0:\nblock unlink file \"/tmp/asc-p\" if AGENT\nbecause \"Canonical namespace-mutation policy\"\n";

/// Parses `/proc/<pid>/stat` field 22 (start time), tolerating spaces in comm.
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
    println!(
        "[setup] created {PROTECTED} (exists={})",
        Path::new(PROTECTED).exists()
    );

    // Agent shell: after a delay it exec-replaces with `rm`, so `exec "**"`
    // tags the deleting process AGENT.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 5; echo '[agent] deleting'; exec rm -vf /tmp/asc-p")
        .spawn()
        .expect("spawn agent");
    let child_pid = child.id() as i32;
    let child_start = start_time(child_pid);
    println!("[setup] agent pid={child_pid} start_time={child_start}");

    let backend = ActPlaneBackend::open().expect("open pinned ActPlane engine (agent-file-guard)");
    // Capability report derived from the actually-loaded profile: under
    // agent-file-guard this must be true, so SecCore can gate APPLY_READY on it.
    let health = backend.health().expect("read backend health");
    println!(
        "[caps] file_delete_guard={} (backend={})",
        health.capabilities.file_delete_guard, health.backend
    );
    let request = ApplyPolicy {
        binding_id: Uuid::new_v4(),
        agent_id: "demo-agent".into(),
        session_id: Some("demo-session".into()),
        root_pid: child_pid,
        process_start_time: child_start,
        policy_id: "canonical-namespace-mutation".into(),
        policy_revision: "0".into(),
        policy_dsl: DSL.into(),
        policy_mode: Some(PolicyMode::Enforce),
    };
    let binding = backend.apply(request).expect("apply enforce policy");
    println!(
        "[bind] policy applied: domain={:?} state={:?}",
        binding.domain_id, binding.state
    );

    let status = child.wait().expect("await agent");
    println!("[agent] exited: {status}");
    sleep(Duration::from_millis(200));

    let survived = Path::new(PROTECTED).exists();
    println!("\n==== RESULT ====");
    println!("protected file still present after AGENT rm: {survived}");
    if survived {
        println!("PASS: enforce blocked the AGENT-tagged unlink (rm should report EPERM above)");
    } else {
        println!("FAIL: the file was deleted despite the block rule");
    }

    if survived {
        match fs::remove_file(PROTECTED) {
            Ok(()) => println!("SCOPE-OK: untagged process deleted it (block is AGENT-scoped)"),
            Err(e) => println!("SCOPE-NOTE: untagged delete failed: {e}"),
        }
    }

    let _ = backend.detach(binding.request.binding_id);
    std::process::exit(if survived { 0 } else { 1 });
}
