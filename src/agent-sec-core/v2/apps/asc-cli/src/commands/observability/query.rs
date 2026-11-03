//! Daemon-only, paginated reports; the socket peer determines query visibility.

use std::{io, path::Path, time::Duration};

use asc_daemon_protocol::{DaemonRequest, DaemonResponse, method};
use clap::Args;
use serde_json::{Value, json};

mod review;

pub(super) fn call(
    socket: &Path,
    timeout: Duration,
    method: &str,
    params: Value,
) -> io::Result<Value> {
    let request = DaemonRequest {
        method: method.into(),
        params,
        trace_context: None,
        compatibility: None,
    };
    match asc_daemon_client::call(socket, &request, timeout).map_err(io::Error::other)? {
        DaemonResponse::Success(response) => Ok(response.result),
        DaemonResponse::Error(response) => Err(io::Error::other(format!(
            "{}: {}",
            response.error.code,
            response.error.message()
        ))),
    }
}

#[derive(Debug, Args)]
pub(crate) struct ReportCommand {
    /// Session ID to summarize; rejects labels shared by multiple owners.
    #[arg(long)]
    session_id: Option<String>,
    /// Select the session with the most recent activity.
    #[arg(long)]
    last: bool,
    /// Output format: text or json.
    #[arg(long, default_value = "text")]
    format: String,
}

struct Page {
    items: Vec<Value>,
    total: u64,
    next: Option<i64>,
}

fn page(
    client: &impl Fn(&str, Value) -> io::Result<Value>,
    method: &str,
    mut params: Value,
    offset: i64,
    limit: u32,
) -> io::Result<Page> {
    params["offset"] = json!(offset);
    params["limit"] = json!(limit);
    let value = client(method, params)?;
    let items = value["items"]
        .as_array()
        .ok_or_else(invalid_response)?
        .clone();
    let total = value["total"].as_u64().ok_or_else(invalid_response)?;
    let next = match value.get("next_offset") {
        Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|next| *next > offset)
                .ok_or_else(invalid_response)?,
        ),
        None => return Err(invalid_response()),
    };
    if next.is_some() && items.is_empty() {
        return Err(invalid_response());
    }
    Ok(Page { items, total, next })
}

fn for_each_page(
    client: &impl Fn(&str, Value) -> io::Result<Value>,
    method: &str,
    params: &Value,
    mut visit: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    loop {
        let result = page(client, method, params.clone(), offset, 100)?;
        for item in result.items {
            visit(item)?;
        }
        match result.next {
            Some(next) => offset = next,
            None => return Ok(()),
        }
    }
}

fn invalid_response() -> io::Error {
    io::Error::other("invalid observability query response")
}

fn field<'a>(value: &'a Value, name: &str) -> io::Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)
}

fn find_session(
    client: &impl Fn(&str, Value) -> io::Result<Value>,
    params: &Value,
    session: Option<&str>,
) -> io::Result<Value> {
    let mut params = params.clone();
    if let Some(session) = session {
        params["session_id"] = json!(session);
    }
    let result = page(
        client,
        method::OBS_SESSIONS_LIST,
        params,
        0,
        if session.is_some() { 2 } else { 1 },
    )?;
    if session.is_some() && result.total > 1 {
        return Err(io::Error::other(
            "session is shared by multiple owners; cannot select it unambiguously",
        ));
    }
    result
        .items
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no matching observability session"))
}

#[derive(Default)]
struct Totals {
    llm_calls: u64,
    request_bytes: u64,
    response_bytes: u64,
    tools: Vec<(String, u64)>,
}

impl Totals {
    fn add(&mut self, row: &Value) -> io::Result<()> {
        match field(row, "hook")? {
            "after_llm_call" => {
                increment(&mut self.llm_calls, 1)?;
                increment(
                    &mut self.request_bytes,
                    byte_count(row["metrics"].get("request_payload_bytes"))?,
                )?;
                increment(
                    &mut self.response_bytes,
                    byte_count(row["metrics"].get("response_stream_bytes"))?,
                )?;
            }
            "before_tool_call" => {
                let name = match row["metrics"].get("tool_name") {
                    None => "unknown",
                    Some(Value::String(name)) => name,
                    _ => return Err(io::Error::other("tool_name must be a string for reporting")),
                };
                if let Some((_, count)) = self.tools.iter_mut().find(|(tool, _)| tool == name) {
                    increment(count, 1)?;
                } else {
                    if self.tools.len() >= 10_000 {
                        return Err(io::Error::other(
                            "too many distinct tools; narrow the report time range",
                        ));
                    }
                    self.tools.push((name.into(), 1));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn increment(total: &mut u64, count: u64) -> io::Result<()> {
    *total = total
        .checked_add(count)
        .ok_or_else(|| io::Error::other("report counter overflow"))?;
    Ok(())
}

impl ReportCommand {
    pub(super) fn run(
        &self,
        client: &impl Fn(&str, Value) -> io::Result<Value>,
        output: &mut dyn io::Write,
    ) -> io::Result<()> {
        if !matches!(self.format.as_str(), "text" | "json") {
            return Err(io::Error::other("--format must be 'text' or 'json'"));
        }
        if !self.last && self.session_id.as_deref().is_none_or(str::is_empty) {
            return Err(io::Error::other("specify --session-id or --last"));
        }
        let mut params = json!({});
        // V1 gives --last precedence when both selectors are supplied.
        let session = find_session(
            client,
            &params,
            if self.last {
                None
            } else {
                self.session_id.as_deref()
            },
        )?;
        params["session_id"] = json!(field(&session, "session_id")?);
        let mut totals = Totals::default();
        for_each_page(client, method::OBS_RUNS_LIST, &params, |run| {
            let mut events_params = params.clone();
            events_params["run_id"] = json!(field(&run, "run_id")?);
            events_params["include_security"] = json!(false);
            for_each_page(client, method::OBS_TIMELINE_GET, &events_params, |event| {
                totals.add(&event)
            })
        })?;
        totals.tools.sort_by(|a, b| b.1.cmp(&a.1));
        let security = session["security_by_category_result"]
            .as_object()
            .ok_or_else(invalid_response)?;
        let first = session["first_seen_epoch"]
            .as_f64()
            .ok_or_else(invalid_response)?;
        let last = session["last_seen_epoch"]
            .as_f64()
            .ok_or_else(invalid_response)?;
        let report = json!({
            "session_id":session["session_id"],
            "first_seen":epoch_text(first)?, "last_seen":epoch_text(last)?,
            "duration_seconds":format!("{:.1}",last-first).parse::<f64>().map_err(|_| invalid_response())?,
            "turn_count":session["turn_count"], "llm_calls":totals.llm_calls,
            "request_bytes":totals.request_bytes, "response_bytes":totals.response_bytes,
            "tool_breakdown":totals.tools.iter().map(|(name,count)| (name.clone(),json!(count))).collect::<serde_json::Map<_,_>>(),
            "security_verdicts":security,
            "security_hint":if security.is_empty() { Some(if session["uid"].is_null() { "historical records have unknown ownership; security correlation is unavailable" } else { "security hooks may not pass session_id yet" }) } else { None }
        });
        // Nothing is printed until every required page and aggregate succeeds.
        if self.format == "json" {
            writeln!(output, "{}", serde_json::to_string_pretty(&report)?)
        } else {
            render_report(&report, &totals, last - first, output)
        }
    }
}

#[allow(clippy::cast_possible_truncation)] // Checked finite timestamp in chrono's supported range.
fn epoch_text(epoch: f64) -> io::Result<String> {
    if !epoch.is_finite() {
        return Err(invalid_response());
    }
    chrono::DateTime::from_timestamp(epoch.floor() as i64, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .ok_or_else(invalid_response)
}

fn render_report(
    report: &Value,
    totals: &Totals,
    duration: f64,
    output: &mut dyn io::Write,
) -> io::Result<()> {
    let duration = if duration >= 60.0 {
        format!(
            "{:.0}m {:.0}s",
            (duration / 60.0).floor(),
            (duration % 60.0).floor()
        )
    } else {
        format!("{duration:.0}s")
    };
    writeln!(
        output,
        "Session {}  ({} — {}, {}, {} turns)\n",
        safe_text(field(report, "session_id")?)
            .chars()
            .take(12)
            .collect::<String>(),
        field(report, "first_seen")?,
        field(report, "last_seen")?,
        duration,
        report["turn_count"]
    )?;
    writeln!(output, "  LLM calls:       {}", report["llm_calls"])?;
    let request = totals.request_bytes;
    let response = totals.response_bytes;
    if request != 0 || response != 0 {
        writeln!(
            output,
            "  Payload:         {} bytes sent, {} bytes received",
            grouped(request),
            grouped(response)
        )?;
    }
    let tools = totals
        .tools
        .iter()
        .map(|(name, count)| format!("{}({count})", safe_text(name)))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        output,
        "\n  Tools used:      {}\n",
        if tools.is_empty() { "(none)" } else { &tools }
    )?;
    let security = report["security_verdicts"]
        .as_object()
        .ok_or_else(invalid_response)?;
    if security.is_empty() {
        write!(output, "  Security:        (no security events)")?;
        if let Some(hint) = report["security_hint"].as_str() {
            write!(output, " — {hint}")?;
        }
        writeln!(output)?;
    } else {
        writeln!(output, "  Security:")?;
        let mut categories: Vec<_> = security.iter().collect();
        categories.sort_by_key(|(name, _)| *name);
        for (category, counts) in categories {
            let mut counts: Vec<_> = counts
                .as_object()
                .ok_or_else(invalid_response)?
                .iter()
                .collect();
            counts.sort_by_key(|(result, _)| *result);
            let counts = counts
                .iter()
                .map(|(result, count)| format!("{result}: {count}"))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(output, "    {category:<20} {counts}")?;
        }
    }
    Ok(())
}

fn grouped(value: u64) -> String {
    let text = value.to_string();
    let mut result = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Explicit finite, nonnegative, exclusive u64 upper bound.
fn byte_count(value: Option<&Value>) -> io::Result<u64> {
    match value {
        None => Ok(0),
        Some(Value::Bool(value)) => Ok(u64::from(*value)),
        Some(Value::String(value)) => value
            .trim()
            .parse()
            .map_err(|_| io::Error::other("invalid byte count")),
        Some(value) => value
            .as_u64()
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v < 18_446_744_073_709_551_616.0)
                    .map(|v| v.trunc() as u64)
            })
            .ok_or_else(|| io::Error::other("invalid byte count")),
    }
}

fn safe_text(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

pub(super) use review::run as run_review;

#[cfg(test)]
mod tests {
    use super::*;

    /// QRY-009: executable query-contract coverage.
    #[test]
    fn root_last_preserves_result_attribution_and_named_session_rejects_ambiguity() {
        let root_view = |method: &str, params: Value| -> io::Result<Value> {
            assert_eq!(method, method::OBS_SESSIONS_LIST);
            let all = [
                json!({"session_id":"same","uid":2000}),
                json!({"session_id":"same","uid":1000}),
            ];
            let limit = usize::try_from(params["limit"].as_u64().unwrap()).unwrap();
            Ok(
                json!({"items":all.iter().take(limit).collect::<Vec<_>>(),"total":2,"next_offset":if limit==1 { json!(1) } else { Value::Null }}),
            )
        };
        assert!(
            find_session(&root_view, &json!({}), Some("same"))
                .unwrap_err()
                .to_string()
                .contains("multiple owners")
        );
        let row = find_session(&root_view, &json!({}), None).unwrap();
        assert_eq!(row["uid"], 2000);
    }

    /// QRY-009: a named session requires one filtered RPC, regardless of history size.
    #[test]
    fn named_session_is_resolved_in_one_filtered_rpc() {
        let calls = std::cell::Cell::new(0);
        let client = |method: &str, params: Value| {
            calls.set(calls.get() + 1);
            assert_eq!(method, method::OBS_SESSIONS_LIST);
            assert_eq!(params["session_id"], "chosen");
            assert_eq!(params["limit"], 2);
            assert_eq!(params["offset"], 0);
            assert!(params.get("uid").is_none());
            Ok(json!({"items":[{"session_id":"chosen","uid":1000}],"total":1,"next_offset":null}))
        };
        let row = find_session(&client, &json!({}), Some("chosen")).unwrap();
        assert_eq!(row["session_id"], "chosen");
        assert_eq!(calls.get(), 1);
    }

    /// QRY-011: executable query-contract coverage.
    #[test]
    fn a_later_page_failure_emits_no_partial_report() {
        let failing_second_page = |method: &str, params: Value| -> io::Result<Value> {
            match method {
                method::OBS_SESSIONS_LIST => Ok(
                    json!({"items":[{"session_id":"s","uid":1000}],"total":1,"next_offset":null}),
                ),
                method::OBS_RUNS_LIST => {
                    assert!(params.get("uid").is_none());
                    Ok(json!({"items":[{"run_id":"r"}],"total":1,"next_offset":null}))
                }
                method::OBS_TIMELINE_GET if params["offset"] == 0 => Ok(
                    json!({"items":[{"hook":"after_llm_call","metrics":{}}],"total":2,"next_offset":1}),
                ),
                _ => Err(io::Error::other("unavailable")),
            }
        };
        let command = ReportCommand {
            session_id: None,
            last: true,
            format: "json".into(),
        };
        let mut output = Vec::new();
        assert!(command.run(&failing_second_page, &mut output).is_err());
        assert!(output.is_empty());
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    /// QRY-006/009: executable query-contract coverage.
    #[test]
    fn report_matches_frozen_v1_json_and_text() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../../fixtures/query/v1-reports.json"
        ))
        .unwrap();
        for case in cases {
            let fixture = |method: &str, params: Value| -> io::Result<Value> {
                let items = match method {
                    method::OBS_SESSIONS_LIST => {
                        let mut session = case["session"].clone();
                        session["uid"] = json!(1000);
                        session["security_by_category_result"] =
                            case["json"]["security_verdicts"].clone();
                        vec![session]
                    }
                    method::OBS_RUNS_LIST => {
                        assert!(params.get("uid").is_none());
                        vec![json!({"run_id":"run"})]
                    }
                    method::OBS_TIMELINE_GET => {
                        assert!(params.get("uid").is_none());
                        case["events"].as_array().unwrap().clone()
                    }
                    _ => panic!("unexpected method"),
                };
                Ok(json!({"total":items.len(),"items":items,"next_offset":null}))
            };
            for format in ["json", "text"] {
                let command = ReportCommand {
                    session_id: Some("ignored-when-last-is-set".into()),
                    last: true,
                    format: format.into(),
                };
                let mut output = Vec::new();
                command.run(&fixture, &mut output).unwrap();
                if format == "json" {
                    let actual: Value = serde_json::from_slice(&output).unwrap();
                    assert_eq!(actual, case["json"], "{}", case["name"]);
                } else {
                    assert_eq!(
                        String::from_utf8(output).unwrap(),
                        case["text"].as_str().unwrap(),
                        "{}",
                        case["name"]
                    );
                }
            }
        }
    }
}
