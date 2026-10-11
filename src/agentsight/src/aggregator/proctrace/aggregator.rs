//! Process Event Aggregator
//!
//! Aggregates process events (exec, stdout, exit) by PID into complete process lifecycles.

use super::process::AggregatedProcess;
use crate::parser::proctrace::{ParsedProcEvent, ProcEventType};
use crate::probes::proctrace::VariableEvent;
use std::collections::HashMap;

/// Process Event Aggregator - aggregates process events by PID
#[derive(Debug, Clone)]
pub struct ProcessEventAggregator {
    /// Map of PID to aggregated process data
    pub aggregates: HashMap<u32, AggregatedProcess>,
    /// pid → session_id map for ppid-chain propagation.
    /// Seeded by direct `<procfs root>/{pid}/environ` reads; children that fail
    /// the direct read inherit from their parent via ppid lookup.
    session_map: HashMap<u32, String>,
}

impl ProcessEventAggregator {
    /// Create a new aggregator
    pub fn new() -> Self {
        ProcessEventAggregator {
            aggregates: HashMap::new(),
            session_map: HashMap::new(),
        }
    }

    /// Look up session_id for a pid: try ppid in session_map.
    fn lookup_parent_session(&self, ppid: u32) -> Option<String> {
        self.session_map.get(&ppid).cloned()
    }

    /// Process a single variable-length event
    ///
    /// Returns Some(AggregatedProcess) if this event completes an aggregation (process exited),
    /// None otherwise.
    pub fn process_event(&mut self, event: &VariableEvent) -> Option<AggregatedProcess> {
        match event {
            VariableEvent::Exec {
                header,
                filename,
                args,
            } => {
                let pid = header.pid;
                let ppid = header.ppid;
                let is_new = !self.aggregates.contains_key(&pid);
                let parent_session = if is_new {
                    self.lookup_parent_session(ppid)
                } else {
                    None
                };
                let aggregated = self.aggregates.entry(pid).or_insert_with(|| {
                    AggregatedProcess::new(
                        pid,
                        header.tid,
                        ppid,
                        header.ptid,
                        event.comm_str(),
                        header.timestamp_ns,
                    )
                });
                aggregated.add_exec(filename.clone(), args.clone(), header.timestamp_ns);
                if is_new {
                    if aggregated.session_id.is_none() {
                        aggregated.session_id = parent_session;
                    }
                    if let Some(sid) = aggregated.session_id.clone() {
                        self.session_map.insert(pid, sid);
                    }
                }
                None
            }
            VariableEvent::Stdout {
                header,
                fd,
                payload,
            } => {
                let pid = header.pid;
                if let Some(aggregated) = self.aggregates.get_mut(&pid) {
                    if *fd == 2 {
                        aggregated.add_stderr(payload, header.timestamp_ns);
                    } else {
                        aggregated.add_stdout(payload, header.timestamp_ns);
                    }
                }
                None
            }
            VariableEvent::Exit { header, .. } => {
                let pid = header.pid;
                self.session_map.remove(&pid);
                if let Some(mut aggregated) = self.aggregates.remove(&pid) {
                    aggregated.mark_complete(header.timestamp_ns);
                    Some(aggregated)
                } else {
                    None
                }
            }
            VariableEvent::Unknown(_) => None,
        }
    }

    /// Process multiple events
    pub fn process_events(&mut self, events: &[VariableEvent]) -> Vec<AggregatedProcess> {
        events
            .iter()
            .filter_map(|e| self.process_event(e))
            .collect()
    }

    /// Process a parsed process event
    ///
    /// Returns Some(AggregatedProcess) if this event completes an aggregation (process exited),
    /// None otherwise.
    pub fn process_parsed_event(&mut self, event: &ParsedProcEvent) -> Option<AggregatedProcess> {
        match event.event_type {
            ProcEventType::Exec => {
                let pid = event.pid;
                let ppid = event.ppid;
                let is_new = !self.aggregates.contains_key(&pid);
                let parent_session = if is_new {
                    self.lookup_parent_session(ppid)
                } else {
                    None
                };
                let aggregated = self.aggregates.entry(pid).or_insert_with(|| {
                    AggregatedProcess::new(
                        pid,
                        event.tid,
                        ppid,
                        event.ptid,
                        event.comm.clone(),
                        event.timestamp_ns,
                    )
                });
                // The probe reports the executable path; `comm` is only the
                // 16-byte task name. Substituting it here made every audit
                // record and chrome-trace `filename` read `bash` where the
                // process actually ran `/bin/bash`, and skipping the call for
                // an args-less exec left both fields unset entirely.
                let filename = event
                    .filename
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| event.comm.clone());
                aggregated.add_exec(
                    filename,
                    event.args.clone().unwrap_or_default(),
                    event.timestamp_ns,
                );
                if is_new {
                    if aggregated.session_id.is_none() {
                        aggregated.session_id = parent_session;
                    }
                    if let Some(sid) = aggregated.session_id.clone() {
                        self.session_map.insert(pid, sid);
                    }
                }
                None
            }
            ProcEventType::Stdout => {
                if let Some(aggregated) = self.aggregates.get_mut(&event.pid) {
                    if let Some(ref data) = event.stdout_data {
                        // fd 2 is stderr; fd 1 (and any other descriptor)
                        // keeps the legacy stdout behaviour.
                        if event.fd == Some(2) {
                            aggregated.add_stderr(data.as_bytes(), event.timestamp_ns);
                        } else {
                            aggregated.add_stdout(data.as_bytes(), event.timestamp_ns);
                        }
                    }
                }
                None
            }
            ProcEventType::Exit => {
                self.session_map.remove(&event.pid);
                if let Some(mut aggregated) = self.aggregates.remove(&event.pid) {
                    aggregated.mark_complete(event.timestamp_ns);
                    Some(aggregated)
                } else {
                    None
                }
            }
        }
    }

    /// Get all incomplete aggregations (running processes)
    pub fn get_incomplete(&self) -> Vec<&AggregatedProcess> {
        self.aggregates
            .values()
            .filter(|agg| !agg.is_complete)
            .collect()
    }

    /// Clear all aggregations
    pub fn clear(&mut self) {
        self.aggregates.clear();
        self.session_map.clear();
    }

    /// Check if there are any pending aggregations
    pub fn has_pending(&self) -> bool {
        !self.aggregates.is_empty()
    }

    /// Get the number of pending aggregations
    pub fn pending_count(&self) -> usize {
        self.aggregates.len()
    }
}

impl Default for ProcessEventAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::proctrace::{ParsedProcEvent, ProcEventType};

    fn exec_event(pid: u32, ppid: u32, comm: &str, args: &str, ts: u64) -> ParsedProcEvent {
        ParsedProcEvent {
            event_type: ProcEventType::Exec,
            pid,
            tid: pid,
            ppid,
            ptid: ppid,
            comm: comm.to_string(),
            filename: None,
            timestamp_ns: ts,
            args: Some(args.to_string()),
            stdout_data: None,
            fd: None,
        }
    }

    fn exit_event(pid: u32, ts: u64) -> ParsedProcEvent {
        ParsedProcEvent {
            event_type: ProcEventType::Exit,
            pid,
            tid: pid,
            ppid: 0,
            ptid: 0,
            comm: String::new(),
            filename: None,
            timestamp_ns: ts,
            args: None,
            stdout_data: None,
            fd: None,
        }
    }

    /// The parsed path used `comm` as the executable name and skipped the exec
    /// entirely when the probe reported no args, so the audit record's
    /// `filename` (and the chrome-trace one) said `bash` where the process ran
    /// `/bin/bash` — or stayed empty.
    #[test]
    fn parsed_exec_records_the_probe_filename() {
        let mut agg = ProcessEventAggregator::new();
        let mut event = exec_event(200, 100, "bash", "echo hi", 1000);
        event.filename = Some("/usr/bin/bash".to_string());

        agg.process_parsed_event(&event);

        let proc = agg.aggregates.get(&200).unwrap();
        assert_eq!(
            proc.filename.as_deref(),
            Some("/usr/bin/bash"),
            "the executable path the probe reported must survive the parsed path"
        );
    }

    /// An exec whose args buffer yielded nothing still happened: the filename
    /// must be recorded (from `comm`, the only identity left) instead of being
    /// dropped along with the exec.
    #[test]
    fn parsed_exec_without_args_still_records_the_execution() {
        let mut agg = ProcessEventAggregator::new();
        let mut event = exec_event(200, 100, "bash", "", 1000);
        event.args = None;

        agg.process_parsed_event(&event);

        let proc = agg.aggregates.get(&200).unwrap();
        assert_eq!(proc.filename.as_deref(), Some("bash"));
        assert_eq!(proc.args.as_deref(), Some(""));
    }

    #[test]
    fn test_ppid_inherits_session_from_parent() {
        let mut agg = ProcessEventAggregator::new();
        agg.session_map.insert(100, "sess-abc".to_string());

        agg.process_parsed_event(&exec_event(200, 100, "bash", "echo hi", 1000));

        let proc = agg.aggregates.get(&200).unwrap();
        assert_eq!(proc.session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn test_ppid_chain_propagates_through_grandchild() {
        let mut agg = ProcessEventAggregator::new();
        agg.session_map.insert(100, "sess-abc".to_string());

        agg.process_parsed_event(&exec_event(200, 100, "bash", "echo hi", 1000));
        agg.process_parsed_event(&exec_event(300, 200, "date", "date +%s", 2000));

        let grandchild = agg.aggregates.get(&300).unwrap();
        assert_eq!(grandchild.session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn test_exit_cleans_session_map() {
        let mut agg = ProcessEventAggregator::new();
        agg.session_map.insert(100, "sess-abc".to_string());

        agg.process_parsed_event(&exec_event(200, 100, "bash", "echo hi", 1000));
        assert!(agg.session_map.contains_key(&200));

        agg.process_parsed_event(&exit_event(200, 3000));
        assert!(!agg.session_map.contains_key(&200));
    }

    #[test]
    fn test_no_session_when_ppid_not_in_map() {
        let mut agg = ProcessEventAggregator::new();

        agg.process_parsed_event(&exec_event(200, 999, "bash", "echo hi", 1000));

        let proc = agg.aggregates.get(&200).unwrap();
        assert_eq!(proc.session_id, None);
    }

    #[test]
    fn test_pid_recycling_does_not_cross_contaminate() {
        let mut agg = ProcessEventAggregator::new();
        agg.session_map.insert(100, "sess-abc".to_string());

        // PID 200 exec as child of 100 → inherits sess-abc
        agg.process_parsed_event(&exec_event(200, 100, "bash", "echo hi", 1000));
        assert_eq!(
            agg.aggregates.get(&200).unwrap().session_id.as_deref(),
            Some("sess-abc")
        );

        // PID 200 exits → cleaned from session_map
        agg.process_parsed_event(&exit_event(200, 2000));
        assert!(!agg.session_map.contains_key(&200));

        // PID 200 recycled as child of 999 (no session) → must NOT inherit old sess-abc
        agg.process_parsed_event(&exec_event(200, 999, "ls", "ls -la", 3000));
        let recycled = agg.aggregates.get(&200).unwrap();
        assert_eq!(recycled.session_id, None);
    }

    /// fd 2 output must land in the stderr buffer. The parser carries the fd
    /// and the aggregator branches on it; otherwise every stderr chunk is
    /// appended to stdout, `stdout_size` is inflated and the chrome-trace
    /// `stderr` arg stays `None`.
    #[test]
    fn test_parsed_stderr_event_lands_in_stderr_buffer() {
        let mut agg = ProcessEventAggregator::new();
        agg.process_parsed_event(&exec_event(200, 100, "bash", "bash -c 'boom'", 1000));

        let stderr = ParsedProcEvent {
            event_type: ProcEventType::Stdout,
            pid: 200,
            tid: 200,
            ppid: 100,
            ptid: 100,
            comm: "bash".to_string(),
            timestamp_ns: 2000,
            args: None,
            stdout_data: Some("boom\n".to_string()),
            fd: Some(2),
            filename: None,
        };
        agg.process_parsed_event(&stderr);

        let proc = agg.aggregates.get(&200).expect("aggregate exists");
        assert!(
            proc.stderr_size() > 0,
            "fd 2 output must be stored as stderr"
        );
        assert_eq!(proc.stdout_size(), 0, "stderr must not leak into stdout");
    }

    #[test]
    fn test_clear_resets_session_map() {
        let mut agg = ProcessEventAggregator::new();
        agg.session_map.insert(100, "sess-abc".to_string());
        agg.clear();
        assert!(agg.session_map.is_empty());
    }

    fn stdout_event(pid: u32, ts: u64, data: &str) -> ParsedProcEvent {
        ParsedProcEvent {
            event_type: ProcEventType::Stdout,
            pid,
            tid: pid,
            ppid: 0,
            ptid: 0,
            comm: "chatty".to_string(),
            timestamp_ns: ts,
            args: None,
            stdout_data: Some(data.to_string()),
            fd: Some(1),
            filename: None,
        }
    }

    #[test]
    fn test_process_stdout_retention_is_bounded() {
        // Regression: stdout/stderr accumulated for the whole process lifetime
        // (and the whole buffer is serialised into the chrome trace args), so a
        // long-lived chatty process retained everything.
        const EXPECTED_CAP: usize = 64 * 1024;
        let mut agg = ProcessEventAggregator::new();
        agg.process_parsed_event(&exec_event(4242, 0, "chatty", "chatty", 1000));

        let chunk = "x".repeat(32 * 1024);
        for i in 0..16 {
            agg.process_parsed_event(&stdout_event(4242, 2000 + i, &chunk));
        }

        let proc = agg.aggregates.get(&4242).unwrap();
        assert!(
            proc.stdout_size() <= EXPECTED_CAP,
            "stdout retention must be capped: {} bytes > {}",
            proc.stdout_size(),
            EXPECTED_CAP
        );
    }
}
