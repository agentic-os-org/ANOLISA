use std::fs;
use std::io::{Read, Write};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::libc;
use ratatui::text::Span;
use wait_timeout::ChildExt;

const RAW_CLI_TIMEOUT: Duration = Duration::from_secs(30);
// Shared-mode sessions run concurrently up to this bound. Override with
// COSH_RAW_CLI_TEST_PARALLELISM to probe higher/lower parallelism locally;
// 16 measured faster still but oversubscribes 12-core hosts (mock provider
// sessions start failing), so the default stays at the original design of 8.
const RAW_CLI_SHARED_PARALLELISM_DEFAULT: usize = 8;
pub(crate) const RAW_CLI_UNSET_ENV: &str = "__cosh_raw_cli_unset_env__";

fn raw_cli_shared_parallelism() -> usize {
    static PARALLELISM: OnceLock<usize> = OnceLock::new();
    *PARALLELISM.get_or_init(|| {
        std::env::var("COSH_RAW_CLI_TEST_PARALLELISM")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value >= 1)
            .unwrap_or(RAW_CLI_SHARED_PARALLELISM_DEFAULT)
    })
}

static RAW_CLI_GIT_FIXTURE: OnceLock<PathBuf> = OnceLock::new();
static RAW_CLI_RUN_GATE: OnceLock<RawCliRunGate> = OnceLock::new();

pub(crate) fn raw_cli_command(binary: &str) -> Command {
    let mut command = Command::new(binary);
    configure_raw_cli_command(&mut command);
    command
}

pub(crate) fn run_raw_cli_with_envs(adapter: &str, envs: &[(&str, &str)]) -> String {
    run_raw_cli_with_args_env_and_delayed_input(
        adapter,
        &[],
        envs,
        vec![
            (b"/explain last error\n".to_vec(), Duration::ZERO),
            (b"ls /path/that/does/not/exist\n".to_vec(), Duration::ZERO),
            (b"echo after-inline\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    )
}

pub(crate) fn run_raw_cli_with_input(adapter: &str, input: &str) -> String {
    run_raw_cli_with_env(adapter, input, &[])
}

pub(crate) fn run_raw_cli_with_env(adapter: &str, input: &str, envs: &[(&str, &str)]) -> String {
    run_raw_cli_with_args_and_env(adapter, &[], input, envs)
}

pub(crate) fn run_raw_cli_with_args_and_env(
    adapter: &str,
    extra_args: &[&str],
    input: &str,
    envs: &[(&str, &str)],
) -> String {
    let _run_guard = raw_cli_shared_run_guard();
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = Command::new(binary);
    command
        .args(["raw", adapter])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_raw_cli_command(&mut command);
    apply_raw_cli_envs(&mut command, envs);
    command.process_group(0);
    let mut child = command.spawn().expect("spawn cosh-shell raw");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(input.as_bytes())
            .expect("write scripted shell input");
    }

    let output = wait_for_raw_cli_output(child);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

pub(crate) fn run_raw_cli_with_delayed_input(
    adapter: &str,
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_and_delayed_input(adapter, &[], chunks)
}

pub(crate) fn run_raw_cli_with_args_and_delayed_input(
    adapter: &str,
    extra_args: &[&str],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_env_and_delayed_input(adapter, extra_args, &[], chunks)
}

pub(crate) fn run_raw_cli_with_args_env_and_delayed_input(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_env_current_dir_and_delayed_input(
        adapter,
        extra_args,
        envs,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        chunks,
    )
}

pub(crate) fn run_raw_cli_serial_with_args_env_and_delayed_input_after_ready(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    chunks: Vec<(Vec<u8>, Duration)>,
    session_ready: mpsc::Sender<()>,
) -> String {
    run_raw_cli_with_args_env_current_dir_and_delayed_input_inner(
        adapter,
        extra_args,
        envs,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        chunks,
        RawCliRunMode::Exclusive,
        Some(session_ready),
        None,
    )
}

pub(crate) fn run_raw_cli_with_analyzer_start_barrier(
    adapter: &str,
    envs: &[(&str, &str)],
    initial_input: &[u8],
    analyzer_started_marker: &Path,
    analyzer_continue_marker: &Path,
    foreground_input: &[u8],
    foreground_marker: &str,
) -> String {
    let _run_guard = raw_cli_run_guard(RawCliRunMode::Exclusive);
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = Command::new(binary);
    command
        .args(["raw", adapter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_raw_cli_command(&mut command);
    apply_raw_cli_envs(&mut command, envs);
    command.process_group(0);
    let mut child = RawCliChildGuard::new(command.spawn().expect("spawn cosh-shell raw"));
    let mut stdin = child.child_mut().stdin.take().expect("child stdin");
    let stdout = child.child_mut().stdout.take().expect("child stdout");
    let stderr = child.child_mut().stderr.take().expect("child stderr");
    let (output_sender, output_receiver) = std::sync::mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let mut stdout = stdout;
        let mut all = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => return all,
                Ok(count) => {
                    all.extend_from_slice(&chunk[..count]);
                    let _ = output_sender.send(chunk[..count].to_vec());
                }
                Err(error) => panic!("read raw cli stdout: {error}"),
            }
        }
    });
    let stderr_reader = read_pipe(stderr);

    let mut observed = Vec::new();
    if let Err(error) = wait_for_raw_cli_marker(
        &output_receiver,
        &mut observed,
        "Prompt recommendations are on; the current AI uses recent Shell and Agent activity.",
    ) {
        abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error);
    }
    let mut recommendations_ready = false;
    for _ in 0..4 {
        let response_start = observed.len();
        stdin
            .write_all(b"/recommendations on\n")
            .expect("enable recommendations");
        stdin.flush().expect("flush recommendation setup");
        let ready = wait_for_raw_cli_marker_or_retry(
            &output_receiver,
            &mut observed,
            response_start,
            "Prompt recommendations are on.",
            "Recommendation operation failed:",
        );
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error)
            }
        };
        if ready {
            recommendations_ready = true;
            break;
        }
    }
    assert!(
        recommendations_ready,
        "recommendation storage did not become ready: {}",
        String::from_utf8_lossy(&observed)
    );
    stdin.write_all(initial_input).expect("write initial input");
    stdin.flush().expect("flush initial input");
    if let Err(error) = wait_for_path(analyzer_started_marker) {
        abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error);
    }

    stdin
        .write_all(foreground_input)
        .expect("write foreground input");
    stdin.flush().expect("flush foreground input");
    if let Err(error) = wait_for_raw_cli_marker(&output_receiver, &mut observed, foreground_marker)
    {
        abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error);
    }

    fs::write(analyzer_continue_marker, b"1\n").expect("release Analyzer start barrier");
    stdin.write_all(b"exit\n").expect("write exit");
    drop(stdin);

    let status = match child.wait_timeout(RAW_CLI_TIMEOUT).expect("wait raw cli") {
        Some(status) => status,
        None => abort_raw_cli_with_output(
            &mut child,
            stdout_reader,
            stderr_reader,
            "raw CLI timed out after Analyzer barrier",
        ),
    };
    let stdout = join_reader(stdout_reader, "stdout");
    let stderr = join_reader(stderr_reader, "stderr");
    assert!(
        status.success(),
        "status={status:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let mut text = String::from_utf8_lossy(&stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&stderr));
    text
}

fn wait_for_raw_cli_marker(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    observed: &mut Vec<u8>,
    marker: &str,
) -> Result<(), String> {
    wait_for_raw_cli_marker_from(receiver, observed, 0, marker).map(|_| ())
}

// Incremental variant: only text at or after `cursor` matches, and the
// returned cursor points past the match so repeated markers (for example a
// repainted shell prompt) can gate successive steps.
fn wait_for_raw_cli_marker_from(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    observed: &mut Vec<u8>,
    cursor: usize,
    marker: &str,
) -> Result<usize, String> {
    if cursor > observed.len() {
        return Err(format!(
            "raw CLI marker cursor {cursor} is past the observed transcript ({} bytes)",
            observed.len()
        ));
    }
    let deadline = std::time::Instant::now() + RAW_CLI_TIMEOUT;
    loop {
        let window = &observed[cursor..];
        if let Some(found) = find_subslice(window, marker.as_bytes()) {
            return Ok(cursor + found + marker.len());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("raw CLI marker was not visible: {marker}"));
        }
        let chunk = match receiver.recv_timeout(remaining) {
            Ok(chunk) => chunk,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!("raw CLI timed out before marker: {marker}"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("raw CLI closed before marker: {marker}"));
            }
        };
        observed.extend(chunk);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn wait_for_raw_cli_input_ready(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    observed: &mut Vec<u8>,
    cursor: usize,
    token: &str,
    request: u64,
) -> Result<(), String> {
    let prefix = format!("\x1e{token}:{request}:");
    let deadline = std::time::Instant::now() + RAW_CLI_TIMEOUT;
    loop {
        let window = &observed[cursor.min(observed.len())..];
        if let Some(found) = find_subslice(window, prefix.as_bytes()) {
            let frame = cursor + found + prefix.len();
            if observed[frame..].contains(&b'\x1f') {
                return Ok(());
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "raw CLI did not acknowledge input readiness request {request}"
            ));
        }
        match receiver.recv_timeout(remaining) {
            Ok(chunk) => observed.extend(chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "raw CLI timed out before input readiness request {request}"
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "raw CLI closed before input readiness request {request}"
                ));
            }
        }
    }
}

fn wait_for_raw_cli_marker_or_retry(
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
    observed: &mut Vec<u8>,
    response_start: usize,
    marker: &str,
    retry_marker: &str,
) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + RAW_CLI_TIMEOUT;
    loop {
        let response = String::from_utf8_lossy(&observed[response_start..]);
        if response.contains(marker) {
            return Ok(true);
        }
        if response.contains(retry_marker) {
            return Ok(false);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "raw CLI marker was not visible: {marker}\n{}",
                String::from_utf8_lossy(observed)
            ));
        }
        let chunk = receiver
            .recv_timeout(remaining)
            .map_err(|_| format!("raw CLI closed before marker: {marker}"))?;
        observed.extend(chunk);
    }
}

fn wait_for_path(path: &Path) -> Result<(), String> {
    let deadline = std::time::Instant::now() + RAW_CLI_TIMEOUT;
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Analyzer did not reach start barrier: {}",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn abort_raw_cli_with_output(
    child: &mut RawCliChildGuard,
    stdout_reader: JoinHandle<Vec<u8>>,
    stderr_reader: JoinHandle<Vec<u8>>,
    reason: &str,
) -> ! {
    child.terminate_and_wait();
    let stdout = join_reader(stdout_reader, "stdout");
    let stderr = join_reader(stderr_reader, "stderr");
    panic!(
        "{reason}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
}

struct RawCliChildGuard {
    child: Option<Child>,
}

impl RawCliChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("raw CLI child is available")
    }

    fn wait_timeout(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        let status = self.child_mut().wait_timeout(timeout)?;
        if status.is_some() {
            self.child = None;
        }
        Ok(status)
    }

    fn terminate_and_wait(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        terminate_raw_cli_processes(child.id(), libc::SIGKILL);
        let _ = child.wait();
    }
}

impl Drop for RawCliChildGuard {
    fn drop(&mut self) {
        self.terminate_and_wait();
    }
}

pub(crate) fn run_raw_cli_serial_with_args_env_and_delayed_input(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_env_current_dir_and_delayed_input_inner(
        adapter,
        extra_args,
        envs,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        chunks,
        RawCliRunMode::Exclusive,
        None,
        None,
    )
}

pub(crate) fn run_raw_cli_with_args_env_current_dir_and_delayed_input(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    current_dir: &Path,
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_env_current_dir_and_delayed_input_inner(
        adapter,
        extra_args,
        envs,
        current_dir,
        chunks,
        RawCliRunMode::Shared,
        None,
        None,
    )
}

pub(crate) fn run_raw_cli_with_args_env_current_dir_and_delayed_input_after_marker(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    current_dir: &Path,
    ready_marker: &str,
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    run_raw_cli_with_args_env_current_dir_and_delayed_input_inner(
        adapter,
        extra_args,
        envs,
        current_dir,
        chunks,
        RawCliRunMode::Shared,
        None,
        Some(ready_marker),
    )
}

pub(crate) fn run_raw_cli_with_args_env_current_dir_and_marker_input(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    current_dir: &Path,
    steps: &[(&str, &[u8])],
) -> String {
    run_raw_cli_marker_input_inner(
        adapter,
        extra_args,
        envs,
        current_dir,
        steps,
        RawCliRunMode::Shared,
    )
}

pub(crate) fn run_raw_cli_serial_with_args_env_and_marker_input(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    steps: &[(&str, &[u8])],
) -> String {
    run_raw_cli_marker_input_inner(
        adapter,
        extra_args,
        envs,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        steps,
        RawCliRunMode::Exclusive,
    )
}

fn run_raw_cli_marker_input_inner(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    current_dir: &Path,
    steps: &[(&str, &[u8])],
    run_mode: RawCliRunMode,
) -> String {
    let _run_guard = raw_cli_run_guard(run_mode);
    let mut input_readiness = RawCliInputReadiness::new();
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = Command::new(binary);
    command
        .current_dir(current_dir)
        .args(["raw", adapter])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_raw_cli_command(&mut command);
    apply_raw_cli_envs(&mut command, envs);
    input_readiness.configure(&mut command);
    command.process_group(0);
    let mut child = RawCliChildGuard::new(command.spawn().expect("spawn cosh-shell raw"));
    let mut stdin = child.child_mut().stdin.take().expect("child stdin");
    let stdout = child.child_mut().stdout.take().expect("child stdout");
    let stderr = child.child_mut().stderr.take().expect("child stderr");
    let (output_receiver, stdout_reader) = read_pipe_with_chunks(stdout);
    let stderr_reader = read_pipe(stderr);
    let mut observed = Vec::new();
    let mut cursor = 0usize;

    for (marker, input) in steps {
        match wait_for_raw_cli_marker_from(&output_receiver, &mut observed, cursor, marker) {
            Ok(next_cursor) => cursor = next_cursor,
            Err(error) => {
                abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error)
            }
        }
        if !input.is_empty() {
            let request = input_readiness.request();
            if let Err(error) = wait_for_raw_cli_input_ready(
                &output_receiver,
                &mut observed,
                cursor,
                input_readiness.token(),
                request,
            ) {
                abort_raw_cli_with_output(&mut child, stdout_reader, stderr_reader, &error);
            }
            cursor = observed.len();
        }
        stdin.write_all(input).expect("write marker-gated input");
        stdin.flush().expect("flush marker-gated input");
    }
    drop(stdin);

    let status = match child.wait_timeout(RAW_CLI_TIMEOUT).expect("wait raw cli") {
        Some(status) => status,
        None => abort_raw_cli_with_output(
            &mut child,
            stdout_reader,
            stderr_reader,
            "raw CLI timed out after marker-gated input",
        ),
    };
    let stdout = input_readiness.strip_frames(&join_reader(stdout_reader, "stdout"));
    let stderr = join_reader(stderr_reader, "stderr");
    assert!(
        status.success(),
        "status={status:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let mut text = String::from_utf8_lossy(&stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&stderr));
    text
}

struct RawCliInputReadiness {
    directory: PathBuf,
    request_path: PathBuf,
    token: String,
    next_request: u64,
}

impl RawCliInputReadiness {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "cosh-raw-cli-input-ready-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create raw CLI input readiness directory");
        let request_path = directory.join("request");
        let token = format!("cosh-input-ready-{}-{nanos}", std::process::id());
        Self {
            directory,
            request_path,
            token,
            next_request: 0,
        }
    }

    fn configure(&self, command: &mut Command) {
        command
            .env("COSH_RAW_CLI_TEST_INPUT_READY_REQUEST", &self.request_path)
            .env("COSH_RAW_CLI_TEST_INPUT_READY_TOKEN", &self.token);
    }

    fn request(&mut self) -> u64 {
        self.next_request = self.next_request.saturating_add(1);
        fs::write(&self.request_path, self.next_request.to_string())
            .expect("write raw CLI input readiness request");
        self.next_request
    }

    fn token(&self) -> &str {
        &self.token
    }

    fn strip_frames(&self, output: &[u8]) -> Vec<u8> {
        let prefix = format!("\x1e{}:", self.token);
        let mut stripped = Vec::with_capacity(output.len());
        let mut cursor = 0;
        while let Some(found) = find_subslice(&output[cursor..], prefix.as_bytes()) {
            let frame_start = cursor + found;
            stripped.extend_from_slice(&output[cursor..frame_start]);
            let payload_start = frame_start + prefix.len();
            let Some(frame_end) = output[payload_start..]
                .iter()
                .position(|byte| *byte == b'\x1f')
            else {
                stripped.extend_from_slice(&output[frame_start..]);
                return stripped;
            };
            cursor = payload_start + frame_end + 1;
        }
        stripped.extend_from_slice(&output[cursor..]);
        stripped
    }
}

impl Drop for RawCliInputReadiness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn run_raw_cli_with_args_env_current_dir_and_delayed_input_inner(
    adapter: &str,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    current_dir: &Path,
    chunks: Vec<(Vec<u8>, Duration)>,
    run_mode: RawCliRunMode,
    session_ready: Option<mpsc::Sender<()>>,
    ready_marker: Option<&str>,
) -> String {
    let _run_guard = raw_cli_run_guard(run_mode);
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = Command::new(binary);
    command
        .current_dir(current_dir)
        .args(["raw", adapter])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_raw_cli_command(&mut command);
    apply_raw_cli_envs(&mut command, envs);
    let mut input_readiness = session_ready.as_ref().map(|_| RawCliInputReadiness::new());
    if let Some(input_readiness) = input_readiness.as_ref() {
        input_readiness.configure(&mut command);
    }
    command.process_group(0);
    let mut child = command.spawn().expect("spawn cosh-shell raw");
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");
    let (output_receiver, stdout_reader) = read_pipe_with_chunks(stdout);
    let stderr_reader = read_pipe(stderr);
    if let Some(session_ready) = session_ready {
        let input_readiness = input_readiness
            .as_mut()
            .expect("session readiness probe is configured");
        let request = input_readiness.request();
        let mut observed = Vec::new();
        wait_for_raw_cli_input_ready(
            &output_receiver,
            &mut observed,
            0,
            input_readiness.token(),
            request,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        session_ready
            .send(())
            .expect("signal raw CLI session readiness");
    }
    if let Some(marker) = ready_marker {
        let mut observed = Vec::new();
        wait_for_raw_cli_marker(&output_receiver, &mut observed, marker)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        for (bytes, delay) in chunks {
            thread::sleep(delay);
            stdin.write_all(&bytes).expect("write delayed input");
            stdin.flush().expect("flush delayed input");
        }
    }

    let mut output = wait_for_raw_cli_output_with_readers(child, (stdout_reader, stderr_reader));
    if let Some(input_readiness) = input_readiness.as_ref() {
        output.stdout = input_readiness.strip_frames(&output.stdout);
    }
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

pub(crate) fn run_raw_cli_default_with_args_env_and_delayed_input(
    extra_args: &[&str],
    envs: &[(&str, &str)],
    chunks: Vec<(Vec<u8>, Duration)>,
) -> String {
    let _run_guard = raw_cli_shared_run_guard();
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let mut command = Command::new(binary);
    command
        .args(["raw"])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_raw_cli_command(&mut command);
    apply_raw_cli_envs(&mut command, envs);
    command.process_group(0);
    let mut child = command.spawn().expect("spawn cosh-shell raw default");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        for (bytes, delay) in chunks {
            thread::sleep(delay);
            stdin.write_all(&bytes).expect("write delayed input");
            stdin.flush().expect("flush delayed input");
        }
    }

    let output = wait_for_raw_cli_output(child);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

#[derive(Clone, Copy)]
enum RawCliRunMode {
    Shared,
    Exclusive,
}

struct RawCliRunGate {
    state: Mutex<RawCliRunGateState>,
    changed: Condvar,
}

#[derive(Default)]
struct RawCliRunGateState {
    active_shared: usize,
    exclusive_active: bool,
    exclusive_waiting: usize,
}

pub(crate) struct RawCliRunGuard {
    gate: &'static RawCliRunGate,
    mode: RawCliRunMode,
}

pub(crate) fn raw_cli_shared_run_guard() -> RawCliRunGuard {
    raw_cli_run_guard(RawCliRunMode::Shared)
}

fn raw_cli_run_guard(mode: RawCliRunMode) -> RawCliRunGuard {
    let gate = RAW_CLI_RUN_GATE.get_or_init(RawCliRunGate::default);
    gate.acquire(mode);
    RawCliRunGuard { gate, mode }
}

impl Default for RawCliRunGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(RawCliRunGateState::default()),
            changed: Condvar::new(),
        }
    }
}

impl RawCliRunGate {
    fn acquire(&self, mode: RawCliRunMode) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match mode {
            RawCliRunMode::Shared => {
                while state.exclusive_active
                    || state.exclusive_waiting > 0
                    || state.active_shared >= raw_cli_shared_parallelism()
                {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                state.active_shared += 1;
            }
            RawCliRunMode::Exclusive => {
                state.exclusive_waiting += 1;
                while state.exclusive_active || state.active_shared > 0 {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                state.exclusive_waiting -= 1;
                state.exclusive_active = true;
            }
        }
    }

    fn release(&self, mode: RawCliRunMode) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match mode {
            RawCliRunMode::Shared => {
                state.active_shared -= 1;
            }
            RawCliRunMode::Exclusive => {
                state.exclusive_active = false;
            }
        }
        self.changed.notify_all();
    }
}

impl Drop for RawCliRunGuard {
    fn drop(&mut self) {
        self.gate.release(self.mode);
    }
}

fn configure_raw_cli_command(command: &mut Command) {
    let home = temp_shell_home("default-home");
    let git_work_tree = raw_cli_git_fixture();
    command
        .env("COSH_SHELL_ISOLATED", "1")
        .env("COSH_SHELL_INTEGRATION", "enhanced")
        .env("COSH_SHELL_RAW_SHELL", "bash")
        .env("COSH_SHELL_DEFAULT_SHELL", "bash")
        .env("COSH_SHELL_LANG", "en-US")
        .env("COSH_SHELL_BOOTSTRAP_PATH", "0")
        .env("COSH_SHELL_HEALTH_SCAN", "disabled")
        .env("COSH_RECOMMENDATIONS_ENABLED", "0")
        // Pin the renderer: markers such as approval request IDs only exist
        // in rich output, so tests must not inherit the caller's TERM.
        // Plain/dumb-renderer tests override TERM explicitly per case.
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("HOME", home)
        .env("GIT_DIR", git_work_tree.join(".git"))
        .env("GIT_WORK_TREE", git_work_tree);
}

fn raw_cli_git_fixture() -> &'static Path {
    RAW_CLI_GIT_FIXTURE
        .get_or_init(|| {
            let path = temp_shell_home("git-fixture");
            let status = Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&path)
                .status()
                .expect("initialize raw cli git fixture");
            assert!(
                status.success(),
                "initialize raw cli git fixture: {status:?}"
            );
            path
        })
        .as_path()
}

struct RawCliOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn wait_for_raw_cli_output(mut child: Child) -> RawCliOutput {
    let readers = start_raw_cli_output_readers(&mut child);
    wait_for_raw_cli_output_with_readers(child, readers)
}

type RawCliOutputReaders = (JoinHandle<Vec<u8>>, JoinHandle<Vec<u8>>);

fn start_raw_cli_output_readers(child: &mut Child) -> RawCliOutputReaders {
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");
    (read_pipe(stdout), read_pipe(stderr))
}

fn wait_for_raw_cli_output_with_readers(
    mut child: Child,
    (stdout_reader, stderr_reader): RawCliOutputReaders,
) -> RawCliOutput {
    let pid = child.id();
    let status = match child.wait_timeout(RAW_CLI_TIMEOUT).expect("wait raw cli") {
        Some(status) => status,
        None => {
            terminate_raw_cli_processes(pid, libc::SIGTERM);
            thread::sleep(Duration::from_millis(100));
            if child
                .try_wait()
                .expect("poll raw cli after SIGTERM")
                .is_none()
            {
                terminate_raw_cli_processes(pid, libc::SIGKILL);
            }
            let status = child.wait().expect("wait killed raw cli");
            terminate_raw_cli_processes(pid, libc::SIGKILL);
            let stdout = join_reader(stdout_reader, "stdout");
            let stderr = join_reader(stderr_reader, "stderr");
            panic!(
                "raw cli timed out after {:?}; status={:?}\nstdout={}\nstderr={}",
                RAW_CLI_TIMEOUT,
                status,
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
    };

    terminate_raw_cli_processes(pid, libc::SIGTERM);
    thread::sleep(Duration::from_millis(50));
    terminate_raw_cli_processes(pid, libc::SIGKILL);
    RawCliOutput {
        status,
        stdout: join_reader(stdout_reader, "stdout"),
        stderr: join_reader(stderr_reader, "stderr"),
    }
}

fn apply_raw_cli_envs(command: &mut Command, envs: &[(&str, &str)]) {
    for (key, value) in envs {
        if *value == RAW_CLI_UNSET_ENV || (*key == "COSH_SHELL_ISOLATED" && *value == "0") {
            command.env_remove(key);
        } else {
            command.env(key, value);
        }
    }
}

fn read_pipe<R>(mut pipe: R) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        pipe.read_to_end(&mut output).expect("read raw cli output");
        output
    })
}

fn read_pipe_with_chunks<R>(
    mut pipe: R,
) -> (std::sync::mpsc::Receiver<Vec<u8>>, JoinHandle<Vec<u8>>)
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => return output,
                Ok(count) => {
                    output.extend_from_slice(&chunk[..count]);
                    let _ = sender.send(chunk[..count].to_vec());
                }
                Err(error) => panic!("read raw cli stdout: {error}"),
            }
        }
    });
    (receiver, reader)
}

fn join_reader(reader: JoinHandle<Vec<u8>>, label: &str) -> Vec<u8> {
    reader
        .join()
        .unwrap_or_else(|_| panic!("join raw cli {label} reader"))
}

fn terminate_raw_cli_processes(pid: u32, signal: i32) {
    terminate_process_group(pid, signal);
    terminate_raw_session_processes(pid, signal);
}

fn terminate_raw_session_processes(pid: u32, signal: i32) {
    let marker = format!("cosh-shell-raw-session-{pid}/cosh-marker");
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().filter(|line| line.contains(&marker)) {
        let Some(raw_pid) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(child_pid) = raw_pid.parse::<i32>() else {
            continue;
        };
        terminate_pid(child_pid, signal);
        terminate_pid(-child_pid, signal);
    }
}

fn terminate_process_group(pid: u32, signal: i32) {
    terminate_pid(-(pid as i32), signal);
}

fn terminate_pid(pid: i32, signal: i32) {
    let result = unsafe { libc::kill(pid, signal) };
    if result < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            eprintln!("failed to signal raw cli process {pid}: {err}");
        }
    }
}

pub(crate) fn temp_zsh_home(label: &str) -> PathBuf {
    temp_shell_home(label)
}

pub(crate) fn temp_shell_home(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "cosh-raw-cli-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join(".hushlogin"), "").unwrap();
    path
}

pub(crate) fn write_cosh_config(home: &Path, content: &str) {
    let config_dir = home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), content).unwrap();
}

pub(crate) fn write_legacy_cosh_config(home: &Path, content: &str) {
    let config_dir = home.join(".config/cosh");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), content).unwrap();
}

pub(crate) fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub(crate) fn assert_inline_before_followup(
    output: &str,
    inline_marker: &str,
    followup_output: &str,
) {
    let inline_pos = output.find(inline_marker).expect("inline guidance marker");
    let followup_pos = output
        .rfind(followup_output)
        .expect("followup shell output");
    assert!(inline_pos < followup_pos, "{output}");
}

pub(crate) fn assert_agent_loading_visible(output: &str) {
    assert!(
        output.contains("Thinking...") || output.contains("正在思考..."),
        "{output}"
    );
}

pub(crate) fn agent_loading_count(output: &str) -> usize {
    count_occurrences(output, "Thinking...") + count_occurrences(output, "正在思考...")
}

pub(crate) fn json_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn process_is_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn read_pid_file_with_retry(path: &Path) -> u32 {
    let mut last_error = None;
    for _ in 0..40 {
        match fs::read_to_string(path) {
            Ok(pid_text) => return pid_text.trim().parse::<u32>().expect("provider pid"),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    panic!("provider pid file {}: {last_error:?}", path.display());
}

pub(crate) fn signal_process_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([format!("-{signal}"), format!("-{pid}")])
        .status();
}

pub(crate) fn signal_pid(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status();
}

pub(crate) fn assert_no_prompt_between(output: &str, start_marker: &str, end_marker: &str) {
    let start = output.find(start_marker).expect("start marker");
    let end = output[start..]
        .find(end_marker)
        .map(|idx| start + idx)
        .expect("end marker");
    assert!(!output[start..end].contains("cosh-osc$"), "{output}");
}

pub(crate) fn assert_ordered(output: &str, markers: &[&str]) {
    let mut previous = 0;
    for marker in markers {
        let relative = output[previous..]
            .find(marker)
            .unwrap_or_else(|| panic!("missing ordered marker `{marker}` in output:\n{output}"));
        previous += relative + marker.len();
    }
}

pub(crate) fn count_occurrences(output: &str, needle: &str) -> usize {
    output.match_indices(needle).count()
}

pub(crate) fn count_occurrences_between(
    output: &str,
    start: &str,
    end: &str,
    needle: &str,
) -> usize {
    let start_idx = output.find(start).expect("start marker") + start.len();
    let end_idx = output[start_idx..]
        .find(end)
        .map(|idx| start_idx + idx)
        .expect("end marker");
    count_occurrences(&output[start_idx..end_idx], needle)
}

pub(crate) fn assert_no_standalone_percent_line(output: &str) {
    let clean = strip_ansi_escape(output).replace('\r', "\n");
    assert!(
        !clean.lines().any(|line| line.trim_end() == "%"),
        "{output}"
    );
}

pub(crate) fn assert_agent_block_width(output: &str, max_width: usize) {
    let clean = strip_ansi_escape(output);
    for line in clean
        .lines()
        .flat_map(|line| line.split('\r'))
        .filter(|line| {
            line.contains('╭')
                || line.contains('╰')
                || line.contains('│')
                || line.contains('┌')
                || line.contains('└')
        })
    {
        assert!(
            display_width(line) <= max_width,
            "line width {} exceeds {max_width}: {line:?}\n{output}",
            display_width(line)
        );
    }
}

pub(crate) fn strip_ansi_escape(text: &str) -> String {
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            stripped.push(ch);
            continue;
        }

        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    stripped
}

pub(crate) fn compact_terminal_words(text: &str) -> String {
    strip_ansi_escape(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}
