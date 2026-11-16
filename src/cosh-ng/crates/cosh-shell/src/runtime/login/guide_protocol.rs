//! Private, versioned, bounded privilege protocol for the guide login page.
//!
//! The parent owns both files and passes the result as stdout, never as a path
//! for root to open. Sudo reads passwords on the tty; replies contain no secrets.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};

use serde::{Deserialize, Serialize};

use super::{account, cli, query_status, Action, LoginStatus};
use crate::i18n::I18n;
use crate::runtime::terminal::IgnoreInterrupts;

pub(crate) const WORKER_FLAG: &str = "--__cosh-login-guide-v2";
pub(super) const VERSION: u32 = 2;
pub(super) const MAX_REQUEST_BYTES: usize = 4096;
pub(super) const MAX_REPLY_BYTES: usize = 16384;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) version: u32,
    pub(crate) action: String,
    pub(crate) username: String,
    pub(crate) expected_uid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reply {
    pub(crate) version: u32,
    pub(crate) outcome: String,
    pub(crate) username: String,
    pub(crate) uid: u32,
    pub(crate) current_shell: String,
    pub(crate) original_bash: Option<String>,
    pub(crate) managed: bool,
    pub(crate) conflict: bool,
    pub(crate) error: Option<String>,
}

impl Reply {
    fn from_status(status: &LoginStatus) -> Self {
        Self {
            version: VERSION,
            outcome: "ok".into(),
            username: status.username.clone(),
            uid: status.uid,
            current_shell: status.current_shell.clone(),
            original_bash: status.original_bash.clone(),
            managed: status.managed,
            conflict: status.conflict,
            error: None,
        }
    }

    pub(crate) fn error(request: &Request, message: String) -> Self {
        Self {
            version: VERSION,
            outcome: "error".into(),
            username: request.username.clone(),
            uid: request.expected_uid,
            current_shell: String::new(),
            original_bash: None,
            managed: false,
            conflict: false,
            error: Some(message),
        }
    }

    pub(crate) fn unconfirmed(request: &Request) -> Self {
        Self {
            version: VERSION,
            outcome: "unconfirmed".into(),
            username: request.username.clone(),
            uid: request.expected_uid,
            current_shell: String::new(),
            original_bash: None,
            managed: false,
            conflict: false,
            error: None,
        }
    }
}

/// Classification of a handoff result; anything without a valid, identity-matching
/// structured result is Unconfirmed and must not be blindly retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Verified(LoginStatus),
    Unconfirmed,
    Error(String),
    HandoffFailed(HandoffFailure),
}

/// Why the foreground handoff produced no structured result. None of these
/// prove the account is unchanged, so callers must re-read status before
/// reporting a definite failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandoffFailure {
    Spawn {
        message: String,
        kind: std::io::ErrorKind,
    },
    Cancelled,
    Usage,
    Exit(i32),
}

/// Maps a non-zero child status: only the interrupt signals the operator sends
/// at the prompt mean the change was cancelled, 2 is a usage rejection, and a
/// crash signal or any other exit code is a failure the page must not dress up
/// as a cancellation.
pub(super) fn failure_from_status(status: ExitStatus) -> HandoffFailure {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        // sudo re-raises whatever killed the worker, so a worker crash arrives
        // here as the crash signal rather than as a sudo exit code.
        return if matches!(signal, nix::libc::SIGINT | nix::libc::SIGQUIT) {
            HandoffFailure::Cancelled
        } else {
            HandoffFailure::Exit(128 + signal)
        };
    }
    match status.code() {
        Some(2) => HandoffFailure::Usage,
        Some(code) => HandoffFailure::Exit(code),
        None => HandoffFailure::Cancelled,
    }
}

pub(crate) fn classify(
    raw: Option<&str>,
    username: &str,
    expected_uid: u32,
    target_shell: &str,
) -> Outcome {
    let Some(raw) = raw else {
        return Outcome::Unconfirmed;
    };
    if raw.len() > MAX_REPLY_BYTES {
        return Outcome::Unconfirmed;
    }
    let Ok(reply) = serde_json::from_str::<Reply>(raw) else {
        return Outcome::Unconfirmed;
    };
    if reply.version != VERSION || reply.username != username || reply.uid != expected_uid {
        return Outcome::Unconfirmed;
    }
    match reply.outcome.as_str() {
        "error" => {
            return Outcome::Error(reply.error.unwrap_or_else(|| "login worker failed".into()));
        }
        "ok" if reply.current_shell == target_shell => {}
        _ => return Outcome::Unconfirmed,
    }
    Outcome::Verified(LoginStatus {
        username: reply.username,
        uid: reply.uid,
        current_shell: reply.current_shell,
        original_bash: reply.original_bash,
        transaction: String::new(),
        managed: reply.managed,
        conflict: reply.conflict,
    })
}

/// Runs as root through sudo, restricted to the initiating account and inherited
/// result FD; never opens any caller-selected output path.
pub(crate) fn run_worker(request_path: &str) -> i32 {
    if unsafe { nix::libc::geteuid() } != 0 || unsafe { nix::libc::getuid() } != 0 {
        return 1;
    }
    let Ok(sudo_uid) = std::env::var("SUDO_UID") else {
        return 1;
    };
    let Some(caller_uid) = parse_sudo_uid(&sudo_uid) else {
        return 1;
    };
    let Ok(output) = std::io::stdout().as_fd().try_clone_to_owned() else {
        return 1;
    };
    run_worker_for(request_path, &mut File::from(output), caller_uid)
}

pub(super) fn parse_sudo_uid(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

pub(super) fn private_file(metadata: &Metadata, uid: u32) -> bool {
    metadata.is_file()
        && metadata.uid() == uid
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == 1
}

pub(super) fn read_request(path: &str, caller_uid: u32) -> Option<Request> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !private_file(&metadata, caller_uid) || metadata.len() > MAX_REQUEST_BYTES as u64 {
        return None;
    }
    let mut content = Vec::new();
    file.take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .ok()?;
    if content.len() > MAX_REQUEST_BYTES {
        return None;
    }
    let request: Request = serde_json::from_slice(&content).ok()?;
    if request.version != VERSION {
        return None;
    }
    Some(request)
}

pub(super) fn validate_identity(request: &Request, caller_uid: u32) -> bool {
    if request.expected_uid != caller_uid || !account::valid_name(&request.username) {
        return false;
    }
    let Ok(caller) = account::lookup_nss(None, caller_uid) else {
        return false;
    };
    let Ok(named) = account::lookup_nss(Some(&request.username), caller_uid) else {
        return false;
    };
    caller.uid == caller_uid && caller.name == request.username && caller == named
}

pub(super) fn run_worker_for(request_path: &str, output: &mut File, caller_uid: u32) -> i32 {
    let Some(request) = read_request(request_path, caller_uid) else {
        return 1;
    };
    let flags = unsafe { nix::libc::fcntl(output.as_raw_fd(), nix::libc::F_GETFL) };
    if !validate_identity(&request, caller_uid)
        || flags < 0
        || !matches!(
            flags & nix::libc::O_ACCMODE,
            nix::libc::O_WRONLY | nix::libc::O_RDWR
        )
        || !validate_output(output, caller_uid, flags)
    {
        return 1;
    }
    let reply = execute(&request);
    let Ok(bytes) = serde_json::to_vec(&reply) else {
        return 1;
    };
    if bytes.len() > MAX_REPLY_BYTES || output.write_all(&bytes).is_err() {
        return 1;
    }
    0
}

fn validate_output(output: &mut File, caller_uid: u32, flags: i32) -> bool {
    let Ok(metadata) = output.metadata() else {
        return false;
    };
    if metadata.file_type().is_fifo() {
        // Sudo output auditing forwards this inherited pipe into the parent's
        // file. A stalled reader must not hang the worker after an account write.
        return unsafe {
            nix::libc::fcntl(
                output.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        } == 0;
    }
    private_file(&metadata, caller_uid)
        && metadata.len() == 0
        && flags & nix::libc::O_APPEND == 0
        && output.stream_position().ok() == Some(0)
}

fn execute(request: &Request) -> Reply {
    let action = match request.action.as_str() {
        "query" => Action::Status,
        "set" => Action::Set,
        "restore" => Action::Restore,
        _ => return Reply::error(request, "unknown login action".into()),
    };
    if action == Action::Status {
        return match query_status(Some(&request.username)) {
            Ok(status) => Reply::from_status(&status),
            Err(error) => Reply::error(request, error),
        };
    }
    let options = cli::Options {
        action,
        user: Some(request.username.clone()),
        yes: true,
        expected_uid: Some(request.expected_uid),
    };
    if let Err(error) = cli::run_quiet(options, I18n::new(crate::config::Language::EnUs)) {
        return reply_from_run_error(request, &error);
    }
    match query_status(Some(&request.username)) {
        Ok(status) => Reply::from_status(&status),
        Err(_) => Reply::unconfirmed(request),
    }
}

/// Maps a login owner error to a reply: a post-write uncertainty (the account
/// may already have changed) is Unconfirmed; a definite pre-write failure is
/// Error.
pub(super) fn reply_from_run_error(request: &Request, error: &super::Error) -> Reply {
    if error.is_unconfirmed() {
        Reply::unconfirmed(request)
    } else {
        Reply::error(request, error.to_string())
    }
}

/// Parent side: hand the terminal to foreground sudo running the worker, then
/// classify the structured result. The password is read by sudo on the tty.
pub(crate) fn elevated(
    action: &str,
    username: &str,
    expected_uid: u32,
    target_shell: &str,
) -> Outcome {
    let Ok(dir) = tempfile::tempdir() else {
        return Outcome::Unconfirmed;
    };
    let request_path = dir.path().join("request.json");
    let Ok(mut result) = tempfile::NamedTempFile::new_in(dir.path()) else {
        return Outcome::Unconfirmed;
    };
    let request = Request {
        version: VERSION,
        action: action.into(),
        username: username.into(),
        expected_uid,
    };
    let Ok(bytes) = serde_json::to_vec(&request) else {
        return Outcome::Unconfirmed;
    };
    let request_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&request_path);
    if bytes.len() > MAX_REQUEST_BYTES
        || !request_file.is_ok_and(|mut file| file.write_all(&bytes).is_ok())
    {
        return Outcome::Unconfirmed;
    }
    let Ok(exe) = std::env::current_exe() else {
        return Outcome::Unconfirmed;
    };
    let Ok(output) = result.as_file().try_clone() else {
        return Outcome::Unconfirmed;
    };
    let mut command = Command::new("/usr/bin/sudo");
    command
        .arg("--")
        .arg(exe)
        .arg(WORKER_FLAG)
        .arg(&request_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::from(output))
        .stderr(Stdio::inherit());
    // Safety: only async-signal-safe `signal` calls run between fork and exec,
    // and they restore the dispositions the guard below masked so sudo's own
    // Ctrl-C handling still applies to the child.
    unsafe {
        command.pre_exec(|| {
            nix::libc::signal(nix::libc::SIGINT, nix::libc::SIG_DFL);
            nix::libc::signal(nix::libc::SIGQUIT, nix::libc::SIG_DFL);
            Ok(())
        });
    }
    // sudo shares this foreground process group; a Ctrl-C aimed at its password
    // prompt must cancel the change and return to the page, not tear the
    // wizard down through the terminal recovery handler.
    let _interrupts = IgnoreInterrupts::new();
    let status = command.status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => return Outcome::HandoffFailed(failure_from_status(status)),
        Err(error) => {
            return Outcome::HandoffFailed(HandoffFailure::Spawn {
                message: error.to_string(),
                kind: error.kind(),
            })
        }
    }
    if result.seek(SeekFrom::Start(0)).is_err() {
        return Outcome::Unconfirmed;
    }
    let mut raw = String::new();
    if result
        .take(MAX_REPLY_BYTES as u64 + 1)
        .read_to_string(&mut raw)
        .is_err()
    {
        return Outcome::Unconfirmed;
    }
    classify(Some(&raw), username, expected_uid, target_shell)
}
