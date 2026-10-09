//! Compatibility command for querying the caller's own security events.
//!
//! This is the v2 restoration of v1's `agent-sec-cli events` entry point
//! (`agent_sec_cli/cli.py`). The v1 CLI read its per-user database directly;
//! the v2 CLI goes through the system daemon, which enforces the owner scope
//! from the connection's kernel peer credentials — the flags below can only
//! narrow the query. `--owner-uid` is a filter within that scope: root may
//! pick any UID (or read all owners), a non-root caller only itself.
//!
//! The v1 output contract is reproduced shape for shape: `--output json` is
//! the event array, `jsonl` one event per line, `--count` a bare number,
//! `--count-by` a JSON object, `--summary` the posture text, and the default
//! table keeps v1's kubectl columns and footer. Large `--limit` values are
//! satisfied through bounded page requests instead of being rejected by the
//! page-size cap, and `--count-by --offset` skips the newest rows before
//! grouping, exactly as v1's repository did.

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use asc_daemon_protocol::{DaemonRequest, DaemonResponse, SecQueryParams, method};
use clap::Args;
use serde_json::{Map, Value, json};

use crate::InputError;
use crate::commands::events_summary::format_summary;

/// v1's CLI-level `--count-by` allowlist.
const COUNT_BY_ALLOWED: [&str; 3] = ["category", "event_type", "trace_id"];
/// v1's `--output` formats, sorted for the error message.
const OUTPUT_FORMATS: [&str; 3] = ["json", "jsonl", "table"];
/// The daemon's hard cap on one page; the CLI paginates beyond it.
const PAGE_LIMIT: u64 = 1000;
/// v1's summary reads at most this many events (`limit=10000`).
const SUMMARY_LIMIT: usize = 10_000;
/// One resolved time window as epoch-nanosecond bounds.
type Window = (Option<u64>, Option<u64>);

/// Nanoseconds in one hour.
///
/// v1 computed `now - hours * 3600` in seconds; the v2 wire carries
/// nanoseconds, so the conversion factor is `3_600_000_000_000` — the PR's
/// first cut was off by three orders of magnitude, which turned one hour
/// into a thousand.
const NANOS_PER_HOUR: f64 = 3_600_000_000_000.0;

/// Queries the caller's own security events through `asc-daemon`.
#[derive(Debug, Args)]
pub struct EventsCommand {
    /// Filter by event type.
    #[arg(long)]
    event_type: Option<String>,
    /// Filter by category.
    #[arg(long)]
    category: Option<String>,
    /// Filter by trace ID.
    #[arg(long)]
    trace_id: Option<String>,
    /// Filter by session ID.
    #[arg(long)]
    session_id: Option<String>,
    /// Filter by run ID.
    #[arg(long)]
    run_id: Option<String>,
    /// Inclusive lower bound (ISO-8601 timestamp).
    #[arg(long)]
    since: Option<String>,
    /// Exclusive upper bound (ISO-8601 timestamp).
    #[arg(long)]
    until: Option<String>,
    /// Query events from the last N hours; mutually exclusive with --since/--until.
    #[arg(long, allow_hyphen_values = true)]
    last_hours: Option<f64>,
    /// Max results (default 100).
    #[arg(long, default_value_t = 100)]
    limit: u64,
    /// Skip N results (default 0).
    #[arg(long, default_value_t = 0)]
    offset: u64,
    /// Output only the count of matching events.
    #[arg(long)]
    count: bool,
    /// Group and count by one field: `category`, `event_type` or `trace_id`.
    #[arg(long)]
    count_by: Option<String>,
    /// Output format: table, json or jsonl.
    #[arg(long, short = 'o')]
    output: Option<String>,
    /// Include the details payload of each event.
    #[arg(long)]
    include_details: bool,
    /// Output a human-readable security posture summary (own text format).
    #[arg(long)]
    summary: bool,
    /// Owner filter within the authorized scope; non-root may only name itself.
    #[arg(long)]
    owner_uid: Option<u32>,
}

/// One round trip to the daemon.
pub trait EventsTransport {
    /// Sends one request and returns the daemon's response.
    ///
    /// # Errors
    ///
    /// Returns the transport failure when the call could not complete.
    fn call(
        &mut self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, asc_daemon_client::ClientError>;
}

/// Production transport over the daemon socket.
pub struct SocketTransport<'a> {
    socket: &'a Path,
    timeout: Duration,
}

impl<'a> SocketTransport<'a> {
    /// Prepares the transport for `socket`.
    pub const fn new(socket: &'a Path, timeout: Duration) -> Self {
        Self { socket, timeout }
    }
}

impl EventsTransport for SocketTransport<'_> {
    fn call(
        &mut self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, asc_daemon_client::ClientError> {
        asc_daemon_client::call(self.socket, request, self.timeout)
    }
}

/// Failures of one events invocation.
#[derive(Debug, thiserror::Error)]
pub enum EventsRunError {
    /// Invalid CLI input; the message is user-facing wording.
    #[error(transparent)]
    Input(#[from] InputError),
    /// The daemon could not be reached or the call failed.
    #[error(transparent)]
    Client(#[from] asc_daemon_client::ClientError),
    /// A request could not be encoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Writing the report failed.
    #[error(transparent)]
    Output(#[from] io::Error),
}

impl EventsCommand {
    /// Resolves the invocation against the daemon over `socket`.
    ///
    /// # Errors
    ///
    /// Returns the first validation, transport, or rendering failure.
    pub fn run(
        &self,
        socket: &Path,
        timeout: Duration,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        let mut transport = SocketTransport::new(socket, timeout);
        self.run_with(&mut transport, stdout, stderr)
    }

    /// Resolves the invocation through `transport`, v1 output contract intact.
    ///
    /// # Errors
    ///
    /// Returns the first validation, transport, or rendering failure.
    pub fn run_with(
        &self,
        transport: &mut dyn EventsTransport,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        self.validate()?;
        let output = self.output.clone().unwrap_or_else(|| "table".to_owned());
        if self.summary {
            return self.run_summary(transport, stdout, stderr);
        }
        if self.count {
            return self.run_count(transport, stdout, stderr);
        }
        if let Some(field) = self.count_by.as_deref() {
            return self.run_count_by(field, transport, stdout, stderr);
        }
        self.run_list(&output, transport, stdout, stderr)
    }

    /// v1's `--count`: one probe page carries the pre-pagination total; the
    /// displayed number is what remains after the offset.
    fn run_count(
        &self,
        transport: &mut dyn EventsTransport,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        let mut params = self.params(None, None);
        params.limit = Some(1);
        params.offset = Some(self.offset);
        let response = Self::request(transport, method::SEC_EVENTS_LIST, &params)?;
        let Some(result) = response_result(&response, stderr) else {
            return Ok(1);
        };
        let total = result["total"].as_u64().unwrap_or_default();
        let remaining = total.saturating_sub(self.offset);
        writeln!(stdout, "{remaining}")?;
        Ok(0)
    }

    /// v1's `--count-by`: group the offset-skipped rows client-side, because
    /// the daemon method rejects pagination parameters by v1 wire contract.
    fn run_count_by(
        &self,
        field: &str,
        transport: &mut dyn EventsTransport,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        let template = self.params(None, None);
        let (items, _total, code) =
            Self::fetch_pages(transport, &template, self.offset, usize::MAX, false, stderr)?;
        if code != 0 {
            return Ok(code);
        }
        let mut groups: Map<String, Value> = Map::new();
        for item in &items {
            let key = group_key(item, field);
            let count = groups.get(&key).and_then(Value::as_u64).unwrap_or(0) + 1;
            groups.insert(key, json!(count));
        }
        // v1 printed the grouped counts as a JSON object regardless of
        // `--output`, sorted as the SQL GROUP BY produced them.
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&groups).expect("serializes")
        )?;
        Ok(0)
    }

    /// v1's `--summary`: the posture text over the (default 24h) window.
    fn run_summary(
        &self,
        transport: &mut dyn EventsTransport,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        let (window, label) = self.summary_window();
        let template = self.params(window.0, window.1);
        let (items, _total, code) =
            Self::fetch_pages(transport, &template, 0, SUMMARY_LIMIT, true, stderr)?;
        if code != 0 {
            return Ok(code);
        }
        let text = format_summary(&items, &label);
        writeln!(stdout, "{text}")?;
        Ok(0)
    }

    /// v1's list mode: table, json array, or jsonl lines.
    fn run_list(
        &self,
        output: &str,
        transport: &mut dyn EventsTransport,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> Result<u8, EventsRunError> {
        let want_details = self.include_details || output == "json" || output == "jsonl";
        let template = self.params(None, None);
        let limit = usize::try_from(self.limit).unwrap_or(usize::MAX);
        let (items, _total, code) = Self::fetch_pages(
            transport,
            &template,
            self.offset,
            limit,
            want_details,
            stderr,
        )?;
        if code != 0 {
            return Ok(code);
        }
        match output {
            // v1's `json` was the whole array, details included, indented 2.
            "json" => {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string_pretty(&items).expect("serializes")
                )?;
            }
            // v1's `jsonl` was one compact event per line.
            "jsonl" => {
                for item in &items {
                    writeln!(
                        stdout,
                        "{}",
                        serde_json::to_string(item).expect("serializes")
                    )?;
                }
            }
            _ => Self::render_table(&items, stdout)?,
        }
        Ok(0)
    }

    /// Collects up to `max_rows` rows starting at `start_offset` through
    /// bounded page requests.
    ///
    /// Nothing is printed until every page has arrived, so a failure partway
    /// cannot leave a truncated "complete" report behind. The returned code
    /// is `1` when a page failed (the failure wording already went to
    /// stderr), and the collected rows are then unusable by contract.
    fn fetch_pages(
        transport: &mut dyn EventsTransport,
        template: &SecQueryParams,
        start_offset: u64,
        max_rows: usize,
        include_details: bool,
        stderr: &mut dyn io::Write,
    ) -> Result<(Vec<Value>, u64, u8), EventsRunError> {
        let mut collected: Vec<Value> = Vec::new();
        let mut cursor = start_offset;
        let mut total = 0_u64;
        loop {
            let remaining = max_rows.saturating_sub(collected.len());
            if remaining == 0 {
                break;
            }
            let page = u64::try_from(remaining).unwrap_or(u64::MAX).min(PAGE_LIMIT);
            let mut params = template.clone();
            params.limit = Some(page);
            params.offset = Some(cursor);
            params.include_details = Some(include_details);
            let response = Self::request(transport, method::SEC_EVENTS_LIST, &params)?;
            let Some(result) = response_result(&response, stderr) else {
                return Ok((Vec::new(), 0, 1));
            };
            total = result["total"].as_u64().unwrap_or_default();
            let items = result["items"].as_array().cloned().unwrap_or_default();
            let returned = items.len() as u64;
            collected.extend(items);
            let next = result["next_offset"].as_u64();
            match next {
                Some(next) if next > cursor && returned > 0 => cursor = next,
                _ => break,
            }
        }
        Ok((collected, total, 0))
    }

    /// Sends one request and returns the response.
    fn request(
        transport: &mut dyn EventsTransport,
        method_name: &str,
        params: &SecQueryParams,
    ) -> Result<DaemonResponse, EventsRunError> {
        let request = DaemonRequest {
            trace_context: None,
            compatibility: None,
            method: method_name.to_owned(),
            params: serde_json::to_value(params)?,
        };
        Ok(transport.call(&request)?)
    }

    /// Builds the shared filter parameters with one page's pagination.
    fn params(&self, limit: Option<u64>, offset: Option<u64>) -> SecQueryParams {
        let (start_ns, end_ns) = self.window();
        SecQueryParams {
            event_type: self.event_type.clone(),
            category: self.category.clone(),
            trace_id: self.trace_id.clone(),
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            since: self.since.clone(),
            until: self.until.clone(),
            start_ns,
            end_ns,
            limit,
            offset,
            include_details: Some(self.include_details),
            owner_uid: self.owner_uid,
            ..SecQueryParams::default()
        }
    }

    /// Resolves `--last-hours` into epoch-nanosecond bounds, v1 semantics
    /// (non-negative, zero allowed).
    fn window(&self) -> Window {
        match self.last_hours {
            Some(hours) if hours.is_finite() && hours >= 0.0 => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("the system clock is after the epoch");
                let now_ns = now.as_nanos();
                // The span is clamped to the epoch, so the window start never
                // underflows; the float multiply is bounded by the same
                // clamp, which keeps the cast exact.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let span_ns = (hours * NANOS_PER_HOUR).min(now_ns as f64) as u128;
                (
                    Some(u64::try_from(now_ns - span_ns).expect("fits u64")),
                    Some(u64::try_from(now_ns).expect("fits u64")),
                )
            }
            _ => (None, None),
        }
    }

    /// Resolves the summary window and its v1 time label.
    ///
    /// v1 defaulted the summary to the last 24 hours only when no explicit
    /// range was given at all.
    fn summary_window(&self) -> (Window, String) {
        if self.last_hours.is_none() && self.since.is_none() && self.until.is_none() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the system clock is after the epoch");
            let now_ns = now.as_nanos();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let span_ns = (24.0_f64 * NANOS_PER_HOUR).min(now_ns as f64) as u128;
            return (
                (
                    Some(u64::try_from(now_ns - span_ns).expect("fits u64")),
                    Some(u64::try_from(now_ns).expect("fits u64")),
                ),
                "last 24 hours".to_owned(),
            );
        }
        let label = match self.last_hours {
            Some(hours) => format!("last {hours:.0} hours"),
            None => format!(
                "{} to {}",
                self.since.as_deref().unwrap_or("..."),
                self.until.as_deref().unwrap_or("now")
            ),
        };
        (self.window(), label)
    }

    /// v1's validation ladder, in v1's order, with v1's wording.
    fn validate(&self) -> Result<(), InputError> {
        if self.summary && (self.count || self.count_by.is_some()) {
            return Err(InputError::Events(
                "--summary is incompatible with --count and --count-by.".to_owned(),
            ));
        }
        if self.summary && self.output.is_some() {
            return Err(InputError::Events(
                "--summary is incompatible with --output (summary has its own format).".to_owned(),
            ));
        }
        if let Some(output) = self.output.as_deref() {
            if !OUTPUT_FORMATS.contains(&output) {
                return Err(InputError::Events(format!(
                    "--output must be one of: {}.",
                    OUTPUT_FORMATS.join(", ")
                )));
            }
        }
        if let Some(field) = self.count_by.as_deref() {
            if !COUNT_BY_ALLOWED.contains(&field) {
                return Err(InputError::Events(format!(
                    "--count-by must be one of: {}.",
                    COUNT_BY_ALLOWED.join(", ")
                )));
            }
        }
        if let Some(hours) = self.last_hours {
            if hours < 0.0 || !hours.is_finite() {
                return Err(InputError::Events(
                    "--last-hours must be non-negative.".to_owned(),
                ));
            }
        }
        if self.limit == 0 {
            return Err(InputError::Events("--limit must be positive.".to_owned()));
        }
        if self.last_hours.is_some() && (self.since.is_some() || self.until.is_some()) {
            return Err(InputError::Events(
                "--last-hours is mutually exclusive with --since/--until.".to_owned(),
            ));
        }
        if self.count && self.count_by.is_some() {
            return Err(InputError::Events(
                "--count and --count-by are mutually exclusive.".to_owned(),
            ));
        }
        Ok(())
    }

    /// Renders v1's kubectl-style columnar table.
    fn render_table(items: &[Value], stdout: &mut dyn io::Write) -> io::Result<()> {
        if items.is_empty() {
            writeln!(stdout, "No events found.")?;
            return Ok(());
        }
        let headers = ["EVENT_TYPE", "CATEGORY", "RESULT", "TIMESTAMP"];
        let rows: Vec<[String; 4]> = items
            .iter()
            .map(|item| {
                [
                    item["event_type"].as_str().unwrap_or_default().to_owned(),
                    item["category"].as_str().unwrap_or_default().to_owned(),
                    item["result"].as_str().unwrap_or("succeeded").to_owned(),
                    format_timestamp(item["timestamp"].as_str().unwrap_or_default()),
                ]
            })
            .collect();
        let widths: Vec<usize> = headers
            .iter()
            .enumerate()
            .map(|(column, header)| {
                rows.iter()
                    .map(|row| row[column].len())
                    .chain([header.len()])
                    .max()
                    .unwrap_or_default()
                    + 2
            })
            .collect();
        let mut line = String::new();
        for (header, width) in headers.iter().zip(&widths) {
            let _ = write!(line, "{header:<width$}");
        }
        writeln!(stdout, "{}", line.trim_end())?;
        for row in &rows {
            let mut line = String::new();
            for (value, width) in row.iter().zip(&widths) {
                let _ = write!(line, "{value:<width$}");
            }
            writeln!(stdout, "{}", line.trim_end())?;
        }
        let count = items.len();
        writeln!(
            stdout,
            "\n{count} event{}",
            if count == 1 { "" } else { "s" }
        )?;
        Ok(())
    }
}

/// Returns the success result, or prints v1's query-failure wording and
/// `None` for a daemon error response.
fn response_result<'a>(
    response: &'a DaemonResponse,
    stderr: &mut dyn io::Write,
) -> Option<&'a Value> {
    match response {
        DaemonResponse::Success(success) => Some(&success.result),
        DaemonResponse::Error(error) => {
            writeln!(
                stderr,
                "agent-sec-cli: events query failed: {} ({})",
                error.error.message(),
                error.error.code
            )
            .ok();
            None
        }
    }
}

/// The bucket key of one grouped row, v1's SQL semantics: a `NULL` column
/// lands in the `null` bucket (`json.dumps({None: ...})` spelling), an empty
/// string stays its own bucket.
fn group_key(item: &Value, field: &str) -> String {
    match item.get(field) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => "null".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// Renders an ISO-8601 timestamp in local time, v1's `_format_timestamp`.
fn format_timestamp(timestamp: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|moment| moment.to_utc())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|naive| naive.and_utc())
        })
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S%.f")
                .map(|naive| naive.and_utc())
        });
    match parsed {
        Ok(moment) => moment
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => timestamp.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;
    use std::collections::VecDeque;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        events: EventsCommand,
    }

    fn command(args: &[&str]) -> EventsCommand {
        Cli::parse_from(std::iter::once("cli").chain(args.iter().copied())).events
    }

    /// Scripted transport: hands back queued responses and records every
    /// request it served, so tests can pin what actually went on the wire.
    struct Scripted {
        responses: VecDeque<DaemonResponse>,
        requests: Vec<DaemonRequest>,
    }

    impl Scripted {
        fn new(responses: Vec<DaemonResponse>) -> Self {
            Self {
                requests: Vec::new(),
                responses: responses.into(),
            }
        }
    }

    impl EventsTransport for Scripted {
        fn call(
            &mut self,
            request: &DaemonRequest,
        ) -> Result<DaemonResponse, asc_daemon_client::ClientError> {
            self.requests.push(request.clone());
            Ok(self.responses.pop_front().expect("a queued response"))
        }
    }

    fn request_id() -> asc_daemon_protocol::RequestId {
        asc_daemon_protocol::RequestId::new("test".to_owned()).expect("non-empty")
    }

    fn page(items: Value, total: u64, next_offset: Option<u64>) -> DaemonResponse {
        DaemonResponse::success(
            request_id(),
            json!({
                "items": items,
                "total": total,
                "limit": 100,
                "offset": 0,
                "next_offset": next_offset,
            }),
        )
    }

    fn event(id: &str, category: &str) -> Value {
        json!({
            "event_id": id,
            "event_type": "sandbox_prehook",
            "category": category,
            "result": "succeeded",
            "timestamp": "2026-01-02T00:00:00+00:00",
            "details": {"k": "v"},
        })
    }

    #[test]
    fn the_v1_flag_set_parses() {
        let events = command(&[
            "--event-type",
            "sandbox_prehook",
            "--category",
            "exec",
            "--limit",
            "20",
            "--offset",
            "40",
            "-o",
            "json",
        ]);
        assert!(events.validate().is_ok());
        let params = events.params(None, None);
        assert_eq!(params.event_type.as_deref(), Some("sandbox_prehook"));
        assert_eq!(params.category.as_deref(), Some("exec"));
        assert_eq!(params.owner_uid, None);
    }

    #[test]
    fn owner_uid_is_forwarded_as_a_filter() {
        let events = command(&["--owner-uid", "1000"]);
        assert_eq!(events.owner_uid, Some(1000));
        assert!(events.validate().is_ok());
    }

    #[test]
    fn v1_mutual_exclusions_are_restored() {
        for args in [
            vec!["--count", "--count-by", "category"],
            vec!["--summary", "--count"],
            vec!["--summary", "--count-by", "category"],
            vec!["--summary", "-o", "json"],
            vec!["--last-hours", "1", "--since", "2026-01-01T00:00:00+00:00"],
            vec!["--last-hours", "-1"],
            vec!["--limit", "0"],
            vec!["--count-by", "verdict"],
            vec!["-o", "csv"],
        ] {
            let events = command(&args);
            assert!(
                events.validate().is_err(),
                "{args:?} must be rejected before transport"
            );
        }
        // Zero hours is a legal (empty) window in v1.
        assert!(command(&["--last-hours", "0"]).validate().is_ok());
    }

    #[test]
    fn last_hours_is_one_hour_not_one_thousand() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock");
        let events = command(&["--last-hours", "1"]);
        let (start_ns, end_ns) = events.window();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock");
        let (start_ns, end_ns) = (start_ns.expect("start"), end_ns.expect("end"));
        assert!(end_ns >= before.as_nanos() as u64 && end_ns <= after.as_nanos() as u64);
        let span = end_ns - start_ns;
        // One hour, give or take the clock movement across the call.
        assert!(
            span <= 3_600_000_000_000_u64 + 1_000_000_000,
            "one hour window, got {span} ns"
        );
        assert!(
            span >= 3_599_000_000_000_u64,
            "one hour window, got {span} ns"
        );
    }

    #[test]
    fn count_prints_the_rows_remaining_after_the_offset() {
        let events = command(&["--count", "--offset", "2"]);
        let mut transport = Scripted::new(vec![page(json!([]), 5, None)]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut err)
            .expect("run");
        assert_eq!(code, 0);
        assert_eq!(String::from_utf8(out).expect("text"), "3\n");
    }

    #[test]
    fn count_by_applies_the_offset_before_grouping() {
        // Five rows: e1..e5 (newest first: e5, e4, e3, e2, e1). Skipping the
        // newest two leaves e3, e2, e1 — three exec rows.
        let events = command(&["--count-by", "category", "--offset", "2"]);
        let mut transport = Scripted::new(vec![page(
            json!([
                event("e3", "exec"),
                event("e2", "exec"),
                event("e1", "exec"),
            ]),
            5,
            None,
        )]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut err)
            .expect("run");
        assert_eq!(code, 0);
        let text = String::from_utf8(out).expect("text");
        let parsed: Value = serde_json::from_str(&text).expect("json object");
        assert_eq!(parsed, json!({"exec": 3}));
        // The offset must reach the daemon: v1 skipped the newest rows in SQL
        // before grouping, and the CLI may not silently drop it even though
        // `sec.events.count_by` itself rejects pagination parameters.
        let first = &transport.requests[0];
        let params: SecQueryParams = serde_json::from_value(first.params.clone()).expect("params");
        assert_eq!(params.offset, Some(2), "the offset went on the wire");
    }

    #[test]
    fn count_by_null_buckets_follow_the_sql_spelling() {
        let events = command(&["--count-by", "trace_id"]);
        let mut row = event("e1", "exec");
        row.as_object_mut().expect("object").remove("trace_id");
        let mut transport = Scripted::new(vec![page(json!([row]), 1, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        let parsed: Value =
            serde_json::from_str(&String::from_utf8(out).expect("text")).expect("json");
        assert_eq!(parsed, json!({"null": 1}));
    }

    #[test]
    fn json_output_is_the_v1_event_array_with_details() {
        let events = command(&["--output", "json"]);
        let mut transport = Scripted::new(vec![page(json!([event("e1", "exec")]), 1, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        let parsed: Value =
            serde_json::from_str(&String::from_utf8(out).expect("text")).expect("json");
        let items = parsed.as_array().expect("array, not a page object");
        assert_eq!(items.len(), 1);
        assert!(
            items[0]["details"].is_object(),
            "v1 JSON kept the details payload"
        );
    }

    #[test]
    fn jsonl_output_is_one_event_per_line() {
        let events = command(&["--output", "jsonl"]);
        let mut transport = Scripted::new(vec![page(
            json!([event("e1", "exec"), event("e2", "network")]),
            2,
            None,
        )]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        let text = String::from_utf8(out).expect("text");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).expect("event");
        assert_eq!(first["event_id"], json!("e1"));
    }

    #[test]
    fn a_large_limit_is_satisfied_through_bounded_pages() {
        // 2,500 rows requested: pages of 1,000 + 1,000 + 500.
        let events = command(&["--limit", "2500"]);
        let page_of = |prefix: &str, n: usize| -> Value {
            Value::Array(
                (0..n)
                    .map(|index| {
                        let id = format!("{prefix}{index}");
                        event(&id, "exec")
                    })
                    .collect(),
            )
        };
        let mut transport = Scripted::new(vec![
            page(page_of("a", 1000), 2500, Some(1000)),
            page(page_of("b", 1000), 2500, Some(2000)),
            page(page_of("c", 500), 2500, None),
        ]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        // The table footer reports the full traversal.
        let text = String::from_utf8(out).expect("text");
        assert!(text.contains("2500 events"), "footer: {text}");
    }

    #[test]
    fn a_page_failure_prints_no_report() {
        let events = command(&["--limit", "2500"]);
        let mut transport = Scripted::new(vec![
            page(json!([event("e1", "exec")]), 2500, Some(1000)),
            DaemonResponse::error(
                request_id(),
                asc_daemon_protocol::error_code::UNAVAILABLE,
                "security event store is unavailable or corrupt",
            ),
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut err)
            .expect("run");
        assert_eq!(code, 1, "the daemon error is an exit failure");
        assert!(
            out.is_empty(),
            "a failed pagination must not print a pseudo-complete report"
        );
        let stderr = String::from_utf8(err).expect("text");
        assert!(stderr.contains("events query failed"), "{stderr}");
    }

    #[test]
    fn the_table_render_matches_the_v1_shape() {
        let events = command(&[]);
        let mut transport = Scripted::new(vec![page(json!([event("e1", "exec")]), 1, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        let text = String::from_utf8(out).expect("text");
        assert!(text.contains("EVENT_TYPE"), "headers: {text}");
        assert!(text.contains("sandbox_prehook"));
        assert!(text.contains("1 event"), "footer: {text}");
    }

    #[test]
    fn an_empty_table_says_no_events_found() {
        let events = command(&[]);
        let mut transport = Scripted::new(vec![page(json!([]), 0, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        assert_eq!(String::from_utf8(out).expect("text"), "No events found.\n");
    }

    #[test]
    fn summary_defaults_to_the_last_24_hours_and_renders_the_posture() {
        let events = command(&["--summary"]);
        let mut prompt = event("e1", "prompt_scan");
        prompt["result"] = json!("succeeded");
        prompt["details"] = json!({"result": {"verdict": "pass"}});
        let mut transport = Scripted::new(vec![page(json!([prompt]), 1, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        let text = String::from_utf8(out).expect("text");
        assert!(
            text.contains("Security Posture Summary (last 24 hours)"),
            "{text}"
        );
        assert!(text.contains("System Status:"), "{text}");
        assert!(text.contains("--- Prompt Scan ---"), "{text}");
    }

    #[test]
    fn an_empty_summary_says_no_events_recorded() {
        let events = command(&["--summary"]);
        let mut transport = Scripted::new(vec![page(json!([]), 0, None)]);
        let mut out = Vec::new();
        let code = events
            .run_with(&mut transport, &mut out, &mut Vec::new())
            .expect("run");
        assert_eq!(code, 0);
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "No security events recorded.\n\n",
            "v1 echoed the formatter's trailing newline with another"
        );
    }

    #[test]
    fn summary_without_a_range_label_mentions_the_explicit_bounds() {
        let events = command(&["--summary", "--since", "2026-01-01T00:00:00+00:00"]);
        let (_, label) = events.summary_window();
        assert_eq!(label, "2026-01-01T00:00:00+00:00 to now");
    }

    #[test]
    fn timestamps_render_in_local_time_for_the_table() {
        // A fixed instant: the local rendering is computed by the same rule
        // the assertion uses, so the test is timezone-independent.
        let rendered = format_timestamp("2026-01-02T00:00:00+00:00");
        let expected = {
            use chrono::TimeZone;
            chrono::Utc
                .with_ymd_and_hms(2026, 1, 2, 0, 0, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        };
        assert_eq!(rendered, expected);
        // Unparsable input falls back to the raw text, as in v1.
        assert_eq!(format_timestamp("not a timestamp"), "not a timestamp");
    }
}
