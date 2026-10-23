//! Hermes `state.db` discovery and ATIF conversion.
//!
//! Hermes persists sessions in SQLite rather than JSONL. This source opens the
//! live database read-only, converts each active session independently, and
//! leaves the writer's WAL and schema untouched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    open_connection, retention_cutoff_ns, ConnectionMode, ConnectionOptions,
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::hermes_convert::{
    convert_session, scoped_session_id, source_fingerprint, timestamp_ns, HermesMessage,
    HermesSession,
};
use crate::store::{extract_user_message_previews, TrajectoryRecord, TrajectoryStore};

const SOURCE: &str = "hermes";

/// Result of scanning one Hermes state database.
#[derive(Debug, Default)]
pub(crate) struct HermesScanOutcome {
    pub(crate) sessions: usize,
    pub(crate) ingested: usize,
}

/// Finds live Hermes databases without requiring an AgentSight config change.
///
/// `$HERMES_HOME/state.db` is authoritative for container deployments. The
/// regular `~/.hermes/state.db` layout and explicitly configured scan roots are
/// also checked so the same adapter works for a stock Hermes installation.
pub(crate) fn discover_state_databases(scan_dirs: Option<&[PathBuf]>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dirs) = scan_dirs {
        for dir in dirs {
            if dir.file_name().is_some_and(|name| name == "state.db") {
                candidates.push(dir.clone());
            } else {
                candidates.push(dir.join("state.db"));
                candidates.push(dir.join(".hermes/state.db"));
            }
        }
    } else {
        if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
            candidates.push(PathBuf::from(home).join("state.db"));
        }
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".hermes/state.db"));
        }
    }

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let key = path.canonicalize().unwrap_or_else(|_| path.clone());
            seen.insert(key).then_some(path)
        })
        .collect()
}

/// Converts all eligible sessions from one Hermes database.
///
/// The connection is strictly read-only. SQLite itself merges committed WAL
/// frames into the read view, so a collector in the Hermes container observes
/// live sessions without copying or checkpointing the database.
pub(crate) fn collect_database(
    store: &TrajectoryStore,
    db_path: &Path,
    retention_days: u64,
) -> Result<HermesScanOutcome> {
    let db_path = db_path
        .canonicalize()
        .context("resolve Hermes database path")?;
    let connection = open_connection(
        &db_path,
        ConnectionOptions {
            mode: ConnectionMode::ReadOnlyExisting,
            busy_timeout: Duration::from_millis(500),
            enable_wal: false,
        },
    )
    .with_context(|| format!("open Hermes state database {}", db_path.display()))?;

    let schema = validate_schema(&connection)?;
    let database_namespace = database_namespace(&db_path);
    let sessions = load_sessions(&connection, &schema.sessions)?;
    let mut outcome = HermesScanOutcome {
        sessions: sessions.len(),
        ingested: 0,
    };

    // 按会话隔离错误，避免一条坏消息在每轮扫描中阻塞同库的其他会话。
    for session in sessions {
        match collect_session(
            store,
            &connection,
            &db_path,
            &database_namespace,
            &schema,
            &session,
            retention_days,
        ) {
            Ok(true) => outcome.ingested += 1,
            Ok(false) => {}
            Err(err) => log::warn!(
                "Hermes session {} in {} skipped: {err:#}",
                session.id,
                db_path.display()
            ),
        }
    }
    Ok(outcome)
}

fn collect_session(
    store: &TrajectoryStore,
    connection: &Connection,
    db_path: &Path,
    database_namespace: &str,
    schema: &HermesSchema,
    session: &HermesSession,
    retention_days: u64,
) -> Result<bool> {
    let messages =
        load_messages(connection, &schema.messages, &session.id).context("load Hermes messages")?;
    if messages.is_empty() || session_is_expired(session, &messages, retention_days)? {
        return Ok(false);
    }
    ingest_session(
        store,
        db_path,
        database_namespace,
        schema.version,
        session,
        &messages,
    )
}

/// 调用方先 canonicalize，使相对路径和符号链接别名使用相同库身份。
pub(crate) fn database_namespace(canonical_path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agentsight-hermes-database-v1\0");
    digest.update(canonical_path.as_os_str().as_encoded_bytes());
    format!("{:x}", digest.finalize())
}

struct HermesSchema {
    version: Option<i64>,
    sessions: HashSet<String>,
    messages: HashSet<String>,
}

fn validate_schema(connection: &Connection) -> Result<HermesSchema> {
    let sessions = table_columns(connection, "sessions")?;
    let messages = table_columns(connection, "messages")?;
    for column in ["id"] {
        if !sessions.contains(column) {
            bail!("Hermes sessions table is missing required column {column}");
        }
    }
    for column in ["id", "session_id", "role", "timestamp"] {
        if !messages.contains(column) {
            bail!("Hermes messages table is missing required column {column}");
        }
    }
    let version = connection
        .query_row(
            "SELECT version FROM schema_version ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(HermesSchema {
        version,
        sessions,
        messages,
    })
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let sql = match table {
        "sessions" => "PRAGMA table_info(sessions)",
        "messages" => "PRAGMA table_info(messages)",
        _ => return Err(anyhow!("unsupported Hermes table {table}")),
    };
    let mut statement = connection.prepare(sql)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    if columns.is_empty() {
        bail!("Hermes state database has no {table} table");
    }
    Ok(columns)
}

fn sql_column(columns: &HashSet<String>, name: &str, fallback: &str) -> String {
    if columns.contains(name) {
        format!("\"{name}\"")
    } else {
        fallback.to_string()
    }
}

fn load_sessions(connection: &Connection, columns: &HashSet<String>) -> Result<Vec<HermesSession>> {
    let field = |name, fallback| sql_column(columns, name, fallback);
    let sql = format!(
        "SELECT id, {} AS source, {} AS model, {} AS parent_session_id, {} AS cwd, \
         CAST({} AS REAL) AS started_at, CAST({} AS REAL) AS ended_at, \
         CAST({} AS INTEGER) AS input_tokens, CAST({} AS INTEGER) AS output_tokens, \
         CAST({} AS INTEGER) AS cache_read_tokens, CAST({} AS INTEGER) AS cache_write_tokens, \
         CAST({} AS INTEGER) AS reasoning_tokens, CAST({} AS REAL) AS estimated_cost_usd, \
         CAST({} AS REAL) AS actual_cost_usd, CAST({} AS INTEGER) AS api_call_count \
         FROM sessions ORDER BY started_at, id",
        field("source", "'hermes'"),
        field("model", "NULL"),
        field("parent_session_id", "NULL"),
        field("cwd", "NULL"),
        field("started_at", "0"),
        field("ended_at", "NULL"),
        field("input_tokens", "0"),
        field("output_tokens", "0"),
        field("cache_read_tokens", "0"),
        field("cache_write_tokens", "0"),
        field("reasoning_tokens", "0"),
        field("estimated_cost_usd", "NULL"),
        field("actual_cost_usd", "NULL"),
        field("api_call_count", "0"),
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok(HermesSession {
            id: row.get(0)?,
            source: row.get(1)?,
            model: row.get(2)?,
            parent_session_id: row.get(3)?,
            cwd: row.get(4)?,
            started_at: row.get(5)?,
            ended_at: row.get(6)?,
            input_tokens: row.get(7)?,
            output_tokens: row.get(8)?,
            cache_read_tokens: row.get(9)?,
            cache_write_tokens: row.get(10)?,
            reasoning_tokens: row.get(11)?,
            estimated_cost_usd: row.get(12)?,
            actual_cost_usd: row.get(13)?,
            api_call_count: row.get(14)?,
        })
    })?;
    let mut sessions = Vec::new();
    for (index, row) in rows.enumerate() {
        match row {
            Ok(session) if !session.id.is_empty() => sessions.push(session),
            Ok(_) => log::warn!("Hermes sessions 第 {index} 行 ID 为空，跳过该行"),
            Err(err) => log::warn!("Hermes sessions 第 {index} 行解码失败，跳过该行: {err}"),
        }
    }
    Ok(sessions)
}

fn load_messages(
    connection: &Connection,
    columns: &HashSet<String>,
    session_id: &str,
) -> Result<Vec<HermesMessage>> {
    let text_field = |name: &str| {
        if columns.contains(name) {
            format!("CAST(\"{name}\" AS TEXT)")
        } else {
            "NULL".to_string()
        }
    };
    let active_filter = if columns.contains("active") {
        " AND COALESCE(active, 1) = 1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, role, {}, {}, {}, {}, CAST(timestamp AS REAL), {}, {}, {} \
         FROM messages WHERE session_id = ?1{active_filter} ORDER BY id",
        text_field("content"),
        text_field("tool_call_id"),
        text_field("tool_calls"),
        text_field("tool_name"),
        text_field("finish_reason"),
        text_field("reasoning"),
        text_field("reasoning_content"),
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok(HermesMessage {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                tool_call_id: row.get(3)?,
                tool_calls: row.get(4)?,
                tool_name: row.get(5)?,
                timestamp: row.get(6)?,
                finish_reason: row.get(7)?,
                reasoning: row.get(8)?,
                reasoning_content: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn session_is_expired(
    session: &HermesSession,
    messages: &[HermesMessage],
    retention_days: u64,
) -> Result<bool> {
    if retention_days == 0 {
        return Ok(false);
    }
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let cutoff = retention_cutoff_ns(now_ns, retention_days)?;
    let last_timestamp = messages
        .last()
        .map(|message| message.timestamp)
        .or(session.ended_at)
        .unwrap_or(session.started_at);
    Ok(timestamp_ns(last_timestamp) < i64::try_from(cutoff).unwrap_or(i64::MAX))
}

fn ingest_session(
    store: &TrajectoryStore,
    db_path: &Path,
    database_namespace: &str,
    schema_version: Option<i64>,
    session: &HermesSession,
    messages: &[HermesMessage],
) -> Result<bool> {
    let session_id = scoped_session_id(database_namespace, &session.id);
    let fingerprint = source_fingerprint(schema_version, session, messages);
    let last_seen_ns = messages
        .last()
        .map(|message| timestamp_ns(message.timestamp))
        .unwrap_or_else(|| timestamp_ns(session.started_at));
    let file_path = db_path.to_string_lossy().to_string();
    if store.get_source_session_state(&session_id, SOURCE, &file_path)?
        == Some((fingerprint, last_seen_ns))
    {
        return Ok(false);
    }
    // 未变更会话不读取旧 ATIF，也不重复构建新轨迹。
    let trajectory = convert_session(database_namespace, schema_version, session, messages);
    if trajectory.steps.is_empty() {
        return Ok(false);
    }

    let atif_json = serde_json::to_string(&trajectory.to_json_value()?)?;
    let (first_user_message, last_user_message) = extract_user_message_previews(&atif_json);
    let project = session
        .cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("(default)")
        .to_string();
    let record = TrajectoryRecord {
        session_id,
        schema_version: trajectory.schema_version.clone(),
        agent_name: trajectory.agent.name.clone(),
        model_name: trajectory.agent.model_name.clone(),
        num_steps: i64::try_from(trajectory.steps.len()).unwrap_or(i64::MAX),
        total_prompt_tokens: nonnegative(session.input_tokens),
        total_completion_tokens: nonnegative(session.output_tokens),
        start_time: trajectory
            .steps
            .iter()
            .find_map(|step| step.timestamp.clone()),
        end_time: trajectory
            .steps
            .iter()
            .rev()
            .find_map(|step| step.timestamp.clone()),
        first_user_message,
        last_user_message,
        atif_json,
        project,
        source: SOURCE.to_string(),
        is_subagent: false,
        file_path,
        file_size: fingerprint,
        file_mtime_ns: last_seen_ns,
    };
    store.upsert_trajectory(&record)?;
    Ok(true)
}

fn nonnegative(value: i64) -> Option<i64> {
    (value >= 0).then_some(value)
}
