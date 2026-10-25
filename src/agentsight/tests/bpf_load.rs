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
