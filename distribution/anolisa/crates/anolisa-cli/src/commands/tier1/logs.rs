//! `anolisa logs [OBJECT]` — query the central operation/audit log.
//!
//! Reads the JSONL log file resolved by [`anolisa_platform::fs_layout::FsLayout`]
//! and runs the request through [`anolisa_core::CentralLog::query`] (see
//! launch spec §7.1 and §8.4). Output is human by default; `--json` wraps the
//! result in the standard [`crate::response::CliResponse`] envelope.
//!
//! A missing log file is the expected fresh-install state and produces an
//! empty result (no error). All flags are passive filters; this command
//! never writes to the log.

use clap::Parser;

use anolisa_core::{CentralLog, LogFilter, LogKind, LogRecord, Severity};

use crate::color::{Palette, pad_right};
use crate::commands::common;
use crate::context::CliContext;
use crate::response::{CliError, render_json};

const COMMAND: &str = "logs";

/// Default cap on returned records when `--limit` is omitted.
const DEFAULT_LIMIT: usize = 50;
/// Maximum accepted `--limit` value.
const MAX_LIMIT: usize = 1000;

#[derive(Parser)]
pub struct LogsArgs {
    /// Filter target: component / operation id / log source / `all`.
    /// Omit to query everything.
    #[arg(value_name = "OBJECT")]
    pub object: Option<String>,
    /// Match exact operation id (e.g. `op-20260601-001`).
    #[arg(long, value_name = "ID")]
    pub operation_id: Option<String>,
    /// Restrict to a record kind: `operation` or `component`.
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Match exact source (e.g. `anolisa-cli`, `agentsight`).
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
    /// Match exact component name.
    #[arg(long, value_name = "COMP")]
    pub component: Option<String>,
    /// Minimum severity: `debug` | `info` | `warn` | `error`.
    // Aliased rather than duplicated: log CLIs commonly name this filter
    // "level", and one field keeps both spellings on the same validation and
    // filtering path. Visible so clap advertises `[aliases: --level]` itself.
    #[arg(long, visible_alias = "level", value_name = "LEVEL")]
    pub severity: Option<String>,
    /// Lexicographic ISO8601 lower bound on `started_at`.
    #[arg(long, value_name = "ISO")]
    pub since: Option<String>,
    /// Cap returned records to the most recent N (default 50, max 1000; 0 returns none).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

pub fn handle(mut args: LogsArgs, ctx: &CliContext) -> Result<(), CliError> {
    // Resolve the component identity (e.g., "copilot-shell" → "cosh") before
    // filtering — log records store the canonical component name.
    if let Some(component) = args.component.take() {
        let installed = common::load_state_store(ctx, COMMAND)
            .unwrap_or_else(|_| anolisa_core::state_store::StateStore::empty());
        args.component = Some(common::lookup_component_name_in_store(
            &component, &installed, ctx, COMMAND,
        )?);
    }
    let filter = build_filter(&args)?;
    let layout = common::resolve_layout(ctx);
    let log = CentralLog::open(layout.central_log.clone());

    let records = log
        .query(&filter)
        .map_err(|err| CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "failed to query central log at {}: {err}",
                layout.central_log.display()
            ),
        })?;

    if ctx.json {
        let data = serde_json::to_value(&records).map_err(|err| CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!("failed to serialize log records: {err}"),
        })?;
        return render_json(COMMAND, data);
    }

    if !ctx.quiet {
        render_human(&records, ctx.verbose, ctx.no_color);
    }
    Ok(())
}

/// Translate the parsed CLI args into a [`LogFilter`], surfacing
/// validation errors as `INVALID_ARGUMENT`.
fn build_filter(args: &LogsArgs) -> Result<LogFilter, CliError> {
    let kind = match args.kind.as_deref() {
        None => None,
        Some(v) => Some(parse_kind(v)?),
    };
    let severity_at_least = match args.severity.as_deref() {
        None => None,
        Some(v) => Some(parse_severity(v)?),
    };
    let since = match args.since.as_deref() {
        None => None,
        Some(v) => Some(parse_since(v)?),
    };
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if limit > MAX_LIMIT {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!("--limit must be <= {MAX_LIMIT}, got {limit}"),
        });
    }
    Ok(LogFilter {
        kind,
        source: args.source.clone(),
        component: args.component.clone(),
        operation_id: args.operation_id.clone(),
        severity_at_least,
        object: args.object.clone(),
        since,
        limit: Some(limit),
    })
}

fn parse_since(raw: &str) -> Result<String, CliError> {
    chrono::DateTime::parse_from_rfc3339(raw).map_err(|_| CliError::InvalidArgument {
        command: COMMAND.to_string(),
        reason: format!(
            "--since expects an RFC3339/ISO8601 timestamp like 2026-06-01T10:00:00Z, got '{raw}'"
        ),
    })?;
    Ok(raw.to_string())
}

fn parse_kind(raw: &str) -> Result<LogKind, CliError> {
    match raw {
        "operation" => Ok(LogKind::Operation),
        "component" => Ok(LogKind::Component),
        other => Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!("--kind expects 'operation' or 'component', got '{other}'"),
        }),
    }
}

fn parse_severity(raw: &str) -> Result<Severity, CliError> {
    match raw {
        "debug" => Ok(Severity::Debug),
        "info" => Ok(Severity::Info),
        "warn" | "warning" => Ok(Severity::Warn),
        "error" => Ok(Severity::Error),
        other => Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "--severity/--level expects one of debug|info|warn|error, got '{other}'"
            ),
        }),
    }
}

fn render_human(records: &[LogRecord], verbose: bool, no_color: bool) {
    let color = Palette::new(no_color);
    if records.is_empty() {
        println!("{}", color.muted("no log records"));
        return;
    }

    println!(
        "{}",
        color.header(format!(
            "{:<20}  {:<5}  {:<18}  {:<40}  {}",
            "STARTED_AT", "SEV", "OPERATION_ID", "COMMAND", "STATUS"
        ))
    );
    for record in records {
        let op_id = record.operation_id.as_deref().unwrap_or("-");
        let status = record
            .status
            .map(severity_status_str)
            .unwrap_or_else(|| "-".to_string());
        let severity = severity_str(record.severity);
        let command = truncate(&record.command, 40);
        println!(
            "{ts}  {sev:<5}  {op:<18}  {cmd:<40}  {status}",
            ts = color.muted(&record.started_at),
            sev = color.severity(pad_right(severity, 5)),
            op = if op_id == "-" {
                color.muted(pad_right(op_id, 18))
            } else {
                color.id(pad_right(op_id, 18))
            },
            cmd = color.command(pad_right(&command, 40)),
            status = color.status(status),
        );
        if verbose {
            if !record.message.is_empty() {
                println!("    {} {}", color.label("message:"), record.message);
            }
            if !record.objects.is_empty() {
                println!(
                    "    {} {}",
                    color.label("objects:"),
                    record.objects.join(", ")
                );
            }
            if !record.warnings.is_empty() {
                for warning in &record.warnings {
                    println!("    {} {}", color.warn("warning:"), warning);
                }
            }
        }
    }
}

fn severity_str(sev: Severity) -> &'static str {
    match sev {
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

fn severity_status_str(status: anolisa_core::LogStatus) -> String {
    match status {
        anolisa_core::LogStatus::Ok => "ok".to_string(),
        anolisa_core::LogStatus::Failed => "failed".to_string(),
        anolisa_core::LogStatus::RolledBack => "rolled_back".to_string(),
        anolisa_core::LogStatus::Partial => "partial".to_string(),
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_core::{CentralLog, LogFilter};
    use clap::CommandFactory;

    fn default_args() -> LogsArgs {
        LogsArgs {
            object: None,
            operation_id: None,
            kind: None,
            source: None,
            component: None,
            severity: None,
            since: None,
            limit: None,
        }
    }

    /// Write three hand-crafted JSONL records and confirm `CentralLog::query`
    /// applies the operation_id filter (the lower-level helper we depend on).
    #[test]
    fn query_helper_filters_records_by_operation_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"operation","operation_id":"op-1","command":"enable agent-observability","source":"anolisa-cli","severity":"info","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:00Z","status":"ok"}"#,
                "\n",
                r#"{"kind":"operation","operation_id":"op-2","command":"enable tokenless","source":"anolisa-cli","severity":"info","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:01Z","status":"ok"}"#,
                "\n",
                r#"{"kind":"operation","operation_id":"op-3","command":"enable ws-ckpt","source":"anolisa-cli","severity":"info","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:02Z","status":"ok"}"#,
                "\n",
            ),
        )
        .expect("write log file");

        let log = CentralLog::open(path);
        let hits = log
            .query(&LogFilter {
                operation_id: Some("op-2".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].operation_id.as_deref(), Some("op-2"));
    }

    #[test]
    fn missing_log_file_yields_empty() {
        // Fresh-install path — file does not exist, query returns []
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.jsonl");
        let log = CentralLog::open(path);
        let hits = log.query(&LogFilter::default()).expect("query");
        assert!(hits.is_empty());
    }

    #[test]
    fn parse_severity_accepts_known_values() {
        assert_eq!(parse_severity("debug").unwrap(), Severity::Debug);
        assert_eq!(parse_severity("info").unwrap(), Severity::Info);
        assert_eq!(parse_severity("warn").unwrap(), Severity::Warn);
        assert_eq!(parse_severity("warning").unwrap(), Severity::Warn);
        assert_eq!(parse_severity("error").unwrap(), Severity::Error);
        assert!(parse_severity("loud").is_err());
    }

    #[test]
    fn level_alias_parses_like_severity() {
        let severity = LogsArgs::try_parse_from(["logs", "--severity", "warn"]).expect("parse");
        let level = LogsArgs::try_parse_from(["logs", "--level", "warn"]).expect("parse");

        assert_eq!(severity.severity.as_deref(), Some("warn"));
        assert_eq!(level.severity.as_deref(), Some("warn"));
        assert_eq!(
            build_filter(&severity).expect("filter").severity_at_least,
            build_filter(&level).expect("filter").severity_at_least
        );
    }

    #[test]
    fn level_alias_rejects_unknown_severity() {
        let args = LogsArgs::try_parse_from(["logs", "--level", "loud"]).expect("parse");

        let err = build_filter(&args).expect_err("unknown severity should fail");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        // Names both spellings so the message stays actionable for whichever
        // one the user typed.
        assert!(err.reason().contains("--severity/--level"));
    }

    #[test]
    fn help_advertises_level_alias() {
        let mut cmd = LogsArgs::command();
        let help = cmd.render_help().to_string();
        assert!(
            help.contains("--severity <LEVEL>") && help.contains("--level"),
            "help must relate --level to --severity, got:\n{help}"
        );
    }

    #[test]
    fn parse_kind_accepts_known_values() {
        assert_eq!(parse_kind("operation").unwrap(), LogKind::Operation);
        assert_eq!(parse_kind("component").unwrap(), LogKind::Component);
        assert!(parse_kind("other").is_err());
    }

    #[test]
    fn build_filter_rejects_invalid_since() {
        let mut args = default_args();
        args.since = Some("2026-06-01 10:00:00".to_string());

        let err = build_filter(&args).expect_err("invalid since should fail");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("--since"));
        assert!(err.reason().contains("RFC3339/ISO8601"));
    }

    #[test]
    fn build_filter_rejects_limit_above_max() {
        let mut args = default_args();
        args.limit = Some(MAX_LIMIT + 1);

        let err = build_filter(&args).expect_err("limit above max should fail");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("--limit"));
        assert!(err.reason().contains("1000"));
    }

    #[test]
    fn query_combined_filter_honors_zero_and_one_limits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"kind":"operation","operation_id":"op-1","command":"enable agent-observability","source":"anolisa-cli","severity":"error","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:00Z","status":"ok"}"#,
                "\n",
                r#"{"kind":"component","command":"report agentsight","source":"agentsight","component":"agentsight","severity":"info","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:01Z"}"#,
                "\n",
                r#"{"kind":"component","command":"report agentsight","source":"agentsight","component":"agentsight","severity":"warn","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:02Z"}"#,
                "\n",
                r#"{"kind":"component","command":"report sec-core","source":"sec-core","component":"sec-core","severity":"error","message":"ok","actor":"test-actor","started_at":"2026-06-01T10:00:03Z"}"#,
                "\n",
            ),
        )
        .expect("write log file");

        let log = CentralLog::open(path);
        let mut args = default_args();
        args.kind = Some("component".to_string());
        args.severity = Some("warn".to_string());
        args.limit = Some(1);

        let one = log
            .query(&build_filter(&args).expect("filter"))
            .expect("query");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].kind, LogKind::Component);
        // Two component rows are at-or-above warn (warn then error); limit 1
        // must keep the later error, not the earlier warn.
        assert_eq!(one[0].severity, Severity::Error);
        assert_eq!(one[0].started_at, "2026-06-01T10:00:03Z");

        args.limit = Some(0);
        let zero = log
            .query(&build_filter(&args).expect("filter"))
            .expect("query");
        assert!(zero.is_empty());
    }
}
