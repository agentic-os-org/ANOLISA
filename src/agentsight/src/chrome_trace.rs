use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Global flow ID counter for generating unique flow identifiers
static FLOW_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Get the next flow ID (auto-incrementing)
pub fn next_flow_id() -> u64 {
    FLOW_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Trait for converting data to Chrome Trace Event args
///
/// Implement this trait for types that need to be serialized as args
/// in Chrome Trace Events for Perfetto visualization.
///
/// # Example
/// ```rust,ignore
/// impl TraceArgs for ParsedRequest {
///     fn to_trace_args(&self) -> serde_json::Value {
///         json!({
///             "method": self.method,
///             "path": self.path,
///         })
///     }
/// }
/// ```
pub trait TraceArgs {
    /// Convert the data to a JSON value for Chrome Trace Event args
    fn to_trace_args(&self) -> serde_json::Value;
}

/// Trait for converting data to complete Chrome Trace Events
///
/// Implement this trait for types that can be directly converted
/// to one or more Chrome Trace Events for Perfetto visualization.
///
/// # Example
/// ```rust,ignore
/// impl ToChromeTraceEvent for HttpPair {
///     fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
///         vec![ChromeTraceEvent {
///             name: format!("{} {}", self.request.method, self.request.path),
///             cat: "http".to_string(),
///             ph: "X".to_string(),
///             ts: ns_to_us(self.request_timestamp_ns),
///             dur: Some(ns_to_us(self.duration_ns())),
///             pid: self.connection_id.pid,
///             tid: self.connection_id.ssl_ptr,
///             args: Some(self.to_trace_args()),
///             id: Some(self.flow_id.to_string()),
///             bp: None,
///         }]
///     }
/// }
/// ```
pub trait ToChromeTraceEvent {
    /// Convert the data to one or more Chrome Trace Events
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent>;
}

/// Chrome Trace Event format for Perfetto visualization
/// See: https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKcasNAojs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChromeTraceEvent {
    /// Event name (displayed in the timeline)
    pub name: String,
    /// Event category (used for filtering and coloring)
    pub cat: String,
    /// Event type: B=begin, E=end, X=complete, i=instant, C=counter, s=start flow, f=flow step, t=end flow
    pub ph: String,
    /// Timestamp in microseconds
    pub ts: u64,
    /// Duration in microseconds (only for 'X' type events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dur: Option<u64>,
    /// Process ID
    pub pid: u32,
    /// Thread ID
    pub tid: u64,
    /// Additional event arguments
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// Flow ID for associating events (used with s/f/t phases)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// Binding point for flow events: "e" = end, "s" = start
    /// Used with ph="f" to specify where the arrow attaches
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bp: Option<String>,
}

impl ChromeTraceEvent {
    /// Create a new instant event
    pub fn instant(
        name: impl Into<String>,
        cat: impl Into<String>,
        pid: u32,
        tid: u64,
        ts_us: u64,
    ) -> Self {
        ChromeTraceEvent {
            name: name.into(),
            cat: cat.into(),
            ph: "i".to_string(),
            ts: ts_us,
            dur: None,
            pid,
            tid,
            args: None,
            id: None,
            bp: None,
        }
    }

    /// Create a new complete event (with duration)
    pub fn complete(
        name: impl Into<String>,
        cat: impl Into<String>,
        pid: u32,
        tid: u64,
        ts_us: u64,
        dur_us: u64,
    ) -> Self {
        ChromeTraceEvent {
            name: name.into(),
            cat: cat.into(),
            ph: "X".to_string(),
            ts: ts_us,
            dur: Some(dur_us),
            pid,
            tid,
            args: None,
            id: None,
            bp: None,
        }
    }

    /// Create a flow start event (s phase)
    /// This marks the beginning of a flow that can be visualized as an arrow in Perfetto
    pub fn flow_start(
        name: impl Into<String>,
        cat: impl Into<String>,
        pid: u32,
        tid: u64,
        ts_us: u64,
        flow_id: u64,
    ) -> Self {
        ChromeTraceEvent {
            name: name.into(),
            cat: cat.into(),
            ph: "s".to_string(),
            ts: ts_us,
            dur: None,
            pid,
            tid,
            args: None,
            id: Some(flow_id),
            bp: None,
        }
    }

    /// Create a flow end event (f phase with bp="e")
    /// This marks the end of a flow, connecting back to the start via flow_id
    /// Uses ph="f" with bp="e" for proper Perfetto arrow rendering
    pub fn flow_end(
        name: impl Into<String>,
        cat: impl Into<String>,
        pid: u32,
        tid: u64,
        ts_us: u64,
        flow_id: u64,
    ) -> Self {
        ChromeTraceEvent {
            name: name.into(),
            cat: cat.into(),
            ph: "f".to_string(), // Use 'f' instead of 't'
            ts: ts_us,
            dur: None,
            pid,
            tid,
            args: None,
            id: Some(flow_id),
            bp: Some("e".to_string()), // Binding point at end
        }
    }

    /// Create a flow step event (f phase)
    /// Used for intermediate steps in a flow
    pub fn flow_step(
        name: impl Into<String>,
        cat: impl Into<String>,
        pid: u32,
        tid: u64,
        ts_us: u64,
        flow_id: u64,
    ) -> Self {
        ChromeTraceEvent {
            name: name.into(),
            cat: cat.into(),
            ph: "f".to_string(),
            ts: ts_us,
            dur: None,
            pid,
            tid,
            args: None,
            id: Some(flow_id),
            bp: None,
        }
    }

    /// Create flow events connecting two Chrome Trace Events
    ///
    /// This generates a pair of flow events (s and f phases) that create
    /// an arrow in Perfetto from the start event to the end event.
    ///
    /// Flow events only carry the arrow connection, no args.
    /// The semantic information should be in the accompanying complete/instant events.
    ///
    /// Flow ID is automatically generated using a global atomic counter.
    ///
    /// # Arguments
    /// * `start` - The source event (arrow starts here)
    /// * `end` - The target event (arrow ends here)
    ///
    /// # Returns
    /// A tuple of (flow_start, flow_end, flow_id) events
    ///
    /// # Example
    /// ```rust,ignore
    /// let request_event = ChromeTraceEvent::complete("GET /api", "http", pid1, tid1, ts1, dur1);
    /// let response_event = ChromeTraceEvent::complete("200 OK", "http", pid2, tid2, ts2, dur2);
    /// let (flow_s, flow_f, flow_id) = ChromeTraceEvent::flow_from_events(&request_event, &response_event);
    /// ```
    pub fn flow_from_events(start: &ChromeTraceEvent, end: &ChromeTraceEvent) -> (Self, Self, u64) {
        let flow_id = next_flow_id();
        let (flow_start, flow_end) = Self::flow_from_events_with_id(start, end, flow_id);
        (flow_start, flow_end, flow_id)
    }

    /// Create flow events connecting two Chrome Trace Events with a given flow_id
    ///
    /// This generates a pair of flow events (s and f phases) that create
    /// an arrow in Perfetto from the start event to the end event.
    ///
    /// Flow events only carry the arrow connection, no args.
    /// The semantic information should be in the accompanying complete/instant events.
    ///
    /// # Arguments
    /// * `start` - The source event (arrow starts here)
    /// * `end` - The target event (arrow ends here)
    /// * `flow_id` - Unique identifier to link the flow events
    ///
    /// # Returns
    /// A tuple of (flow_start, flow_end) events
    pub fn flow_from_events_with_id(
        start: &ChromeTraceEvent,
        end: &ChromeTraceEvent,
        flow_id: u64,
    ) -> (Self, Self) {
        let flow_start = ChromeTraceEvent {
            name: "flow".to_string(),
            cat: "flow".to_string(),
            ph: "s".to_string(),
            ts: start.ts,
            dur: None,
            pid: start.pid,
            tid: start.tid,
            args: None,
            id: Some(flow_id),
            bp: None,
        };

        let flow_end = ChromeTraceEvent {
            name: "flow".to_string(),
            cat: "flow".to_string(),
            ph: "f".to_string(),
            ts: end.ts,
            dur: None,
            pid: end.pid,
            tid: end.tid,
            args: None,
            id: Some(flow_id),
            bp: Some("e".to_string()),
        };

        (flow_start, flow_end)
    }

    /// Add arguments to the event
    pub fn with_args(mut self, args: serde_json::Map<String, serde_json::Value>) -> Self {
        self.args = Some(serde_json::Value::Object(args));
        self
    }

    /// Add arguments from a JSON Value (must be an object)
    pub fn with_trace_args_value(mut self, args: serde_json::Value) -> Self {
        if let serde_json::Value::Object(map) = args {
            self.args = Some(serde_json::Value::Object(map));
        }
        self
    }

    /// Add a single argument
    pub fn with_arg(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        let mut args = match self.args.take() {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        args.insert(key.into(), value.into());
        self.args = Some(serde_json::Value::Object(args));
        self
    }

    /// Add arguments from a TraceArgs implementor
    ///
    /// # Example
    /// ```rust,ignore
    /// let event = ChromeTraceEvent::instant("HTTP Request", "http", pid, tid, ts)
    ///     .with_trace_args(&request);
    /// ```
    pub fn with_trace_args<T: TraceArgs>(mut self, args: &T) -> Self {
        let trace_args = args.to_trace_args();
        // Only set args if it's not null and not an empty object
        let should_set = match &trace_args {
            serde_json::Value::Null => false,
            serde_json::Value::Object(map) => !map.is_empty(),
            _ => true,
        };
        if should_set {
            self.args = Some(trace_args);
        }
        self
    }
}

/// Helper function to convert nanoseconds to microseconds
pub fn ns_to_us(ns: u64) -> u64 {
    ns / 1000
}

/// Helper function to format a trace file header
pub fn trace_file_header() -> &'static str {
    "[\n"
}

/// Helper function to format a trace file footer
pub fn trace_file_footer() -> &'static str {
    "\n]"
}

// ==================== Trace File Export ====================

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;

/// Chrome trace output file path (includes date and time to minute, plus pid).
///
/// The name carries the process id as well as the minute. Two `agentsight
/// trace` processes started in the same working directory within the same
/// minute used to resolve to the same name, and `init_trace_file`'s
/// `File::create` truncated the first one's trace — an entire run's events were
/// lost purely because of the clock. The comma bookkeeping in
/// `append_trace_event` is per-process too, so the two writers then produced
/// adjacent JSON objects with no separator, leaving `agentsight analyze-chatml
/// --chrome-trace` with a file it cannot parse.
fn trace_file_path() -> &'static std::path::PathBuf {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let datetime = chrono::Local::now().format("%Y-%m-%d_%H-%M");
        std::env::current_dir()
            .unwrap_or_default()
            .join(format!("trace-{datetime}-{}.json", std::process::id()))
    })
}

/// Write the trace file's opening bracket.
fn write_trace_header<W: Write>(writer: &mut W) -> std::io::Result<()> {
    writer.write_all(b"[\n")
}

/// Write the trace file's closing bracket.
fn write_trace_footer<W: Write>(writer: &mut W) -> std::io::Result<()> {
    writer.write_all(b"]\n")
}

/// Write one trace event as a JSON array element.
///
/// The whole element — separator, serialized event, newline — is assembled
/// first and handed to the writer as one `write_all`, so a failure cannot
/// strand a dangling comma in front of an element that never arrived, and
/// two writes from the same append cannot interleave. Serialization happens
/// before anything is written, so an event that cannot serialize writes
/// nothing instead of an empty array element. The `first_write` claim is
/// consumed only on success.
fn write_trace_event<W: Write>(
    writer: &mut W,
    event: &ChromeTraceEvent,
    first_write: &mut bool,
) -> std::io::Result<()> {
    let serialized = serde_json::to_string(event)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut element = String::with_capacity(serialized.len() + 2);
    if !*first_write {
        element.push(',');
    }
    element.push_str(&serialized);
    element.push('\n');
    writer.write_all(element.as_bytes())?;
    *first_write = false;
    Ok(())
}

/// Initialize trace file with opening bracket and register exit hook.
///
/// Every failure is logged: the chrome-trace export is explicitly enabled
/// via `AGENTSIGHT_CHROME_TRACE`, so an operator whose trace file cannot be
/// created (read-only working directory, deleted cwd) or whose header write
/// fails must be told the export produced nothing — a silent no-op leaves
/// `analyze-chatml --chrome-trace` looking for a file that was never born.
fn init_trace_file() {
    let path = trace_file_path();
    match std::fs::File::create(path) {
        Ok(mut file) => {
            if let Err(error) = write_trace_header(&mut file) {
                log::warn!(
                    "chrome-trace header write to {} failed: {error}; the trace file will not be valid JSON",
                    path.display()
                );
            }
        }
        Err(error) => {
            log::warn!(
                "chrome-trace file {} cannot be created: {error}; trace export is enabled but will produce nothing",
                path.display()
            );
        }
    }

    // Register exit hook to close the JSON array
    extern "C" fn close_trace_file() {
        let path = trace_file_path();
        match OpenOptions::new().append(true).open(path) {
            Ok(mut file) => {
                if let Err(error) = write_trace_footer(&mut file) {
                    log::warn!(
                        "chrome-trace footer write to {} failed: {error}; the trace file will not be valid JSON",
                        path.display()
                    );
                }
            }
            Err(error) => {
                // Only a warning when the file exists but cannot be reopened;
                // a file that was never created was already reported at init.
                if path.exists() {
                    log::warn!(
                        "chrome-trace file {} cannot be reopened for the footer: {error}",
                        path.display()
                    );
                }
            }
        }
    }
    unsafe {
        libc::atexit(close_trace_file);
    }
}

/// Track if first write (no comma needed)
static FIRST_WRITE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Append a trace event to the file.
///
/// Write failures are logged instead of swallowed, and a failed append
/// returns the first-write claim so the next event cannot strand a
/// separator after an element that never landed.
pub fn append_trace_event(event: &ChromeTraceEvent) {
    let path = trace_file_path();
    let Ok(mut file) = OpenOptions::new().append(true).create(false).open(path) else {
        log::warn!(
            "chrome-trace event dropped: {} cannot be opened; was the trace file removed while the process runs?",
            path.display()
        );
        return;
    };
    // Claim the first-write slot atomically (same swap the writer always
    // used); `write_trace_event` consumes it only on success, and a failure
    // releases the claim back so the next event cannot strand a separator
    // after an element that never landed.
    let mut is_first = FIRST_WRITE.swap(false, std::sync::atomic::Ordering::SeqCst);
    if let Err(error) = write_trace_event(&mut file, event, &mut is_first) {
        FIRST_WRITE.store(is_first, std::sync::atomic::Ordering::SeqCst);
        log::warn!("chrome-trace event write failed: {error}");
    }
}

/// Export trace events to file if enabled
///
/// This function checks if chrome trace export is enabled via environment variable
/// AGENTSIGHT_CHROME_TRACE, and if so, writes the events to trace.json.
pub fn export_trace_events<T: ToChromeTraceEvent>(result: &T) {
    if !crate::config::chrome_trace() {
        return;
    }

    // Initialize trace file once
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        init_trace_file();
    });

    for event in result.to_chrome_trace_events() {
        append_trace_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A writer that fails on every write, keeping what did land.
    #[derive(Default)]
    struct FailingWriter {
        written: Vec<u8>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full (fixture)"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that records everything and never fails.
    #[derive(Default)]
    struct VecWriter {
        written: Vec<u8>,
    }

    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sample_event(name: &str) -> ChromeTraceEvent {
        ChromeTraceEvent::instant(name, "test", 1, 2, 100)
    }

    #[test]
    fn trace_elements_are_single_writes_with_correct_separators() {
        // The element — separator, serialized event, newline — must leave the
        // writer as one write: a comma written in its own write_all could be
        // stranded in front of an event that never arrived.
        let mut writer = VecWriter::default();
        let mut first = true;
        write_trace_event(&mut writer, &sample_event("one"), &mut first).unwrap();
        assert!(!first, "sanity: the flag flips on success");
        write_trace_event(&mut writer, &sample_event("two"), &mut first).unwrap();
        assert!(!first);

        let text = String::from_utf8(writer.written).unwrap();
        let mut lines = text.split('\n');
        let first_line = lines.next().unwrap();
        let second_line = lines.next().unwrap();
        assert!(
            first_line.starts_with('{'),
            "the first element must start with its JSON object, not a separator: {first_line:?}"
        );
        assert!(
            second_line.starts_with(",{"),
            "the second element must carry the separator in front: {second_line:?}"
        );
        // Each element, separator excluded, is one complete JSON object.
        let first_event: serde_json::Value = serde_json::from_str(first_line)
            .unwrap_or_else(|error| panic!("first element must parse: {error}"));
        let second_event: serde_json::Value = serde_json::from_str(&second_line[1..])
            .unwrap_or_else(|error| panic!("second element must parse: {error}"));
        assert!(first_event.is_object() && second_event.is_object());
    }

    #[test]
    fn a_failed_event_write_is_surfaced_and_keeps_the_first_write_claim() {
        // On the pre-fix behaviour the comma went out in its own write_all
        // and every error was swallowed: a failing disk left a dangling
        // separator with no report, and the first-write claim was spent.
        let mut writer = FailingWriter::default();
        let mut first = true;
        let result = write_trace_event(&mut writer, &sample_event("doomed"), &mut first);
        assert!(
            result.is_err(),
            "a write failure must surface to the caller (which logs it), not vanish"
        );
        assert!(first, "the claim is consumed only on success");
        assert!(
            writer.written.is_empty(),
            "a failed element write must not leave a stray separator in the file"
        );

        // The retained claim means the next successful event is still the
        // first: no separator before it.
        let mut good = VecWriter::default();
        write_trace_event(&mut good, &sample_event("next"), &mut first).unwrap();
        let text = String::from_utf8(good.written).unwrap();
        assert!(!text.starts_with(','), "{text:?}");
        assert!(serde_json::from_str::<serde_json::Value>(text.trim()).is_ok());
    }

    #[test]
    fn header_and_footer_write_their_brackets_or_report_the_failure() {
        let mut ok_writer = VecWriter::default();
        write_trace_header(&mut ok_writer).unwrap();
        write_trace_footer(&mut ok_writer).unwrap();
        assert_eq!(ok_writer.written, b"[\n]\n");

        let mut failing = FailingWriter::default();
        assert!(write_trace_header(&mut failing).is_err());
        assert!(write_trace_footer(&mut failing).is_err());
    }

    #[test]
    fn test_next_flow_id_increments() {
        let id1 = next_flow_id();
        let id2 = next_flow_id();
        assert!(id2 > id1);
    }

    #[test]
    fn test_ns_to_us() {
        assert_eq!(ns_to_us(1000), 1);
        assert_eq!(ns_to_us(0), 0);
        assert_eq!(ns_to_us(999), 0);
        assert_eq!(ns_to_us(1_000_000), 1000);
    }

    #[test]
    fn test_trace_file_header_footer() {
        assert_eq!(trace_file_header(), "[\n");
        assert_eq!(trace_file_footer(), "\n]");
    }

    #[test]
    fn test_instant_event() {
        let e = ChromeTraceEvent::instant("test", "cat", 1, 2, 100);
        assert_eq!(e.ph, "i");
        assert_eq!(e.name, "test");
        assert_eq!(e.cat, "cat");
        assert_eq!(e.pid, 1);
        assert_eq!(e.tid, 2);
        assert_eq!(e.ts, 100);
        assert!(e.dur.is_none());
    }

    #[test]
    fn test_complete_event() {
        let e = ChromeTraceEvent::complete("req", "http", 10, 20, 500, 100);
        assert_eq!(e.ph, "X");
        assert_eq!(e.dur, Some(100));
    }

    #[test]
    fn test_flow_start_event() {
        let e = ChromeTraceEvent::flow_start("flow", "net", 1, 1, 0, 42);
        assert_eq!(e.ph, "s");
        assert_eq!(e.id, Some(42));
        assert!(e.bp.is_none());
    }

    #[test]
    fn test_flow_end_event() {
        let e = ChromeTraceEvent::flow_end("flow", "net", 1, 1, 100, 42);
        assert_eq!(e.ph, "f");
        assert_eq!(e.id, Some(42));
        assert_eq!(e.bp.as_deref(), Some("e"));
    }

    #[test]
    fn test_flow_step_event() {
        let e = ChromeTraceEvent::flow_step("mid", "net", 1, 1, 50, 42);
        assert_eq!(e.ph, "f");
        assert_eq!(e.id, Some(42));
        assert!(e.bp.is_none());
    }

    #[test]
    fn test_flow_from_events() {
        let start = ChromeTraceEvent::complete("start", "c", 1, 1, 0, 10);
        let end = ChromeTraceEvent::complete("end", "c", 2, 2, 100, 10);
        let (fs, fe, fid) = ChromeTraceEvent::flow_from_events(&start, &end);
        assert_eq!(fs.ph, "s");
        assert_eq!(fe.ph, "f");
        assert_eq!(fs.id, Some(fid));
        assert_eq!(fe.id, Some(fid));
        assert_eq!(fs.pid, 1);
        assert_eq!(fe.pid, 2);
    }

    #[test]
    fn test_flow_from_events_with_id() {
        let start = ChromeTraceEvent::instant("a", "c", 1, 1, 0);
        let end = ChromeTraceEvent::instant("b", "c", 2, 2, 50);
        let (fs, fe) = ChromeTraceEvent::flow_from_events_with_id(&start, &end, 99);
        assert_eq!(fs.id, Some(99));
        assert_eq!(fe.id, Some(99));
        assert_eq!(fe.bp.as_deref(), Some("e"));
    }

    #[test]
    fn test_with_args() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_string(), json!("value"));
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0).with_args(map);
        assert_eq!(e.args.unwrap()["key"], "value");
    }

    #[test]
    fn test_with_arg_multiple() {
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0)
            .with_arg("a", json!(1))
            .with_arg("b", json!("two"));
        let args = e.args.unwrap();
        assert_eq!(args["a"], 1);
        assert_eq!(args["b"], "two");
    }

    #[test]
    fn test_with_trace_args_value() {
        let val = json!({"method": "GET", "path": "/api"});
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0).with_trace_args_value(val);
        assert_eq!(e.args.unwrap()["method"], "GET");
    }

    #[test]
    fn test_with_trace_args_value_non_object() {
        // Non-object values should not be set
        let val = json!("string");
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0).with_trace_args_value(val);
        assert!(e.args.is_none());
    }

    #[test]
    fn test_chrome_trace_event_serde() {
        let e = ChromeTraceEvent::complete("test", "cat", 1, 2, 100, 50);
        let json = serde_json::to_string(&e).unwrap();
        let parsed: ChromeTraceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.ph, "X");
        assert_eq!(parsed.dur, Some(50));
    }

    struct MockTraceArgs;
    impl TraceArgs for MockTraceArgs {
        fn to_trace_args(&self) -> serde_json::Value {
            json!({"custom": true})
        }
    }

    #[test]
    fn test_with_trace_args_trait() {
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0).with_trace_args(&MockTraceArgs);
        assert_eq!(e.args.unwrap()["custom"], true);
    }

    struct EmptyTraceArgs;
    impl TraceArgs for EmptyTraceArgs {
        fn to_trace_args(&self) -> serde_json::Value {
            json!({})
        }
    }

    #[test]
    fn test_with_trace_args_empty_object() {
        let e = ChromeTraceEvent::instant("t", "c", 1, 1, 0).with_trace_args(&EmptyTraceArgs);
        // Empty object should not be set
        assert!(e.args.is_none());
    }

    #[test]
    fn trace_file_path_is_scoped_to_the_writing_process() {
        // A same-minute second `agentsight trace` in the same directory used to
        // resolve to the same file, so its `File::create` truncated the first
        // process's trace and the two writers then interleaved objects without
        // a separating comma.
        let path = trace_file_path();
        let name = path
            .file_name()
            .expect("the trace path has a file name")
            .to_string_lossy()
            .into_owned();
        let expected = regex::Regex::new(r"^trace-\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d+\.json$")
            .expect("the pattern is valid");
        assert!(
            expected.is_match(&name),
            "the trace file name must be scoped to this process, got {name:?}"
        );
        assert!(
            name.ends_with(&format!("-{}.json", std::process::id())),
            "the trace file name must carry this process id, got {name:?}"
        );
    }
}
