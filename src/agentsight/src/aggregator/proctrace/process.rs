//! Aggregated process data structure
//!
//! Defines the `AggregatedProcess` structure for representing a complete
//! process lifecycle with exec, stdout, stderr, and exit events.

use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, next_flow_id, ns_to_us};

/// Aggregated process data for a specific PID
#[derive(Debug, Clone)]
pub struct AggregatedProcess {
    /// Process ID
    pub pid: u32,
    /// Parent PID
    pub ppid: u32,
    /// Parent TID (thread ID that spawned this process)
    pub ptid: u32,
    /// Thread ID
    pub tid: u32,
    /// Process name
    pub comm: String,
    /// Executable filename (from exec event)
    pub filename: Option<String>,
    /// Command arguments (from exec event)
    pub args: Option<String>,
    /// Collected stdout data
    pub stdout_data: Vec<u8>,
    /// Collected stderr data
    pub stderr_data: Vec<u8>,
    /// Whether this aggregation is complete (process exited)
    pub is_complete: bool,
    /// First timestamp when this process was seen (nanoseconds)
    pub start_timestamp_ns: u64,
    /// Last timestamp when data was added
    pub end_timestamp_ns: u64,
    /// Session ID read from process environment at exec time
    pub session_id: Option<String>,
}

impl AggregatedProcess {
    /// Create a new aggregated process
    pub fn new(pid: u32, tid: u32, ppid: u32, ptid: u32, comm: String, timestamp_ns: u64) -> Self {
        let session_id = read_session_env(pid);
        AggregatedProcess {
            pid,
            ppid,
            ptid,
            tid,
            comm,
            filename: None,
            args: None,
            stdout_data: Vec::new(),
            stderr_data: Vec::new(),
            is_complete: false,
            start_timestamp_ns: timestamp_ns,
            end_timestamp_ns: timestamp_ns,
            session_id,
        }
    }

    /// Add exec event data
    ///
    /// A process can execve multiple times (e.g., bash exec-ing into sleep).
    /// We preserve the first exec's args (the user-initiated command) and mark
    /// subsequent execs with "..." to indicate truncation. The filename is
    /// updated to reflect the final exec state for compatibility.
    pub fn add_exec(&mut self, filename: String, args: String, timestamp_ns: u64) {
        if self.filename.is_none() {
            // First exec: record complete information
            self.filename = Some(filename);
            self.args = Some(args);
        } else {
            // Subsequent exec: mark that additional execs occurred, but preserve
            // the first args (the original command matters most for correlation)
            if let Some(ref mut existing_args) = self.args {
                if !existing_args.ends_with(" ...") {
                    existing_args.push_str(" ...");
                }
            }
            // Update filename to reflect final state (for backward compatibility)
            self.filename = Some(filename);
        }
        self.end_timestamp_ns = timestamp_ns;
    }

    /// Add stdout data
    pub fn add_stdout(&mut self, data: &[u8], timestamp_ns: u64) {
        self.stdout_data.extend_from_slice(data);
        self.end_timestamp_ns = timestamp_ns;
    }

    /// Add stderr data
    pub fn add_stderr(&mut self, data: &[u8], timestamp_ns: u64) {
        self.stderr_data.extend_from_slice(data);
        self.end_timestamp_ns = timestamp_ns;
    }

    /// Mark this aggregation as complete (process exited)
    pub fn mark_complete(&mut self, timestamp_ns: u64) {
        self.is_complete = true;
        self.end_timestamp_ns = timestamp_ns;
    }

    /// Get stdout data as string (lossy conversion)
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout_data).into_owned()
    }

    /// Get stderr data as string (lossy conversion)
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr_data).into_owned()
    }

    /// Get the duration in nanoseconds
    pub fn duration_ns(&self) -> u64 {
        self.end_timestamp_ns
            .saturating_sub(self.start_timestamp_ns)
    }

    /// Get total stdout data size
    pub fn stdout_size(&self) -> usize {
        self.stdout_data.len()
    }

    /// Get total stderr data size
    pub fn stderr_size(&self) -> usize {
        self.stderr_data.len()
    }

    /// Build event name with stdout preview (max 50 chars)
    fn build_event_name(&self, stdout_str: &str, stderr_str: &str) -> String {
        let stdout_preview = if stdout_str.chars().count() > 50 {
            let truncated: String = stdout_str.chars().take(50).collect();
            format!("{}...", truncated.trim())
        } else if !stdout_str.is_empty() {
            stdout_str.trim().to_string()
        } else {
            String::new()
        };

        match (stdout_preview.is_empty(), stderr_str.is_empty()) {
            (false, false) => format!(
                "process: {} | stdout: {} | stderr: {}",
                self.comm,
                stdout_preview,
                self.stderr_size()
            ),
            (false, true) => format!("process: {} | stdout: {}", self.comm, stdout_preview),
            (true, false) => format!("process: {} | stderr: {}", self.comm, self.stderr_size()),
            (true, true) => format!("process: {}", self.comm),
        }
    }
}

impl ToChromeTraceEvent for AggregatedProcess {
    /// Convert to Chrome Trace Events with fork-process flow association
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        let mut events = Vec::new();
        let flow_id = next_flow_id();
        let ts_us = ns_to_us(self.start_timestamp_ns);
        let has_parent = self.ppid != 0 && self.ptid != 0;

        // 1. Fork event in parent process (if valid parent info)
        if has_parent {
            let fork_args = serde_json::json!({
                "child_pid": self.pid,
                "child_tid": self.tid,
                "child_comm": &self.comm,
                "child_filename": self.filename,
                "child_args": self.args,
            });

            let fork_event = ChromeTraceEvent::complete(
                format!("fork: {}", self.comm),
                "process.fork",
                self.ppid,
                self.ptid as u64,
                ts_us,
                10_000, // 10ms duration
            )
            .with_trace_args_value(fork_args);
            events.push(fork_event);
        }

        // 2. Child process lifecycle event
        let stdout_str = self.stdout_string();
        let stderr_str = self.stderr_string();

        // Build event name with stdout preview (max 50 chars)
        let name = self.build_event_name(&stdout_str, &stderr_str);

        let args = serde_json::json!({
            "pid": self.pid,
            "ppid": self.ppid,
            "ptid": self.ptid,
            "tid": self.tid,
            "comm": &self.comm,
            "is_complete": self.is_complete,
            "stdout_size": self.stdout_size(),
            "stderr_size": self.stderr_size(),
            "flow_id": flow_id,
            "filename": self.filename,
            "args": self.args,
            "stdout": if stdout_str.is_empty() { None } else { Some(&stdout_str) },
            "stderr": if stderr_str.is_empty() { None } else { Some(&stderr_str) },
        });

        let lifecycle_event = ChromeTraceEvent::complete(
            name,
            "process_lifecycle",
            self.pid,
            self.tid as u64,
            ts_us,
            ns_to_us(self.duration_ns()),
        )
        .with_trace_args_value(args);
        events.push(lifecycle_event);

        // 3. Create flow events linking parent fork to child lifecycle
        if has_parent {
            let (flow_start, flow_end) = ChromeTraceEvent::flow_from_events_with_id(
                events.first().unwrap(),
                events.last().unwrap(),
                flow_id,
            );
            events.push(flow_start);
            events.push(flow_end);
        }

        events
    }
}

const SESSION_ENV_VARS: &[&str] = &[
    "COSH_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "HERMES_SESSION_ID",
    "AGENT_SEC_SESSION_ID",
];

fn read_session_env(pid: u32) -> Option<String> {
    let data = match std::fs::read(crate::utils::procfs::proc_pid_entry(pid, "environ")) {
        Ok(d) => d,
        Err(e) => {
            log::debug!("read_session_env({pid}): {e}");
            return None;
        }
    };
    parse_session_from_environ(&data)
}

fn parse_session_from_environ(data: &[u8]) -> Option<String> {
    for entry in data.split(|&b| b == 0) {
        if let Ok(s) = std::str::from_utf8(entry) {
            for var in SESSION_ENV_VARS {
                if let Some(val) = s.strip_prefix(var).and_then(|rest| rest.strip_prefix('=')) {
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_exec_single() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "test".to_string(), 1000);
        proc.add_exec("bash".to_string(), "bash -lc 'echo test'".to_string(), 2000);

        assert_eq!(proc.filename, Some("bash".to_string()));
        assert_eq!(proc.args, Some("bash -lc 'echo test'".to_string()));
        // Single exec: no "..." suffix
        assert!(!proc.args.as_ref().unwrap().ends_with(" ..."));
    }

    #[test]
    fn test_add_exec_multiple_preserves_first_args() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "test".to_string(), 1000);

        // First exec
        proc.add_exec(
            "bash".to_string(),
            "bash -lc 'echo test && sleep 1'".to_string(),
            2000,
        );
        assert_eq!(
            proc.args,
            Some("bash -lc 'echo test && sleep 1'".to_string())
        );

        // Second exec (bash exec-ing into sleep)
        proc.add_exec("sleep".to_string(), "sleep 1".to_string(), 3000);

        // Args should preserve first command with "..." marker
        assert_eq!(
            proc.args,
            Some("bash -lc 'echo test && sleep 1' ...".to_string())
        );
        // Filename should be updated to final state
        assert_eq!(proc.filename, Some("sleep".to_string()));
    }

    #[test]
    fn test_add_exec_multiple_appends_marker_once() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "test".to_string(), 1000);

        proc.add_exec("bash".to_string(), "bash script.sh".to_string(), 2000);
        proc.add_exec("cmd1".to_string(), "cmd1 arg".to_string(), 3000);
        proc.add_exec("cmd2".to_string(), "cmd2 arg".to_string(), 4000);

        // "..." should only appear once, not multiple times
        let args = proc.args.unwrap();
        assert_eq!(args, "bash script.sh ...");
        assert_eq!(args.matches(" ...").count(), 1);
    }

    #[test]
    fn test_add_exec_empty_first_args() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "test".to_string(), 1000);

        proc.add_exec("bash".to_string(), "".to_string(), 2000);
        proc.add_exec("sleep".to_string(), "sleep 1".to_string(), 3000);

        // Empty first args should have "..." appended
        assert_eq!(proc.args, Some(" ...".to_string()));
    }

    #[test]
    fn test_parse_session_claude_code() {
        let env = b"HOME=/root\0CLAUDE_CODE_SESSION_ID=abc-123\0TERM=xterm\0";
        assert_eq!(parse_session_from_environ(env), Some("abc-123".to_string()));
    }

    #[test]
    fn test_parse_session_hermes() {
        let env = b"PATH=/usr/bin\0HERMES_SESSION_ID=hermes-42\0";
        assert_eq!(
            parse_session_from_environ(env),
            Some("hermes-42".to_string())
        );
    }

    #[test]
    fn test_parse_session_priority_first_match_wins() {
        let env = b"CLAUDE_CODE_SESSION_ID=first\0HERMES_SESSION_ID=second\0";
        assert_eq!(parse_session_from_environ(env), Some("first".to_string()));
    }

    #[test]
    fn test_parse_session_empty_environ() {
        assert_eq!(parse_session_from_environ(b""), None);
    }

    #[test]
    fn test_parse_session_no_session_vars() {
        let env = b"HOME=/root\0PATH=/usr/bin\0TERM=xterm\0";
        assert_eq!(parse_session_from_environ(env), None);
    }

    #[test]
    fn test_parse_session_empty_value_rejected() {
        let env = b"CLAUDE_CODE_SESSION_ID=\0OTHER=val\0";
        assert_eq!(parse_session_from_environ(env), None);
    }

    #[test]
    fn test_parse_session_prefix_collision() {
        // CLAUDE_CODE_SESSION_ID_V2 must NOT match CLAUDE_CODE_SESSION_ID
        let env = b"CLAUDE_CODE_SESSION_ID_V2=wrong\0";
        assert_eq!(parse_session_from_environ(env), None);
    }

    #[test]
    fn test_parse_session_non_utf8_skipped() {
        let mut env = Vec::new();
        env.extend_from_slice(b"BROKEN=");
        env.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8
        env.push(0);
        env.extend_from_slice(b"CLAUDE_CODE_SESSION_ID=valid-id");
        env.push(0);
        assert_eq!(
            parse_session_from_environ(&env),
            Some("valid-id".to_string())
        );
    }

    #[test]
    fn test_parse_session_no_trailing_null() {
        // /proc/pid/environ may not end with null
        let env = b"CLAUDE_CODE_SESSION_ID=no-trailing";
        assert_eq!(
            parse_session_from_environ(env),
            Some("no-trailing".to_string())
        );
    }
}
