use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::adapter::{parse_initialize_response_value, CONTROL_PROTOCOL_VERSION};

pub(crate) use super::personal_process_group::{
    analyzer_process_is_gone, process_start_identity_token, verified_terminate_process_group,
    ProcessGroupIdentity,
};
#[cfg(test)]
use super::personal_process_group::{group_has_live_members, process_group_identity_matches};
use super::personal_runner::{
    AnalyzerProcess, InitializeResult, ProcessFailure, RunnerCommand, RunnerEvent,
};

const INITIALIZE_REQUEST_ID: &str = "recommendation-init";
const SHUTDOWN_REQUEST_ID: &str = "recommendation-shutdown";
const MAX_JSONL_LINE_BYTES: usize = 128 * 1024;
const MAX_STDOUT_BYTES: usize = 256 * 1024;

pub(crate) struct CoshCoreAnalyzerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<StdoutEvent>,
    process_group_id: u32,
    owner_pid: u32,
    owner_start_identity: Option<String>,
    leader_start_identity: Option<String>,
    initialized: bool,
    shutdown_sent: bool,
    cancelled: bool,
    cancellation_failed: bool,
}

impl CoshCoreAnalyzerProcess {
    pub(crate) fn spawn(command: RunnerCommand) -> Result<Self, ProcessFailure> {
        let owner_pid = std::process::id();
        let owner_start_identity = process_start_identity_token(owner_pid);
        #[cfg(target_os = "linux")]
        let expected_parent = std::process::id() as nix::libc::pid_t;
        let mut child_command = Command::new(command.program);
        child_command
            .args(command.args)
            .envs(command.env)
            .current_dir(command.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            child_command.pre_exec(move || {
                if nix::libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    if nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGTERM, 0, 0, 0)
                        < 0
                        || nix::libc::getppid() != expected_parent
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = child_command
            .spawn()
            .map_err(|_| ProcessFailure::Transport)?;
        let Some(stdin) = child.stdin.take() else {
            terminate_and_reap(&mut child);
            return Err(ProcessFailure::Transport);
        };
        if set_nonblocking(&stdin).is_err() {
            terminate_and_reap(&mut child);
            return Err(ProcessFailure::Transport);
        }
        let Some(stdout) = child.stdout.take() else {
            terminate_and_reap(&mut child);
            return Err(ProcessFailure::Transport);
        };
        let process_group_id = child.id();
        let leader_start_identity = process_start_identity_token(process_group_id);

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: bounded_stdout_reader(stdout),
            process_group_id,
            owner_pid,
            owner_start_identity,
            leader_start_identity,
            initialized: false,
            shutdown_sent: false,
            cancelled: false,
            cancellation_failed: false,
        })
    }

    pub(crate) fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    pub(crate) fn cancellation_failed(&self) -> bool {
        self.cancellation_failed
    }

    fn send_json(&mut self, value: Value, deadline: Instant) -> Result<(), ProcessFailure> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| ProcessFailure::Transport)?;
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or(ProcessFailure::Transport)?;
        write_all_before(stdin, &bytes, deadline)
    }

    fn read_json(&self, timeout: Duration) -> Result<Option<Value>, ProcessFailure> {
        match self.stdout.recv_timeout(timeout) {
            Ok(StdoutEvent::Line(line)) => serde_json::from_str(&line)
                .map(Some)
                .map_err(|_| ProcessFailure::Transport),
            Ok(StdoutEvent::Eof) => Ok(None),
            Ok(StdoutEvent::Invalid) => Err(ProcessFailure::Transport),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ProcessFailure::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ProcessFailure::Transport),
        }
    }

    fn send_shutdown(&mut self, deadline: Instant) -> Result<(), ProcessFailure> {
        if self.shutdown_sent {
            return Ok(());
        }
        self.send_json(
            json!({
                "type": "control_request",
                "request_id": SHUTDOWN_REQUEST_ID,
                "request": { "subtype": "shutdown" }
            }),
            deadline,
        )?;
        self.shutdown_sent = true;
        Ok(())
    }
}

impl AnalyzerProcess for CoshCoreAnalyzerProcess {
    fn initialize(&mut self, timeout: Duration) -> Result<InitializeResult, ProcessFailure> {
        let deadline = Instant::now() + timeout;
        self.send_json(
            json!({
                "type": "control_request",
                "request_id": INITIALIZE_REQUEST_ID,
                "request": {
                    "subtype": "initialize",
                    "protocol_version": CONTROL_PROTOCOL_VERSION
                }
            }),
            deadline,
        )?;
        let mut acknowledged = false;
        let mut init = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProcessFailure::Timeout);
            }
            let Some(message) = self.read_json(remaining)? else {
                return Err(ProcessFailure::Transport);
            };
            if is_auth_request(&message) {
                return Ok(InitializeResult::AuthRequired);
            }
            if initialize_acknowledged(&message)? {
                acknowledged = true;
            }
            if let Some(init_config) = initialize_config(&message)? {
                init = Some(init_config);
            }
            if acknowledged {
                if let Some((model, tools)) = init.take() {
                    self.initialized = true;
                    return Ok(InitializeResult::Ready { model, tools });
                }
            }
        }
    }

    fn send_body(&mut self, body: &str, timeout: Duration) -> Result<(), ProcessFailure> {
        if !self.initialized || self.shutdown_sent {
            return Err(ProcessFailure::Transport);
        }
        let value = json!({
            "type": "user",
            "message": { "role": "user", "content": body },
            "parent_tool_use_id": null,
            "session_id": "default"
        });
        let mut bytes = serde_json::to_vec(&value).map_err(|_| ProcessFailure::Transport)?;
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or(ProcessFailure::Transport)?;
        write_all_before(stdin, &bytes, Instant::now() + timeout)
    }

    fn next_event(&mut self, timeout: Duration) -> Result<RunnerEvent, ProcessFailure> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProcessFailure::Timeout);
            }
            let Some(message) = self.read_json(remaining)? else {
                return if self.shutdown_sent {
                    Ok(RunnerEvent::End)
                } else {
                    Err(ProcessFailure::Transport)
                };
            };
            if let Some(event) = map_output(&message) {
                if matches!(event, RunnerEvent::Result { .. }) {
                    self.send_shutdown(deadline)?;
                }
                return Ok(event);
            }
        }
    }

    fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.stdin.take();
        let (Some(owner_start_identity), Some(leader_start_identity)) = (
            self.owner_start_identity.clone(),
            self.leader_start_identity.clone(),
        ) else {
            terminate_and_reap(&mut self.child);
            self.cancelled = true;
            self.cancellation_failed = true;
            return;
        };
        let terminated = verified_terminate_process_group(&ProcessGroupIdentity {
            owner_pid: self.owner_pid,
            owner_start_identity,
            leader_pid: self.child.id(),
            leader_start_identity,
            process_group_id: self.process_group_id,
        });
        self.cancelled = terminated;
        self.cancellation_failed = !terminated;
        let _ = self.child.try_wait();
    }
}

fn terminate_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

trait DeadlineWriter: Write {
    fn wait_writable(&self, timeout: Duration) -> io::Result<bool>;
}

impl DeadlineWriter for ChildStdin {
    fn wait_writable(&self, timeout: Duration) -> io::Result<bool> {
        let mut descriptor = nix::libc::pollfd {
            fd: self.as_raw_fd(),
            events: nix::libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().max(1).min(i32::MAX as u128) as i32;
        let result = unsafe { nix::libc::poll(&mut descriptor, 1, timeout_ms) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(result > 0 && descriptor.revents & nix::libc::POLLOUT != 0)
    }
}

fn write_all_before(
    writer: &mut impl DeadlineWriter,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), ProcessFailure> {
    let mut written = 0usize;
    while written < bytes.len() {
        if Instant::now() >= deadline {
            return Err(timeout_failure(written));
        }
        match writer.write(&bytes[written..]) {
            Ok(0) => return Err(transport_failure(written)),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero()
                    || !writer
                        .wait_writable(remaining)
                        .map_err(|_| transport_failure(written))?
                {
                    return Err(timeout_failure(written));
                }
            }
            Err(_) => return Err(transport_failure(written)),
        }
    }
    loop {
        if Instant::now() >= deadline {
            return Err(ProcessFailure::TimeoutAfterWrite);
        }
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero()
                    || !writer
                        .wait_writable(remaining)
                        .map_err(|_| ProcessFailure::TransportAfterWrite)?
                {
                    return Err(ProcessFailure::TimeoutAfterWrite);
                }
            }
            Err(_) => return Err(ProcessFailure::TransportAfterWrite),
        }
    }
}

fn transport_failure(written: usize) -> ProcessFailure {
    if written == 0 {
        ProcessFailure::Transport
    } else {
        ProcessFailure::TransportAfterWrite
    }
}

fn timeout_failure(written: usize) -> ProcessFailure {
    if written == 0 {
        ProcessFailure::Timeout
    } else {
        ProcessFailure::TimeoutAfterWrite
    }
}

fn set_nonblocking(stdin: &ChildStdin) -> io::Result<()> {
    let descriptor = stdin.as_raw_fd();
    let flags = unsafe { nix::libc::fcntl(descriptor, nix::libc::F_GETFL) };
    if flags < 0
        || unsafe {
            nix::libc::fcntl(
                descriptor,
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Drop for CoshCoreAnalyzerProcess {
    fn drop(&mut self) {
        self.cancel();
    }
}

enum StdoutEvent {
    Line(String),
    Eof,
    Invalid,
}

fn bounded_stdout_reader(mut stdout: impl Read + Send + 'static) -> Receiver<StdoutEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut pending = Vec::new();
        let mut total = 0usize;
        let mut chunk = [0u8; 4096];
        loop {
            let count = match stdout.read(&mut chunk) {
                Ok(0) => {
                    if !pending.is_empty() && !send_line(&sender, &pending) {
                        return;
                    }
                    let _ = sender.send(StdoutEvent::Eof);
                    return;
                }
                Ok(count) => count,
                Err(_) => {
                    let _ = sender.send(StdoutEvent::Invalid);
                    return;
                }
            };
            total = total.saturating_add(count);
            if total > MAX_STDOUT_BYTES {
                let _ = sender.send(StdoutEvent::Invalid);
                return;
            }
            pending.extend_from_slice(&chunk[..count]);
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = pending.drain(..=newline).collect();
                if !send_line(&sender, &line) {
                    return;
                }
            }
            if pending.len() > MAX_JSONL_LINE_BYTES {
                let _ = sender.send(StdoutEvent::Invalid);
                return;
            }
        }
    });
    receiver
}

fn send_line(sender: &mpsc::Sender<StdoutEvent>, bytes: &[u8]) -> bool {
    if bytes.len() > MAX_JSONL_LINE_BYTES {
        let _ = sender.send(StdoutEvent::Invalid);
        return false;
    }
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let Ok(line) = std::str::from_utf8(line) else {
        let _ = sender.send(StdoutEvent::Invalid);
        return false;
    };
    sender.send(StdoutEvent::Line(line.to_string())).is_ok()
}

fn initialize_acknowledged(message: &Value) -> Result<bool, ProcessFailure> {
    if message.get("type").and_then(Value::as_str) != Some("control_response")
        || message
            .pointer("/response/request_id")
            .and_then(Value::as_str)
            != Some(INITIALIZE_REQUEST_ID)
    {
        return Ok(false);
    }
    match parse_initialize_response_value(message, INITIALIZE_REQUEST_ID) {
        Some(Ok(_)) => Ok(true),
        Some(Err(_)) | None => Err(ProcessFailure::Transport),
    }
}

fn initialize_config(message: &Value) -> Result<Option<(String, Vec<String>)>, ProcessFailure> {
    if message.get("type").and_then(Value::as_str) != Some("system")
        || message.get("subtype").and_then(Value::as_str) != Some("init")
    {
        return Ok(None);
    }
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or(ProcessFailure::Transport)?
        .to_string();
    let tools = message
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(ProcessFailure::Transport)?
        .iter()
        .map(|tool| {
            tool.as_str()
                .map(str::to_string)
                .ok_or(ProcessFailure::Transport)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some((model, tools)))
}

fn is_auth_request(message: &Value) -> bool {
    message.get("type").and_then(Value::as_str) == Some("control_request")
        && message.pointer("/request/subtype").and_then(Value::as_str) == Some("auth_required")
}

fn map_output(message: &Value) -> Option<RunnerEvent> {
    match message.get("type")?.as_str()? {
        "stream_event" => map_stream_event(message),
        "assistant" => map_assistant(message),
        "result" => Some(RunnerEvent::Result {
            success: !message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }),
        "control_request" => match message.pointer("/request/subtype")?.as_str()? {
            "auth_required" => Some(RunnerEvent::AuthRequired),
            "ask_user" => Some(RunnerEvent::Question),
            "can_use_tool" => Some(RunnerEvent::ApprovalRequest),
            _ => Some(RunnerEvent::ToolCall),
        },
        _ => None,
    }
}

fn map_stream_event(message: &Value) -> Option<RunnerEvent> {
    let event = message.get("event")?;
    match event.get("type")?.as_str()? {
        "content_block_start" if event.pointer("/content_block/type")?.as_str()? == "tool_use" => {
            Some(RunnerEvent::ToolCall)
        }
        "content_block_delta" => match event.pointer("/delta/type")?.as_str()? {
            "text_delta" => Some(RunnerEvent::AssistantDelta(
                event.pointer("/delta/text")?.as_str()?.to_string(),
            )),
            "input_json_delta" => Some(RunnerEvent::ToolCall),
            _ => None,
        },
        _ => None,
    }
}

fn map_assistant(message: &Value) -> Option<RunnerEvent> {
    let blocks = message.pointer("/message/content")?.as_array()?;
    if blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
    {
        return Some(RunnerEvent::ToolCall);
    }
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<String>();
    Some(RunnerEvent::Assistant(text))
}

#[cfg(test)]
#[path = "personal_process_tests.rs"]
mod tests;
