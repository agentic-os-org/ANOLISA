//! Compatibility command for querying the caller's own security events.
//!
//! This is the v2 restoration of v1's `agent-sec-cli events` entry point
//! (`agent_sec_cli/cli.py`). The v1 CLI read its per-user database directly;
//! the v2 CLI must go through the system daemon, which enforces the owner
//! scope from the connection's kernel peer credentials — the flags below can
//! only narrow the query, never name a different owner.

use std::fmt::Write as _;
use std::io;

use asc_daemon_protocol::{DaemonRequest, SecQueryParams, method};
use clap::Args;
use serde_json::Value;

use crate::InputError;

/// v1's CLI-level `--count-by` allowlist.
const COUNT_BY_ALLOWED: [&str; 3] = ["category", "event_type", "trace_id"];
/// v1's `--output` formats.
const OUTPUT_FORMATS: [&str; 3] = ["table", "json", "jsonl"];

/// Queries the caller's own security events through `asc-daemon`.
#[derive(Debug, Args)]
pub struct EventsCommand {
    /// Filter by event type.
    #[arg(long)]
    event_type: Option<String>,
    /// Filter by category.
    #[arg(long)]
    category: Option<String>,
    /// Filter by result: succeeded or failed.
    #[arg(long)]
    result: Option<String>,
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
    #[arg(long, default_value = "table")]
    output: String,
    /// Include the details payload of each event.
    #[arg(long)]
    include_details: bool,
}

impl EventsCommand {
    /// Builds the one request this invocation sends.
    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        if !OUTPUT_FORMATS.contains(&self.output.as_str()) {
            return Err(InputError::Events(format!(
                "--output must be one of: {}",
                OUTPUT_FORMATS.join(", ")
            )));
        }
        if let Some(field) = self.count_by.as_deref() {
            if !COUNT_BY_ALLOWED.contains(&field) {
                return Err(InputError::Events(format!(
                    "--count-by must be one of: {}",
                    COUNT_BY_ALLOWED.join(", ")
                )));
            }
        }
        if self.last_hours.is_some() && (self.since.is_some() || self.until.is_some()) {
            return Err(InputError::Events(
                "--last-hours is mutually exclusive with --since/--until".to_owned(),
            ));
        }
        if self.since.is_some() && self.until.is_some() && self.since == self.until {
            // An empty window is almost certainly an inverted range mistake.
            return Err(InputError::Events(
                "--since must be before --until".to_owned(),
            ));
        }

        let (start_ns, end_ns) = match self.last_hours {
            Some(hours) if hours.is_finite() && hours > 0.0 => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("the system clock is after the epoch");
                let now_ns = now.as_nanos();
                // The span is clamped to the epoch, so the window start never
                // underflows; the float multiply is bounded by the same clamp,
                // which keeps every cast below its exactness limit.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let span_ns = (hours * 3_600_000_000_000_000.0).min(now_ns as f64) as u128;
                (Some(now_ns - span_ns), Some(now_ns))
            }
            Some(_) => {
                return Err(InputError::Events(
                    "--last-hours must be a positive number".to_owned(),
                ));
            }
            None => (None, None),
        };

        let method_name = if self.count_by.is_some() {
            method::SEC_EVENTS_COUNT_BY
        } else {
            method::SEC_EVENTS_LIST
        };
        // `--count` only needs the total, so one row is enough to carry it.
        // `sec.events.count_by` rejects pagination parameters outright (v1's
        // `_reject_params`), so they are omitted for that method.
        let (limit, offset) = if self.count_by.is_some() {
            (None, None)
        } else {
            (
                Some(if self.count { 1 } else { self.limit }),
                Some(self.offset),
            )
        };
        let params = SecQueryParams {
            event_type: self.event_type.clone(),
            category: self.category.clone(),
            result: self.result.clone(),
            trace_id: self.trace_id.clone(),
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            since: self.since.clone(),
            until: self.until.clone(),
            start_ns: start_ns.map(|nanos| u64::try_from(nanos).expect("fits u64")),
            end_ns: end_ns.map(|nanos| u64::try_from(nanos).expect("fits u64")),
            limit,
            offset,
            include_details: Some(self.include_details),
            group_by: self.count_by.clone(),
            ..SecQueryParams::default()
        };
        Ok(DaemonRequest {
            trace_context: None,
            compatibility: None,
            method: method_name.to_owned(),
            params: serde_json::to_value(params)?,
        })
    }

    /// Renders one daemon response in the requested output format.
    ///
    /// # Errors
    ///
    /// Returns the first failure of the selected writer.
    ///
    /// # Panics
    ///
    /// Panics when a decoded successful response cannot be re-serialized;
    /// a decoded JSON value always serializes, so that is an internal
    /// invariant, not a caller condition.
    pub fn render(
        &self,
        response: &asc_daemon_protocol::DaemonResponse,
        stdout: &mut dyn io::Write,
        stderr: &mut dyn io::Write,
    ) -> io::Result<u8> {
        let result = match response {
            asc_daemon_protocol::DaemonResponse::Success(success) => &success.result,
            asc_daemon_protocol::DaemonResponse::Error(error) => {
                writeln!(
                    stderr,
                    "agent-sec-cli: events query failed: {} ({})",
                    error.error.message(),
                    error.error.code
                )?;
                return Ok(1);
            }
        };
        match self.output.as_str() {
            "json" => {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(result).expect("serializes")
                )?;
            }
            "jsonl" => {
                if self.count_by.is_some() {
                    for item in array_of(&result["items"]) {
                        writeln!(
                            stdout,
                            "{}",
                            serde_json::to_string(item).expect("serializes")
                        )?;
                    }
                } else if self.count {
                    writeln!(stdout, "{}", result["total"])?;
                } else {
                    for item in array_of(&result["items"]) {
                        writeln!(
                            stdout,
                            "{}",
                            serde_json::to_string(item).expect("serializes")
                        )?;
                    }
                }
            }
            // The default is v1's kubectl-style table.
            _ => {
                self.render_table(result, stdout)?;
            }
        }
        Ok(0)
    }

    /// Renders v1's kubectl-style columnar table.
    fn render_table(&self, result: &Value, stdout: &mut dyn io::Write) -> io::Result<u8> {
        if self.count_by.is_some() {
            let group_by = result["group_by"].as_str().unwrap_or("value");
            for item in array_of(&result["items"]) {
                let value = item["value"].as_str().unwrap_or("");
                let count = item["count"].as_u64().unwrap_or_default();
                writeln!(stdout, "{group_by:<12} {value:<32} {count:>8}")?;
            }
            return Ok(0);
        }
        if self.count {
            writeln!(stdout, "{}", result["total"].as_u64().unwrap_or_default())?;
            return Ok(0);
        }

        let items = array_of(&result["items"]);
        if items.is_empty() {
            writeln!(stdout, "No events found.")?;
            return Ok(0);
        }
        let headers = ["EVENT_TYPE", "CATEGORY", "RESULT", "TIMESTAMP"];
        let rows: Vec<[String; 4]> = items
            .iter()
            .map(|item| {
                [
                    item["event_type"].as_str().unwrap_or_default().to_owned(),
                    item["category"].as_str().unwrap_or_default().to_owned(),
                    item["result"].as_str().unwrap_or("succeeded").to_owned(),
                    item["timestamp"].as_str().unwrap_or_default().to_owned(),
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
            line.push_str(&format!("{header:<width$}"));
        }
        writeln!(stdout, "{}", line.trim_end())?;
        for row in &rows {
            let mut line = String::new();
            for (value, width) in row.iter().zip(&widths) {
                line.push_str(&format!("{value:<width$}"));
            }
            writeln!(stdout, "{}", line.trim_end())?;
        }
        let count = items.len();
        writeln!(
            stdout,
            "\n{count} event{}",
            if count == 1 { "" } else { "s" }
        )?;
        Ok(0)
    }
}

/// Returns one JSON value's array payload, or an empty slice for any other
/// shape, so renderers never panic on a malformed response.
fn array_of(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        events: EventsCommand,
    }

    fn command(args: &[&str]) -> EventsCommand {
        Cli::parse_from(std::iter::once("cli").chain(args.iter().copied())).events
    }

    #[test]
    fn the_v1_flag_set_parses() {
        let events = command(&[
            "--event-type",
            "sandbox_prehook",
            "--category",
            "exec",
            "--result",
            "failed",
            "--limit",
            "20",
            "--offset",
            "40",
            "--output",
            "json",
        ]);
        let request = events.request().expect("request");
        assert_eq!(request.method, "sec.events.list");
        let params: SecQueryParams = serde_json::from_value(request.params).expect("params");
        assert_eq!(params.event_type.as_deref(), Some("sandbox_prehook"));
        assert_eq!(params.limit, Some(20));
        assert_eq!(params.offset, Some(40));
        assert_eq!(params.include_details, Some(false));
    }

    #[test]
    fn count_by_selects_the_grouped_method() {
        let events = command(&["--count-by", "category"]);
        let request = events.request().expect("request");
        assert_eq!(request.method, "sec.events.count_by");
        let params: SecQueryParams = serde_json::from_value(request.params).expect("params");
        assert_eq!(params.group_by.as_deref(), Some("category"));
        assert!(
            params.limit.is_none() && params.offset.is_none(),
            "count_by must not carry pagination the daemon rejects"
        );
    }

    #[test]
    fn last_hours_becomes_an_epoch_nanosecond_window() {
        let events = command(&["--last-hours", "1"]);
        let request = events.request().expect("request");
        let params: SecQueryParams = serde_json::from_value(request.params).expect("params");
        let start = params.start_ns.expect("start");
        let end = params.end_ns.expect("end");
        assert!(end > start, "the window must be non-empty");
        assert!(end - start <= 3_600_000_000_000_001, "about one hour");
    }

    #[test]
    fn invalid_input_is_rejected_locally() {
        let cases: Vec<Vec<&str>> = vec![
            vec!["--output", "csv"],
            vec!["--count-by", "verdict"],
            vec!["--last-hours", "1", "--since", "2026-01-01T00:00:00+00:00"],
            vec!["--last-hours", "-1"],
        ];
        for args in cases {
            let events = command(&args);
            assert!(
                events.request().is_err(),
                "{:?} must be rejected before transport",
                args
            );
        }
    }

    #[test]
    fn the_table_render_matches_the_v1_shape() {
        let events = command(&[]);
        let response = asc_daemon_protocol::DaemonResponse::success(
            request_id(),
            json!({
                "items": [
                    {
                        "event_type": "sandbox_prehook",
                        "category": "exec",
                        "result": "succeeded",
                        "timestamp": "2026-01-02T00:00:00+00:00",
                    }
                ],
                "total": 1,
                "limit": 100,
                "offset": 0,
                "next_offset": null,
            }),
        );
        let mut buffer = Vec::new();
        let code = events
            .render(&response, &mut buffer, &mut Vec::new())
            .expect("render");
        assert_eq!(code, 0);
        let text = String::from_utf8(buffer).expect("text");
        assert!(text.contains("EVENT_TYPE"), "headers: {text}");
        assert!(text.contains("sandbox_prehook"));
        assert!(text.contains("1 event"), "footer: {text}");
    }

    fn request_id() -> asc_daemon_protocol::RequestId {
        asc_daemon_protocol::RequestId::new("test".to_owned()).expect("non-empty")
    }
}
