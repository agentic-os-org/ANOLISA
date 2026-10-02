//! `cosh audit` command surface.
//!
//! Replaces the original stub (which always returned `allowed: true` with a
//! `meta.warning` admitting it was not connected to a policy engine) with a
//! real PEP→PDP→log dispatcher. See `docs/audit-design.md` §5.

use std::path::PathBuf;
use std::time::Instant;

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;

use cosh_platform::audit::{
    self, parse_action_string, split_compound_command, LoadedPolicy, ParseError,
};
use cosh_platform::detect::Distro;
use cosh_types::audit::{
    Action, ActionSubsystem, AuditEventV1, Decision, LogSource, Outcome, Policy,
};
use cosh_types::error::{CoshError, ErrorCode};
use cosh_types::output::ResponseMeta;

use crate::{build_meta, build_meta_with_warning, print_failure, print_success};

#[derive(Subcommand)]
pub enum AuditCommands {
    /// Check whether an action is permitted under the active policy.
    Check(CheckArgs),
    /// View audit log entries for the current session (or filtered).
    Log(LogArgs),
    /// Report audit storage, retention, and reader health.
    Status,
    /// Query a bounded page of canonical audit events.
    Events(EventArgs),
    /// Build a correlated timeline for an event or identity.
    Trace(TraceArgs),
    /// Create a fail-closed redacted audit incident bundle.
    Export(ExportArgs),
    /// Preview the deterministic retention plan.
    Prune(PruneArgs),
    /// Inspect or validate audit policies.
    Policy {
        #[command(subcommand)]
        action: PolicyCommands,
    },
}

#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    /// Subsystem identifier (pkg / svc / checkpoint / shell / cosh).
    /// Required if no --action-string is given.
    #[arg(long)]
    subsystem: Option<String>,
    /// Operation name (install / start / exec / ...).
    /// Required when --subsystem is given.
    #[arg(long)]
    operation: Option<String>,
    /// Action target (package name, service name, command, ...).
    #[arg(long)]
    target: Option<String>,
    /// Argument key. Repeat for multiple args; pair with --arg-value.
    #[arg(long = "arg-key")]
    arg_key: Vec<String>,
    /// Argument value. Position-paired with --arg-key.
    #[arg(long = "arg-value")]
    arg_value: Vec<String>,
    /// Raw action string (parsed into a structured Action). Backwards-
    /// compatible aliases: --action.
    #[arg(long = "action-string", alias = "action")]
    action_string: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct LogArgs {
    /// Filter by session ID.
    #[arg(long)]
    session: Option<String>,
    /// Filter by outcome (allow / deny / requireapproval — case-insensitive).
    #[arg(long)]
    outcome: Option<String>,
    /// Filter to entries newer than `now - <duration>`. Accepts e.g.
    /// "30s", "5m", "2h", "1d".
    #[arg(long)]
    since: Option<String>,
    /// Maximum number of entries to return (most recent first when set).
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct EventArgs {
    /// Inclusive lower bound as duration (for example `2h`) or RFC 3339 timestamp.
    #[arg(long)]
    since: Option<String>,
    /// Inclusive RFC 3339 upper bound.
    #[arg(long)]
    until: Option<String>,
    /// Event names, comma-separated or repeated.
    #[arg(long = "event", value_delimiter = ',')]
    event_types: Vec<String>,
    /// Component names, comma-separated or repeated.
    #[arg(long, value_delimiter = ',')]
    component: Vec<String>,
    /// Outcome names, comma-separated or repeated.
    #[arg(long, value_delimiter = ',')]
    outcome: Vec<String>,
    /// Match an event ID or any correlation identity.
    #[arg(long)]
    identity: Option<String>,
    /// Restrict to `v1` or `legacy_v0`.
    #[arg(long)]
    schema: Option<String>,
    /// Page size from 1 through 1000.
    #[arg(long, default_value_t = audit::query::DEFAULT_PAGE_SIZE)]
    limit: usize,
    /// Opaque continuation cursor from a prior page.
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct TraceArgs {
    /// Event, session, run, turn, request, Tool-use, or command ID.
    id: String,
    /// Inclusive lower bound as duration or RFC 3339 timestamp.
    #[arg(long)]
    since: Option<String>,
    /// Inclusive RFC 3339 upper bound.
    #[arg(long)]
    until: Option<String>,
    /// Page size from 1 through 1000.
    #[arg(long, default_value_t = audit::query::DEFAULT_PAGE_SIZE)]
    limit: usize,
    /// Opaque continuation cursor from a prior trace page.
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Explicit output directory.
    #[arg(long)]
    output: PathBuf,
    /// Replace only a directory containing a valid cosh audit manifest.
    #[arg(long)]
    force: bool,
    /// Inclusive lower bound as duration or RFC 3339 timestamp.
    #[arg(long)]
    since: Option<String>,
    /// Inclusive RFC 3339 upper bound.
    #[arg(long)]
    until: Option<String>,
    /// Event names, comma-separated or repeated.
    #[arg(long = "event", value_delimiter = ',')]
    event_types: Vec<String>,
    /// Component names, comma-separated or repeated.
    #[arg(long, value_delimiter = ',')]
    component: Vec<String>,
    /// Outcome names, comma-separated or repeated.
    #[arg(long, value_delimiter = ',')]
    outcome: Vec<String>,
    /// Match an event ID or any correlation identity.
    #[arg(long, alias = "session")]
    identity: Option<String>,
    /// Restrict to `v1` or `legacy_v0`.
    #[arg(long)]
    schema: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct PruneArgs {
    /// Preview candidates without renaming or deleting anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
pub enum PolicyCommands {
    /// Show the active policy.
    Show,
    /// List built-in presets (permissive / balanced / strict).
    List,
    /// Validate a policy TOML file without writing anything to disk.
    Validate {
        /// Path to a TOML policy file.
        path: PathBuf,
    },
    /// Explain how `<action>` would be evaluated under the active policy.
    Explain {
        /// Action string (parsed via the same rules as `audit check
        /// --action-string`).
        action: String,
    },
}

pub fn run(action: AuditCommands, distro: &Distro, start: Instant) -> i32 {
    match action {
        AuditCommands::Check(args) => run_check(args, distro, start),
        AuditCommands::Log(args) => run_log(args, distro, start),
        AuditCommands::Status => run_status(distro, start),
        AuditCommands::Events(args) => run_events(args, distro, start),
        AuditCommands::Trace(args) => run_trace(args, distro, start),
        AuditCommands::Export(args) => run_export(args, distro, start),
        AuditCommands::Prune(args) => run_prune(args, distro, start),
        AuditCommands::Policy { action: pc } => run_policy(pc, distro, start),
    }
}

// ===========================================================================
// `cosh audit check`
// ===========================================================================

#[derive(Debug, Clone, Serialize)]
struct PolicyListEntry {
    name: String,
    default: Outcome,
    rules: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyShowResult {
    source: String,
    policy_version: String,
    policy: Policy,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyValidateResult {
    valid: bool,
    rules: usize,
    default: Outcome,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyExplainResult {
    action: Action,
    decision: Decision,
}

#[derive(Debug, Clone, Serialize)]
struct LogOutput {
    entries: Vec<AuditEventV1>,
    total: usize,
}

enum BuiltAction {
    Structured(Action),
    ParseDeny { error: ParseError, raw: String },
    Malformed(String),
}

fn build_action(args: &CheckArgs) -> BuiltAction {
    // Highest priority: raw string input. (Wins over per-field flags so
    // that `--action-string "..."` is unambiguously the input mode.)
    if let Some(s) = args.action_string.as_deref() {
        return match parse_action_string(s) {
            Ok(a) => BuiltAction::Structured(a),
            Err(e) => BuiltAction::ParseDeny {
                error: e,
                raw: s.to_string(),
            },
        };
    }

    // Otherwise structural input: --subsystem + --operation [+ --target].
    let subsystem = match &args.subsystem {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            return BuiltAction::Malformed(
                "missing required argument: --subsystem or --action-string".to_string(),
            );
        }
    };
    let operation = match &args.operation {
        Some(o) if !o.is_empty() => o.clone(),
        _ => {
            return BuiltAction::Malformed(
                "missing required argument: --operation (required with --subsystem)".to_string(),
            );
        }
    };
    if args.arg_key.len() != args.arg_value.len() {
        return BuiltAction::Malformed(format!(
            "--arg-key and --arg-value must be paired ({} keys vs {} values)",
            args.arg_key.len(),
            args.arg_value.len()
        ));
    }
    let arg_pairs: Vec<(String, String)> = args
        .arg_key
        .iter()
        .zip(args.arg_value.iter())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    BuiltAction::Structured(Action {
        subsystem: ActionSubsystem::from_token(&subsystem),
        operation,
        target: args.target.clone(),
        args: arg_pairs,
        raw: None,
    })
}

fn run_check(args: CheckArgs, distro: &Distro, start: Instant) -> i32 {
    match build_action(&args) {
        BuiltAction::Structured(action) => {
            let (loaded, load_warning) = LoadedPolicy::load();
            run_check_evaluate(action, &loaded, load_warning, distro, start)
        }
        BuiltAction::ParseDeny { error, raw } => {
            let (loaded, load_warning) = LoadedPolicy::load();
            let (action, decision) = parse_failure_denial(raw, error, &loaded);
            match audit::record_decision(action, &decision, LogSource::Cli) {
                Ok(()) => print_success(
                    decision,
                    meta_with_optional_warning(distro, start, load_warning.as_deref()),
                ),
                Err(mut e) => {
                    if let Ok(v) = serde_json::to_value(&decision) {
                        e = e.with_details(json!({ "decision": v }));
                    }
                    print_failure(e, build_meta("audit", distro, start, false))
                }
            }
        }
        BuiltAction::Malformed(msg) => print_failure(
            CoshError::new(ErrorCode::AuditActionMalformed, msg, "audit")
                .with_hint("see `cosh audit check --help` for valid argument combinations"),
            build_meta("audit", distro, start, false),
        ),
    }
}

fn run_check_evaluate(
    action: Action,
    loaded: &LoadedPolicy,
    load_warning: Option<String>,
    distro: &Distro,
    start: Instant,
) -> i32 {
    match audit::check(action, LogSource::Cli, loaded) {
        Ok(decision) => print_success(
            decision,
            meta_with_optional_warning(distro, start, load_warning.as_deref()),
        ),
        Err(e) => print_failure(e, build_meta("audit", distro, start, false)),
    }
}

// ===========================================================================
// `cosh audit log`
// ===========================================================================

fn run_log(args: LogArgs, distro: &Distro, start: Instant) -> i32 {
    let requested_limit = args.limit;
    let mut event_args = EventArgs {
        since: args.since,
        event_types: vec!["policy.decision".to_string()],
        identity: args.session,
        limit: audit::query::MAX_PAGE_SIZE,
        ..EventArgs::default()
    };
    if let Some(outcome) = args.outcome {
        event_args
            .outcome
            .push(match parse_outcome_filter(&outcome) {
                Ok(Outcome::Allow) => "allowed".to_string(),
                Ok(Outcome::Deny) => "denied".to_string(),
                Ok(Outcome::RequireApproval) => "started".to_string(),
                Err(error) => {
                    return print_failure(error, build_meta("audit", distro, start, false))
                }
            });
    }
    let filter = match build_event_filter(&event_args) {
        Ok(filter) => filter,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let page = match audit::query::query_events(&root.path, filter, event_args.limit, None) {
        Ok(page) => page,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let mut entries = page
        .events
        .into_iter()
        .map(|stored| stored.event)
        .collect::<Vec<_>>();
    if let Some(limit) = requested_limit {
        entries = entries.into_iter().rev().take(limit).collect();
    }
    let total = entries.len();
    print_success(
        LogOutput { entries, total },
        build_meta("audit", distro, start, false),
    )
}

fn parse_outcome_filter(s: &str) -> Result<Outcome, CoshError> {
    match s.to_ascii_lowercase().as_str() {
        "allow" => Ok(Outcome::Allow),
        "deny" => Ok(Outcome::Deny),
        "requireapproval" | "approval" | "require-approval" => Ok(Outcome::RequireApproval),
        other => Err(CoshError::new(
            ErrorCode::InvalidInput,
            format!(
                "unknown outcome filter '{}': expected allow / deny / requireapproval",
                other
            ),
            "audit",
        )),
    }
}

fn parse_duration_filter(s: &str) -> Result<chrono::Duration, CoshError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "empty --since value",
            "audit",
        ));
    }
    let (num_str, unit) = match trimmed.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&trimmed[..trimmed.len() - c.len_utf8()], c),
        _ => {
            return Err(CoshError::new(
                ErrorCode::InvalidInput,
                format!("invalid --since '{}': expected e.g. 30s, 5m, 2h, 1d", s),
                "audit",
            ));
        }
    };
    let n: i64 = num_str.parse().map_err(|_| {
        CoshError::new(
            ErrorCode::InvalidInput,
            format!(
                "invalid --since '{}': numeric component is not a non-negative integer",
                s
            ),
            "audit",
        )
    })?;
    if n < 0 {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            format!("invalid --since '{}': must be non-negative", s),
            "audit",
        ));
    }
    let multiplier = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 24 * 60 * 60,
        other => {
            return Err(CoshError::new(
                ErrorCode::InvalidInput,
                format!("invalid --since unit '{}': expected s/m/h/d", other),
                "audit",
            ));
        }
    };
    let seconds = n.checked_mul(multiplier).ok_or_else(|| {
        CoshError::new(
            ErrorCode::InvalidInput,
            "--since duration is outside the supported range",
            "audit",
        )
    })?;
    chrono::Duration::try_seconds(seconds).ok_or_else(|| {
        CoshError::new(
            ErrorCode::InvalidInput,
            "--since duration is outside the supported range",
            "audit",
        )
    })
}

// ===========================================================================
// Operational audit commands
// ===========================================================================

fn run_status(distro: &Distro, start: Instant) -> i32 {
    let loaded = match audit::config::load_audit_settings(None) {
        Ok(loaded) => loaded,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    match audit::query::audit_status(&root.path, root.source, loaded.settings) {
        Ok(status) => print_success(
            status,
            meta_with_optional_warning(distro, start, loaded.warnings.first().map(String::as_str)),
        ),
        Err(error) => print_failure(error, build_meta("audit", distro, start, false)),
    }
}

fn run_events(args: EventArgs, distro: &Distro, start: Instant) -> i32 {
    let filter = match build_event_filter(&args) {
        Ok(filter) => filter,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    match audit::query::query_events(&root.path, filter, args.limit, args.cursor.as_deref()) {
        Ok(page) => print_success(page, build_meta("audit", distro, start, false)),
        Err(error) => print_failure(error, build_meta("audit", distro, start, false)),
    }
}

fn run_trace(args: TraceArgs, distro: &Distro, start: Instant) -> i32 {
    let since = match args.since.as_deref().map(parse_since_bound).transpose() {
        Ok(value) => value,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let until = match args.until.as_deref().map(parse_timestamp).transpose() {
        Ok(value) => value,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    match audit::query::trace_events(
        &root.path,
        &args.id,
        since.map(|bound| bound.timestamp),
        since.is_some_and(|bound| bound.relative),
        until,
        args.limit,
        args.cursor.as_deref(),
    ) {
        Ok(trace) => print_success(trace, build_meta("audit", distro, start, false)),
        Err(error) => print_failure(error, build_meta("audit", distro, start, false)),
    }
}

fn run_export(args: ExportArgs, distro: &Distro, start: Instant) -> i32 {
    let event_args = EventArgs {
        since: args.since,
        until: args.until,
        event_types: args.event_types,
        component: args.component,
        outcome: args.outcome,
        identity: args.identity,
        schema: args.schema,
        ..EventArgs::default()
    };
    let filter = match build_event_filter(&event_args) {
        Ok(filter) => filter,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    match audit::export::create_export(&root.path, filter, &args.output, args.force) {
        Ok(result) => print_success(result, build_meta("audit", distro, start, false)),
        Err(error) => print_failure(error, build_meta("audit", distro, start, false)),
    }
}

fn run_prune(args: PruneArgs, distro: &Distro, start: Instant) -> i32 {
    if !args.dry_run {
        return print_failure(
            CoshError::new(
                ErrorCode::InvalidInput,
                "version 1 supports only `audit prune --dry-run`",
                "audit",
            ),
            build_meta("audit", distro, start, false),
        );
    }
    let loaded = match audit::config::load_audit_settings(None) {
        Ok(loaded) => loaded,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    let root = match audit::config::resolve_audit_root() {
        Ok(root) => root,
        Err(error) => return print_failure(error, build_meta("audit", distro, start, false)),
    };
    match audit::retention::plan_retention_dry_run(&root.path, &loaded.settings, chrono::Utc::now())
    {
        Ok(plan) => print_success(plan, build_meta("audit", distro, start, true)),
        Err(error) => print_failure(error, build_meta("audit", distro, start, true)),
    }
}

fn build_event_filter(args: &EventArgs) -> Result<audit::query::AuditEventFilter, CoshError> {
    let since = args.since.as_deref().map(parse_since_bound).transpose()?;
    let until = args.until.as_deref().map(parse_timestamp).transpose()?;
    if since
        .map(|bound| bound.timestamp)
        .zip(until)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "--since must not be later than --until",
            "audit",
        ));
    }
    let generation = match args.schema.as_deref() {
        None => None,
        Some("v1") => Some(audit::query::AuditSchemaGenerationFilter::V1),
        Some("legacy_v0" | "v0") => Some(audit::query::AuditSchemaGenerationFilter::LegacyV0),
        Some(_) => {
            return Err(CoshError::new(
                ErrorCode::InvalidInput,
                "--schema must be v1 or legacy_v0",
                "audit",
            ))
        }
    };
    Ok(audit::query::AuditEventFilter {
        since: since.map(|bound| bound.timestamp),
        since_is_relative: since.is_some_and(|bound| bound.relative),
        until,
        event_types: args.event_types.clone(),
        components: args.component.clone(),
        outcomes: args.outcome.clone(),
        identity: args.identity.clone(),
        generation,
    })
}

#[derive(Clone, Copy)]
struct SinceBound {
    timestamp: chrono::DateTime<chrono::Utc>,
    relative: bool,
}

fn parse_since_bound(value: &str) -> Result<SinceBound, CoshError> {
    if let Ok(duration) = parse_duration_filter(value) {
        return chrono::Utc::now()
            .checked_sub_signed(duration)
            .map(|timestamp| SinceBound {
                timestamp,
                relative: true,
            })
            .ok_or_else(|| {
                CoshError::new(
                    ErrorCode::InvalidInput,
                    "--since duration is outside the supported timestamp range",
                    "audit",
                )
            });
    }
    parse_timestamp(value).map(|timestamp| SinceBound {
        timestamp,
        relative: false,
    })
}

fn parse_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>, CoshError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .map_err(|_| {
            CoshError::new(
                ErrorCode::InvalidInput,
                format!("invalid timestamp '{value}': expected RFC 3339"),
                "audit",
            )
        })
}

// ===========================================================================
// `cosh audit policy ...`
// ===========================================================================

fn run_policy(action: PolicyCommands, distro: &Distro, start: Instant) -> i32 {
    match action {
        PolicyCommands::Show => run_policy_show(distro, start),
        PolicyCommands::List => run_policy_list(distro, start),
        PolicyCommands::Validate { path } => run_policy_validate(path, distro, start),
        PolicyCommands::Explain { action } => run_policy_explain(action, distro, start),
    }
}

fn run_policy_show(distro: &Distro, start: Instant) -> i32 {
    let (loaded, load_warning) = LoadedPolicy::load();
    let result = PolicyShowResult {
        source: loaded.source.label(),
        policy_version: loaded.policy_version.clone(),
        policy: loaded.policy.clone(),
    };
    print_success(
        result,
        meta_with_optional_warning(distro, start, load_warning.as_deref()),
    )
}

fn run_policy_list(distro: &Distro, start: Instant) -> i32 {
    let presets = audit::builtin::ALL.map(|p| {
        let loaded = audit::builtin::load(p);
        PolicyListEntry {
            name: p.name().to_string(),
            default: loaded.policy.default,
            rules: loaded.policy.rules.len(),
        }
    });
    print_success(
        json!({ "presets": presets, "total": presets.len() }),
        build_meta("audit", distro, start, false),
    )
}

fn run_policy_validate(path: PathBuf, distro: &Distro, start: Instant) -> i32 {
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return print_failure(
                CoshError::new(
                    ErrorCode::AuditPolicyError,
                    format!("failed to read {}: {}", path.display(), e),
                    "audit",
                ),
                build_meta("audit", distro, start, false),
            );
        }
    };
    match audit::policy::validate_toml_bytes(&bytes) {
        Ok(p) => print_success(
            PolicyValidateResult {
                valid: true,
                rules: p.rules.len(),
                default: p.default,
            },
            build_meta("audit", distro, start, false),
        ),
        Err(msg) => print_failure(
            CoshError::new(
                ErrorCode::AuditPolicyError,
                format!("invalid policy at {}: {}", path.display(), msg),
                "audit",
            )
            .with_hint("see docs/audit-design.md §6 for valid syntax"),
            build_meta("audit", distro, start, false),
        ),
    }
}

fn run_policy_explain(action_str: String, distro: &Distro, start: Instant) -> i32 {
    let action = match parse_action_string(&action_str) {
        Ok(a) => a,
        Err(error) => {
            let (loaded, load_warning) = LoadedPolicy::load();
            let (action, decision) = parse_failure_denial(action_str, error, &loaded);
            return print_success(
                PolicyExplainResult { action, decision },
                meta_with_optional_warning(distro, start, load_warning.as_deref()),
            );
        }
    };
    let (loaded, load_warning) = LoadedPolicy::load();
    let decision = audit::evaluate(&action, &loaded);
    print_success(
        PolicyExplainResult { action, decision },
        meta_with_optional_warning(distro, start, load_warning.as_deref()),
    )
}

// ===========================================================================
// Shared helpers
// ===========================================================================

fn parse_failure_denial(
    raw: String,
    error: ParseError,
    loaded: &LoadedPolicy,
) -> (Action, Decision) {
    if matches!(
        error,
        ParseError::ContainsShellMeta(_) | ParseError::ContainsControlByte
    ) {
        for segment in split_compound_command(&raw) {
            if let Ok(mut action) = parse_action_string(&segment) {
                let decision = audit::evaluate(&action, loaded);
                if decision.outcome == Outcome::Deny && decision.matched_rule.is_some() {
                    action.raw = Some(raw);
                    return (action, decision);
                }
            }
        }
    }

    let action = Action {
        subsystem: ActionSubsystem::Other("unparsed".to_string()),
        operation: "<unparsed>".to_string(),
        target: None,
        args: vec![],
        raw: Some(raw),
    };
    let decision = Decision {
        outcome: Outcome::Deny,
        reason: format!("parse failed: {error}"),
        matched_rule: None,
        policy_version: loaded.policy_version.clone(),
    };
    (action, decision)
}

fn meta_with_optional_warning(
    distro: &Distro,
    start: Instant,
    warning: Option<&str>,
) -> ResponseMeta {
    match warning {
        Some(w) => build_meta_with_warning("audit", distro, start, false, w),
        None => build_meta("audit", distro, start, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tokens_parse_failure_uses_synthetic_denial() {
        let loaded = audit::builtin::balanced();
        let raw = " \t".to_string();
        let (action, decision) = parse_failure_denial(raw.clone(), ParseError::NoTokens, &loaded);

        assert_eq!(
            action.subsystem,
            ActionSubsystem::Other("unparsed".to_string())
        );
        assert_eq!(action.operation, "<unparsed>");
        assert_eq!(action.raw, Some(raw));
        assert_eq!(decision.outcome, Outcome::Deny);
        assert_eq!(decision.reason, "parse failed: no tokens after split");
        assert_eq!(decision.matched_rule, None);
        assert_eq!(decision.policy_version, loaded.policy_version);
    }
}
