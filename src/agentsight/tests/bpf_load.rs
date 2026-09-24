//! BPF verifier load tests — verify every eBPF program passes the kernel
//! verifier on the running kernel.
//!
//! These tests require CAP_BPF + CAP_PERFMON (or root). They are `#[ignore]`
//! by default so `cargo test` skips them; run explicitly with:
//!
//!     sudo cargo test --test bpf_load -- --ignored
//!
//! Each test names the probe it covers so a failure immediately identifies
//! which BPF program the verifier rejected.

use agentsight::{
    config,
    probes::{
        FileWatch, FileWriteProbe, Probes, ProcMon, ProcTrace, SharedMaps, SslSniff, TcpSniff,
        UdpDns,
    },
};

fn make_shared_maps() -> (ProcTrace, SharedMaps) {
    config::set_verbose(true);
    let pt = ProcTrace::new().expect("proctrace open+load");
    let shared = SharedMaps::new(pt.rb_handle().expect("rb handle")).with_traced_processes(
        pt.traced_processes_handle()
            .expect("traced_processes handle"),
    );
    (pt, shared)
}

#[test]
#[ignore]
fn proctrace_bpf_loads() {
    config::set_verbose(true);
    ProcTrace::new().expect("proctrace BPF should load on this kernel");
}

#[test]
#[ignore]
fn sslsniff_bpf_loads() {
    config::set_verbose(true);
    SslSniff::new().expect("sslsniff BPF should load on this kernel");
}

#[test]
#[ignore]
fn procmon_bpf_loads() {
    let (_pt, shared) = make_shared_maps();
    ProcMon::new_with_shared(&shared).expect("procmon BPF should load on this kernel");
}

#[test]
#[ignore]
fn filewatch_bpf_loads() {
    let (_pt, shared) = make_shared_maps();
    FileWatch::new_with_shared(&shared).expect("filewatch BPF should load on this kernel");
}

#[test]
#[ignore]
fn filewrite_bpf_loads() {
    let (_pt, shared) = make_shared_maps();
    FileWriteProbe::new_with_shared(&shared).expect("filewrite BPF should load on this kernel");
}

#[test]
#[ignore]
fn udpdns_bpf_loads() {
    let (_pt, shared) = make_shared_maps();
    UdpDns::new_with_shared(&shared).expect("udpdns BPF should load on this kernel");
}

#[test]
#[ignore]
fn tcpsniff_bpf_loads() {
    let (_pt, shared) = make_shared_maps();
    TcpSniff::new_with_shared(&shared).expect("tcpsniff BPF should load on this kernel");
}

#[test]
#[ignore]
fn all_probes_load() {
    config::set_verbose(true);
    Probes::new(&[], None, true, true, &[])
        .expect("unified Probes (all BPF programs) should load on this kernel");
}

/// Serializes the `AGENTSIGHT_SSL_REATTACH_TTL_SECS` override + sniffer
/// construction across the sslsniff TTL tests: the TTL is cached at
/// construction, so only the set_var → `SslSniff::new` window must not race.
static TTL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Stale re-attach lifecycle: with the TTL forced to 0, a second
/// `attach_process` for the same library must replace the existing
/// attachment (drop old links, attach new ones) instead of skipping,
/// and the traced-inode count must stay stable across the swap.
#[test]
#[ignore]
fn sslsniff_stale_reattach_replaces_expired_links() {
    use std::process::{Command, Stdio};

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: uprobe attach requires root");
        return;
    }
    let guard = TTL_ENV_LOCK.lock().unwrap();
    // SAFETY: single-threaded window guarded by TTL_ENV_LOCK; the value is
    // cached by SslSniff::new before the guard is released.
    unsafe { std::env::set_var("AGENTSIGHT_SSL_REATTACH_TTL_SECS", "0") };

    // Spawn a long-lived process that maps libssl.so dynamically.
    let mut child = match Command::new("python3")
        .args(["-c", "import ssl, time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping: cannot spawn python3: {e}");
            return;
        }
    };
    let mut sniffer = SslSniff::new().expect("sslsniff BPF should load on this kernel");
    drop(guard);
    // Interpreter startup varies; poll until the libssl mapping appears.
    // The TTL=0 override makes every call a real attach attempt, so polling
    // is safe and idempotent.
    let mut before = 0;
    for _ in 0..25 {
        sniffer
            .attach_process(child.id() as i32)
            .expect("attach_process on python3 child");
        before = sniffer.traced_inode_count();
        if before > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if before == 0 {
        let _ = child.kill();
        eprintln!("skipping: python3 child mapped no detectable SSL library");
        return;
    }
    // TTL=0 marks the attachment stale immediately; the second call must
    // rebuild and swap the links rather than skip.
    let reattaches_before = sniffer.stale_reattach_count();
    sniffer
        .attach_process(child.id() as i32)
        .expect("stale re-attach should succeed");
    assert_eq!(
        sniffer.traced_inode_count(),
        before,
        "re-attach must preserve the set of traced inodes"
    );
    assert_eq!(
        sniffer.stale_reattach_count(),
        reattaches_before + 1,
        "expired attachment must be rebuilt, not skipped"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// TTL regression guard: while an attachment is younger than the TTL, a
/// repeated `attach_process` must skip (dedup), not rebuild the probes.
/// Would fail if the staleness check were inverted or removed.
#[test]
#[ignore]
fn sslsniff_fresh_attach_not_rebuilt_before_ttl() {
    use std::process::{Command, Stdio};

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: uprobe attach requires root");
        return;
    }
    let guard = TTL_ENV_LOCK.lock().unwrap();
    // SAFETY: single-threaded window guarded by TTL_ENV_LOCK; the value is
    // cached by SslSniff::new before the guard is released.
    unsafe { std::env::set_var("AGENTSIGHT_SSL_REATTACH_TTL_SECS", "3600") };

    let mut child = match Command::new("python3")
        .args(["-c", "import ssl, time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping: cannot spawn python3: {e}");
            return;
        }
    };
    let mut sniffer = SslSniff::new().expect("sslsniff BPF should load on this kernel");
    drop(guard);
    let mut before = 0;
    for _ in 0..25 {
        sniffer
            .attach_process(child.id() as i32)
            .expect("attach_process on python3 child");
        before = sniffer.traced_inode_count();
        if before > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if before == 0 {
        let _ = child.kill();
        eprintln!("skipping: python3 child mapped no detectable SSL library");
        return;
    }
    // With a 1-hour TTL the attachment is fresh: the second call must dedup.
    let reattaches_before = sniffer.stale_reattach_count();
    sniffer
        .attach_process(child.id() as i32)
        .expect("second attach_process should succeed");
    assert_eq!(
        sniffer.traced_inode_count(),
        before,
        "fresh re-attach must not change the traced-inode set"
    );
    assert_eq!(
        sniffer.stale_reattach_count(),
        reattaches_before,
        "fresh attachment must be skipped, not rebuilt"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// Enforcement e2e: install a real ActPlane binding that blocks `unlink`,
/// then verify the child gets a distinguishable EPERM and a matching violation
/// event is published.
///
/// Requires root + BPF-LSM + the `enforcement-e2e` cargo feature (the default
/// CI clippy pass excludes the vendored ActPlane crates). `#[ignore]` by
/// default; the kernel-runner workflow runs it with the feature on:
///
///     sudo cargo test --features enforcement-e2e --test bpf_load \
///         -- --ignored --nocapture --test-threads=1
///
/// Set `AGENTSIGHT_ENFORCEMENT_E2E_OPTIONAL=1` to downgrade the preflight
/// checks (no root / no BPF-LSM / backend open failure) to skips — only for
/// manually probing environments. The CI job leaves it unset, so a configured
/// runner that fails to satisfy any precondition FAILS instead of passing
/// silently: the whole point of this gate is to catch enforcement regressions
/// (#3021 follow-up 2).
#[test]
#[ignore]
#[cfg(feature = "enforcement-e2e")]
fn enforcement_blocks_unlink() {
    use agentsight_enforcement_protocol::ApplyPolicy;
    use agentsight_enforcer::{ActPlaneBackend, EnforcementBackend, SubscriberClass};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    let optional = std::env::var("AGENTSIGHT_ENFORCEMENT_E2E_OPTIONAL").as_deref() == Ok("1");
    let skip = |reason: &str| {
        if optional {
            eprintln!("skipping (optional mode): {reason}");
        } else {
            panic!("enforcement e2e preflight failed: {reason}");
        }
    };

    if unsafe { libc::geteuid() } != 0 {
        skip("enforcement requires root");
        return;
    }
    let lsm = match std::fs::read_to_string("/sys/kernel/security/lsm") {
        Ok(l) => l,
        Err(_) => {
            skip("securityfs not available");
            return;
        }
    };
    if !lsm.split(',').any(|s| s.trim() == "bpf") {
        skip(&format!("BPF-LSM not active (lsm={})", lsm.trim()));
        return;
    }

    let backend = match ActPlaneBackend::open() {
        Ok(b) => b,
        Err(e) => {
            skip(&format!("{e}"));
            return;
        }
    };

    // Subscribe BEFORE the gate is released so the violation cannot be missed;
    // BestEffort — this is a diagnostic observer, not an ingestion dependency.
    let sub_id = Uuid::new_v4();
    let violations = backend.subscribe(sub_id, SubscriberClass::BestEffort);

    // Fixtures live in one predictable directory so the child references the
    // exact file path — a glob like /tmp/...* would miss when TMPDIR points
    // elsewhere and the test would pass without exercising the policy. The
    // path must also stay under the engine's 63-byte target-pattern ABI
    // limit (PAT=64 incl. NUL), so the directory and file names stay short.
    let fixture_dir = Path::new("/tmp/agt-e2e");
    let _ = std::fs::remove_dir_all(fixture_dir);
    std::fs::create_dir_all(fixture_dir).expect("create fixture dir");
    let test_file = fixture_dir.join(format!(
        "t-{}.txt",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::write(&test_file, b"test").expect("create test file");
    let gate = fixture_dir.join("go");

    // The child polls for the gate file so the deletion starts only after the
    // binding is installed, and reports a distinguishable errno for the failed
    // unlink (exit 1 + stderr marker) — plain `rm -f` cannot distinguish EPERM
    // from a signal kill or a non-LSM denial.
    let script = format!(
        "while [ ! -e {gate} ]; do sleep 0.05; done; \
         if rm {file} 2>err.txt; then exit 0; else \
           echo \"unlink-failed\" >&2; exit 1; \
         fi",
        gate = gate.display(),
        file = test_file.display()
    );
    let child = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(fixture_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");

    // Kill+reap the child on every exit path (including assert panics) so a
    // panicked run never leaks a 50 ms poll loop on a reused runner.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(child);

    let child_pid = child.0.id() as i32;
    let policy_dsl = format!(
        "source AGENT = exec \"**\"\nrule block-unlink:\n  block unlink file \"{}\" if AGENT\n  because \"test enforcement\"",
        test_file.display()
    );
    let binding_id = Uuid::new_v4();
    let request = ApplyPolicy {
        binding_id,
        agent_id: "enforcement-test".into(),
        session_id: None,
        root_pid: child_pid,
        process_start_time: read_start_time(child_pid),
        policy_id: "block-unlink".into(),
        policy_revision: "1".into(),
        policy_dsl,
        policy_mode: None,
    };

    let binding = backend.apply(request).expect("binding should install");
    assert_eq!(
        binding.state,
        agentsight_enforcement_protocol::BindingState::Enforced
    );

    // Release the gate only after the binding is Enforced; then wait for the
    // child to attempt the (blocked) deletion. Bounded so a stuck child fails
    // the job instead of hanging until the workflow timeout.
    std::fs::write(&gate, b"1").expect("release gate");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.0.try_wait().expect("child poll").is_some() {
            break;
        }
        if Instant::now() > deadline {
            panic!("child did not exit within 30s of gate release");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.0.wait().expect("child should exit");
    let stderr = String::from_utf8_lossy(
        &Command::new("bash")
            .arg("-c")
            .arg(format!("cat {}/err.txt 2>/dev/null", fixture_dir.display()))
            .output()
            .expect("read child stderr")
            .stdout,
    )
    .into_owned();

    // The unlink must have failed with a kernel denial, not a signal or an
    // unrelated rm error.
    assert!(
        !output.success(),
        "child should have been blocked by enforcement (exit={:?})",
        output.code()
    );
    assert!(
        stderr.contains("Operation not permitted"),
        "unlink failure should be EPERM, got stderr: {stderr:?}"
    );
    assert!(
        test_file.exists(),
        "file should still exist after blocked deletion"
    );

    // A matching violation event must have been published for this binding.
    // The engine's op taxonomy groups all file mutations (write/unlink/rename)
    // under TOP_WRITE, which userspace maps to "write" — there is no distinct
    // "unlink" operation name (taint.h TOP_* constants). The binding_id +
    // operation + blocked triple is unique to the child's unlink attempt here:
    // this fresh binding guards only the fixture, and nothing else mutates it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = None;
    while Instant::now() < deadline {
        while let Ok(event) = violations.try_recv() {
            if event.binding_id == binding_id && event.operation == "write" {
                found = Some(event);
                break;
            }
        }
        if found.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let event = found
        .unwrap_or_else(|| panic!("no unlink violation event published for binding {binding_id}"));
    assert!(event.blocked, "violation should record the kernel denial");

    backend.unsubscribe(sub_id);
    let _ = std::fs::remove_dir_all(fixture_dir);
}

#[cfg(feature = "enforcement-e2e")]
fn read_start_time(pid: i32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read stat");
    let after = &stat[stat.rfind(')').expect("comm close") + 2..];
    after
        .split_ascii_whitespace()
        .nth(19)
        .and_then(|s| s.parse::<u64>().ok())
        .expect("start_time")
}
