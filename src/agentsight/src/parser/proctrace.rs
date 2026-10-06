//! Process trace event parser
//!
//! Parses process events (execve, stdout, exit) from VariableEvent.

use crate::chrome_trace::{ChromeTraceEvent, TraceArgs, ns_to_us};
use crate::probes::proctrace::VariableEvent;
use serde_json::json;

/// Parser for process events
pub struct ProcTraceParser;

/// Process event type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcEventType {
    /// Process execution (execve)
    Exec,
    /// Stdout output
    Stdout,
    /// Process exit
    Exit,
}

/// Parsed process event
#[derive(Debug, Clone)]
pub struct ParsedProcEvent {
    /// Event type
    pub event_type: ProcEventType,
    /// Process ID
    pub pid: u32,
    /// Thread ID
    pub tid: u32,
    /// Parent PID (for exec events)
    pub ppid: u32,
    /// Parent TID (thread ID that spawned this process)
    pub ptid: u32,
    /// Process name
    pub comm: String,
    /// Executable path the probe reported for an exec event, when it read one.
    ///
    /// Distinct from [`Self::comm`], which is the 16-byte task name: the audit
    /// record's `filename` and the chrome-trace `filename` argument both mean
    /// "the executable that ran", so they need the path.
    pub filename: Option<String>,
    /// Timestamp in nanoseconds
    pub timestamp_ns: u64,
    /// Command arguments (for exec events)
    pub args: Option<String>,
    /// Stdout data (for stdout events)
    pub stdout_data: Option<String>,
    /// Output file descriptor for stdout events (1 = stdout, 2 = stderr).
    /// `None` for event types that carry no descriptor (exec, exit).
    pub fd: Option<u32>,
}

impl ProcTraceParser {
    /// Parse a variable-length process event
    pub fn parse_variable(event: &VariableEvent) -> Option<ParsedProcEvent> {
        match event {
            VariableEvent::Exec {
                header,
                filename,
                args,
            } => Some(ParsedProcEvent {
                event_type: ProcEventType::Exec,
                pid: header.pid,
                tid: header.tid,
                ppid: header.ppid,
                ptid: header.ptid,
                comm: event.comm_str(),
                filename: Some(filename.clone()).filter(|s| !s.is_empty()),
                timestamp_ns: header.timestamp_ns,
                args: Some(args.clone()).filter(|s| !s.is_empty()),
                stdout_data: None,
                fd: None,
            }),
            VariableEvent::Stdout {
                header,
                fd,
                payload,
            } => {
                // A stdout chunk is arbitrary process bytes: a tool printing
                // binary, or a multibyte character split across two chunks,
                // has no valid UTF-8. Dropping the event for that lost the
                // bytes entirely — the aggregation layer is byte-oriented and
                // converts lossily at read time (`stdout_string`), so a lossy
                // decode here keeps the same contract and the event.
                let stdout_data = Some(String::from_utf8_lossy(payload).into_owned());
                Some(ParsedProcEvent {
                    event_type: ProcEventType::Stdout,
                    pid: header.pid,
                    tid: header.tid,
                    ppid: header.ppid,
                    ptid: header.ptid,
                    comm: event.comm_str(),
                    filename: None,
                    timestamp_ns: header.timestamp_ns,
                    args: None,
                    stdout_data,
                    fd: Some(*fd),
                })
            }
            VariableEvent::Exit { header, .. } => Some(ParsedProcEvent {
                event_type: ProcEventType::Exit,
                pid: header.pid,
                tid: header.tid,
                ppid: header.ppid,
                ptid: header.ptid,
                comm: event.comm_str(),
                filename: None,
                timestamp_ns: header.timestamp_ns,
                args: None,
                stdout_data: None,
                fd: None,
            }),
            VariableEvent::Unknown(_) => None,
        }
    }

    /// Convert a variable-length process event to Chrome Trace Event format
    pub fn to_chrome_trace_event(event: &VariableEvent) -> Option<ChromeTraceEvent> {
        let parsed = Self::parse_variable(event)?;
        let ts_us = ns_to_us(parsed.timestamp_ns);

        match parsed.event_type {
            ProcEventType::Exec => {
                let name = format!("exec: {}", parsed.comm);
                let args = json!({
                    "pid": parsed.pid,
                    "ppid": parsed.ppid,
                    "comm": parsed.comm,
                    "args": parsed.args,
                });

                Some(ChromeTraceEvent {
                    name,
                    cat: "process.exec".to_string(),
                    ph: "i".to_string(),
                    ts: ts_us,
                    dur: None,
                    pid: parsed.pid,
                    tid: parsed.tid as u64,
                    args: Some(args),
                    id: None,
                    bp: None,
                })
            }
            ProcEventType::Stdout => {
                let data = parsed.stdout_data?;
                let display_data = if data.len() > 100 {
                    format!("{}...", boundary_preview(&data, 100))
                } else {
                    data.clone()
                };

                Some(ChromeTraceEvent {
                    name: format!("stdout: {}", display_data.trim()),
                    cat: "process.stdout".to_string(),
                    ph: "i".to_string(),
                    ts: ts_us,
                    dur: None,
                    pid: parsed.pid,
                    tid: parsed.tid as u64,
                    args: Some(json!({
                        "pid": parsed.pid,
                        "comm": parsed.comm,
                        "data": data,
                        "len": data.len(),
                    })),
                    id: None,
                    bp: None,
                })
            }
            ProcEventType::Exit => Some(ChromeTraceEvent {
                name: format!("exit: {}", parsed.comm),
                cat: "process.exit".to_string(),
                ph: "i".to_string(),
                ts: ts_us,
                dur: None,
                pid: parsed.pid,
                tid: parsed.tid as u64,
                args: Some(json!({
                    "pid": parsed.pid,
                    "comm": parsed.comm,
                })),
                id: None,
                bp: None,
            }),
        }
    }

    /// Parse multiple variable-length events
    pub fn parse_events(events: &[VariableEvent]) -> Vec<ParsedProcEvent> {
        events.iter().filter_map(Self::parse_variable).collect()
    }

    /// Convert multiple variable-length events to Chrome Trace Events
    pub fn to_chrome_trace_events(events: &[VariableEvent]) -> Vec<ChromeTraceEvent> {
        events
            .iter()
            .filter_map(Self::to_chrome_trace_event)
            .collect()
    }
}

impl TraceArgs for ParsedProcEvent {
    fn to_trace_args(&self) -> serde_json::Value {
        let mut args = serde_json::Map::new();

        // Common fields
        args.insert("pid".to_string(), json!(self.pid));
        args.insert("comm".to_string(), json!(&self.comm));

        // Event type specific fields
        match self.event_type {
            ProcEventType::Exec => {
                args.insert("ppid".to_string(), json!(self.ppid));
                args.insert("ptid".to_string(), json!(self.ptid));
                if let Some(ref cmd_args) = self.args {
                    args.insert("args".to_string(), json!(cmd_args));
                }
            }
            ProcEventType::Stdout => {
                if let Some(ref data) = self.stdout_data {
                    args.insert("len".to_string(), json!(data.len()));

                    // Add data preview (truncated)
                    let preview = if data.len() > 200 {
                        format!(
                            "{}... ({} bytes total)",
                            boundary_preview(data, 200),
                            data.len()
                        )
                    } else {
                        data.clone()
                    };
                    args.insert("data".to_string(), json!(preview));
                }
            }
            ProcEventType::Exit => {
                // Exit event has minimal args
            }
        }

        serde_json::Value::Object(args)
    }
}

/// Shorten a preview to at most `max_bytes`, cutting on a character boundary
/// so multi-byte stdout text is never split mid-character.
fn boundary_preview(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl ParsedProcEvent {
    /// Convert to Chrome Trace Event
    pub fn to_chrome_trace_event(&self) -> ChromeTraceEvent {
        let ts_us = ns_to_us(self.timestamp_ns);
        let name = match self.event_type {
            ProcEventType::Exec => format!("exec: {}", self.comm),
            ProcEventType::Stdout => {
                let data = self.stdout_data.as_ref().cloned().unwrap_or_default();
                let display_data = if data.len() > 100 {
                    format!("{}...", boundary_preview(&data, 100))
                } else {
                    data.clone()
                };
                format!("stdout: {}", display_data.trim())
            }
            ProcEventType::Exit => format!("exit: {}", self.comm),
        };

        let cat = match self.event_type {
            ProcEventType::Exec => "process.exec",
            ProcEventType::Stdout => "process.stdout",
            ProcEventType::Exit => "process.exit",
        };

        ChromeTraceEvent {
            name,
            cat: cat.to_string(),
            ph: "i".to_string(),
            ts: ts_us,
            dur: None,
            pid: self.pid,
            tid: self.tid as u64,
            args: Some(self.to_trace_args()),
            id: None,
            bp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout_event(data: &str) -> ParsedProcEvent {
        ParsedProcEvent {
            event_type: ProcEventType::Stdout,
            pid: 1,
            tid: 1,
            ppid: 0,
            ptid: 0,
            comm: "proc".to_string(),
            filename: None,
            timestamp_ns: 0,
            args: None,
            stdout_data: Some(data.to_string()),
            fd: Some(1),
        }
    }

    /// A stdout chunk is arbitrary process bytes; a non-UTF-8 chunk used to be
    /// dropped outright, so its bytes never reached the aggregated stdout
    /// (which is byte-oriented and decodes lossily when read) nor the trace.
    #[test]
    fn non_utf8_stdout_chunk_is_kept() {
        // SAFETY: `proc_event_header` is a `#[repr(C)]` bindgen struct of
        // plain integers, so an all-zero bit pattern is a valid value. Only
        // the pid/tid fields are read from it.
        let header: crate::probes::proctrace::ProcEventHeader = unsafe { std::mem::zeroed() };
        let event = VariableEvent::Stdout {
            header,
            fd: 1,
            payload: vec![0xff, 0xfe, b'A'],
        };

        let parsed =
            ProcTraceParser::parse_variable(&event).expect("a stdout chunk must not be dropped");
        assert_eq!(parsed.event_type, ProcEventType::Stdout);
        let data = parsed.stdout_data.expect("stdout data");
        assert!(
            data.ends_with('A'),
            "the decodable tail must survive: {data:?}"
        );
    }

    #[test]
    fn stdout_event_name_cuts_on_char_boundary() {
        // 99 ASCII bytes then a 3-byte character straddling offset 100: the
        // chrome-trace event name sliced at byte 100, inside the character.
        let data = format!("{}中中", "a".repeat(99));
        let event = stdout_event(&data);
        let chrome = event.to_chrome_trace_event();
        assert!(chrome.name.starts_with("stdout: aaa"));
        assert!(chrome.name.ends_with("..."));
    }

    #[test]
    fn stdout_args_preview_cuts_on_char_boundary() {
        // 199 ASCII bytes then a 3-byte character straddling offset 200.
        let data = format!("{}中", "b".repeat(199));
        let event = stdout_event(&data);
        let args = event.to_trace_args();
        let preview = args["data"].as_str().expect("data preview");
        assert!(preview.ends_with("(202 bytes total)"));
        assert!(!preview.contains('中'));

        // Short data is untouched.
        assert_eq!(boundary_preview("hello", 100), "hello");
        assert!(!preview.contains('中'));
    }

    /// The parser must forward the probe's fd so the aggregator can route
    /// fd 2 output to the stderr buffer instead of stdout.
    #[test]
    fn stdout_parse_preserves_fd() {
        use crate::probes::proctrace::{PROCTRACE_EVENT_STDOUT, ProcEventHeader};

        let header = ProcEventHeader {
            source: 0,
            timestamp_ns: 1234,
            pid: 42,
            tid: 42,
            ppid: 1,
            ptid: 1,
            uid: 0,
            event_type: PROCTRACE_EVENT_STDOUT,
            data_len: 0,
            comm: [0; 16],
            cgroup_id: 0,
        };
        let event = VariableEvent::Stdout {
            header,
            fd: 2,
            payload: b"err\n".to_vec(),
        };

        let parsed = ProcTraceParser::parse_variable(&event).expect("stdout event parses");
        assert_eq!(parsed.fd, Some(2));
        assert_eq!(parsed.stdout_data.as_deref(), Some("err\n"));
    }
}
