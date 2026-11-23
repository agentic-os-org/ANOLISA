#![cfg(target_os = "linux")]

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
    analyzer::{AnalysisResult, Analyzer, AuditExtra, AuditRecord},
    config,
    event::Event,
    pipeline_core::PipelineCore,
    probes::{
        FileWatch, FileWriteProbe, Probes, ProcMon, ProcTrace, SharedMaps, SslSniff, TcpSniff,
        UdpDns, proctrace::VariableEvent,
    },
};

fn make_shared_maps() -> (ProcTrace, SharedMaps) {
    config::set_verbose(true);
    let pt = ProcTrace::new().expect("proctrace open+load");
    let shared = SharedMaps::new(pt.rb_handle().expect("rb handle"))
        .with_internal_metrics(
            pt.internal_metrics_handle()
                .expect("internal_metrics handle"),
        )
        .with_traced_processes(
            pt.traced_processes_handle()
                .expect("traced_processes handle"),
        );
    (pt, shared)
}

#[test]
#[ignore]
fn traced_filter_recovers_after_late_exit_registrations() {
    use libbpf_rs::MapFlags;

    let mut trace = ProcTrace::new().expect("load isolated filter maps");
    let live = std::process::id();
    trace.add_traced_pid(live).expect("register live agent");
    // 模拟异步处理 exec 晚于 exit：残留键直接耗尽实际 BPF HASH 容量。
    for pid in (u32::MAX - 1023)..u32::MAX {
        trace.add_traced_pid(pid).expect("seed exited pid");
    }
    let map = trace.traced_processes_handle().expect("filter handle");
    assert_eq!(map.keys().count(), 1024);
    trace
        .add_traced_pid(1)
        .expect("reap and register next agent");
    assert_eq!(map.keys().count(), 2, "only live registrations remain");
    for pid in [1u32, live] {
        assert!(
            map.lookup(&pid.to_ne_bytes(), MapFlags::ANY)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
#[ignore]
fn unclassified_non_tls_processes_leave_filter_slots_free() {
    use libbpf_rs::MapFlags;
    use std::process::Command;

    let mut probes = Probes::new(&[], None, false, false, &[]).expect("load isolated probes");
    let map = probes.traced_processes_handle().expect("filter handle");
    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("non-TLS fixture");
    let result = probes.attach_unclassified_process(child.id() as i32);
    let registered = map
        .lookup(&child.id().to_ne_bytes(), MapFlags::ANY)
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    result.expect("unsupported TLS is a normal skip");
    assert!(registered.is_none());
    assert_eq!(map.keys().count(), 0);

    // 已识别 Agent 保留注册兜底；不支持 TLS 也不能改变已有契约。
    probes
        .attach_process(std::process::id() as i32)
        .expect("classified fallback");
    assert!(
        map.lookup(&std::process::id().to_ne_bytes(), MapFlags::ANY)
            .unwrap()
            .is_some()
    );
}

#[test]
#[ignore]
fn unclassified_tls_processes_still_register_for_first_call() {
    use libbpf_rs::MapFlags;
    use std::process::Command;

    let mut probes = Probes::new(&[], None, false, false, &[]).expect("load isolated probes");
    let map = probes.traced_processes_handle().expect("filter handle");
    let mut child = Command::new("python3")
        .args(["-c", "import ssl,time; time.sleep(60)"])
        .spawn()
        .expect("TLS fixture");
    let mut registered = false;
    for _ in 0..25 {
        probes
            .attach_unclassified_process(child.id() as i32)
            .expect("attach TLS fixture");
        registered = map
            .lookup(&child.id().to_ne_bytes(), MapFlags::ANY)
            .unwrap()
            .is_some();
        if registered {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        registered,
        "unknown TLS process must retain early-call coverage"
    );
}

#[test]
#[ignore]
fn unclassified_late_python_ssl_registers_via_retry() {
    use libbpf_rs::MapFlags;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: uprobe attach requires root");
        return;
    }
    let mut child = Command::new("python3")
        .args([
            "-u",
            "-c",
            "import sys; print('ready', flush=True); sys.stdin.readline(); import ssl, time; time.sleep(60)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start delayed Python SSL fixture");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("fixture stdout"))
        .read_line(&mut ready)
        .expect("read fixture readiness");
    assert_eq!(ready.trim(), "ready");

    let mut probes = Probes::new(&[], None, false, false, &[]).expect("load isolated probes");
    let map = probes.traced_processes_handle().expect("filter handle");
    probes
        .attach_unclassified_process(child.id() as i32)
        .expect("attach before Python imports ssl");
    let registered_before_import = map
        .lookup(&child.id().to_ne_bytes(), MapFlags::ANY)
        .unwrap()
        .is_some();
    if registered_before_import {
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("skipping: this Python maps SSL before the delayed import");
        return;
    }

    writeln!(child.stdin.as_mut().expect("fixture stdin")).expect("trigger delayed import");
    let mut registered = false;
    for _ in 0..250 {
        probes.poll_pending_ssl();
        registered = map
            .lookup(&child.id().to_ne_bytes(), MapFlags::ANY)
            .unwrap()
            .is_some();
        if registered {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for _ in 0..5 {
        probes.poll_pending_ssl();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let filter_slots = map.keys().count();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        registered,
        "late libssl mapping must attach and register without another exec or attach call"
    );
    assert_eq!(filter_slots, 1, "retries must not duplicate registration");
}

#[test]
#[ignore]
fn proctrace_bpf_loads() {
    config::set_verbose(true);
    ProcTrace::new().expect("proctrace BPF should load on this kernel");
}

struct ExitFixtureChild(std::process::Child, bool);

impl ExitFixtureChild {
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.0.wait()?;
        self.1 = true;
        Ok(status)
    }
}

impl Drop for ExitFixtureChild {
    fn drop(&mut self) {
        if !self.1 {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn process_bpf_event(
    event: VariableEvent,
    pid: u32,
    core: &mut PipelineCore,
    exits: &mut Vec<(i32, u32)>,
    records: &mut Vec<AuditRecord>,
) {
    let is_target = match &event {
        VariableEvent::Exec { header, .. }
        | VariableEvent::Stdout { header, .. }
        | VariableEvent::Exit { header, .. } => header.pid == pid,
        VariableEvent::Unknown(_) => false,
    };
    if !is_target {
        return;
    }
    if let VariableEvent::Exit {
        header,
        raw_exit_status,
    } = &event
    {
        exits.push((*raw_exit_status, header.tid));
    }
    for completed in core.process_event(Event::Proc(event)) {
        for result in core.analyze_for_owned_delivery(completed, false) {
            if let AnalysisResult::Audit(record) = result
                && record.pid == pid
            {
                records.push(record);
            }
        }
    }
}

fn pump_bpf_events(
    trace: &ProcTrace,
    core: &mut PipelineCore,
    pid: u32,
    duration: std::time::Duration,
    exits: &mut Vec<(i32, u32)>,
    records: &mut Vec<AuditRecord>,
) {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        if let Some(event) = trace.try_recv() {
            process_bpf_event(event, pid, core, exits, records);
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

fn compile_exit_fixture(directory: &std::path::Path) -> std::path::PathBuf {
    let source = directory.join("exit_fixture.c");
    let binary = directory.join("exit_fixture");
    std::fs::write(
        &source,
        r#"
#include <pthread.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *mode;
static sigset_t final_set;

static void terminate(void)
{
    if (strcmp(mode, "exit0") == 0)
        _exit(0);
    if (strcmp(mode, "exit7") == 0 || strcmp(mode, "leader-exit7") == 0)
        exit(7);
    raise(SIGTERM);
    _exit(125);
}

static void *worker(void *unused)
{
    (void)unused;
    if (write(STDOUT_FILENO, "R", 1) != 1)
        _exit(110);
    int signal_number;
    if (sigwait(&final_set, &signal_number) != 0)
        _exit(111);
    terminate();
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc != 2)
        return 64;
    mode = argv[1];
    sigemptyset(&final_set);
    sigaddset(&final_set, SIGUSR2);
    if (pthread_sigmask(SIG_BLOCK, &final_set, NULL) != 0)
        return 65;

    char start;
    if (strncmp(mode, "leader-", 7) == 0) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, worker, NULL) != 0)
            return 66;
        if (read(STDIN_FILENO, &start, 1) != 1)
            return 67;
        pthread_exit(NULL);
    }
    if (read(STDIN_FILENO, &start, 1) != 1)
        return 68;
    terminate();
    return 0;
}
"#,
    )
    .expect("write exit fixture");
    let output = std::process::Command::new("cc")
        .args(["-std=gnu99", "-pthread"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("compile exit fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

#[test]
#[ignore]
fn proctrace_exit_status_matches_wait_for_final_thread() {
    use libbpf_rs::MapFlags;
    use std::io::{Read, Write};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().expect("exit fixture directory");
    let fixture = compile_exit_fixture(directory.path());
    config::set_verbose(true);
    let mut trace = ProcTrace::new().expect("proctrace open+load");
    trace
        .add_traced_pid(std::process::id())
        .expect("register fixture parent");
    let traced = trace.traced_processes_handle().expect("traced map handle");
    trace.attach().expect("attach proctrace");
    let _poller = trace.run().expect("poll proctrace");

    for (mode, expected_code, expected_signal, leader_first) in [
        ("exit0", Some(0), None, false),
        ("exit7", Some(7), None, false),
        ("signal", None, Some(15), false),
        ("leader-exit7", Some(7), None, true),
        ("leader-signal", None, Some(15), true),
    ] {
        let mut core = PipelineCore::new(&agentsight::AgentsightConfig::default(), Analyzer::new());
        let mut child = ExitFixtureChild(
            Command::new(&fixture)
                .arg(mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn exit fixture"),
            false,
        );
        let pid = child.0.id();
        let mut exits = Vec::new();
        let mut records = Vec::new();

        if leader_first {
            let mut ready = [0_u8; 1];
            child
                .0
                .stdout
                .as_mut()
                .expect("fixture stdout")
                .read_exact(&mut ready)
                .expect("worker ready");
            assert_eq!(&ready, b"R");
        }
        child
            .0
            .stdin
            .as_mut()
            .expect("fixture stdin")
            .write_all(b"S")
            .expect("start fixture");

        if leader_first {
            let leader_status = format!("/proc/{pid}/task/{pid}/status");
            let leader_state = || {
                std::fs::read_to_string(&leader_status)
                    .ok()
                    .and_then(|status| {
                        status
                            .lines()
                            .find_map(|line| line.strip_prefix("State:"))?
                            .trim_start()
                            .chars()
                            .next()
                    })
            };
            // The leader remains a zombie until the final worker exits.
            let deadline = Instant::now() + Duration::from_secs(5);
            while leader_state() != Some('Z') && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(leader_state(), Some('Z'), "{mode}");
            assert!(
                std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "{mode}"
            );
            pump_bpf_events(
                &trace,
                &mut core,
                pid,
                Duration::from_millis(200),
                &mut exits,
                &mut records,
            );
            assert!(exits.is_empty(), "early Exit for {mode}: {exits:?}");
            assert!(records.is_empty(), "early audit for {mode}: {records:?}");
            assert!(
                traced
                    .lookup(&pid.to_ne_bytes(), MapFlags::ANY)
                    .expect("read traced map")
                    .is_some(),
                "leader exit removed {mode} from traced map"
            );
            // SAFETY: the fixture still has a worker waiting for this blocked signal.
            assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGUSR2) }, 0);
        }

        let wait_status = child.wait().expect("wait for exit fixture").into_raw();
        let deadline = Instant::now() + Duration::from_secs(10);
        while (exits.is_empty() || records.is_empty()) && Instant::now() < deadline {
            pump_bpf_events(
                &trace,
                &mut core,
                pid,
                Duration::from_millis(50),
                &mut exits,
                &mut records,
            );
        }
        pump_bpf_events(
            &trace,
            &mut core,
            pid,
            Duration::from_millis(200),
            &mut exits,
            &mut records,
        );

        assert_eq!(exits.len(), 1, "{mode}: {exits:?}");
        assert_eq!(records.len(), 1, "{mode}: {records:?}");
        assert_eq!(exits[0].0, wait_status, "{mode}");
        if leader_first {
            assert_ne!(exits[0].1, pid, "{mode} exited on its leader");
            assert!(records[0].duration_ns >= 150_000_000, "{mode}");
        } else {
            assert_eq!(exits[0].1, pid, "{mode}");
        }
        assert!(
            traced
                .lookup(&pid.to_ne_bytes(), MapFlags::ANY)
                .expect("read traced map")
                .is_none(),
            "final exit retained {mode} in traced map"
        );
        let AuditExtra::ProcessAction {
            exit_code, signal, ..
        } = &records[0].extra
        else {
            panic!("expected process action for {mode}");
        };
        assert_eq!(*exit_code, expected_code, "{mode}");
        assert_eq!(*signal, expected_signal, "{mode}");
    }
}

#[test]
#[ignore]
fn sslsniff_bpf_loads() {
    config::set_verbose(true);
    SslSniff::new().expect("sslsniff BPF should load on this kernel");
}

#[test]
#[ignore]
fn python_shared_openssl_library_attaches() {
    use std::process::{Command, Stdio};

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: uprobe attach requires root");
        return;
    }

    // Reproduce uv's shared build without depending on an installed Python:
    // SSL functions are LOCAL symbols in libpython, absent from the launcher.
    let directory = tempfile::tempdir().expect("Python SSL fixture directory");
    let library_source = directory.path().join("python_ssl.c");
    let library = directory.path().join("libpython3.11.so.1.0");
    std::fs::write(
        &library_source,
        r#"
#include <stddef.h>
#include <unistd.h>
#define SSL_FN __attribute__((used, noinline))
static SSL_FN int SSL_write(void *ssl, const void *buf, int len) { return len; }
static SSL_FN int SSL_read(void *ssl, void *buf, int len) { return 0; }
static SSL_FN int SSL_write_ex(void *ssl, const void *buf, size_t len, size_t *out) { return 0; }
static SSL_FN int SSL_read_ex(void *ssl, void *buf, size_t len, size_t *out) { return 0; }
static SSL_FN int SSL_do_handshake(void *ssl) { return 1; }
void python_ssl_wait(void) { sleep(60); }
"#,
    )
    .expect("write shared Python SSL fixture");
    let build = Command::new("cc")
        .args([
            "-shared",
            "-fPIC",
            "-O0",
            "-Wl,-soname,libpython3.11.so.1.0",
        ])
        .arg(&library_source)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile shared Python SSL fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let launcher_source = directory.path().join("launcher.c");
    let launcher = directory.path().join("python3.11");
    std::fs::write(
        &launcher_source,
        "extern void python_ssl_wait(void); int main(void) { python_ssl_wait(); return 0; }\n",
    )
    .expect("write Python launcher fixture");
    let build = Command::new("cc")
        .arg(&launcher_source)
        .arg("-L")
        .arg(directory.path())
        .args(["-l:libpython3.11.so.1.0", "-Wl,-rpath,$ORIGIN", "-o"])
        .arg(&launcher)
        .output()
        .expect("compile Python launcher fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    config::set_verbose(true);
    let mut sniffer = SslSniff::new().expect("sslsniff BPF should load on this kernel");
    let mut child = Command::new(&launcher)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start Python launcher fixture");
    let mut result = Ok(());
    for _ in 0..25 {
        result = sniffer.attach_process(child.id() as i32);
        if result.is_err() || sniffer.traced_inode_count() > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let attached = sniffer.traced_inode_count();
    let _ = child.kill();
    let _ = child.wait();

    result.expect("discover and attach Python SSL probes");
    assert_eq!(attached, 1, "attach to libpython rather than the launcher");
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
