//! Compile-time parity ledger between v1's public API and v2's.
//!
//! This file lives in `asc-event-sink` because it is the convergence point of the
//! dependency graph: from here every migrated crate is reachable in one place.
//!
//! Two things are enforced, and the distinction matters:
//!
//! - [`V1_TO_V2`] is the **ledger**: one row per v1 public symbol, naming its v2
//!   counterpart. A test asserts no row is left unmapped, so an omission is loud.
//! - [`reference_every_v2_symbol`] **uses** each v2 counterpart for real — taking
//!   a typed function pointer where the signature is worth pinning, and resolving
//!   the path otherwise. A renamed or deleted item fails to *compile* rather than
//!   quietly passing a test.

use std::path::{Path, PathBuf};

use asc_event_log::{
    DEFAULT_BACKUP_COUNT, DEFAULT_ERROR_PREFIX, DEFAULT_MAX_BYTES,
    DEFAULT_OBSERVABILITY_BACKUP_COUNT, DEFAULT_OBSERVABILITY_MAX_BYTES, EventLogError,
    JsonlEventWriter, ObservabilityWriter, SecurityEventWriter, is_backup_suffix,
};
use asc_observability::{
    DEFAULT_OBSERVABILITY_RETENTION_DAYS, OBSERVABILITY_HOOKS, OBSERVABILITY_LOG_PREFIX,
    OBSERVABILITY_SQLITE_SCHEMA_VERSION, OBSERVABILITY_STREAM, ObservabilityHook,
    ObservabilityMetadata, ObservabilityRecord, RunSummary, SessionSummary,
    USER_INPUT_PREVIEW_LIMIT, allowed_metrics_for_hook, hook_metric_allowlist,
};
use asc_persistence_sqlite::observability::{
    OBSERVABILITY_TABLES, ObservabilityEventRepository, ObservabilityReader,
    ObservabilitySqliteWriter,
};
use asc_persistence_sqlite::security_events::{
    DropSink, EventFilters, SECURITY_EVENTS_TABLES, SecurityEventRepository,
    SecurityEventsFaultPolicy, SecurityEventsMigrator, SqliteEventReader, SqliteEventWriter,
    StderrDropSink, VALID_GROUP_FIELDS, VERDICT_MIGRATION_BATCH_SIZE,
};
use asc_security_events::config::{
    get_data_dir, get_db_path, get_log_path, get_stream_db_path, get_stream_log_path,
    resolve_data_dir, validate_stream_name,
};
use asc_security_events::timestamp::{
    epoch_to_utc_iso, format_utc_iso, normalize_iso_to_utc_iso, now_iso, utc_iso_to_epoch,
};
use asc_security_events::{
    ConfigError, EventResult, SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS,
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SECURITY_EVENTS_VERDICT_SCHEMA_VERSION, SecurityEvent,
    extract_verdict,
};
use asc_security_summary::{NO_EVENTS, format_summary, format_summary_at};
use asc_sqlite_kernel::{
    DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS, ReadOnlySource, SchemaMigrator, SqliteSink,
    SqliteStore, current_epoch, ensure_schema, ensure_schema_if_needed, is_busy, is_corruption,
    is_schema, is_valid_identifier, normalize_sqlite_path, open_connection,
    run_sqlite_maintenance_if_due, sqlite_database_files, warn_readonly_schema_readiness,
};

/// One row per v1 public symbol: `(v1 symbol, v2 counterpart, note)`.
///
/// The note column carries the reason whenever the name changed. An empty note
/// means a straight rename-free port. The literal `未迁移` is forbidden.
const V1_TO_V2: &[(&str, &str, &str)] = &[
    // --- security_events: event envelope and configuration -----------------
    (
        "security_events.schema.SecurityEvent",
        "asc_security_events::SecurityEvent",
        "",
    ),
    (
        "SecurityEvent.result literals",
        "asc_security_events::EventResult",
        "a two-variant enum replaces the free-form string",
    ),
    (
        "security_events.schema.extract_verdict",
        "asc_security_events::extract_verdict",
        "",
    ),
    (
        "security_events.config.get_data_dir",
        "asc_security_events::config::get_data_dir",
        "",
    ),
    (
        "security_events.config.get_log_path",
        "asc_security_events::config::get_log_path",
        "",
    ),
    (
        "security_events.config.get_db_path",
        "asc_security_events::config::get_db_path",
        "",
    ),
    (
        "security_events.config.get_stream_log_path",
        "asc_security_events::config::get_stream_log_path",
        "",
    ),
    (
        "security_events.config.get_stream_db_path",
        "asc_security_events::config::get_stream_db_path",
        "",
    ),
    (
        "security_events.config._resolve_data_dir",
        "asc_security_events::config::resolve_data_dir",
        "promoted to public so the fallback tiers are testable without env mutation",
    ),
    (
        "security_events.config._validate_stream",
        "asc_security_events::config::validate_stream_name",
        "promoted to public: the stream name reaches a path, so its regex is a boundary",
    ),
    (
        "utils.timestamp.now_iso",
        "asc_security_events::timestamp::now_iso",
        "",
    ),
    (
        "utils.timestamp.isoformat rendering",
        "asc_security_events::timestamp::format_utc_iso",
        "explicit function because Python omits the fraction when microsecond == 0",
    ),
    (
        "utils.timestamp.parse_iso",
        "asc_security_events::timestamp::parse_iso",
        "referenced through asc-security-summary; naive policy became an explicit argument",
    ),
    (
        "utils.timestamp.normalize_iso_to_utc_iso",
        "asc_security_events::timestamp::normalize_iso_to_utc_iso",
        "",
    ),
    (
        "utils.timestamp.utc_iso_to_epoch",
        "asc_security_events::timestamp::utc_iso_to_epoch",
        "",
    ),
    (
        "utils.timestamp.epoch_to_utc_iso",
        "asc_security_events::timestamp::epoch_to_utc_iso",
        "",
    ),
    (
        "security_events.schema_version.SECURITY_EVENTS_SQLITE_SCHEMA_VERSION",
        "asc_security_events::SECURITY_EVENTS_SQLITE_SCHEMA_VERSION",
        "",
    ),
    (
        "security_events.schema_version.SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS",
        "asc_security_events::SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS",
        "",
    ),
    (
        "security_events.schema_version.SECURITY_EVENTS_VERDICT_SCHEMA_VERSION",
        "asc_security_events::SECURITY_EVENTS_VERDICT_SCHEMA_VERSION",
        "",
    ),
    // --- JSONL append log ---------------------------------------------------
    (
        "security_events.writer.JsonlEventWriter",
        "asc_event_log::JsonlEventWriter",
        "",
    ),
    (
        "security_events.writer.SecurityEventWriter",
        "asc_event_log::SecurityEventWriter",
        "",
    ),
    (
        "security_events.writer.DEFAULT_MAX_BYTES",
        "asc_event_log::DEFAULT_MAX_BYTES",
        "",
    ),
    (
        "security_events.writer.DEFAULT_BACKUP_COUNT",
        "asc_event_log::DEFAULT_BACKUP_COUNT",
        "",
    ),
    (
        "security_events.writer error_prefix default",
        "asc_event_log::DEFAULT_ERROR_PREFIX",
        "kept although v1 never reads it; see ISSUES D-03",
    ),
    (
        "security_events.writer backup-name regex",
        "asc_event_log::is_backup_suffix",
        "the retention filter became a named predicate",
    ),
    (
        "observability.writer.ObservabilityWriter",
        "asc_event_log::ObservabilityWriter",
        "",
    ),
    (
        "observability.writer max_bytes default",
        "asc_event_log::DEFAULT_OBSERVABILITY_MAX_BYTES",
        "",
    ),
    (
        "observability.writer backup_count default",
        "asc_event_log::DEFAULT_OBSERVABILITY_BACKUP_COUNT",
        "",
    ),
    // --- SQLite kernel ------------------------------------------------------
    (
        "security_events.orm_store.SqliteStore",
        "asc_sqlite_kernel::SqliteStore",
        "",
    ),
    (
        "security_events.orm_store.create_sqlite_engine",
        "asc_sqlite_kernel::open_connection",
        "no ORM engine in v2; a configured Connection is the equivalent",
    ),
    (
        "security_events.orm_store.ensure_schema",
        "asc_sqlite_kernel::ensure_schema",
        "",
    ),
    (
        "security_events.orm_store.ensure_schema_if_needed",
        "asc_sqlite_kernel::ensure_schema_if_needed",
        "",
    ),
    (
        "security_events.orm_store.warn_readonly_schema_readiness",
        "asc_sqlite_kernel::warn_readonly_schema_readiness",
        "",
    ),
    (
        "security_events.orm_store.normalize_sqlite_path",
        "asc_sqlite_kernel::normalize_sqlite_path",
        "",
    ),
    (
        "security_events.orm_store.sqlite_database_files",
        "asc_sqlite_kernel::sqlite_database_files",
        "",
    ),
    (
        "security_events.orm_store._IDENTIFIER_RE",
        "asc_sqlite_kernel::is_valid_identifier",
        "the ALTER TABLE allowlist became a named predicate",
    ),
    (
        "security_events.orm_store.SchemaMigration",
        "asc_sqlite_kernel::SchemaMigrator",
        "a trait instead of a callable, so the kernel stays domain neutral",
    ),
    (
        "security_events.orm_store busy/corrupt/schema classification",
        "asc_sqlite_kernel::{is_busy, is_corruption, is_schema}",
        "three predicates plus SCHEMA_ERROR_MARKERS",
    ),
    (
        "security_events.sqlite_maintenance.run_sqlite_maintenance_if_due",
        "asc_sqlite_kernel::run_sqlite_maintenance_if_due",
        "",
    ),
    (
        "security_events.sqlite_maintenance interval",
        "asc_sqlite_kernel::DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS",
        "",
    ),
    (
        "time.time() in the maintenance gate",
        "asc_sqlite_kernel::current_epoch",
        "extracted so the gate can be driven by an injected clock",
    ),
    (
        "the eight-rung write ladder, written twice in v1",
        "asc_sqlite_kernel::SqliteSink",
        "one implementation; the per-stream differences moved into FaultPolicy",
    ),
    (
        "the read paths' `except SQLAlchemyError: dispose()`",
        "asc_sqlite_kernel::ReadOnlySource",
        "one implementation shared by both readers",
    ),
    // --- security_events SQLite binding ------------------------------------
    (
        "security_events.models.SecurityEventRecord",
        "asc_persistence_sqlite::security_events::SECURITY_EVENTS_TABLES",
        "a declarative TableSpec replaces the ORM model",
    ),
    (
        "security_events.repositories.SecurityEventRepository",
        "asc_persistence_sqlite::security_events::SecurityEventRepository",
        "",
    ),
    (
        "SecurityEventRepository.insert",
        "SecurityEventRepository::insert_or_raise",
        "the trait method; an unserializable record still reports Ok(false)",
    ),
    (
        "SecurityEventRepository.query",
        "SecurityEventRepository::query",
        "",
    ),
    (
        "SecurityEventRepository.get",
        "SecurityEventRepository::get",
        "",
    ),
    (
        "SecurityEventRepository.query_correlation_candidates",
        "SecurityEventRepository::query_correlation_candidates",
        "seven parameters aggregated into CorrelationRequest; see ISSUES D-11",
    ),
    (
        "SecurityEventRepository.count",
        "SecurityEventRepository::count",
        "",
    ),
    (
        "SecurityEventRepository.count_by",
        "SecurityEventRepository::count_by",
        "",
    ),
    (
        "SecurityEventRepository.summary",
        "SecurityEventRepository::summary",
        "",
    ),
    (
        "SecurityEventRepository.prune",
        "SecurityEventRepository::prune",
        "the trait method, with `now` injected",
    ),
    (
        "SecurityEventRepository.checkpoint",
        "SecurityEventRepository::checkpoint",
        "the trait method",
    ),
    (
        "SecurityEventRepository._build_filters",
        "asc_persistence_sqlite::security_events::EventFilters",
        "the filter set became a value the caller builds",
    ),
    (
        "SecurityEventRepository count_by allowlist",
        "asc_persistence_sqlite::security_events::VALID_GROUP_FIELDS",
        "",
    ),
    (
        "security_events.schema.SecurityEventsSummary",
        "asc_security_events::SecurityEventsSummary",
        "",
    ),
    (
        "security_events correlation candidate rows",
        "asc_security_events::CorrelationCandidate",
        "",
    ),
    (
        "security_events.schema.migrate_security_events_schema",
        "asc_persistence_sqlite::security_events::SecurityEventsMigrator",
        "a unit struct implementing SchemaMigrator, since v1 registers the function as a callback",
    ),
    (
        "the verdict backfill batch size",
        "asc_persistence_sqlite::security_events::VERDICT_MIGRATION_BATCH_SIZE",
        "",
    ),
    (
        "security_events.sqlite_writer.SqliteEventWriter",
        "asc_persistence_sqlite::security_events::SqliteEventWriter",
        "",
    ),
    (
        "security_events.sqlite_writer._log_drop",
        "asc_persistence_sqlite::security_events::{DropSink, StderrDropSink, WriteDrop}",
        "injected instead of hard-wired to cli.jsonl; see ISSUES D-09",
    ),
    (
        "security_events.sqlite_writer per-rung behaviour",
        "asc_persistence_sqlite::security_events::SecurityEventsFaultPolicy",
        "",
    ),
    (
        "security_events.sqlite_reader.SqliteEventReader",
        "asc_persistence_sqlite::security_events::SqliteEventReader",
        "",
    ),
    // --- observability SQLite binding --------------------------------------
    (
        "observability.models.ObservabilityEventRecord",
        "asc_persistence_sqlite::observability::OBSERVABILITY_TABLES",
        "a declarative TableSpec replaces the ORM model",
    ),
    (
        "observability.repositories.ObservabilityEventRepository",
        "asc_persistence_sqlite::observability::ObservabilityEventRepository",
        "",
    ),
    (
        "ObservabilityEventRepository.insert",
        "ObservabilityEventRepository::insert_or_raise",
        "the trait method; the swallowing variant is the sink's job",
    ),
    (
        "ObservabilityEventRepository.insert_or_raise",
        "ObservabilityEventRepository::insert_or_raise",
        "",
    ),
    (
        "ObservabilityEventRepository.count",
        "ObservabilityEventRepository::count",
        "",
    ),
    (
        "ObservabilityEventRepository.count_sessions",
        "ObservabilityEventRepository::count_sessions",
        "",
    ),
    (
        "ObservabilityEventRepository.count_runs",
        "ObservabilityEventRepository::count_runs",
        "",
    ),
    (
        "ObservabilityEventRepository.list_sessions",
        "ObservabilityEventRepository::list_sessions",
        "",
    ),
    (
        "ObservabilityEventRepository.list_runs",
        "ObservabilityEventRepository::list_runs",
        "",
    ),
    (
        "ObservabilityEventRepository.list_events",
        "ObservabilityEventRepository::list_events",
        "",
    ),
    (
        "ObservabilityEventRepository.prune",
        "ObservabilityEventRepository::prune",
        "the trait method",
    ),
    (
        "ObservabilityEventRepository.checkpoint",
        "ObservabilityEventRepository::checkpoint",
        "the trait method",
    ),
    (
        "observability.sqlite_writer.ObservabilitySqliteWriter",
        "asc_persistence_sqlite::observability::ObservabilitySqliteWriter",
        "both write and write_or_raise",
    ),
    (
        "observability.sqlite_reader.ObservabilityReader",
        "asc_persistence_sqlite::observability::ObservabilityReader",
        "",
    ),
    (
        "observability.schema.ObservabilityRecord",
        "asc_observability::ObservabilityRecord",
        "",
    ),
    (
        "observability.schema.ObservabilityMetadata",
        "asc_observability::ObservabilityMetadata",
        "",
    ),
    (
        "observability.metrics.HOOK_METRIC_ALLOWLIST",
        "asc_observability::hook_metric_allowlist",
        "a function because a Rust const cannot own a map",
    ),
    (
        "observability.metrics per-hook lookup",
        "asc_observability::allowed_metrics_for_hook",
        "",
    ),
    (
        "observability hook name literals",
        "asc_observability::{ObservabilityHook, OBSERVABILITY_HOOKS}",
        "",
    ),
    (
        "observability.config.get_observability_db_path",
        "asc_observability::config::get_observability_db_path",
        "",
    ),
    (
        "observability.config.get_observability_log_path",
        "asc_observability::config::get_observability_log_path",
        "",
    ),
    (
        "observability.config.DEFAULT_OBSERVABILITY_RETENTION_DAYS",
        "asc_observability::DEFAULT_OBSERVABILITY_RETENTION_DAYS",
        "",
    ),
    (
        "observability stream / log prefix literals",
        "asc_observability::{OBSERVABILITY_STREAM, OBSERVABILITY_LOG_PREFIX}",
        "",
    ),
    (
        "observability.schema_version.OBSERVABILITY_SQLITE_SCHEMA_VERSION",
        "asc_observability::OBSERVABILITY_SQLITE_SCHEMA_VERSION",
        "",
    ),
    (
        "observability run/session summary rows",
        "asc_observability::{RunSummary, SessionSummary}",
        "",
    ),
    (
        "observability user_input truncation limit",
        "asc_observability::USER_INPUT_PREVIEW_LIMIT",
        "",
    ),
    // --- the assembly layer -------------------------------------------------
    ("security_events.log_event", "asc_event_sink::log_event", ""),
    (
        "security_events.get_writer",
        "asc_event_sink::writer",
        "the get_ prefix is not Rust convention",
    ),
    (
        "security_events.get_sqlite_writer",
        "asc_event_sink::sqlite_writer",
        "the get_ prefix is not Rust convention",
    ),
    (
        "security_events.get_reader",
        "asc_event_sink::reader",
        "the get_ prefix is not Rust convention",
    ),
    (
        "observability.record_observability",
        "asc_event_sink::ConfiguredObservabilitySinks::record",
        "daemon-owned configured sink; returns Result with the v1 write ordering",
    ),
    (
        "observability.get_writer",
        "asc_event_sink::ConfiguredObservabilitySinks::new",
        "global access removed; composition root supplies explicit paths",
    ),
    (
        "observability.get_sqlite_writer",
        "asc_event_sink::ConfiguredObservabilitySinks::new",
        "global access removed; configured sink lazily owns the SQLite writer",
    ),
    // --- the display layer --------------------------------------------------
    (
        "security_events.summary_formatter.format_summary",
        "asc_security_summary::format_summary",
        "",
    ),
    (
        "summary_formatter empty-input text",
        "asc_security_summary::NO_EVENTS",
        "pinned as a constant so the contract is not a bare literal",
    ),
    // --- errors -------------------------------------------------------------
    (
        "the ConfigError-shaped RuntimeErrors",
        "asc_security_events::ConfigError",
        "",
    ),
    (
        "the writer's OSError family",
        "asc_event_log::EventLogError",
        "",
    ),
    // --- v2-only additions --------------------------------------------------
    (
        "(none — v2 addition)",
        "asc_event_sink::shutdown_sinks",
        "Rust has no atexit; the host owns shutdown. See ISSUES / the crate docs",
    ),
    (
        "(none — v2 addition)",
        "asc_event_sink::shutdown_sinks_at",
        "an injected clock, because the maintenance pass is time-gated",
    ),
    (
        "(none — v2 addition)",
        "asc_security_summary::format_summary_at",
        "an injected clock, because the footer prints an age",
    ),
    (
        "(none — v2 addition)",
        "asc_persistence_sqlite::observability::ObservabilityReader::count",
        "v1 has the count on the repository but not on the reader; the differential probe needs it on the read path",
    ),
];

/// The maintenance gate's signature, named so the reference below stays readable.
type MaintenanceGate =
    fn(&Path, Option<f64>, Option<f64>, fn() -> Result<(), asc_sqlite_kernel::KernelError>) -> bool;

/// The store constructor's signature.
type StoreConstructor = fn(
    &Path,
    bool,
    u32,
    &'static [asc_sqlite_kernel::TableSpec],
    Option<std::sync::Arc<dyn SchemaMigrator>>,
    &str,
) -> Result<SqliteStore, asc_sqlite_kernel::KernelError>;

/// Uses the contract-layer counterparts for real.
///
/// None of the `reference_*` functions is ever called: a missing or renamed item
/// is a compile error in the body, which is the whole point.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_contract_symbols() {
    // Typed function pointers where a signature drift would be a real behaviour
    // change rather than a rename.
    let _: fn() -> String = now_iso;
    let _: fn(&serde_json::Map<String, serde_json::Value>) -> Option<String> = extract_verdict;
    let _: fn(&str, &str) -> Result<f64, asc_security_events::TimestampError> = utc_iso_to_epoch;
    let _: fn(f64) -> Result<String, asc_security_events::TimestampError> = epoch_to_utc_iso;
    let _: fn() -> Result<PathBuf, ConfigError> = get_data_dir;
    let _: fn() -> Result<PathBuf, ConfigError> = get_log_path;
    let _: fn() -> Result<PathBuf, ConfigError> = get_db_path;
    let _: fn(&str) -> Result<PathBuf, ConfigError> = get_stream_log_path;
    let _: fn(&str) -> Result<PathBuf, ConfigError> = get_stream_db_path;
    let _: fn() -> Result<PathBuf, ConfigError> = resolve_data_dir;
    let _: fn(&str) -> Result<(), ConfigError> = validate_stream_name;
    let _: fn(&str) -> bool = is_backup_suffix;
    let _: fn(String, String, serde_json::Map<String, serde_json::Value>) -> SecurityEvent =
        SecurityEvent::new;

    // Path resolution for the remainder. A rename fails here.
    let _ = format_utc_iso;
    let _ = normalize_iso_to_utc_iso;
    let _ = asc_security_events::timestamp::parse_iso;
    let _ = asc_security_events::config::fallback_log_path;
    let _ = asc_observability::config::get_observability_db_path;
    let _ = asc_observability::config::get_observability_log_path;
    let _ = hook_metric_allowlist;
    let _ = allowed_metrics_for_hook;
    let _ = ObservabilityHook::metric_names;
    let _ = ObservabilityMetadata::new;
    let _ = ObservabilityRecord::new;
    let _ = SecurityEvent::set_timestamp;
}

/// Uses the `SQLite` kernel counterparts for real.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_kernel_symbols() {
    let _: fn(&str) -> bool = is_valid_identifier;
    let _: fn() -> f64 = current_epoch;
    let _ = open_connection;
    let _ = ensure_schema;
    let _ = ensure_schema_if_needed;
    let _ = warn_readonly_schema_readiness;
    // `impl AsRef<Path>` parameters cannot be turbofished, and a bare function
    // item leaves the lifetime unresolved, so these two go through a closure.
    let _: fn(&Path) -> PathBuf = |path| normalize_sqlite_path(path);
    let _ = sqlite_database_files;
    let _: MaintenanceGate = run_sqlite_maintenance_if_due;
    let _ = is_busy;
    let _ = is_corruption;
    let _ = is_schema;
    let _ = asc_sqlite_kernel::SCHEMA_ERROR_MARKERS;
    let _: StoreConstructor = |path, read_only, version, tables, migrator, prefix| {
        SqliteStore::new(path, read_only, version, tables, migrator, prefix)
    };
    let _ = SqliteStore::dispose;
    let _ = SqliteStore::close;
    let _ = SqliteStore::request_schema_repair;
    let _ = SqliteStore::handle_corruption;
    let _ = SqliteSink::<SecurityEventRepository, SecurityEventsFaultPolicy<StderrDropSink>>::write;
    let _ = SqliteSink::<SecurityEventRepository, SecurityEventsFaultPolicy<StderrDropSink>>::write_or_raise;
    let _ = ReadOnlySource::<SecurityEventRepository>::close;
    let _ = <SecurityEventsMigrator as SchemaMigrator>::migrate;
    let _ = <StderrDropSink as DropSink>::on_drop;
}

/// Uses both repositories' methods for real.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_repository_symbols() {
    let _ = SecurityEventRepository::query;
    let _ = SecurityEventRepository::get;
    let _ = SecurityEventRepository::query_correlation_candidates;
    let _ = SecurityEventRepository::count;
    let _ = SecurityEventRepository::count_by;
    let _ = SecurityEventRepository::summary;
    let _ = <SecurityEventRepository as asc_sqlite_kernel::RecordRepository>::insert_or_raise;
    let _ = <SecurityEventRepository as asc_sqlite_kernel::RecordRepository>::prune;
    let _ = <SecurityEventRepository as asc_sqlite_kernel::RecordRepository>::checkpoint;
    let _ = ObservabilityEventRepository::count;
    let _ = ObservabilityEventRepository::count_sessions;
    let _ = ObservabilityEventRepository::count_runs;
    let _ = ObservabilityEventRepository::list_sessions;
    let _ = ObservabilityEventRepository::list_runs;
    let _ = ObservabilityEventRepository::list_events;
    let _ = <ObservabilityEventRepository as asc_sqlite_kernel::RecordRepository>::insert_or_raise;
    let _ = <ObservabilityEventRepository as asc_sqlite_kernel::RecordRepository>::prune;
    let _ = <ObservabilityEventRepository as asc_sqlite_kernel::RecordRepository>::checkpoint;
}

/// Uses every writer and reader constructor and method for real.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_writer_and_reader_symbols() {
    let _: fn(&Path) -> SecurityEventWriter = |path| SecurityEventWriter::new(path);
    let _: fn() -> Result<SecurityEventWriter, EventLogError> =
        SecurityEventWriter::with_default_path;
    let _ = SecurityEventWriter::with_max_bytes;
    let _ = SecurityEventWriter::with_backup_count;
    let _ = SecurityEventWriter::write;
    let _ = SecurityEventWriter::write_or_raise;
    let _ = SecurityEventWriter::inner;
    // The JSONL writer is generic over the payload; both streams are covered.
    let _ = JsonlEventWriter::write::<SecurityEvent>;
    let _ = JsonlEventWriter::write_or_raise::<ObservabilityRecord>;
    let _: fn() -> Result<ObservabilityWriter, EventLogError> =
        ObservabilityWriter::with_default_path;
    let _ = ObservabilityWriter::write;
    let _ = SqliteEventWriter::new;
    let _ = SqliteEventWriter::at_default_path;
    let _ = SqliteEventWriter::<StderrDropSink>::with_options;
    let _ = SqliteEventWriter::<StderrDropSink>::write;
    let _ = SqliteEventWriter::<StderrDropSink>::close;
    let _ = SqliteEventWriter::<StderrDropSink>::close_at;
    let _ = SqliteEventReader::new;
    let _ = SqliteEventReader::at_default_path;
    let _ = SqliteEventReader::query;
    let _ = SqliteEventReader::query_default_page;
    let _ = SqliteEventReader::get;
    let _ = SqliteEventReader::query_correlation_candidates;
    let _ = SqliteEventReader::count;
    let _ = SqliteEventReader::count_by;
    let _ = SqliteEventReader::summary;
    let _ = SqliteEventReader::close;
    let _ = ObservabilitySqliteWriter::new;
    let _ = ObservabilitySqliteWriter::at_default_path;
    let _ = ObservabilitySqliteWriter::with_max_age_days;
    let _ = ObservabilitySqliteWriter::write;
    let _ = ObservabilitySqliteWriter::write_or_raise;
    let _ = ObservabilitySqliteWriter::close;
    let _ = ObservabilityReader::new;
    let _ = ObservabilityReader::at_default_path;
    let _ = ObservabilityReader::count;
    let _ = ObservabilityReader::count_sessions;
    let _ = ObservabilityReader::count_runs;
    let _ = ObservabilityReader::list_sessions;
    let _ = ObservabilityReader::list_runs;
    let _ = ObservabilityReader::list_events;
    let _ = ObservabilityReader::close;
}

/// Uses the assembly and display layers for real.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_assembly_and_display_symbols() {
    let _: fn(&SecurityEvent) = asc_event_sink::log_event;
    let _ = asc_event_sink::ConfiguredObservabilitySinks::record;
    let _ = asc_event_sink::ConfiguredObservabilitySinks::new;
    let _ = asc_event_sink::ConfiguredObservabilitySinks::close;
    let _: fn() = asc_event_sink::shutdown_sinks;
    let _: fn(f64) = asc_event_sink::shutdown_sinks_at;
    let _: fn(&[SecurityEvent], &str) -> String = format_summary;
    let _: fn(&[SecurityEvent], &str, chrono::DateTime<chrono::Utc>) -> String = format_summary_at;
    let _ = asc_event_sink::writer;
    let _ = asc_event_sink::sqlite_writer;
    let _ = asc_event_sink::reader;
}

/// Uses every migrated constant and value type, so a deletion is a compile error.
#[expect(
    dead_code,
    reason = "the point is that this compiles, not that it runs"
)]
fn reference_values() {
    let _ = (
        DEFAULT_MAX_BYTES,
        DEFAULT_BACKUP_COUNT,
        DEFAULT_ERROR_PREFIX,
        DEFAULT_OBSERVABILITY_MAX_BYTES,
        DEFAULT_OBSERVABILITY_BACKUP_COUNT,
        DEFAULT_OBSERVABILITY_RETENTION_DAYS,
        DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS,
        SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
        SECURITY_EVENTS_VERDICT_SCHEMA_VERSION,
        OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        OBSERVABILITY_STREAM,
        OBSERVABILITY_LOG_PREFIX,
        USER_INPUT_PREVIEW_LIMIT,
        VERDICT_MIGRATION_BATCH_SIZE,
        NO_EVENTS,
    );
    let _ = (
        SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS,
        SECURITY_EVENTS_TABLES,
        OBSERVABILITY_TABLES,
        OBSERVABILITY_HOOKS,
        VALID_GROUP_FIELDS,
    );
    let _ = (
        EventResult::Succeeded,
        EventResult::Failed,
        EventFilters::default(),
        SecurityEventsMigrator,
        StderrDropSink,
        SecurityEventRepository,
        ObservabilityEventRepository,
    );
    let _: Option<asc_security_events::SecurityEventsSummary> = None;
    let _: Option<asc_security_events::CorrelationCandidate> = None;
    let _: Option<RunSummary> = None;
    let _: Option<SessionSummary> = None;
}

#[test]
fn the_ledger_has_no_unmigrated_entry() {
    for (v1, v2, _) in V1_TO_V2 {
        assert!(
            !v2.contains("未迁移") && !v2.is_empty(),
            "{v1} has no v2 counterpart"
        );
    }
}

#[test]
fn every_renamed_symbol_carries_a_reason() {
    for (v1, v2, note) in V1_TO_V2 {
        // Rows whose left column is prose rather than a dotted symbol describe a
        // v1 behaviour that had no name of its own, so "renamed" is meaningless.
        if v1.contains(' ') {
            continue;
        }
        let v1_tail = v1.rsplit('.').next().unwrap_or(v1);
        let v2_tail = v2.rsplit("::").next().unwrap_or(v2);
        if v1_tail == v2_tail {
            continue;
        }
        assert!(
            !note.is_empty(),
            "{v1} → {v2} changed name without a recorded reason"
        );
    }
}

#[test]
fn the_v2_only_additions_are_the_four_that_were_agreed() {
    let additions: Vec<&str> = V1_TO_V2
        .iter()
        .filter(|(v1, _, _)| v1.starts_with("(none"))
        .map(|(_, v2, _)| *v2)
        .collect();
    assert_eq!(
        additions,
        [
            "asc_event_sink::shutdown_sinks",
            "asc_event_sink::shutdown_sinks_at",
            "asc_security_summary::format_summary_at",
            "asc_persistence_sqlite::observability::ObservabilityReader::count",
        ],
        "a new public symbol needs a plan amendment, not a silent ledger row"
    );
}
