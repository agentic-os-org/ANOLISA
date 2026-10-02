#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use linux_sandbox::policy::FileSystemSandboxPolicy;
use linux_sandbox::policy::NetworkSandboxPolicy;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

// At least on GitHub CI, the arm64 tests appear to need longer timeouts.

#[cfg(not(target_arch = "aarch64"))]
const SHORT_TIMEOUT_MS: u64 = 1_000;
#[cfg(target_arch = "aarch64")]
const SHORT_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const LONG_TIMEOUT_MS: u64 = 1_000;
#[cfg(target_arch = "aarch64")]
const LONG_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const NETWORK_TIMEOUT_MS: u64 = 2_000;
#[cfg(target_arch = "aarch64")]
const NETWORK_TIMEOUT_MS: u64 = 10_000;

const BWRAP_UNAVAILABLE_ERR: &str = "system bubblewrap (bwrap) is not available";

/// Run sandbox command directly using the binary
async fn run_sandbox_command(
    cmd: &[&str],
    fs_policy: &FileSystemSandboxPolicy,
    net_policy: NetworkSandboxPolicy,
    timeout_ms: u64,
) -> Output {
    let cwd = std::env::current_dir().expect("cwd should exist");
    let fs_policy_json = serde_json::to_string(fs_policy).expect("fs policy should serialize");
    let net_policy_json = serde_json::to_string(&net_policy).expect("net policy should serialize");

    let mut args = vec![
        "--sandbox-policy-cwd".to_string(),
        cwd.to_string_lossy().to_string(),
        "--file-system-sandbox-policy".to_string(),
        fs_policy_json,
        "--network-sandbox-policy".to_string(),
        net_policy_json,
    ];
    args.push("--".to_string());
    args.extend(cmd.iter().map(|entry| (*entry).to_string()));

    let sandbox_program = env!("CARGO_BIN_EXE_linux-sandbox");
    let mut command = Command::new(sandbox_program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output =
        match tokio::time::timeout(Duration::from_millis(timeout_ms), command.output()).await {
            Ok(output) => output,
            Err(err) => panic!("sandbox command timed out: {err}"),
        };

    match output {
        Ok(output) => output,
        Err(err) => panic!("sandbox command failed to execute: {err}"),
    }
}

fn is_bwrap_unavailable_output(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains(BWRAP_UNAVAILABLE_ERR)
        || (stderr.contains("Can't mount proc on /newroot/proc")
            && (stderr.contains("Operation not permitted")
                || stderr.contains("Permission denied")
                || stderr.contains("Invalid argument")))
}

async fn should_skip_bwrap_tests() -> bool {
    let output = run_sandbox_command(
        &["bash", "-c", "true"],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        NETWORK_TIMEOUT_MS,
    )
    .await;
    is_bwrap_unavailable_output(&output)
}

#[tokio::test]
async fn test_root_read() {
    let output = run_sandbox_command(
        &["ls", "-l", "/bin"],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        SHORT_TIMEOUT_MS,
    )
    .await;
    assert!(output.status.success(), "should be able to read /bin");
}

#[tokio::test]
async fn test_root_write_blocked() {
    let tmpfile = tempfile::NamedTempFile::new().unwrap();
    let tmpfile_path = tmpfile.path().to_string_lossy();

    let output = run_sandbox_command(
        &["bash", "-lc", &format!("echo blah > {tmpfile_path}")],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        SHORT_TIMEOUT_MS,
    )
    .await;

    // Write should fail in read-only sandbox
    assert!(
        !output.status.success(),
        "write should be blocked in read-only sandbox"
    );
}

#[tokio::test]
async fn test_dev_null_write() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let output = run_sandbox_command(
        &["bash", "-c", "echo blah > /dev/null"],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        LONG_TIMEOUT_MS,
    )
    .await;

    assert!(
        output.status.success(),
        "should be able to write to /dev/null"
    );
}

#[tokio::test]
async fn bwrap_populates_minimal_dev_nodes() {
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bwrap sandbox prerequisites are unavailable");
        return;
    }

    let output = run_sandbox_command(
        &[
            "bash",
            "-c",
            "for node in null zero full random urandom tty; do [ -c \"/dev/$node\" ] || { echo \"missing /dev/$node\" >&2; exit 1; }; done",
        ],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        LONG_TIMEOUT_MS,
    )
    .await;

    assert!(
        output.status.success(),
        "minimal dev nodes should be present"
    );
}

#[tokio::test]
async fn test_writable_root() {
    let tmpdir = tempfile::tempdir().unwrap();
    let file_path = tmpdir.path().join("test");

    use linux_sandbox::path::AbsolutePathBuf;
    use linux_sandbox::policy::FileSystemAccessMode;
    use linux_sandbox::policy::FileSystemPath;
    use linux_sandbox::policy::FileSystemSandboxEntry;
    use linux_sandbox::policy::FileSystemSpecialPath;

    let writable_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir"),
            },
            access: FileSystemAccessMode::Write,
        },
    ]);

    let output = run_sandbox_command(
        &[
            "bash",
            "-c",
            &format!("echo blah > {}", file_path.to_string_lossy()),
        ],
        &writable_policy,
        NetworkSandboxPolicy::Restricted,
        LONG_TIMEOUT_MS,
    )
    .await;

    assert!(
        output.status.success(),
        "should be able to write to writable root"
    );
}

#[tokio::test]
async fn test_no_new_privs_is_enabled() {
    let output = run_sandbox_command(
        &["bash", "-c", "grep '^NoNewPrivs:' /proc/self/status"],
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        LONG_TIMEOUT_MS,
    )
    .await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("NoNewPrivs:"))
        .unwrap_or("");
    assert!(line.contains("1"), "NoNewPrivs should be enabled");
}

/// Helper that runs `cmd` under the Linux sandbox and asserts that the command
/// does NOT succeed (i.e. returns a non‑zero exit code)
async fn assert_network_blocked(cmd: &[&str]) {
    let output = run_sandbox_command(
        cmd,
        &FileSystemSandboxPolicy::default(),
        NetworkSandboxPolicy::Restricted,
        NETWORK_TIMEOUT_MS,
    )
    .await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // A completely missing binary exits with 127.  Anything else should also
    // be non‑zero (EPERM from seccomp will usually bubble up as 1, 2, 13…)
    // If—*and only if*—the command exits 0 we consider the sandbox breached.

    if output.status.success() {
        panic!("Network sandbox FAILED - {cmd:?} exited 0\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
}

#[tokio::test]
async fn sandbox_blocks_curl() {
    assert_network_blocked(&["curl", "-I", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_wget() {
    assert_network_blocked(&["wget", "-qO-", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_ping() {
    // ICMP requires raw socket – should be denied quickly with EPERM.
    assert_network_blocked(&["ping", "-c", "1", "8.8.8.8"]).await;
}

#[tokio::test]
async fn sandbox_blocks_nc() {
    // Zero‑length connection attempt to localhost.
    assert_network_blocked(&["nc", "-z", "127.0.0.1", "80"]).await;
}

#[tokio::test]
async fn sandbox_blocks_ssh() {
    // Force ssh to attempt a real TCP connection but fail quickly.  `BatchMode`
    // avoids password prompts, and `ConnectTimeout` keeps the hang time low.
    assert_network_blocked(&[
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=1",
        "github.com",
    ])
    .await;
}

#[tokio::test]
async fn sandbox_blocks_getent() {
    assert_network_blocked(&["getent", "ahosts", "openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_dev_tcp_redirection() {
    // This syntax is only supported by bash and zsh. We try bash first.
    // Fallback generic socket attempt using /bin/sh with bash‑style /dev/tcp.  Not
    // all images ship bash, so we guard against 127 as well.
    assert_network_blocked(&["bash", "-c", "echo hi > /dev/tcp/127.0.0.1/80"]).await;
}
