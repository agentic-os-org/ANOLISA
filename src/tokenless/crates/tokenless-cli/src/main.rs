//! Tokenless CLI - LLM token optimization via schema and response compression.
mod env_check;
mod init;

use clap::{Parser, Subcommand, ValueEnum};
use std::fs;
use std::io::{self, IsTerminal as _, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use tokenless_ccr::{SqliteStore, StashStore};
use tokenless_protocol::{Disposition, Request, RequestEnvelope, Response};
use tokenless_runtime::{
    CompressOptions, CompressResult, EntryOptions, MAX_INPUT_BYTES, MIN_TOON_CHARS,
    compress_response_with_store, compress_toon, dispatch_with_store, record_compression,
    retrieve_recorded,
};
use tokenless_schema::SchemaCompressor;
use tokenless_stats::{
    CompressionMode, DiffSort, OperationType, StatsRecord, StatsRecorder, TokenlessConfig,
    estimate_tokens,
};
use tokenless_stats::{
    ensure_state_dir, format_compare, format_compare_json, format_diff_report, format_list,
    format_show, format_summary, record_report, resolve_data_dir, session_report, tool_use_report,
    validate_database_path,
};

#[derive(Parser)]
#[command(
    name = "tokenless",
    version,
    about = "LLM token optimization via schema and response compression"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run one Protocol v2 lifecycle operation from a JSON request.
    Compress {
        #[arg(short, long)]
        file: Option<String>,
        /// Override the stash database path. Defaults to
        /// $TOKENLESS_DATA_DIR/stash.db or ~/.tokenless/stash.db.
        /// Must remain under the real home or selected data directory.
        #[arg(long)]
        stash_db: Option<String>,
    },
    /// Compress OpenAI Function Calling tool schemas
    CompressSchema {
        #[arg(short, long)]
        file: Option<String>,
        /// Compress a JSON array of schemas
        #[arg(long)]
        batch: bool,
        /// Agent ID for stats (e.g. "copilot-shell")
        #[arg(long)]
        agent_id: Option<String>,
        /// Session ID for grouping
        #[arg(long)]
        session_id: Option<String>,
        /// Tool use ID
        #[arg(long)]
        tool_use_id: Option<String>,
        /// Disable reversible stash. By default, truncated descriptions are
        /// stashed so they can be retrieved via `tokenless retrieve`; this
        /// flag makes truncation lossy (the pre-stash behavior).
        #[arg(long)]
        no_stash: bool,
        /// Override the stash database path. Defaults to
        /// $TOKENLESS_DATA_DIR/stash.db or ~/.tokenless/stash.db.
        /// Must remain under the real home or selected data directory.
        #[arg(long)]
        stash_db: Option<String>,
    },
    /// Compress API responses
    CompressResponse {
        #[arg(short, long)]
        file: Option<String>,
        /// Agent ID for stats
        #[arg(long)]
        agent_id: Option<String>,
        /// Session ID for grouping
        #[arg(long)]
        session_id: Option<String>,
        /// Tool use ID
        #[arg(long)]
        tool_use_id: Option<String>,
        /// Max string length before truncation
        #[arg(long)]
        truncate_strings_at: Option<usize>,
        /// Array length that triggers truncation; the first n items are
        /// kept plus a tail window (see --array-tail-preserve). Arrays of
        /// at least 33 JSON objects use record reduction instead.
        #[arg(long)]
        truncate_arrays_at: Option<usize>,
        /// Items preserved from the tail of truncated arrays (default: 8).
        /// Does not apply to record reduction.
        #[arg(long)]
        array_tail_preserve: Option<usize>,
        /// Max nesting depth before truncation
        #[arg(long)]
        max_depth: Option<usize>,
        /// Disable reversible stash. By default, dropped array items and
        /// the complete arrays behind record reduction are stashed so they
        /// can be retrieved via `tokenless retrieve`; this flag makes
        /// truncation lossy and skips record reduction.
        #[arg(long)]
        no_stash: bool,
        /// Override the stash database path. Defaults to
        /// $TOKENLESS_DATA_DIR/stash.db or ~/.tokenless/stash.db.
        /// Must remain under the real home or selected data directory.
        #[arg(long)]
        stash_db: Option<String>,
    },
    /// Retrieve a stashed payload by its hash key. Accepts a bare 24-hex hash
    /// or any text containing a `<<tokenless:HASH>>` marker (the marker is
    /// extracted automatically).
    Retrieve {
        /// The stash hash, or a line containing a `<<tokenless:HASH>>` marker.
        hash: String,
        /// Override the stash database path. Defaults to
        /// $TOKENLESS_DATA_DIR/stash.db or ~/.tokenless/stash.db.
        #[arg(long)]
        stash_db: Option<String>,
    },
    /// View and export statistics
    #[command(subcommand)]
    Stats(StatsCommands),
    /// Encode JSON to TOON format. Payloads shorter than the minimum
    /// length (500 characters by default, matching the adapter hooks) pass
    /// through unchanged because TOON savings on small JSON are near-zero.
    /// Passthrough and no-savings runs write the original payload to stdout
    /// byte-for-byte (no trailing newline is added or stripped) and exit 0;
    /// the stderr note is informational only, so automation should detect
    /// encoding by comparing stdout with the input payload, not by parsing
    /// stderr. Encoded runs emit the TOON text without a trailing newline.
    CompressToon {
        #[arg(short, long)]
        file: Option<String>,
        /// Agent ID for stats
        #[arg(long)]
        agent_id: Option<String>,
        /// Session ID for grouping
        #[arg(long)]
        session_id: Option<String>,
        /// Tool use ID
        #[arg(long)]
        tool_use_id: Option<String>,
        /// Minimum payload length (in characters) for TOON encoding.
        /// Shorter payloads pass through unchanged; set to 0 to encode any
        /// payload that yields token savings.
        #[arg(long, value_name = "CHARS", default_value_t = MIN_TOON_CHARS)]
        min_toon_chars: usize,
    },
    /// Decode TOON format back to JSON
    DecompressToon {
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Check tool environment readiness
    EnvCheck {
        /// Check a specific tool
        #[arg(long)]
        tool: Option<String>,
        /// Check all tools
        #[arg(long)]
        all: bool,
        /// Auto-fix missing dependencies
        #[arg(long)]
        fix: bool,
        /// Output full checklist
        #[arg(long)]
        checklist: bool,
        /// Output machine-readable JSON (for hook/plugin consumption)
        #[arg(long)]
        json: bool,
    },
    /// Detect installed agent frameworks and install tokenless adapters
    Init {
        /// Install a specific framework by name (e.g. claude-code, qoder)
        #[arg(long)]
        framework: Option<String>,
        /// Install all installable frameworks without prompting
        #[arg(long)]
        all: bool,
        /// List framework status without installing
        #[arg(long)]
        list: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DiffSortArg {
    Saved,
    Time,
}

const DEFAULT_DIFF_LIMIT: usize = 20;

impl From<DiffSortArg> for DiffSort {
    fn from(value: DiffSortArg) -> Self {
        match value {
            DiffSortArg::Saved => DiffSort::Saved,
            DiffSortArg::Time => DiffSort::Time,
        }
    }
}

#[derive(Subcommand)]
enum StatsCommands {
    /// Show summary statistics with breakdown by operation
    Summary {
        #[arg(long, value_parser = parse_positive_usize)]
        limit: Option<usize>,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Compare two runs by session: baseline (compression-off) vs tokenless (compression-on).
        /// Provide exactly two session IDs.
        #[arg(long, num_args = 2, value_names = ["BASELINE_SESSION", "TOKENLESS_SESSION"])]
        compare: Option<Vec<String>>,
    },
    /// List recent records
    List {
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Show before/after text content for a specific record
    Show {
        /// Record database ID
        id: i64,
    },
    /// Explain estimated token savings with a unified content diff
    Diff {
        /// Record database ID
        #[arg(
            value_name = "RECORD_ID",
            required_unless_present = "session",
            conflicts_with = "session"
        )]
        id: Option<i64>,
        /// Session ID to summarize or inspect
        #[arg(long, required_unless_present = "id")]
        session: Option<String>,
        /// Restrict a session diff to one tool call
        #[arg(long, requires = "session")]
        tool_use_id: Option<String>,
        /// Maximum number of chains in a session overview
        #[arg(
            short,
            long,
            default_value_t = DEFAULT_DIFF_LIMIT,
            value_parser = parse_positive_usize,
            conflicts_with_all = ["id", "tool_use_id"]
        )]
        limit: usize,
        /// Session overview ordering
        #[arg(long, value_enum, conflicts_with_all = ["id", "tool_use_id"])]
        sort: Option<DiffSortArg>,
        /// Unchanged lines around each content change
        #[arg(short = 'U', long, default_value_t = 3)]
        context: usize,
        /// Disable ANSI colors even when stdout is a terminal
        #[arg(long)]
        no_color: bool,
        /// Output machine-readable JSON with structured diff hunks
        #[arg(long)]
        json: bool,
    },
    /// Clear all statistics
    Clear {
        #[arg(long)]
        yes: bool,
    },
    /// Show stats recording status
    Status,
    /// Enable stats recording
    Enable,
    /// Disable stats recording
    Disable,
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid positive integer: {value}"))?;
    if parsed == 0 {
        return Err("value must be greater than zero".to_string());
    }
    Ok(parsed)
}

fn read_input(file: &Option<String>) -> Result<String, String> {
    // Cap stream reads at MAX_INPUT_BYTES + 1 via Read::take so a hostile
    // input cannot allocate gigabytes before the size check fires. The
    // post-read length comparison catches the truncated-at-limit case so
    // we still reject (rather than silently process a partial buffer).
    let limit = MAX_INPUT_BYTES as u64 + 1;
    let too_large = || {
        format!(
            "Input exceeds {} MiB limit",
            MAX_INPUT_BYTES / (1024 * 1024)
        )
    };
    match file {
        Some(path) => {
            let mut content = String::new();
            fs::File::open(path)
                .map_err(|e| format!("Failed to open file '{path}': {e}"))?
                .take(limit)
                .read_to_string(&mut content)
                .map_err(|e| format!("Failed to read file '{path}': {e}"))?;
            if content.len() > MAX_INPUT_BYTES {
                return Err(too_large());
            }
            Ok(content)
        }
        None => {
            use std::io::IsTerminal as _;
            if io::stdin().is_terminal() {
                return Err("No input provided. Use --file <path> or pipe via stdin: echo '{...}' | tokenless <command>".to_string());
            }
            let mut buf = String::new();
            io::stdin()
                .lock()
                .take(limit)
                .read_to_string(&mut buf)
                .map_err(|e| format!("Failed to read stdin: {e}"))?;
            if buf.len() > MAX_INPUT_BYTES {
                return Err(too_large());
            }
            if buf.trim().is_empty() {
                return Err("No input received on stdin".to_string());
            }
            Ok(buf)
        }
    }
}

/// Resolve the current user's home directory.
///
/// Re-exports `tokenless_stats::get_home_dir` so both the CLI binary and
/// shared stats/config code agree on a single passwd-rooted source of
/// truth (see `tokenless_stats::home`).
pub fn get_home_dir() -> String {
    tokenless_stats::get_home_dir()
}

/// Read a state override, including its DSH-managed shell alias.
fn state_path_override(primary: &str, dsh_managed: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .filter(|path| !path.is_empty())
        .or_else(|| {
            if std::env::var("DSH_SHELL").as_deref() == Ok("1") {
                std::env::var(dsh_managed)
                    .ok()
                    .filter(|path| !path.is_empty())
            } else {
                None
            }
        })
}

/// Resolve the directory containing tokenless SQLite databases.
///
/// `TOKENLESS_DATA_DIR` affects `stats.db` and `stash.db` only and may be an
/// absolute directory outside the real home. DSH's managed shell supplies the
/// same path through `DSH_TOKENLESS_DATA_DIR` because it removes inherited
/// `TOKENLESS_*` variables. An invalid explicit override is returned as an
/// error so state never silently moves back to the default.
fn get_data_dir(home: &str) -> Result<PathBuf, String> {
    let override_path = state_path_override("TOKENLESS_DATA_DIR", "DSH_TOKENLESS_DATA_DIR");
    let home = (!home.is_empty()).then(|| Path::new(home));
    resolve_data_dir(home, override_path.as_deref()).map_err(|error| error.to_string())
}

// Lazily snapshot path inputs for one command so stats and stash share the
// same passwd lookup and data-directory validation without global caching.
#[derive(Default)]
struct DatabasePathResolver {
    home: std::sync::OnceLock<String>,
    data_dir: std::sync::OnceLock<Result<PathBuf, String>>,
}

impl DatabasePathResolver {
    fn home(&self) -> &str {
        self.home.get_or_init(get_home_dir)
    }

    fn data_dir(&self) -> Result<&Path, &str> {
        self.data_dir
            .get_or_init(|| get_data_dir(self.home()))
            .as_ref()
            .map(PathBuf::as_path)
            .map_err(String::as_str)
    }
}

/// Resolve the database path. When `TOKENLESS_STATS_DB` is set, the path
/// is validated against the user's home and selected data directory;
/// otherwise `TOKENLESS_DATA_DIR` and then the default data directory are
/// used.
fn get_db_path_with(paths: &DatabasePathResolver) -> Result<PathBuf, String> {
    let home = paths.home();
    let data_dir = paths.data_dir();
    if let Some(env_path) = state_path_override("TOKENLESS_STATS_DB", "DSH_TOKENLESS_STATS_DB") {
        match validate_db_path(Path::new(&env_path), home, data_dir.ok()) {
            Ok(path) => return Ok(path),
            Err(reason) => eprintln!("[tokenless] ignoring TOKENLESS_STATS_DB: {reason}"),
        }
    }
    let data_dir = data_dir.map_err(str::to_string)?;
    let path = data_dir.join("stats.db");
    validate_db_path(&path, home, Some(data_dir))
}

/// Validate a file-level override against the real home and selected data dir.
fn validate_db_path(path: &Path, home: &str, data_dir: Option<&Path>) -> Result<PathBuf, String> {
    let mut roots = Vec::with_capacity(2);
    if !home.is_empty() {
        roots.push(Path::new(home));
    }
    if let Some(data_dir) = data_dir {
        roots.push(data_dir);
    }
    validate_database_path(path, &roots).map_err(|error| error.to_string())
}

fn ensure_db_dir(db_path: &Path) -> Result<(), (String, i32)> {
    if let Some(parent) = db_path.parent() {
        ensure_state_dir(parent)
            .map_err(|e| (format!("Failed to create database directory: {e}"), 1))?;
    }
    Ok(())
}

fn open_recorder() -> Result<StatsRecorder, (String, i32)> {
    open_recorder_with(&DatabasePathResolver::default())
}

fn open_recorder_with(paths: &DatabasePathResolver) -> Result<StatsRecorder, (String, i32)> {
    let db_path = get_db_path_with(paths)
        .map_err(|error| (format!("Stats database unavailable: {error}"), 1))?;
    ensure_db_dir(&db_path)?;
    StatsRecorder::new(db_path).map_err(|e| (format!("Failed to open database: {e}"), 1))
}

/// Resolve the stash database path under a trusted state root.
///
/// File-level overrides may reside beneath the passwd-backed home or the
/// selected data directory. Callers fail open when no valid data directory is
/// available rather than writing state to an unintended fallback location.
fn get_stash_db_path_with(
    paths: &DatabasePathResolver,
    override_path: Option<&str>,
) -> Result<PathBuf, String> {
    let home = paths.home();
    let data_dir = paths.data_dir();
    // An explicit --stash-db override is validated the same way as the
    // TOKENLESS_STASH_DB env var: on rejection we warn AND fall back to the
    // selected data directory, so a bad file override does not silently drop
    // recoverability. An invalid data-directory override still fails closed.
    if let Some(p) = override_path.filter(|s| !s.is_empty()) {
        match validate_db_path(Path::new(p), home, data_dir.ok()) {
            Ok(valid) => return Ok(valid),
            Err(reason) => eprintln!("[tokenless] rejecting --stash-db {p}: {reason}"),
        }
    }
    if let Some(env_path) = state_path_override("TOKENLESS_STASH_DB", "DSH_TOKENLESS_STASH_DB") {
        match validate_db_path(Path::new(&env_path), home, data_dir.ok()) {
            Ok(path) => return Ok(path),
            Err(reason) => eprintln!("[tokenless] ignoring TOKENLESS_STASH_DB: {reason}"),
        }
    }
    let data_dir = data_dir.map_err(str::to_string)?;
    let path = data_dir.join("stash.db");
    validate_db_path(&path, home, Some(data_dir))
}

/// Open a stash store, returning the specific failure cause. Used by
/// user-initiated paths (`retrieve`) where a generic "unavailable" message
/// would hide the real reason (invalid data dir, path rejected, corrupt DB, …).
fn open_stash_store_or_err(override_path: Option<&str>) -> Result<Arc<dyn StashStore>, String> {
    open_stash_store_or_err_with(&DatabasePathResolver::default(), override_path)
}

fn open_stash_store_or_err_with(
    paths: &DatabasePathResolver,
    override_path: Option<&str>,
) -> Result<Arc<dyn StashStore>, String> {
    let path = get_stash_db_path_with(paths, override_path)?;
    ensure_db_dir(&path).map_err(|(message, _)| message)?;
    SqliteStore::new(&path)
        .map_err(|e| format!("cannot open stash db at {}: {}", path.display(), e))
        .map(|s| Arc::new(s) as Arc<dyn StashStore>)
}

/// Open a stash store, failing open to `None` on any error. Compression
/// proceeds without stash (lossy truncation) when no valid data directory is
/// available, the parent cannot be created, or the database cannot be opened.
fn open_stash_store_with(
    paths: &DatabasePathResolver,
    override_path: Option<&str>,
) -> Option<Arc<dyn StashStore>> {
    match open_stash_store_or_err_with(paths, override_path) {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("[tokenless] stash disabled: {e}");
            None
        }
    }
}

fn run() -> Result<(), (String, i32)> {
    let cli = Cli::parse();
    run_command(cli.command)
}

fn run_command(command: Commands) -> Result<(), (String, i32)> {
    match command {
        Commands::Compress { file, stash_db } => {
            let input = read_input(&file).map_err(|e| (e, 2))?;
            // Undecodable or version-mismatched requests carry no content to
            // echo, so no fail-open response can be built: exit 2.
            let request = RequestEnvelope::from_json(&input).map_err(|e| (e.to_string(), 2))?;

            // PostTool dry-runs still open the configured store to verify that
            // recovery is available. The pipeline substitutes an in-memory
            // store for candidate generation, so the persistent store receives
            // no Stash entries while the original result is emitted.
            let config = TokenlessConfig::load();
            let database_paths = DatabasePathResolver::default();
            let compression_on = config.is_compression_enabled();
            let needs_stash = match &request.request {
                Request::BeforeModel(value) => {
                    compression_on
                        && matches!(
                            value.capabilities.recovery,
                            tokenless_protocol::RecoveryMethod::Tool { .. }
                        )
                }
                Request::PostTool(value) => value.capabilities.recovery.is_available(),
                Request::Retrieve(_) => true,
                Request::PreTool(_) => false,
            };
            let stash = needs_stash
                .then(|| open_stash_store_or_err_with(&database_paths, stash_db.as_deref()))
                .transpose()
                .map_err(|error| (error, 1))?;
            let recorder = if config.is_stats_enabled() {
                open_recorder_with(&database_paths).ok()
            } else {
                None
            };
            let rtk_path = matches!(&request.request, Request::PreTool(_))
                .then(env_check::resolve_rtk_path)
                .flatten()
                .map(PathBuf::from);
            let rtk_data_dir = if matches!(&request.request, Request::PreTool(_)) {
                Some(
                    database_paths
                        .data_dir()
                        .map_err(|error| (error.to_owned(), 1))?
                        .to_path_buf(),
                )
            } else {
                None
            };
            let outcome = dispatch_with_store(
                &request,
                &EntryOptions {
                    compression_enabled: compression_on,
                    search_path_sharing_enabled: std::env::var(
                        "TOKENLESS_SEARCH_PATH_SHARING_ENABLED",
                    )
                    .map_or(true, |value| {
                        value == "1"
                            || value.eq_ignore_ascii_case("true")
                            || value.eq_ignore_ascii_case("yes")
                    }),
                    stash_enabled: true,
                    rtk_path,
                    rtk_data_dir,
                },
                stash.as_ref(),
                recorder.as_ref(),
            )
            .map_err(|error| (error.to_string(), 1))?;
            if let Response::PostTool(response) = &outcome.response.response
                && response.disposition == Disposition::NoSavings
            {
                eprintln!("tokenless: compression did not reduce size, outputting original");
            }

            let response_json = outcome.response.to_json().map_err(|e| (e.to_string(), 1))?;
            println!("{response_json}");

            record_compression(
                &request.attribution,
                &outcome,
                recorder.as_ref(),
                config.is_sls_enabled(),
            );
        }
        Commands::CompressSchema {
            file,
            batch,
            agent_id,
            session_id,
            tool_use_id,
            no_stash,
            stash_db,
        } => {
            let input = read_input(&file).map_err(|e| (e, 2))?;
            let value: serde_json::Value =
                serde_json::from_str(&input).map_err(|e| (format!("JSON parse error: {e}"), 2))?;

            // Load config before deciding on the stash so we can skip it
            // entirely when compression is disabled (dry-run). Attaching the
            // stash in dry-run would write entries whose `<<tokenless:KEY>>`
            // markers never reach the LLM (the original input is emitted),
            // orphaning them.
            let config = TokenlessConfig::load();
            let database_paths = DatabasePathResolver::default();
            let compression_on = config.is_compression_enabled();
            let stash = if no_stash || !compression_on {
                None
            } else {
                open_stash_store_with(&database_paths, stash_db.as_deref())
            };
            let mut compressor = SchemaCompressor::new();
            if let Some(ref store) = stash {
                compressor = compressor.with_stash_store(store.clone());
            }

            let after_compact = if batch || value.is_array() {
                let arr = value
                    .as_array()
                    .ok_or_else(|| ("Expected a JSON array for --batch mode".to_string(), 1))?;
                let results: Vec<serde_json::Value> =
                    arr.iter().map(|item| compressor.compress(item)).collect();
                serde_json::to_string(&results).unwrap_or_default()
            } else {
                let result = compressor.compress(&value);
                serde_json::to_string(&result).unwrap_or_default()
            };

            let before_tokens = estimate_tokens(&input);
            let after_tokens = estimate_tokens(&after_compact);
            let output_text = if after_tokens >= before_tokens {
                eprintln!(
                    "tokenless: schema compression did not reduce size ({before_tokens} -> {after_tokens} est. tokens), outputting original"
                );
                // Discarded compressed output never reaches the LLM, so roll
                // back stash keys created during this compress — otherwise
                // markers live only in `after_compact` and orphan stash rows.
                compressor.rollback_stash_writes();
                input.clone()
            } else {
                after_compact.clone()
            };

            let stash_writes = stash.as_ref().map(|_| compressor.stash_writes());
            let stash_errors = stash.as_ref().map(|_| compressor.stash_errors());
            let stash_size = stash.as_ref().map(|s| s.len());
            if matches!(stash_errors, Some(e) if e > 0) {
                eprintln!(
                    "[tokenless] stash: {} stash operation(s) failed; truncated entries are not retrievable (check stash db health)",
                    stash_errors.expect("checked Some above")
                );
            }

            let mode = resolve_mode(compression_on, before_tokens, after_tokens);
            let emit_text = if compression_on {
                output_text.clone()
            } else {
                input.clone()
            };
            println!("{emit_text}");

            record_compression_stats(
                &config,
                &database_paths,
                OperationType::CompressSchema,
                agent_id,
                session_id,
                tool_use_id,
                input,
                output_text,
                mode,
                stash_writes,
                stash_errors,
                stash_size,
            );
        }
        Commands::CompressResponse {
            file,
            agent_id,
            session_id,
            tool_use_id,
            truncate_strings_at,
            truncate_arrays_at,
            array_tail_preserve,
            max_depth,
            no_stash,
            stash_db,
        } => {
            let input = read_input(&file).map_err(|e| (e, 2))?;
            // Load config before deciding on the stash so we can skip it
            // entirely when compression is disabled (dry-run). Attaching the
            // stash in dry-run would write entries whose `<<tokenless:KEY>>`
            // markers never reach the LLM (the original input is emitted),
            // orphaning them.
            let config = TokenlessConfig::load();
            let database_paths = DatabasePathResolver::default();
            let compression_on = config.is_compression_enabled();
            let stash = if no_stash || !compression_on {
                None
            } else {
                open_stash_store_with(&database_paths, stash_db.as_deref())
            };
            let result = compress_response_with_store(
                &input,
                &CompressOptions {
                    truncate_strings_at,
                    truncate_arrays_at,
                    array_tail_preserve,
                    max_depth,
                    stash_enabled: !no_stash,
                    // Preserve the existing CLI contract: stash failure is
                    // visible but does not block lossy compression. Embedded
                    // frontends can require reversible output instead.
                    require_reversible: false,
                },
                compression_on,
                stash.as_ref(),
            )
            .map_err(|error| {
                let code = if matches!(
                    error,
                    tokenless_runtime::RuntimeError::InvalidJson(_)
                        | tokenless_runtime::RuntimeError::InputTooLarge { .. }
                ) {
                    2
                } else {
                    1
                };
                (error.to_string(), code)
            })?;
            // Surface persistent stash backend failures (disk full, locked DB,
            // I/O, rollback delete) so they aren't invisible. `stash_errors`
            // counts both compress-time stash writes and no-savings rollback
            // deletes; a non-zero count means the stash path is broken and
            // retrievals will miss.
            if matches!(result.stash_errors, Some(errors) if errors > 0) {
                eprintln!(
                    "[tokenless] stash: {} stash operation(s) failed; truncated entries are not retrievable (check stash db health)",
                    result.stash_errors.expect("checked Some above")
                );
            }
            if result.disposition == Disposition::NoSavings {
                eprintln!(
                    "tokenless: response compression did not reduce size ({} -> {} est. tokens), outputting original",
                    result.before_tokens, result.after_tokens
                );
            }

            let mode = resolve_mode(compression_on, result.before_tokens, result.after_tokens);
            let output_text = stats_after_text(&result, &input);
            println!("{}", result.output);

            record_compression_stats(
                &config,
                &database_paths,
                OperationType::CompressResponse,
                agent_id,
                session_id,
                tool_use_id,
                input,
                output_text,
                mode,
                result.stash_writes,
                result.stash_errors,
                result.stash_size,
            );
        }
        Commands::Retrieve { hash, stash_db } => {
            let store = match open_stash_store_or_err(stash_db.as_deref()) {
                Ok(s) => s,
                Err(e) => {
                    return Err((format!("stash unavailable: {e}"), 1));
                }
            };
            // Retrieve events are attribution, never a gate: a recorder that
            // fails to open degrades to unrecorded retrieval.
            let recorder = if TokenlessConfig::load().is_stats_enabled() {
                open_recorder_with(&DatabasePathResolver::default()).ok()
            } else {
                None
            };
            let payload = retrieve_recorded(store.as_ref(), &hash, recorder.as_ref(), "cli")
                .map_err(|error| (error.to_string(), 1))?;
            // Byte-exact restore: do not append a trailing newline.
            let mut out = io::stdout().lock();
            out.write_all(payload.as_bytes())
                .map_err(|e| (format!("failed to write retrieved payload: {e}"), 1))?;
            out.flush()
                .map_err(|e| (format!("failed to flush retrieved payload: {e}"), 1))?;
        }
        Commands::Stats(stats_cmd) => {
            match stats_cmd {
                StatsCommands::Summary {
                    limit,
                    json,
                    compare,
                } => {
                    let recorder = open_recorder()?;
                    if let Some(sessions) = compare {
                        let baseline_sid = sessions[0].as_str();
                        let tokenless_sid = sessions[1].as_str();
                        let baseline = recorder
                            .records_by_session(baseline_sid, limit)
                            .map_err(|e| (format!("Failed to query baseline: {e}"), 1))?;
                        let tokenless = recorder
                            .records_by_session(tokenless_sid, limit)
                            .map_err(|e| (format!("Failed to query tokenless: {e}"), 1))?;
                        if let Some(message) = missing_compare_sessions(
                            baseline_sid,
                            tokenless_sid,
                            &baseline,
                            &tokenless,
                        ) {
                            return Err((message, 1));
                        }
                        // Warn if a session's records do not match the expected mode,
                        // i.e. the baseline run was not recorded as dry-run.
                        warn_mode_mismatch("baseline", &baseline, CompressionMode::DryRun);
                        warn_mode_mismatch("tokenless", &tokenless, CompressionMode::Active);
                        if json {
                            println!("{}", format_compare_json(&baseline, &tokenless));
                        } else {
                            println!("{}", format_compare(&baseline, &tokenless));
                        }
                        return Ok(());
                    }
                    let records = recorder
                        .all_records(limit)
                        .map_err(|e| (format!("Failed to query records: {e}"), 1))?;
                    let retrieve = recorder
                        .retrieve_totals()
                        .map_err(|e| (format!("Failed to query retrieve events: {e}"), 1))?;
                    if json {
                        println!(
                            "{}",
                            tokenless_stats::format_summary_json(&records, None, Some(&retrieve))
                        );
                    } else {
                        println!(
                            "{}",
                            format_summary(
                                &records,
                                Some("Tokenless Statistics Summary"),
                                None,
                                Some(&retrieve)
                            )
                        );
                    }
                }
                StatsCommands::List { limit } => {
                    let recorder = open_recorder()?;
                    let records = recorder
                        .all_records(Some(limit))
                        .map_err(|e| (format!("Failed to query records: {e}"), 1))?;
                    println!("{}", format_list(&records, limit));
                }
                StatsCommands::Show { id } => {
                    let recorder = open_recorder()?;
                    let record = recorder
                        .record_by_id(id)
                        .map_err(|e| (format!("Failed to query record: {e}"), 1))?
                        .ok_or_else(|| (format!("Record not found: {id}"), 1))?;
                    println!("{}", format_show(&record));
                }
                StatsCommands::Diff {
                    id,
                    session,
                    tool_use_id,
                    limit,
                    sort,
                    context,
                    no_color,
                    json,
                } => {
                    let recorder = open_recorder()?;
                    let report = match (id, session) {
                        (Some(id), None) => {
                            let record = recorder
                                .record_by_id(id)
                                .map_err(|e| (format!("Failed to query record: {e}"), 1))?
                                .ok_or_else(|| (format!("Record not found: {id}"), 1))?;
                            record_report(&record, context)
                        }
                        (None, Some(session_id)) => {
                            let records = recorder
                                .records_for_diff(&session_id, tool_use_id.as_deref())
                                .map_err(|e| (format!("Failed to query diff records: {e}"), 1))?;
                            if records.is_empty() {
                                let scope = tool_use_id.as_deref().map_or_else(
                                    || format!("session {session_id:?}"),
                                    |tool| format!("tool use {tool:?} in session {session_id:?}"),
                                );
                                return Err((format!("No records found for {scope}"), 1));
                            }
                            if let Some(tool_use_id) = tool_use_id {
                                tool_use_report(&records, &session_id, &tool_use_id, context)
                            } else {
                                session_report(
                                    &records,
                                    &session_id,
                                    limit,
                                    sort.map(DiffSort::from).unwrap_or_default(),
                                )
                            }
                        }
                        _ => {
                            return Err((
                                "Specify exactly one of RECORD_ID or --session".to_string(),
                                2,
                            ));
                        }
                    };
                    if json {
                        let output = serde_json::to_string_pretty(&report)
                            .map_err(|e| (format!("Failed to serialize diff report: {e}"), 1))?;
                        println!("{output}");
                    } else {
                        let color = !no_color
                            && std::env::var_os("NO_COLOR").is_none()
                            && io::stdout().is_terminal();
                        println!("{}", format_diff_report(&report, color));
                    }
                }
                StatsCommands::Clear { yes } => {
                    let recorder = open_recorder()?;
                    if !yes {
                        print!("Are you sure you want to clear all statistics? [y/N] ");
                        let _ = io::stdout().flush();
                        let mut input = String::new();
                        if io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
                            println!("Cancelled.");
                            return Ok(());
                        }
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("Cancelled.");
                            return Ok(());
                        }
                    }
                    recorder
                        .clear()
                        .map_err(|e| (format!("Failed to clear: {e}"), 1))?;
                    println!("Statistics cleared.");
                }
                StatsCommands::Status => {
                    let stats_env_set = std::env::var("TOKENLESS_STATS_ENABLED")
                        .ok()
                        .filter(|v| !v.is_empty());
                    let sls_env_set = std::env::var("TOKENLESS_SLS_ENABLED")
                        .ok()
                        .filter(|v| !v.is_empty());
                    let config = TokenlessConfig::load();
                    let file_exists = TokenlessConfig::config_file_exists();

                    let stats_state = if config.is_stats_enabled() {
                        "ENABLED"
                    } else {
                        "DISABLED"
                    };
                    let stats_source = if stats_env_set.is_some() {
                        "env override"
                    } else if file_exists {
                        "config file"
                    } else {
                        "default"
                    };
                    println!("Stats recording: {stats_state} (via {stats_source})");

                    let sls_state = if config.is_sls_enabled() {
                        "ENABLED"
                    } else {
                        "DISABLED"
                    };
                    let sls_source = if sls_env_set.is_some() {
                        "env override"
                    } else if file_exists {
                        "config file"
                    } else {
                        "default"
                    };
                    println!("SLS recording:   {sls_state} (via {sls_source})");
                }
                StatsCommands::Enable => {
                    persist_stats_enabled(true)?;
                    println!("Stats recording enabled.");
                }
                StatsCommands::Disable => {
                    persist_stats_enabled(false)?;
                    println!("Stats recording disabled.");
                }
            }
        }
        Commands::CompressToon {
            file,
            agent_id,
            session_id,
            tool_use_id,
            min_toon_chars,
        } => {
            let input = read_input(&file).map_err(|e| (e, 2))?;
            let config = TokenlessConfig::load();
            let compression_on = config.is_compression_enabled();
            // Share runtime scoring so CLI dry-run predictions match
            // `stats summary` and the Python/SDK compress_toon path. The
            // previous bytes/4 heuristic under-counted CJK. The shared
            // minimum-length gate keeps the CLI aligned with the adapter
            // hooks, which skip payloads under the same threshold.
            let result =
                compress_toon(&input, compression_on, min_toon_chars).map_err(|error| {
                    // JSON parse, oversized input, and TOON encode keep the
                    // documented compress-command exit code 2.
                    let code = if matches!(
                        error,
                        tokenless_runtime::RuntimeError::InvalidJson(_)
                            | tokenless_runtime::RuntimeError::InputTooLarge { .. }
                            | tokenless_runtime::RuntimeError::ToonEncode(_)
                    ) {
                        2
                    } else {
                        1
                    };
                    (error.to_string(), code)
                })?;
            match result.disposition {
                Disposition::Passthrough => {
                    eprintln!(
                        "tokenless: payload under the {min_toon_chars}-character TOON minimum ({} chars), skipping encoding and outputting original JSON",
                        input.chars().count()
                    );
                }
                Disposition::NoSavings => {
                    eprintln!(
                        "tokenless: TOON encoding did not reduce size ({} -> {} est. tokens), outputting original JSON",
                        result.before_tokens, result.after_tokens
                    );
                }
                _ => {}
            }

            let mode = resolve_mode(compression_on, result.before_tokens, result.after_tokens);
            // Byte-exact stdout: the documented detection contract compares
            // stdout with the input payload, so passthrough and no-savings
            // runs must reproduce the input verbatim. `println!` would append
            // a LF (added for inputs without one, doubled for inputs with
            // one) and break that comparison.
            {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(result.output.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|e| (format!("Failed to write output: {e}"), 1))?;
            }

            // Recorded `after` = the predicted TOON result (or original when
            // TOON did not reduce size), so dry-run captures the prediction.
            let record_after = stats_after_text(&result, &input);
            let database_paths = DatabasePathResolver::default();
            record_compression_stats(
                &config,
                &database_paths,
                OperationType::CompressToon,
                agent_id,
                session_id,
                tool_use_id,
                input,
                record_after,
                mode,
                None,
                None,
                None,
            );
        }
        Commands::DecompressToon { file } => {
            let input = read_input(&file).map_err(|e| (e, 2))?;
            let value: serde_json::Value = toon_format::decode_default(&input)
                .map_err(|e| (format!("toon decode failed: {e}"), 2))?;
            let output = serde_json::to_string_pretty(&value)
                .map_err(|e| (format!("Serialization error: {e}"), 2))?;
            let output = output.trim_end().to_string();
            if !output.is_empty() {
                println!("{output}");
            }
        }
        Commands::EnvCheck {
            tool,
            all,
            fix,
            checklist,
            json,
        } => {
            env_check::run(tool.as_deref(), all, fix, checklist, json)?;
        }
        Commands::Init {
            framework,
            all,
            list,
        } => {
            init::run(framework, all, list)?;
        }
    }

    Ok(())
}

/// Text recorded as the statistics "after" side for one compression result.
///
/// Only `Applied` and `DryRun` measure the candidate. Every other
/// disposition (no-savings, timeout, passthrough, recoverability, tool error)
/// emitted the original, so recording the discarded candidate would book
/// savings that never reached the model. Mirrors the Runtime's
/// `record_stats` selection; `record_compression_stats` then skips records
/// whose after side is not smaller.
fn stats_after_text(result: &CompressResult, input: &str) -> String {
    match result.disposition {
        Disposition::Applied | Disposition::DryRun => result.compressed_output.clone(),
        _ => input.to_string(),
    }
}

/// Resolve the recording mode from the compression toggle.
///
/// When compression is disabled (dry-run), the original input is emitted so
/// the LLM context stays uncompressed, but the predicted savings are still
/// recorded — enabling A/B comparison of the same task with/without
/// compression.
fn resolve_mode(
    compression_on: bool,
    before_tokens: usize,
    after_tokens: usize,
) -> CompressionMode {
    if compression_on {
        CompressionMode::Active
    } else {
        eprintln!(
            "tokenless: dry-run mode (compression disabled) — emitted original, predicted {before_tokens} -> {after_tokens} est. tokens"
        );
        CompressionMode::DryRun
    }
}

/// Error text when a `--compare` side has no recorded stats.
///
/// An empty side used to format as a successful 0% report, which hid typos
/// and made A/B scripts look like "no savings". Fail closed like
/// `stats diff --session` and the Python `TokenlessStats.compare` client.
fn missing_compare_sessions(
    baseline_sid: &str,
    tokenless_sid: &str,
    baseline: &[StatsRecord],
    tokenless: &[StatsRecord],
) -> Option<String> {
    let mut missing = Vec::new();
    if baseline.is_empty() {
        missing.push(format!("baseline session {baseline_sid:?}"));
    }
    if tokenless.is_empty() {
        missing.push(format!("tokenless session {tokenless_sid:?}"));
    }
    (!missing.is_empty()).then(|| format!("No records found for {}", missing.join(" and ")))
}

/// Warn (to stderr) when a session's records were not recorded in the expected
/// mode, e.g. a "baseline" session that was not run with compression disabled.
/// A non-blocking sanity hint — comparison still proceeds.
fn warn_mode_mismatch(label: &str, records: &[StatsRecord], expected: CompressionMode) {
    if records.is_empty() {
        return;
    }
    let mismatched = records.iter().filter(|r| r.mode != expected).count();
    if mismatched > 0 {
        eprintln!(
            "tokenless: warning — {} session has {} record(s) not in {} mode (comparison may be inaccurate)",
            label,
            mismatched,
            expected.as_str()
        );
    }
}

/// Persist only the stats recording toggle to `config.json`.
///
/// Starts from the on-disk file rather than [`TokenlessConfig::load`], so a
/// session `TOKENLESS_COMPRESSION_ENABLED` / `TOKENLESS_SLS_ENABLED` override
/// is not copied into durable config. Runtime `stats status` still uses
/// [`TokenlessConfig::load`] and therefore still honors those env vars.
fn persist_stats_enabled(enabled: bool) -> Result<(), (String, i32)> {
    let config = stats_persist_snapshot(TokenlessConfig::load_from_file(), enabled);
    config
        .save()
        .map_err(|e| (format!("Failed to save config: {e}"), 1))?;
    Ok(())
}

/// Snapshot written by `stats enable` / `stats disable`.
///
/// `file_config` is the on-disk document with no process-env merge. Only
/// `stats_enabled` changes; compression and SLS stay as stored.
fn stats_persist_snapshot(file_config: TokenlessConfig, enabled: bool) -> TokenlessConfig {
    let mut config = file_config;
    config.stats_enabled = enabled;
    config
}

/// Record compression stats — fail-silent so compression output
/// is never blocked by database errors.
///
/// All metrics (chars, tokens) are derived from actual text content,
/// never from caller-supplied estimates.
#[allow(clippy::too_many_arguments)]
fn record_compression_stats(
    config: &TokenlessConfig,
    database_paths: &DatabasePathResolver,
    op: OperationType,
    agent_id: Option<String>,
    session_id: Option<String>,
    tool_use_id: Option<String>,
    before_text: String,
    after_text: String,
    mode: CompressionMode,
    stash_writes: Option<usize>,
    stash_errors: Option<usize>,
    stash_size: Option<usize>,
) {
    // Short-circuit only if both stats and SLS are disabled.
    if !config.is_stats_enabled() && !config.is_sls_enabled() {
        return;
    }

    let before_bytes = before_text.len();
    let after_bytes = after_text.len();

    // Skip recording if there was no actual token savings
    let before_tokens = estimate_tokens(&before_text);
    let after_tokens = estimate_tokens(&after_text);
    if after_tokens >= before_tokens {
        return;
    }

    let pid = std::process::id();
    let agent = agent_id
        .as_deref()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "cli".to_string());
    let mut record = StatsRecord::new(
        op,
        agent,
        before_bytes,
        before_tokens,
        after_bytes,
        after_tokens,
    )
    .with_before_text(before_text)
    .with_after_text(after_text);
    if let Some(sid) = session_id {
        record = record.with_session_id(sid);
    }
    if let Some(tuid) = tool_use_id {
        record = record.with_tool_use_id(tuid);
    }
    record = record
        .with_source_pid(pid as i64)
        .with_mode(mode)
        .with_stash(stash_writes, stash_errors, stash_size);

    // SQLite stats recording — gated by stats_enabled
    if config.is_stats_enabled()
        && let Ok(recorder) = open_recorder_with(database_paths)
    {
        let _ = recorder.record(&record);
    }

    // SLS recording — fail-silent, independent of SQLite
    if config.is_sls_enabled() {
        let writer = tokenless_stats::SlsWriter::new();
        writer.write(&record);
    }
}

fn main() {
    if let Err((msg, code)) = run() {
        eprintln!("Error: {msg}");
        process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/main_tests.rs");
}
