use anyhow::Context;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

// ==================== Default Constants ====================

/// Default LRU cache capacity for HTTP connections
pub const DEFAULT_CONNECTION_CAPACITY: usize = 24;

/// Default poll timeout for ring buffer polling (milliseconds)
pub const DEFAULT_POLL_TIMEOUT_MS: u64 = 100;

/// Default minimum duration threshold for HTTP requests (microseconds)
pub const DEFAULT_MIN_DUR_US: u64 = 10_000;

/// Default maximum body length for audit analyzer
pub const DEFAULT_MAX_BODY_LEN: usize = 64 * 1024;

/// Default maximum headers for HTTP parser
pub const DEFAULT_MAX_HEADERS: usize = 64;

/// Primary event database filename.
pub const PRIMARY_DB_NAME: &str = "agentsight.db";
/// GenAI event and evaluation database filename.
pub const GENAI_DB_NAME: &str = "genai_events.db";
/// Interruption database filename.
pub const INTERRUPTION_DB_NAME: &str = "interruption_events.db";
/// Collected trajectory database filename.
pub const TRAJECTORY_DB_NAME: &str = "trajectories.db";
/// Optimization result database filename.
pub const OPTIMIZATION_DB_NAME: &str = "optimization.db";
/// Private security audit database filename.
pub const SECURITY_AUDIT_DB_NAME: &str = "security.db";
/// Private enforcement database filename.
pub const ENFORCEMENT_DB_NAME: &str = "enforcement.db";
/// Private trajectory reuse database filename.
pub const REUSE_DB_NAME: &str = "reuse.db";
/// Private causal analysis database filename.
pub const CAUSAL_DB_NAME: &str = "causal.db";

/// Default audit table name.
pub const DEFAULT_AUDIT_TABLE: &str = "audit_events";
/// Default token table name.
pub const DEFAULT_TOKEN_TABLE: &str = "token_records";
/// Default HTTP table name.
pub const DEFAULT_HTTP_TABLE: &str = "http_records";

/// Default data retention period in days.
pub const DEFAULT_RETENTION_DAYS: u64 = 30;
/// Default primary-store maintenance interval in seconds.
pub const DEFAULT_MAINTENANCE_INTERVAL_SECS: u64 = 60;
/// Default primary database size limit in MiB.
pub const DEFAULT_MAX_DB_SIZE_MB: u64 = 500;

/// Default bounded channel capacity for probe → event loop events.
pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 10_000;

/// Default byte budget for probe events queued in the event channel (64 MiB).
///
/// A slot count cannot bound memory on its own: one SSL record carries up to
/// `MAX_BUF_SIZE` (4 MiB) of payload, so the 10 000 default slots admit
/// gigabytes of in-flight data under real HTTPS traffic. Admission is therefore
/// gated on bytes as well.
pub const DEFAULT_EVENT_CHANNEL_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Default maximum number of pending GenAI events waiting for session_id resolution.
pub const DEFAULT_PENDING_GENAI_MAX_COUNT: usize = 1_000;

/// Default maximum bytes for pending GenAI events waiting for session_id resolution (64 MB).
pub const DEFAULT_PENDING_GENAI_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Default PID → agent_name cache size.
pub const DEFAULT_PID_CACHE_SIZE: usize = 1024;

/// Default tokenizer cache size (number of loaded tokenizer models).
pub const DEFAULT_TOKENIZER_CACHE_SIZE: usize = 4;

/// Default seconds between two trajectory-collector scan rounds.
pub const DEFAULT_TRAJECTORY_SCAN_INTERVAL_SECS: u64 = 30;

/// Default maximum per-connection HTTP body buffer size (8 MB).
pub const DEFAULT_MAX_CONNECTION_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Default idle timeout for HTTP connection states before forced cleanup (60 s).
pub const DEFAULT_CONNECTION_IDLE_TIMEOUT_SECS: u64 = 60;

/// Default eBPF ring buffer size in MiB.
pub const DEFAULT_RING_BUFFER_MB: usize = 32;

pub const HF_ENDPOINT: &str = "https://hf-mirror.com";

/// Get the HF_HOME path, expanding `~` to the user's home directory.
///
/// Uses `$HOME` on Unix and `$USERPROFILE` on Windows as fallback.
/// Returns `./.agentsight/tokenizers` if home directory cannot be determined.
pub fn hf_home() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".agentsight/tokenizers")
}

// ==================== Global Verbose State ====================

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    init_logging(v, None);
}

/// Initialize or reconfigure logging with optional file output.
///
/// Safe to call on every `AgentSight::new` in the same process: the output
/// destination and filter are updated in place.
///
/// * `verbose` — true = debug level, false = warn level (unless `RUST_LOG` is set)
/// * `log_path` — if `Some`, log output is appended to this file; otherwise stderr
pub fn init_logging(verbose: bool, log_path: Option<&str>) {
    VERBOSE.store(verbose, Ordering::SeqCst);
    crate::logging::init(verbose, log_path);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}

// ==================== FFI Rule Configuration ====================

/// Cmdline rule for process matching (allowlist / denylist)
#[derive(Debug, Clone)]
pub struct CmdlineRule {
    /// Glob patterns matched against cmdline args position-by-position
    pub patterns: Vec<String>,
    /// Agent name for allow=1 rules (None for deny rules)
    pub agent_name: Option<String>,
    /// true = allowlist (attach), false = denylist (don't attach)
    pub allow: bool,
}

/// HTTPS rule for DNS-based SSL attachment filtering
#[derive(Debug, Clone)]
pub struct HttpsRule {
    /// Glob pattern for domain matching
    pub pattern: String,
}

/// HTTP target entry — can be an IP/port endpoint or a domain name.
/// Code auto-detects: entries parseable as TcpTarget are treated as endpoints;
/// everything else is treated as a domain (resolved via DNS at startup + runtime).
#[derive(Debug, Clone)]
pub enum HttpTarget {
    Endpoint(TcpTarget),
    Domain(String),
}

// ==================== Agent Discovery Configuration ====================

/// Default agents configuration JSON (embedded in binary).
///
/// Uses the same format as FFI's `agentsight_config_load_config()`:
/// `cmdline.allow` entries with `rule` and `agent_name`.
const DEFAULT_AGENTS_JSON: &str = include_str!("../agentsight.json");

/// Current schema version of `agentsight.json`.
///
/// At startup the on-disk config's `schema_version` is compared against this
/// value. If the on-disk version is missing or older, the config file is
/// backed up (`.bak`) and overwritten with the embedded default.
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

// ==================== TCP Target Configuration ====================

/// A single TCP traffic capture target.
///
/// Filters captured plain-HTTP traffic by destination IP and/or port.
/// `ip = None` means any destination IP; `port = None` means any port.
///
/// String format (used in JSON config and CLI):
///   `":8080"`          → port-only (any IP, port 8080)
///   `"*:8080"`         → port-only (alias of `:8080`)
///   `"10.0.0.1"`       → IP-only   (IP 10.0.0.1, any port)
///   `"10.0.0.1:*"`     → IP-only   (alias of `10.0.0.1`)
///   `"10.0.0.1:8080"`  → exact     (IP 10.0.0.1, port 8080)
///   `"*"` / `"*:*"` / `":*"` → full wildcard (any IP, any port — captures **all** TCP traffic)
#[derive(Debug, Clone, PartialEq)]
pub struct TcpTarget {
    pub ip: Option<Ipv4Addr>,
    pub port: Option<u16>,
}

impl FromStr for TcpTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty TcpTarget string".to_string());
        }

        // Full wildcard shortcuts: "*", "*:*", ":*"
        if s == "*" || s == "*:*" || s == ":*" {
            return Ok(TcpTarget {
                ip: None,
                port: None,
            });
        }

        // Helper: parse `"*"` as wildcard, otherwise as IPv4.
        let parse_ip = |t: &str| -> Result<Option<Ipv4Addr>, String> {
            if t == "*" {
                Ok(None)
            } else {
                t.parse::<Ipv4Addr>()
                    .map(Some)
                    .map_err(|_| format!("invalid IP address '{t}'"))
            }
        };
        // Helper: parse `"*"` as wildcard, otherwise as u16 port.
        let parse_port = |t: &str| -> Result<Option<u16>, String> {
            if t == "*" {
                Ok(None)
            } else {
                t.parse::<u16>()
                    .map(Some)
                    .map_err(|_| format!("invalid port '{t}'"))
            }
        };

        if s.starts_with(':') {
            // ":port" — port-only
            let port = parse_port(s.strip_prefix(':').unwrap())?;
            Ok(TcpTarget { ip: None, port })
        } else if s.contains(':') {
            // "ip:port" (either side may be `*`)
            let (ip_str, port_str) = s.rsplit_once(':').unwrap();

            let ip = parse_ip(ip_str)?;
            let port = parse_port(port_str)?;
            Ok(TcpTarget { ip, port })
        } else {
            // "ip" — IP-only (no `*` here — already handled above)
            let ip = parse_ip(s)?;
            Ok(TcpTarget { ip, port: None })
        }
    }
}

/// Server authentication configuration (JSON).
///
/// Only `enabled` is configurable.  The token is always auto-generated
/// (or read from the default `.dashboard_token` file) — there is no
/// way to specify a fixed token or custom token-file path.
#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonServerAuth {
    pub enabled: Option<bool>,
}

/// Server configuration block (JSON).
#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonServer {
    pub auth: Option<JsonServerAuth>,
}

/// Runtime server authentication configuration.
#[derive(Debug, Clone)]
pub struct ServerAuthConfig {
    /// Whether dashboard authentication is enabled.
    pub enabled: bool,
}

impl Default for ServerAuthConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Retention and capacity policy checked on a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodicStoragePolicy {
    /// Maximum age in days; zero disables age-based cleanup.
    pub retention_days: u64,
    /// Maximum logical database size in MiB; zero disables size cleanup.
    pub max_db_size_mb: u64,
    /// Seconds between checks; zero disables automatic checks.
    pub check_interval_secs: u64,
}

impl PeriodicStoragePolicy {
    /// Creates a periodic retention and capacity policy.
    pub const fn new(retention_days: u64, max_db_size_mb: u64, check_interval_secs: u64) -> Self {
        Self {
            retention_days,
            max_db_size_mb,
            check_interval_secs,
        }
    }

    const fn primary_default() -> Self {
        Self::new(
            DEFAULT_RETENTION_DAYS,
            DEFAULT_MAX_DB_SIZE_MB,
            DEFAULT_MAINTENANCE_INTERVAL_SECS,
        )
    }

    const fn genai_default() -> Self {
        Self::new(30, 200, 60)
    }
}

impl Default for PeriodicStoragePolicy {
    fn default() -> Self {
        Self::new(30, 200, 300)
    }
}

/// Effective SQLite storage configuration.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Directory containing AgentSight-owned databases.
    pub base_path: PathBuf,
    /// Policy for `agentsight.db`.
    pub primary: PeriodicStoragePolicy,
    /// Policy for `genai_events.db`.
    pub genai: PeriodicStoragePolicy,
    /// Policy for `interruption_events.db`.
    pub interruptions: PeriodicStoragePolicy,
    /// Policy for `trajectories.db`.
    pub trajectories: PeriodicStoragePolicy,
    /// Policy for `optimization.db`.
    pub optimization: PeriodicStoragePolicy,
    /// Policy for the private security audit database.
    pub security_audit: PeriodicStoragePolicy,
    /// Policy for the private trajectory reuse database.
    pub reuse: PeriodicStoragePolicy,
    /// Policy for the private causal analysis database.
    pub causal: PeriodicStoragePolicy,
    /// Policy for the private enforcement database.
    pub enforcement: PeriodicStoragePolicy,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            base_path: default_base_path(),
            primary: PeriodicStoragePolicy::primary_default(),
            genai: PeriodicStoragePolicy::genai_default(),
            interruptions: PeriodicStoragePolicy::new(30, 100, 60),
            trajectories: PeriodicStoragePolicy::new(30, 500, 300),
            optimization: PeriodicStoragePolicy::new(30, 200, 300),
            security_audit: PeriodicStoragePolicy::new(30, 200, 3_600),
            reuse: PeriodicStoragePolicy::new(30, 200, 300),
            causal: PeriodicStoragePolicy::new(30, 200, 300),
            enforcement: PeriodicStoragePolicy::new(30, 100, 60),
        }
    }
}

impl StorageConfig {
    /// Returns the fixed path of the primary event database.
    pub fn primary_path(&self) -> PathBuf {
        self.base_path.join(PRIMARY_DB_NAME)
    }

    /// Returns the fixed path of the GenAI event database.
    pub fn genai_path(&self) -> PathBuf {
        self.base_path.join(GENAI_DB_NAME)
    }

    /// Returns the fixed path of the interruption database.
    pub fn interruption_path(&self) -> PathBuf {
        self.base_path.join(INTERRUPTION_DB_NAME)
    }

    /// Returns the fixed path of the trajectory database.
    pub fn trajectory_path(&self) -> PathBuf {
        self.base_path.join(TRAJECTORY_DB_NAME)
    }

    /// Returns the fixed path of the optimization database.
    pub fn optimization_path(&self) -> PathBuf {
        self.base_path.join(OPTIMIZATION_DB_NAME)
    }

    fn validate(&self) -> Result<(), String> {
        if self.base_path.as_os_str().is_empty() {
            return Err("storage.base_path must not be empty".to_string());
        }
        let validate = |name: &str, retention_days: u64, max_db_size_mb: u64| {
            retention_days
                .checked_mul(24 * 60 * 60 * 1_000_000_000)
                .ok_or_else(|| format!("storage.{name}.retention_days is too large"))?;
            max_db_size_mb
                .checked_mul(1024 * 1024)
                .ok_or_else(|| format!("storage.{name}.max_db_size_mb is too large"))?;
            Ok::<(), String>(())
        };
        validate(
            "primary",
            self.primary.retention_days,
            self.primary.max_db_size_mb,
        )?;
        validate(
            "genai",
            self.genai.retention_days,
            self.genai.max_db_size_mb,
        )?;
        for (name, policy) in [
            ("interruptions", self.interruptions),
            ("trajectories", self.trajectories),
            ("optimization", self.optimization),
            ("security_audit", self.security_audit),
            ("reuse", self.reuse),
            ("causal", self.causal),
            ("enforcement", self.enforcement),
        ] {
            validate(name, policy.retention_days, policy.max_db_size_mb)?;
        }
        Ok(())
    }

    fn apply_json(&mut self, json: JsonStorageConfig) {
        if let Some(base_path) = json.base_path {
            self.base_path = base_path;
        }
        apply_periodic_policy(&mut self.primary, json.primary);
        apply_periodic_policy(&mut self.genai, json.genai);
        apply_periodic_policy(&mut self.interruptions, json.interruptions);
        apply_periodic_policy(&mut self.trajectories, json.trajectories);
        apply_periodic_policy(&mut self.optimization, json.optimization);
        apply_periodic_policy(&mut self.security_audit, json.security_audit);
        apply_periodic_policy(&mut self.reuse, json.reuse);
        apply_periodic_policy(&mut self.causal, json.causal);
        apply_periodic_policy(&mut self.enforcement, json.enforcement);
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct JsonStorageConfig {
    base_path: Option<PathBuf>,
    primary: Option<JsonPeriodicStoragePolicy>,
    genai: Option<JsonPeriodicStoragePolicy>,
    interruptions: Option<JsonPeriodicStoragePolicy>,
    trajectories: Option<JsonPeriodicStoragePolicy>,
    optimization: Option<JsonPeriodicStoragePolicy>,
    security_audit: Option<JsonPeriodicStoragePolicy>,
    reuse: Option<JsonPeriodicStoragePolicy>,
    causal: Option<JsonPeriodicStoragePolicy>,
    enforcement: Option<JsonPeriodicStoragePolicy>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct JsonPeriodicStoragePolicy {
    retention_days: Option<u64>,
    max_db_size_mb: Option<u64>,
    check_interval_secs: Option<u64>,
}

fn apply_periodic_policy(
    policy: &mut PeriodicStoragePolicy,
    json: Option<JsonPeriodicStoragePolicy>,
) {
    let Some(json) = json else { return };
    if let Some(value) = json.retention_days {
        policy.retention_days = value;
    }
    if let Some(value) = json.max_db_size_mb {
        policy.max_db_size_mb = value;
    }
    if let Some(value) = json.check_interval_secs {
        policy.check_interval_secs = value;
    }
}

/// Internal JSON structures for parsing the config file (same format as FFI).
#[derive(serde::Deserialize)]
struct JsonFullConfig {
    #[serde(default, rename = "traceEnabled")]
    trace_enabled: Option<bool>,
    #[serde(default)]
    verbose: Option<i32>,
    #[serde(default)]
    log_path: Option<String>,
    #[serde(default)]
    cmdline: Option<JsonCmdline>,
    #[serde(default)]
    https: Option<Vec<JsonDomainGroup>>,
    #[serde(default)]
    http: Option<Vec<JsonHttpGroup>>,
    #[serde(default)]
    encryption: Option<JsonEncryption>,
    #[serde(default)]
    runtime: Option<JsonRuntime>,
    #[serde(default)]
    deadloop: Option<JsonDeadloop>,
    #[serde(default)]
    cgroup_filter_enabled: Option<bool>,
    #[serde(default)]
    cgroup_ids: Option<Vec<u64>>,
    #[serde(default)]
    features: Option<JsonFeatures>,
    #[serde(default)]
    runtime_limits: Option<JsonRuntimeLimits>,
    #[serde(default)]
    server: Option<JsonServer>,
    #[serde(default)]
    storage: Option<JsonStorageConfig>,
    #[serde(default)]
    schema_version: Option<u32>,
}

/// DeadLoop 检测配置区段
#[derive(serde::Deserialize, Clone, Debug)]
struct JsonDeadloop {
    /// 是否启用自动 kill（默认 false，仅记录日志）
    #[serde(default)]
    enabled: Option<bool>,
    /// 触发自动 kill 的循环检测次数阈值（默认 3）
    #[serde(default)]
    kill_after_count: Option<usize>,
}

/// Feature toggle configuration.
///
/// `token_stats` is the only feature enabled by default; all other features
/// must be explicitly enabled in `agentsight.json`.
#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonFeatures {
    /// Core token metering feature (always required, defaults to true).
    pub token_stats: Option<bool>,
    /// Local tokenizer for accurate token counting when APIs do not report usage.
    pub tokenizer: Option<JsonTokenizerFeature>,
    /// Response ID → session ID mapping for GenAI session attribution.
    pub session_mapping: Option<JsonSessionMappingFeature>,
    /// Local SQLite persistence of GenAI events.
    pub sqlite_storage: Option<JsonSqliteStorageFeature>,
    /// Periodic Agent process CPU and RSS resource sampling.
    pub resource_sampling: Option<bool>,
    /// Interruption / DeadLoop detection.
    pub interruption_detection: Option<JsonInterruptionFeature>,
    /// Audit event storage.
    pub audit: Option<bool>,
    /// Token consumption breakdown storage.
    pub token_consumption: Option<bool>,
    /// SLS Logtail export.
    pub sls_logtail: Option<bool>,
    /// Qoder/QoderWork trajectory collection into trajectories.db.
    pub trajectory_collection: Option<JsonTrajectoryCollectionFeature>,
    /// Second-level model labelling of collected trajectories.
    pub reuse_llm_judge: Option<bool>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonTokenizerFeature {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub cache_size: Option<usize>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonSessionMappingFeature {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonSqliteStorageFeature {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub batch: Option<JsonBatchConfig>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonBatchConfig {
    pub max_size: Option<usize>,
    pub flush_ms: Option<u64>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonInterruptionFeature {
    pub enabled: Option<bool>,
}

#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonTrajectoryCollectionFeature {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub scan_interval_secs: Option<u64>,
    /// Optional override of the projects directories to scan.
    #[serde(default)]
    pub scan_dirs: Option<Vec<String>>,
}

/// Resource limits and channel policy for memory-leak prevention.
#[derive(serde::Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct JsonRuntimeLimits {
    pub event_channel_capacity: Option<usize>,
    #[serde(default)]
    pub event_channel_policy: Option<String>,
    /// Byte budget for events queued in the probe event channel, in MiB.
    /// Bounds in-flight memory that the slot count alone cannot (#2888).
    pub event_channel_max_bytes_mb: Option<usize>,
    pub pending_genai_max_count: Option<usize>,
    pub pending_genai_max_bytes_mb: Option<usize>,
    pub pid_cache_size: Option<usize>,
    pub max_connection_body_mb: Option<usize>,
    pub connection_idle_timeout_secs: Option<u64>,
    /// eBPF ring buffer size in MiB. Must be a power-of-two multiple of the page
    /// size (common valid values: 8, 16, 32, 64). Loaded from `agentsight.json`
    /// and applied at BPF load time via `bpf_map__set_max_entries`.
    pub ring_buffer_mb: Option<usize>,
}

/// Runtime 动态配置区段（支持热加载，无需重启）
#[derive(serde::Deserialize, Clone, Debug)]
pub struct JsonRuntime {
    /// SLS Logtail 输出文件路径。非空时激活 SLS 上传。
    #[serde(default)]
    pub sls_logtail_path: Option<String>,
}

/// 加密配置：可选公钥（PEM 字符串）或公钥文件路径
#[derive(serde::Deserialize)]
struct JsonEncryption {
    #[serde(default)]
    public_key: Option<String>,
    #[serde(default)]
    public_key_path: Option<String>,
}

#[derive(serde::Deserialize)]
struct JsonCmdline {
    #[serde(default)]
    allow: Option<Vec<JsonCmdlineEntry>>,
    #[serde(default)]
    deny: Option<Vec<JsonCmdlineEntry>>,
}

#[derive(serde::Deserialize)]
struct JsonCmdlineEntry {
    rule: Vec<String>,
    #[serde(default)]
    agent_name: Option<String>,
}

#[derive(serde::Deserialize)]
struct JsonDomainGroup {
    rule: Vec<String>,
}

#[derive(serde::Deserialize)]
struct JsonHttpGroup {
    rule: Vec<String>,
}

/// Extract cmdline, https, and http rules from a parsed JsonFullConfig.
fn extract_rules(parsed: &JsonFullConfig) -> (Vec<CmdlineRule>, Vec<HttpsRule>, Vec<HttpTarget>) {
    let mut cmdline_rules = Vec::new();
    let mut https_rules = Vec::new();
    let mut http_targets = Vec::new();

    if let Some(ref cmdline) = parsed.cmdline {
        if let Some(ref allow_list) = cmdline.allow {
            for entry in allow_list {
                if !entry.rule.is_empty() {
                    cmdline_rules.push(CmdlineRule {
                        patterns: entry.rule.clone(),
                        agent_name: entry.agent_name.clone(),
                        allow: true,
                    });
                }
            }
        }
        if let Some(ref deny_list) = cmdline.deny {
            for entry in deny_list {
                if !entry.rule.is_empty() {
                    cmdline_rules.push(CmdlineRule {
                        patterns: entry.rule.clone(),
                        agent_name: None,
                        allow: false,
                    });
                }
            }
        }
    }

    if let Some(ref https_groups) = parsed.https {
        for group in https_groups {
            for pat in &group.rule {
                if !pat.is_empty() {
                    https_rules.push(HttpsRule {
                        pattern: pat.clone(),
                    });
                }
            }
        }
    }

    if let Some(ref http_groups) = parsed.http {
        for group in http_groups {
            for entry in &group.rule {
                if entry.is_empty() {
                    continue;
                }
                match entry.parse::<TcpTarget>() {
                    Ok(t) => http_targets.push(HttpTarget::Endpoint(t)),
                    Err(_) => http_targets.push(HttpTarget::Domain(entry.clone())),
                }
            }
        }
    }

    (cmdline_rules, https_rules, http_targets)
}

/// Parse a JSON config string into cmdline rules, https rules, and http targets.
///
/// This is the shared parser for both the config file and FFI's `load_config()`.
pub fn parse_json_rules(
    json: &str,
) -> Result<(Vec<CmdlineRule>, Vec<HttpsRule>, Vec<HttpTarget>), String> {
    let parsed: JsonFullConfig =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;
    Ok(extract_rules(&parsed))
}

/// Ensure the agents configuration file exists and is up to date.
///
/// - If the file does not exist, creates it with the embedded default.
/// - If the file exists but `schema_version` is missing or older than
///   `CURRENT_SCHEMA_VERSION`, backs up the old file and replaces it with the
///   embedded default.
/// - If the file exists and `schema_version` matches, leaves it untouched.
pub fn ensure_default_agents_config(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {parent:?}"))?;
        }
        std::fs::write(path, DEFAULT_AGENTS_JSON)
            .with_context(|| format!("Failed to write default agents config to {path:?}"))?;
        log::info!("Generated default agents config at {path:?}");
        return Ok(());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read existing config at {path:?}"))?;

    // If the file is not valid JSON at all, leave it untouched so the caller's
    // load_from_file → parse_json_rules surfaces "JSON parse error: ..." and
    // AgentSight::new emits its `Failed to load config from {path:?}: {e},
    // using embedded defaults` warn log. Auto-upgrade is only meaningful for
    // *valid* JSON whose schema_version is outdated; silently overwriting an
    // invalid file masks the parse error and breaks the documented fallback
    // contract (see issue #1502).
    if serde_json::from_str::<serde_json::Value>(&content).is_err() {
        return Ok(());
    }

    let on_disk_version = extract_schema_version(&content);

    if on_disk_version >= Some(CURRENT_SCHEMA_VERSION) {
        return Ok(());
    }

    let backup = {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        path.with_extension(format!("json.bak.{ts}"))
    };
    std::fs::copy(path, &backup)
        .with_context(|| format!("Failed to back up {path:?} to {backup:?}"))?;
    std::fs::write(path, DEFAULT_AGENTS_JSON)
        .with_context(|| format!("Failed to replace outdated config at {path:?}"))?;
    log::info!(
        "Config schema_version {:?} < {}, replaced it with defaults at {path:?} (backup at {backup:?})",
        on_disk_version,
        CURRENT_SCHEMA_VERSION
    );
    Ok(())
}

/// Extract `schema_version` from a JSON config string.
///
/// Returns `None` when the field is absent or the JSON is unparseable,
/// which covers the old 0.6-era configs that have no version field.
fn extract_schema_version(json: &str) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        #[serde(default)]
        schema_version: Option<u32>,
    }
    serde_json::from_str::<VersionProbe>(json)
        .ok()
        .and_then(|p| p.schema_version)
}

/// Load default cmdline rules (embedded), without touching the filesystem.
pub fn default_cmdline_rules() -> Vec<CmdlineRule> {
    let (rules, _, _) =
        parse_json_rules(DEFAULT_AGENTS_JSON).expect("embedded DEFAULT_AGENTS_JSON is valid");
    rules
}

// ==================== Chrome Trace Export ====================

/// Check if chrome trace export is enabled (set once at startup)
pub fn chrome_trace() -> bool {
    static CHROME_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CHROME_TRACE.get_or_init(|| std::env::var("AGENTSIGHT_CHROME_TRACE").is_ok())
}

// ==================== Channel Policy ====================

/// Policy applied when a bounded event channel is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelPolicy {
    /// Block the producer until space is available (backpressure).
    #[default]
    Backpressure,
    /// Drop the newest event and log a warning.
    DropNewest,
    /// Sample events: keep roughly 1 in N when overloaded.
    Sample(usize),
}

impl From<&str> for ChannelPolicy {
    fn from(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "backpressure" => ChannelPolicy::Backpressure,
            "drop_newest" | "drop-newest" | "drop" => ChannelPolicy::DropNewest,
            s if s.starts_with("sample") => {
                let n = s
                    .split([':', '='])
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(10);
                ChannelPolicy::Sample(n.max(1))
            }
            _ => ChannelPolicy::Backpressure,
        }
    }
}

// ==================== Feature Flags ====================

/// Fine-grained feature toggles parsed from `agentsight.json`.
#[derive(Debug, Clone)]
pub struct FeatureFlags {
    /// Core token metering (always enabled).
    pub token_stats: bool,
    /// Local tokenizer for usage-less token counting.
    pub tokenizer_enabled: bool,
    /// Max cached tokenizer models.
    pub tokenizer_cache_size: usize,
    /// Response ID → session ID mapping.
    pub session_mapping_enabled: bool,
    /// Max entries in the response/session mapper.
    pub session_mapping_max_entries: usize,
    /// Local SQLite persistence of GenAI events.
    pub sqlite_storage_enabled: bool,
    /// Periodic Agent process CPU and RSS resource sampling.
    pub resource_sampling_enabled: bool,
    /// Optional SQLite batch insert config.
    pub sqlite_batch: Option<BatchConfig>,
    /// Interruption / DeadLoop detection.
    pub interruption_detection_enabled: bool,
    /// Audit event storage.
    pub audit_enabled: bool,
    /// Token consumption breakdown storage.
    pub token_consumption_enabled: bool,
    /// SLS Logtail export.
    pub sls_logtail_enabled: bool,
    /// Qoder/QoderWork trajectory collection (scan + ATIF convert + persist).
    pub trajectory_collection_enabled: bool,
    /// Seconds between two trajectory scan rounds.
    pub trajectory_scan_interval_secs: u64,
    /// Optional override of the trajectory projects directories to scan.
    pub trajectory_scan_dirs: Option<Vec<String>>,
    /// Whether a model may be asked to label collected trajectories.
    ///
    /// Off by default because every judgement is a paid request. The
    /// deterministic rules keep working without it; they simply never reach
    /// `bad`, which then only a person can assign.
    pub reuse_llm_judge_enabled: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            token_stats: true,
            tokenizer_enabled: false,
            tokenizer_cache_size: DEFAULT_TOKENIZER_CACHE_SIZE,
            session_mapping_enabled: false,
            session_mapping_max_entries: 10_000,
            sqlite_storage_enabled: false,
            resource_sampling_enabled: false,
            sqlite_batch: None,
            interruption_detection_enabled: false,
            audit_enabled: false,
            token_consumption_enabled: false,
            sls_logtail_enabled: false,
            trajectory_collection_enabled: false,
            trajectory_scan_interval_secs: DEFAULT_TRAJECTORY_SCAN_INTERVAL_SECS,
            trajectory_scan_dirs: None,
            reuse_llm_judge_enabled: false,
        }
    }
}

/// SQLite batch insert configuration.
#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub max_size: usize,
    pub flush_ms: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_size: 100,
            flush_ms: 100,
        }
    }
}

/// Resource limits to prevent in-memory buffering leaks.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeLimits {
    /// Bounded channel capacity between probes and the event loop.
    pub event_channel_capacity: usize,
    /// Policy when the event channel is full.
    pub event_channel_policy: ChannelPolicy,
    /// Byte budget for events queued in the event channel.
    ///
    /// Enforced alongside `event_channel_capacity`: whichever limit is reached
    /// first stops admission. Needed because event sizes span four orders of
    /// magnitude (a 16 KiB SSL record vs. a 4 MiB one).
    pub event_channel_max_bytes: usize,
    /// Max pending GenAI events waiting for session_id resolution.
    pub pending_genai_max_count: usize,
    /// Max bytes for pending GenAI events.
    pub pending_genai_max_bytes: usize,
    /// Max entries in PID → agent_name cache.
    pub pid_cache_size: usize,
    /// Max bytes buffered per HTTP connection.
    pub max_connection_body_bytes: usize,
    /// Idle timeout before an HTTP connection state is dropped.
    pub connection_idle_timeout_secs: u64,
    /// eBPF ring buffer size in MiB.
    pub ring_buffer_mb: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            event_channel_capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
            event_channel_policy: ChannelPolicy::Backpressure,
            event_channel_max_bytes: DEFAULT_EVENT_CHANNEL_MAX_BYTES,
            pending_genai_max_count: DEFAULT_PENDING_GENAI_MAX_COUNT,
            pending_genai_max_bytes: DEFAULT_PENDING_GENAI_MAX_BYTES,
            pid_cache_size: DEFAULT_PID_CACHE_SIZE,
            max_connection_body_bytes: DEFAULT_MAX_CONNECTION_BODY_BYTES,
            connection_idle_timeout_secs: DEFAULT_CONNECTION_IDLE_TIMEOUT_SECS,
            ring_buffer_mb: DEFAULT_RING_BUFFER_MB,
        }
    }
}

// ==================== AgentsightConfig ====================

/// Unified configuration for AgentSight
///
/// This struct contains all configuration parameters for the AgentSight system,
/// including storage, probing, parsing, aggregation, and analysis settings.
#[derive(Debug, Clone)]
pub struct AgentsightConfig {
    // --- Storage Configuration ---
    /// SQLite paths and lifecycle policies.
    pub storage: StorageConfig,
    /// Audit table name
    pub audit_table: String,
    /// Token table name
    pub token_table: String,
    /// HTTP table name
    pub http_table: String,

    // --- Trace Control ---
    /// Controls whether SLS-uploaded `LLMCall` records carry conversation
    /// content fields (`gen_ai.input.messages`, `gen_ai.output.messages`,
    /// `gen_ai.system_instructions`).
    ///
    /// * `false` (default): only token / model / provider / timing metadata
    ///   is uploaded; conversation bodies are dropped at the SLS layer.
    /// * `true`: full conversation content is uploaded.
    ///
    /// This flag does **not** stop the agent itself; eBPF probes, local
    /// SQLite persistence and token metering keep running regardless.
    /// Local SQLite always stores full content; this only controls SLS.
    ///
    /// JSON field name: `traceEnabled`.
    pub trace_enabled: bool,

    // --- Probe Configuration ---
    /// Optional UID filter for process tracing
    pub target_uid: Option<u32>,
    /// Poll timeout for ring buffer polling (milliseconds)
    pub poll_timeout_ms: u64,
    /// Enable file watch probe (monitors .jsonl file opens from traced processes)
    pub enable_filewatch: bool,
    /// Enable cgroup-level event filtering. When true, proctrace /
    /// filewatch / filewrite only emit events from cgroup ids
    /// registered via `Probes::add_traced_cgroup`. procmon is unaffected
    /// (full audit coverage). Default: false (no cgroup filtering).
    pub cgroup_filter_enabled: bool,
    /// Cgroup inode IDs seeded into `cgroup_filter` at startup.
    /// Only effective when `cgroup_filter_enabled` is true.
    /// Obtain via `stat -c %i /sys/fs/cgroup/<path>` (v2) or
    /// `stat -c %i /sys/fs/cgroup/memory/<path>` (v1).
    pub cgroup_ids: Vec<u64>,
    /// TCP capture targets for plain HTTP capture (empty = disabled).
    /// Each entry specifies destination IP, port, or both.
    pub tcp_targets: Vec<TcpTarget>,

    // --- HTTP/Aggregation Configuration ---
    /// LRU cache capacity for HTTP connections
    pub connection_capacity: usize,
    /// Minimum duration threshold for HTTP requests (microseconds)
    pub min_duration_us: u64,

    // --- Parser Configuration ---
    /// Maximum number of HTTP headers to parse
    pub max_headers: usize,

    // --- Analyzer Configuration ---
    /// Maximum body length for audit analysis
    pub max_body_len: usize,

    // --- Logging Configuration ---
    /// Enable verbose logging
    pub verbose: bool,
    /// Log file path (None = stderr)
    pub log_path: Option<String>,

    // --- Tokenizer Configuration ---
    /// Path to tokenizer file for accurate token counting (e.g., "/path/to/tokenizer.json")
    pub tokenizer_path: Option<PathBuf>,
    /// URL to download tokenizer from (e.g., "https://modelscope.cn/.../tokenizer.json")
    pub tokenizer_url: Option<String>,

    // --- FFI Rule Configuration ---
    /// User-defined cmdline rules for process allowlist/denylist
    pub cmdline_rules: Vec<CmdlineRule>,
    /// User-defined HTTPS rules for DNS-based SSL attachment
    pub https_rules: Vec<HttpsRule>,
    /// User-defined HTTP targets (IP/port endpoints + domains for tcpsniff)
    pub http_targets: Vec<HttpTarget>,
    /// Whether FFI mode emits raw HTTPS fallback events.
    /// This is configured only through the C API and is not part of agentsight.json.
    pub ffi_enable_raw_https: bool,
    /// Whether FFI mode subscribes to normalized security audit events.
    /// This is configured only through the C API and is not part of agentsight.json.
    pub ffi_enable_security_audit: bool,
    /// Local enforcer socket used by the optional FFI security subscription.
    pub ffi_enforcer_socket: PathBuf,

    // --- Procfs Root ---
    /// Procfs mount point that pid-keyed lookups resolve through.
    ///
    /// Defaults to `/proc`. Point it at a bind-mounted host procfs
    /// (`/logtail_host/proc`) to observe processes living in other pid
    /// namespaces without joining them -- a DaemonSet then needs no
    /// `hostPID: true`. Applied process-wide at startup; see
    /// [`crate::utils::procfs`] for what does and does not follow it.
    pub procfs_root: PathBuf,

    // --- Config File Path ---
    /// Path to JSON configuration file
    pub config_path: Option<PathBuf>,

    // --- Encryption Configuration ---
    /// RSA 公钥（PEM 字符串）。从 agentsight.json `encryption.public_key`
    /// 或 `encryption.public_key_path` 加载。若为 None，则不加密敏感消息字段。
    pub encryption_public_key: Option<String>,

    // --- Runtime Dynamic Configuration ---
    /// SLS Logtail 输出文件路径（来自 `runtime.sls_logtail_path`）。
    /// 非空时激活 SLS 上传。支持运行期热加载。
    pub sls_logtail_path: Option<String>,

    // --- DeadLoop Auto-Kill Configuration ---
    /// 是否启用 DeadLoop 自动 kill 止血（默认 false）
    pub deadloop_kill_enabled: bool,
    /// 触发 kill 的循环次数阈值（检测到 N 次后 kill）
    pub deadloop_kill_after_count: usize,

    // --- Feature Toggles ---
    /// Fine-grained feature flags (token_stats is the only default-on feature).
    pub features: FeatureFlags,

    // --- Runtime Resource Limits ---
    /// Bounded channel capacities, pending queue limits, etc.
    pub runtime_limits: RuntimeLimits,

    // --- Server Authentication ---
    /// Dashboard authentication configuration.
    pub server_auth: ServerAuthConfig,
}

impl Default for AgentsightConfig {
    fn default() -> Self {
        Self {
            // Storage defaults
            storage: StorageConfig::default(),
            audit_table: DEFAULT_AUDIT_TABLE.to_string(),
            token_table: DEFAULT_TOKEN_TABLE.to_string(),
            http_table: DEFAULT_HTTP_TABLE.to_string(),

            // Trace control defaults
            // Default = false (privacy-safe): SLS uploads carry only token /
            // model / timing metadata. Conversation content is opt-in via
            // explicit `"traceEnabled": true` in the config file.
            trace_enabled: false,

            // Probe defaults
            target_uid: None,
            poll_timeout_ms: DEFAULT_POLL_TIMEOUT_MS,
            enable_filewatch: false,
            cgroup_filter_enabled: false,
            cgroup_ids: Vec::new(),
            tcp_targets: Vec::new(),

            // HTTP/Aggregation defaults
            connection_capacity: DEFAULT_CONNECTION_CAPACITY,
            min_duration_us: DEFAULT_MIN_DUR_US,

            // Parser defaults
            max_headers: DEFAULT_MAX_HEADERS,

            // Analyzer defaults
            max_body_len: DEFAULT_MAX_BODY_LEN,

            // Logging defaults
            verbose: false,
            log_path: None,

            // Tokenizer defaults (read from env vars)
            tokenizer_path: std::env::var("AGENTSIGHT_TOKENIZER_PATH")
                .ok()
                .map(PathBuf::from),
            tokenizer_url: Some(
                "https://www.modelscope.cn/models/Qwen/Qwen3.5-27B/resolve/master/tokenizer.json"
                    .to_owned(),
            ),

            // FFI Rule defaults
            cmdline_rules: Vec::new(),
            https_rules: Vec::new(),
            http_targets: Vec::new(),
            ffi_enable_raw_https: false,
            ffi_enable_security_audit: false,
            ffi_enforcer_socket: PathBuf::from("/run/agentsight/enforcer.sock"),

            // Read our own procfs unless a deployment points us elsewhere.
            procfs_root: PathBuf::from(crate::utils::procfs::DEFAULT_PROC_ROOT),

            // Config file path default
            config_path: None,

            // Encryption defaults (loaded from config file)
            encryption_public_key: None,

            // Runtime dynamic configuration defaults
            sls_logtail_path: None,

            // DeadLoop auto-kill defaults (disabled by default)
            deadloop_kill_enabled: false,
            deadloop_kill_after_count: 3,

            // Feature toggles (only token_stats is on by default)
            features: FeatureFlags::default(),

            // Runtime resource limits
            runtime_limits: RuntimeLimits::default(),

            // Server authentication
            server_auth: ServerAuthConfig::default(),
        }
    }
}

impl AgentsightConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new configuration with custom storage base path
    pub fn with_storage_path(base_path: PathBuf) -> Self {
        let mut config = Self::default();
        config.storage.base_path = base_path;
        config
    }

    /// Get the full path to the primary database
    pub fn db_path(&self) -> PathBuf {
        self.storage.primary_path()
    }

    /// Get the audit table name
    pub fn audit_table_name(&self) -> &str {
        &self.audit_table
    }

    /// Get the token table name
    pub fn token_table_name(&self) -> &str {
        &self.token_table
    }

    /// Set verbose mode
    pub fn set_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Set storage base path
    pub fn set_storage_path(mut self, path: PathBuf) -> Self {
        self.storage.base_path = path;
        self
    }

    /// Set target UID
    pub fn set_target_uid(mut self, uid: Option<u32>) -> Self {
        self.target_uid = uid;
        self
    }

    /// Set enable_filewatch
    pub fn set_enable_filewatch(mut self, enable: bool) -> Self {
        self.enable_filewatch = enable;
        self
    }

    /// Enable or disable cgroup-level event filtering.
    ///
    /// When enabled, only events from cgroup ids registered via
    /// `Probes::add_traced_cgroup` are emitted by the filtered probes.
    /// procmon keeps full audit coverage regardless. Must be set before
    /// probes are created (the value is baked into the BPF rodata at load).
    pub fn set_cgroup_filter_enabled(mut self, enabled: bool) -> Self {
        self.cgroup_filter_enabled = enabled;
        self
    }

    /// Set connection capacity
    pub fn set_connection_capacity(mut self, capacity: usize) -> Self {
        self.connection_capacity = capacity;
        self
    }

    /// Apply verbose setting to the global state
    pub fn apply_verbose(&self) {
        init_logging(self.verbose, self.log_path.as_deref());
    }

    /// Load configuration from a JSON string, appending rules to existing ones.
    ///
    /// Parses `verbose`, `log_path`, `cmdline`, `https` and `http` fields.
    pub fn load_from_json(&mut self, json: &str) -> Result<(), String> {
        let mut parsed: JsonFullConfig =
            serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;

        // Warn if the config's schema_version is older than expected. By this
        // point ensure_default_agents_config should have already upgraded stale
        // configs, so this is just a safety net for edge cases.
        if let Some(v) = parsed.schema_version {
            if v < CURRENT_SCHEMA_VERSION {
                log::warn!(
                    "Config schema_version {v} < {CURRENT_SCHEMA_VERSION}; \
                     some features may not work as expected"
                );
            }
        }

        if let Some(t) = parsed.trace_enabled {
            self.trace_enabled = t;
        }
        if let Some(v) = parsed.verbose {
            self.verbose = v != 0;
        }
        if let Some(p) = parsed.log_path.take() {
            self.log_path = Some(p);
        }

        // 加载加密公钥：优先 public_key（内联 PEM），其次 public_key_path（文件路径）
        if let Some(enc) = parsed.encryption.take() {
            if let Some(pem) = enc.public_key {
                let trimmed = pem.trim();
                if !trimmed.is_empty() {
                    self.encryption_public_key = Some(trimmed.to_string());
                }
            } else if let Some(path) = enc.public_key_path {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    match std::fs::read_to_string(trimmed) {
                        Ok(content) => {
                            self.encryption_public_key = Some(content);
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to read encryption public_key_path {trimmed:?}: {e}, encryption disabled"
                            );
                        }
                    }
                }
            }
        }

        // 解析 runtime 动态配置
        if let Some(ref rt) = parsed.runtime {
            if let Some(ref path) = rt.sls_logtail_path {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    self.sls_logtail_path = Some(trimmed.to_string());
                }
            }
        }

        // 解析 deadloop 自动 kill 配置
        if let Some(ref dl) = parsed.deadloop {
            if let Some(enabled) = dl.enabled {
                self.deadloop_kill_enabled = enabled;
            }
            if let Some(count) = dl.kill_after_count {
                self.deadloop_kill_after_count = count;
            }
        }

        // Parse cgroup filter settings
        if let Some(v) = parsed.cgroup_filter_enabled {
            self.cgroup_filter_enabled = v;
        }
        if let Some(ids) = parsed.cgroup_ids.take() {
            self.cgroup_ids = ids;
        }

        if let Some(storage) = parsed.storage.take() {
            let mut resolved = self.storage.clone();
            resolved.apply_json(storage);
            resolved.validate()?;
            self.storage = resolved;
        }

        // Parse feature toggles
        if let Some(features) = parsed.features.take() {
            self.features = FeatureFlags {
                token_stats: features.token_stats.unwrap_or(true),
                tokenizer_enabled: features
                    .tokenizer
                    .as_ref()
                    .and_then(|f| f.enabled)
                    .unwrap_or(false),
                tokenizer_cache_size: features
                    .tokenizer
                    .as_ref()
                    .and_then(|f| f.cache_size)
                    .unwrap_or(DEFAULT_TOKENIZER_CACHE_SIZE),
                session_mapping_enabled: features
                    .session_mapping
                    .as_ref()
                    .and_then(|f| f.enabled)
                    .unwrap_or(false),
                session_mapping_max_entries: features
                    .session_mapping
                    .as_ref()
                    .and_then(|f| f.max_entries)
                    .unwrap_or(10_000),
                sqlite_storage_enabled: features
                    .sqlite_storage
                    .as_ref()
                    .and_then(|f| f.enabled)
                    .unwrap_or(false),
                resource_sampling_enabled: features.resource_sampling.unwrap_or(false),
                sqlite_batch: features.sqlite_storage.as_ref().and_then(|f| {
                    f.batch.as_ref().map(|b| BatchConfig {
                        max_size: b.max_size.unwrap_or(100),
                        flush_ms: b.flush_ms.unwrap_or(100),
                    })
                }),
                interruption_detection_enabled: features
                    .interruption_detection
                    .as_ref()
                    .and_then(|f| f.enabled)
                    .unwrap_or(false),
                audit_enabled: features.audit.unwrap_or(false),
                token_consumption_enabled: features.token_consumption.unwrap_or(false),
                sls_logtail_enabled: features.sls_logtail.unwrap_or(false),
                reuse_llm_judge_enabled: features.reuse_llm_judge.unwrap_or(false),
                trajectory_collection_enabled: features
                    .trajectory_collection
                    .as_ref()
                    .and_then(|f| f.enabled)
                    .unwrap_or(false),
                trajectory_scan_interval_secs: features
                    .trajectory_collection
                    .as_ref()
                    .and_then(|f| f.scan_interval_secs)
                    .unwrap_or(DEFAULT_TRAJECTORY_SCAN_INTERVAL_SECS),
                trajectory_scan_dirs: features
                    .trajectory_collection
                    .as_ref()
                    .and_then(|f| f.scan_dirs.clone()),
            };
        }

        // Parse runtime limits
        if let Some(limits) = parsed.runtime_limits.take() {
            self.runtime_limits = RuntimeLimits {
                event_channel_capacity: limits
                    .event_channel_capacity
                    .unwrap_or(DEFAULT_EVENT_CHANNEL_CAPACITY),
                event_channel_policy: limits
                    .event_channel_policy
                    .as_deref()
                    .map(ChannelPolicy::from)
                    .unwrap_or_default(),
                event_channel_max_bytes: limits
                    .event_channel_max_bytes_mb
                    .map(|mb| mb * 1024 * 1024)
                    .unwrap_or(DEFAULT_EVENT_CHANNEL_MAX_BYTES),
                pending_genai_max_count: limits
                    .pending_genai_max_count
                    .unwrap_or(DEFAULT_PENDING_GENAI_MAX_COUNT),
                pending_genai_max_bytes: limits
                    .pending_genai_max_bytes_mb
                    .map(|mb| mb * 1024 * 1024)
                    .unwrap_or(DEFAULT_PENDING_GENAI_MAX_BYTES),
                pid_cache_size: limits.pid_cache_size.unwrap_or(DEFAULT_PID_CACHE_SIZE),
                max_connection_body_bytes: limits
                    .max_connection_body_mb
                    .map(|mb| mb * 1024 * 1024)
                    .unwrap_or(DEFAULT_MAX_CONNECTION_BODY_BYTES),
                connection_idle_timeout_secs: limits
                    .connection_idle_timeout_secs
                    .unwrap_or(DEFAULT_CONNECTION_IDLE_TIMEOUT_SECS),
                ring_buffer_mb: limits.ring_buffer_mb.unwrap_or(DEFAULT_RING_BUFFER_MB),
            };
        }

        // Parse server auth configuration
        if let Some(ref server) = parsed.server {
            if let Some(ref auth) = server.auth {
                if let Some(enabled) = auth.enabled {
                    self.server_auth.enabled = enabled;
                }
            }
        }

        let (cmdline_rules, https_rules, http_targets) = extract_rules(&parsed);
        self.cmdline_rules.extend(cmdline_rules);
        self.https_rules.extend(https_rules);
        self.http_targets.extend(http_targets);
        Ok(())
    }

    /// Set tokenizer path
    pub fn set_tokenizer_path(mut self, path: Option<PathBuf>) -> Self {
        self.tokenizer_path = path;
        self
    }

    /// Set tokenizer URL
    pub fn set_tokenizer_url(mut self, url: Option<String>) -> Self {
        self.tokenizer_url = url;
        self
    }

    /// Add a cmdline rule
    pub fn add_cmdline_rule(mut self, rule: CmdlineRule) -> Self {
        self.cmdline_rules.push(rule);
        self
    }

    /// Add an HTTPS rule (domain glob pattern for SSL attachment)
    pub fn add_https_rule(mut self, rule: HttpsRule) -> Self {
        self.https_rules.push(rule);
        self
    }

    /// Add an HTTP target (IP/port endpoint or domain for tcpsniff)
    pub fn add_http_target(mut self, target: HttpTarget) -> Self {
        self.http_targets.push(target);
        self
    }

    /// Set config file path
    pub fn set_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Load configuration from a JSON file, appending rules to existing ones.
    ///
    /// In the standard flow the config starts from `AgentsightConfig::new()`
    /// (empty rules), so this call effectively **replaces** the embedded
    /// defaults with the file content.
    ///
    /// Reads the file and delegates to `load_from_json`. All fields supported by
    /// `load_from_json` (verbose, log_path, cmdline, domain) are loaded.
    pub fn load_from_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config from {path:?}"))?;
        self.load_from_json(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config from {path:?}: {e}"))
    }

    /// Resolve the effective config file path.
    ///
    /// # Panics
    /// Panics if `config_path` was not set via `set_config_path` (CLI `--config`).
    pub fn resolve_config_path(&self) -> PathBuf {
        assert!(
            self.config_path.is_some(),
            "config_path must be set via --config"
        );
        self.config_path.clone().unwrap()
    }
}

/// Parse `runtime.sls_logtail_path` from a JSON config string (tri-state).
///
/// Returns:
/// * `None`               — field is absent or JSON parse failed (no signal).
/// * `Some(None)`         — field is present but empty / whitespace-only
///                          → **deactivation** signal (pause SLS uploads).
/// * `Some(Some(path))`   — field is present and non-empty (after trim)
///                          → **activation / re-activation** signal.
///
/// Used by the config watcher to react to runtime hot-reload changes
/// (empty string → pause SLS uploads; non-empty path → activate).
pub fn parse_runtime_sls_path(json: &str) -> Option<Option<String>> {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        runtime: Option<JsonRuntime>,
    }
    let parsed: Partial = serde_json::from_str(json).ok()?;
    let rt = parsed.runtime?;
    let path = rt.sls_logtail_path?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        Some(None)
    } else {
        Some(Some(trimmed.to_string()))
    }
}

/// Get the default base path for storage
///
/// Returns `$HOME/.agentsight` or `/tmp/.agentsight` if HOME is not set
pub fn default_base_path() -> PathBuf {
    let home = "/var/log/sysak/";
    PathBuf::from(home).join(".agentsight")
}

mod event_clock;
pub use event_clock::{ClockConversionError, ktime_to_unix_ns};
#[cfg(target_os = "linux")]
pub(crate) use event_clock::{initialize_event_clock, report_clock_error};
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tcp_target_parse_exact() {
        let t: TcpTarget = "10.0.0.1:8080".parse().unwrap();
        assert_eq!(t.ip, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(t.port, Some(8080));
    }

    #[test]
    fn test_tcp_target_parse_port_only() {
        let t: TcpTarget = ":8080".parse().unwrap();
        assert_eq!(t.ip, None);
        assert_eq!(t.port, Some(8080));

        // "*:8080" is an alias of ":8080"
        let t2: TcpTarget = "*:8080".parse().unwrap();
        assert_eq!(t2, t);
    }

    #[test]
    fn test_tcp_target_parse_ip_only() {
        let t: TcpTarget = "10.0.0.1".parse().unwrap();
        assert_eq!(t.ip, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(t.port, None);

        // "10.0.0.1:*" is an alias of "10.0.0.1"
        let t2: TcpTarget = "10.0.0.1:*".parse().unwrap();
        assert_eq!(t2, t);
    }

    #[test]
    fn test_tcp_target_parse_full_wildcard() {
        for s in ["*", "*:*", ":*"] {
            let t: TcpTarget = s.parse().unwrap();
            assert_eq!(t.ip, None, "{s}");
            assert_eq!(t.port, None, "{s}");
        }
    }

    #[test]
    fn test_tcp_target_parse_invalid() {
        assert!("".parse::<TcpTarget>().is_err());
        assert!("not-an-ip".parse::<TcpTarget>().is_err());
        assert!("10.0.0.1:bad".parse::<TcpTarget>().is_err());
        assert!("bad:8080".parse::<TcpTarget>().is_err());
    }

    #[test]
    fn test_tcp_target_parse_via_http_targets() {
        let json = r#"{"http": [{"rule": ["*", "*:8080", "10.0.0.1:*", "10.0.0.1:9090", "some.host.com"]}]}"#;
        let (_, _, http_targets) = parse_json_rules(json).unwrap();
        assert_eq!(http_targets.len(), 5);
        // 0: full wildcard endpoint
        match &http_targets[0] {
            HttpTarget::Endpoint(t) => {
                assert_eq!(t.ip, None);
                assert_eq!(t.port, None);
            }
            _ => panic!("expected Endpoint"),
        }
        // 4: domain (unparseable as TcpTarget)
        matches!(http_targets[4], HttpTarget::Domain(_));
    }

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_CONNECTION_CAPACITY, 24);
        assert_eq!(DEFAULT_POLL_TIMEOUT_MS, 100);
        assert_eq!(DEFAULT_MIN_DUR_US, 10_000);
        assert_eq!(DEFAULT_MAX_BODY_LEN, 64 * 1024);
        assert_eq!(DEFAULT_MAX_HEADERS, 64);
        assert_eq!(PRIMARY_DB_NAME, "agentsight.db");
        assert_eq!(DEFAULT_AUDIT_TABLE, "audit_events");
        assert_eq!(DEFAULT_TOKEN_TABLE, "token_records");
        assert_eq!(DEFAULT_HTTP_TABLE, "http_records");
        assert_eq!(DEFAULT_RETENTION_DAYS, 30);
        assert_eq!(DEFAULT_MAINTENANCE_INTERVAL_SECS, 60);
    }

    #[test]
    fn test_hf_home() {
        let path = hf_home();
        assert!(path.to_str().unwrap().contains(".agentsight/tokenizers"));
    }

    #[test]
    fn test_default_base_path() {
        let path = default_base_path();
        assert_eq!(path, PathBuf::from("/var/log/sysak/.agentsight"));
    }

    #[test]
    fn test_config_new_defaults() {
        let config = AgentsightConfig::new();
        assert_eq!(
            config.storage.primary_path(),
            default_base_path().join(PRIMARY_DB_NAME)
        );
        assert_eq!(config.connection_capacity, 24);
        assert_eq!(config.poll_timeout_ms, 100);
        assert_eq!(config.min_duration_us, 10_000);
        assert_eq!(config.max_headers, 64);
        assert_eq!(config.max_body_len, 64 * 1024);
        assert!(!config.verbose);
        assert!(config.log_path.is_none());
        assert!(config.target_uid.is_none());
        assert!(!config.enable_filewatch);
        assert!(!config.cgroup_filter_enabled);
        assert_eq!(config.storage.primary.retention_days, 30);
        assert_eq!(config.storage.primary.check_interval_secs, 60);
        assert_eq!(config.storage.primary.max_db_size_mb, 500);
        assert_eq!(config.storage.genai.max_db_size_mb, 200);
        assert_eq!(config.storage.genai.check_interval_secs, 60);
        assert_eq!(config.storage.interruptions.check_interval_secs, 60);
        assert_eq!(
            config.storage.reuse,
            PeriodicStoragePolicy::new(30, 200, 300)
        );
        assert_eq!(
            config.storage.causal,
            PeriodicStoragePolicy::new(30, 200, 300)
        );
        assert_eq!(
            config.storage.enforcement,
            PeriodicStoragePolicy::new(30, 100, 60)
        );
    }

    /// `traceEnabled` is **off** by default (privacy-safe). Conversation
    /// content fields are dropped from SLS uploads unless the config file
    /// explicitly sets `"traceEnabled": true`. This test locks that default
    /// so it cannot silently regress.
    #[test]
    fn test_trace_enabled_default_is_false() {
        let config = AgentsightConfig::new();
        assert!(
            !config.trace_enabled,
            "AgentsightConfig::new() must default trace_enabled=false to keep \
             SLS uploads free of conversation content unless explicitly opted in"
        );
    }

    /// Loading a config that omits `traceEnabled` must NOT flip the default
    /// (the field is `Option<bool>` and only overrides when `Some`).
    #[test]
    fn test_load_from_json_missing_trace_enabled_keeps_default_false() {
        let mut config = AgentsightConfig::new();
        config.load_from_json("{}").unwrap();
        assert!(!config.trace_enabled);
    }

    /// Explicit `"traceEnabled": true` opts into uploading conversation content.
    #[test]
    fn test_load_from_json_explicit_trace_enabled_true() {
        let mut config = AgentsightConfig::new();
        config.load_from_json(r#"{"traceEnabled": true}"#).unwrap();
        assert!(config.trace_enabled);
    }

    #[test]
    fn test_storage_config_loads_per_store_policies() {
        let mut config = AgentsightConfig::new();
        config
            .load_from_json(
                r#"{
                    "storage": {
                        "base_path": "/tmp/agentsight-storage",
                        "primary": {"retention_days": 7, "check_interval_secs": 25},
                        "genai": {"max_db_size_mb": 321},
                        "interruptions": {"check_interval_secs": 15},
                        "reuse": {"retention_days": 9},
                        "causal": {"max_db_size_mb": 123},
                        "enforcement": {"check_interval_secs": 17}
                    }
                }"#,
            )
            .unwrap();

        assert_eq!(
            config.storage.base_path,
            PathBuf::from("/tmp/agentsight-storage")
        );
        assert_eq!(config.storage.primary.retention_days, 7);
        assert_eq!(config.storage.primary.max_db_size_mb, 500);
        assert_eq!(config.storage.primary.check_interval_secs, 25);
        assert_eq!(config.storage.genai.retention_days, 30);
        assert_eq!(config.storage.genai.max_db_size_mb, 321);
        assert_eq!(config.storage.interruptions.check_interval_secs, 15);
        assert_eq!(config.storage.reuse.retention_days, 9);
        assert_eq!(config.storage.causal.max_db_size_mb, 123);
        assert_eq!(config.storage.enforcement.check_interval_secs, 17);
    }

    #[test]
    fn test_storage_config_rejects_insert_interval() {
        let mut config = AgentsightConfig::new();
        let error = config
            .load_from_json(r#"{"storage":{"primary":{"check_interval_inserts":1}}}"#)
            .unwrap_err();
        assert!(error.contains("unknown field `check_interval_inserts`"));
    }

    #[test]
    fn test_storage_config_rejects_invalid_values() {
        let mut config = AgentsightConfig::new();
        assert!(
            config
                .load_from_json(r#"{"storage":{"base_path":""}}"#)
                .is_err()
        );

        let mut config = AgentsightConfig::new();
        let json = format!(
            r#"{{"storage":{{"primary":{{"retention_days":{}}}}}}}"#,
            u64::MAX
        );
        assert!(config.load_from_json(&json).is_err());
    }

    #[test]
    fn test_config_with_storage_path() {
        let config = AgentsightConfig::with_storage_path(PathBuf::from("/tmp/test"));
        assert_eq!(config.storage.base_path, PathBuf::from("/tmp/test"));
        assert_eq!(
            config.storage.primary_path(),
            PathBuf::from("/tmp/test/agentsight.db")
        );
    }

    #[test]
    fn test_config_db_path() {
        let config = AgentsightConfig::with_storage_path(PathBuf::from("/tmp/mydata"));
        assert_eq!(config.db_path(), PathBuf::from("/tmp/mydata/agentsight.db"));
    }

    #[test]
    fn test_config_table_names() {
        let config = AgentsightConfig::new();
        assert_eq!(config.audit_table_name(), "audit_events");
        assert_eq!(config.token_table_name(), "token_records");
    }

    #[test]
    fn test_config_builder_methods() {
        let config = AgentsightConfig::new()
            .set_verbose(true)
            .set_storage_path(PathBuf::from("/custom"))
            .set_target_uid(Some(1000))
            .set_enable_filewatch(true)
            .set_connection_capacity(48);
        assert!(config.verbose);
        assert_eq!(config.storage.base_path, PathBuf::from("/custom"));
        assert_eq!(config.target_uid, Some(1000));
        assert!(config.enable_filewatch);
        assert_eq!(config.connection_capacity, 48);
    }

    #[test]
    fn test_set_tokenizer_path() {
        let config = AgentsightConfig::new()
            .set_tokenizer_path(Some(PathBuf::from("/path/to/tokenizer.json")));
        assert_eq!(
            config.tokenizer_path,
            Some(PathBuf::from("/path/to/tokenizer.json"))
        );
    }

    #[test]
    fn test_set_tokenizer_url() {
        let config =
            AgentsightConfig::new().set_tokenizer_url(Some("https://example.com/tok.json".into()));
        assert_eq!(
            config.tokenizer_url,
            Some("https://example.com/tok.json".to_string())
        );
    }

    #[test]
    fn test_verbose_default_false() {
        // verbose() reads from global static; default should be false
        // Note: other tests might have set it, so just check it doesn't panic
        let _ = verbose();
    }

    #[test]
    fn test_add_cmdline_rule() {
        let rule = CmdlineRule {
            patterns: vec!["node".to_string(), "*claude*".to_string()],
            agent_name: Some("Claude Code".to_string()),
            allow: true,
        };
        let config = AgentsightConfig::new().add_cmdline_rule(rule);
        assert_eq!(config.cmdline_rules.len(), 1);
        assert_eq!(config.cmdline_rules[0].patterns, vec!["node", "*claude*"]);
        assert_eq!(
            config.cmdline_rules[0].agent_name,
            Some("Claude Code".to_string())
        );
        assert!(config.cmdline_rules[0].allow);
    }

    #[test]
    fn test_add_cmdline_rule_deny() {
        let rule = CmdlineRule {
            patterns: vec!["node".to_string(), "*webpack*".to_string()],
            agent_name: None,
            allow: false,
        };
        let config = AgentsightConfig::new().add_cmdline_rule(rule);
        assert_eq!(config.cmdline_rules.len(), 1);
        assert!(!config.cmdline_rules[0].allow);
        assert!(config.cmdline_rules[0].agent_name.is_none());
    }

    #[test]
    fn test_add_https_rule() {
        let rule = HttpsRule {
            pattern: "*.openai.com".to_string(),
        };
        let config = AgentsightConfig::new().add_https_rule(rule);
        assert_eq!(config.https_rules.len(), 1);
        assert_eq!(config.https_rules[0].pattern, "*.openai.com");
    }

    #[test]
    fn test_add_multiple_rules() {
        let config = AgentsightConfig::new()
            .add_cmdline_rule(CmdlineRule {
                patterns: vec!["node".to_string()],
                agent_name: Some("Agent1".to_string()),
                allow: true,
            })
            .add_cmdline_rule(CmdlineRule {
                patterns: vec!["python3".to_string()],
                agent_name: Some("Agent2".to_string()),
                allow: true,
            })
            .add_https_rule(HttpsRule {
                pattern: "*.openai.com".to_string(),
            })
            .add_https_rule(HttpsRule {
                pattern: "*.anthropic.com".to_string(),
            });
        assert_eq!(config.cmdline_rules.len(), 2);
        assert_eq!(config.https_rules.len(), 2);
    }

    #[test]
    fn test_default_cmdline_rules() {
        let rules = default_cmdline_rules();
        assert!(!rules.is_empty());
        // Contains both allow rules and the default deny rules (e.g. the
        // sftp-server subprocess exclusion).
        assert!(rules.iter().any(|r| r.allow));
        assert!(
            rules
                .iter()
                .any(|r| !r.allow && r.patterns.iter().any(|p| p.contains("sftp-server")))
        );
        // Should contain Hermes, Cosh, OpenClaw agent names
        let names: Vec<&str> = rules
            .iter()
            .filter_map(|r| r.agent_name.as_deref())
            .collect();
        assert!(names.contains(&"Hermes"));
        assert!(names.contains(&"Cosh"));
        assert!(names.contains(&"OpenClaw"));
    }

    #[test]
    fn test_default_agents_json_valid() {
        let (cmdline_rules, https_rules, http_targets) =
            parse_json_rules(DEFAULT_AGENTS_JSON).unwrap();
        assert!(!cmdline_rules.is_empty());
        // https rules configured by default: the DashScope compatible-mode host,
        // the Bailian native-protocol wildcard (`{WorkspaceId}.{region}.maas.
        // aliyuncs.com`), and api.openai.com.
        let patterns: Vec<&str> = https_rules.iter().map(|r| r.pattern.as_str()).collect();
        assert_eq!(
            patterns,
            vec![
                "dashscope.aliyuncs.com",
                "*.maas.aliyuncs.com",
                "api.openai.com"
            ]
        );
        assert!(http_targets.is_empty());
    }

    #[test]
    fn test_parse_json_rules_cmdline() {
        let json = r#"{
            "cmdline": {
                "allow": [{"rule": ["node", "*claude*"], "agent_name": "Claude Code"}],
                "deny": [{"rule": ["node", "*webpack*"]}]
            }
        }"#;
        let (cmdline_rules, https_rules, http_targets) = parse_json_rules(json).unwrap();
        assert_eq!(cmdline_rules.len(), 2);
        assert!(cmdline_rules[0].allow);
        assert_eq!(cmdline_rules[0].agent_name, Some("Claude Code".to_string()));
        assert!(!cmdline_rules[1].allow);
        assert!(cmdline_rules[1].agent_name.is_none());
        assert!(https_rules.is_empty());
        assert!(http_targets.is_empty());
    }

    #[test]
    fn test_parse_json_rules_https() {
        let json = r#"{"https": [{"rule": ["*.openai.com", "*.anthropic.com"]}]}"#;
        let (cmdline_rules, https_rules, http_targets) = parse_json_rules(json).unwrap();
        assert!(cmdline_rules.is_empty());
        assert_eq!(https_rules.len(), 2);
        assert_eq!(https_rules[0].pattern, "*.openai.com");
        assert_eq!(https_rules[1].pattern, "*.anthropic.com");
        assert!(http_targets.is_empty());
    }

    #[test]
    fn test_parse_json_rules_invalid() {
        let json = r#"{ invalid json }"#;
        assert!(parse_json_rules(json).is_err());
    }

    #[test]
    fn test_parse_json_rules_empty_rule_skipped() {
        let json = r#"{"cmdline":{"allow":[{"rule":[],"agent_name":"Skipped"},{"rule":["node"],"agent_name":"Kept"}]}}"#;
        let (cmdline_rules, _, _) = parse_json_rules(json).unwrap();
        assert_eq!(cmdline_rules.len(), 1);
        assert_eq!(cmdline_rules[0].agent_name, Some("Kept".to_string()));
    }

    #[test]
    fn test_parse_runtime_sls_path_present() {
        let json = r#"{"runtime": {"sls_logtail_path": "/var/log/sls/agentsight.log"}}"#;
        assert_eq!(
            parse_runtime_sls_path(json),
            Some(Some("/var/log/sls/agentsight.log".to_string()))
        );
    }

    #[test]
    fn test_parse_runtime_sls_path_empty() {
        let json = r#"{"runtime": {"sls_logtail_path": ""}}"#;
        // Empty string is now a deactivation signal (Some(None)), not absence (None).
        assert_eq!(parse_runtime_sls_path(json), Some(None));
    }

    #[test]
    fn test_parse_runtime_sls_path_whitespace_only() {
        let json = r#"{"runtime": {"sls_logtail_path": "   "}}"#;
        // Whitespace-only is treated as empty → deactivation signal.
        assert_eq!(parse_runtime_sls_path(json), Some(None));
    }

    #[test]
    fn test_parse_runtime_sls_path_missing_section() {
        let json = r#"{"cmdline": {"allow": []}}"#;
        // Field absent → no signal at all.
        assert_eq!(parse_runtime_sls_path(json), None);
    }

    #[test]
    fn test_parse_runtime_sls_path_invalid_json() {
        assert_eq!(parse_runtime_sls_path("not json"), None);
    }

    #[test]
    fn test_load_from_json_runtime_sls_path() {
        let json = r#"{"runtime": {"sls_logtail_path": "/tmp/sls.log"}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert_eq!(config.sls_logtail_path, Some("/tmp/sls.log".to_string()));
    }

    #[test]
    fn test_load_from_json_runtime_sls_path_empty_is_none() {
        let json = r#"{"runtime": {"sls_logtail_path": ""}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert_eq!(config.sls_logtail_path, None);
    }

    // ─── DeadLoop config tests ───────────────────────────────────────────────

    #[test]
    fn test_deadloop_config_defaults() {
        let config = AgentsightConfig::new();
        assert!(!config.deadloop_kill_enabled);
        assert_eq!(config.deadloop_kill_after_count, 3);
    }

    #[test]
    fn test_load_from_json_deadloop_enabled() {
        let json = r#"{"deadloop": {"enabled": true, "kill_after_count": 5}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(config.deadloop_kill_enabled);
        assert_eq!(config.deadloop_kill_after_count, 5);
    }

    #[test]
    fn test_load_from_json_deadloop_disabled_explicit() {
        let json = r#"{"deadloop": {"enabled": false}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(!config.deadloop_kill_enabled);
        assert_eq!(config.deadloop_kill_after_count, 3); // keeps default
    }

    #[test]
    fn test_load_from_json_deadloop_partial_only_count() {
        let json = r#"{"deadloop": {"kill_after_count": 10}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(!config.deadloop_kill_enabled); // keeps default
        assert_eq!(config.deadloop_kill_after_count, 10);
    }

    #[test]
    fn test_load_from_json_no_deadloop_section() {
        let json = r#"{"cmdline": {"allow": []}}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(!config.deadloop_kill_enabled);
        assert_eq!(config.deadloop_kill_after_count, 3);
    }

    #[test]
    fn test_channel_policy_from_str() {
        assert!(matches!(
            ChannelPolicy::from("backpressure"),
            ChannelPolicy::Backpressure
        ));
        assert!(matches!(
            ChannelPolicy::from("drop_newest"),
            ChannelPolicy::DropNewest
        ));
        assert!(matches!(
            ChannelPolicy::from("drop-newest"),
            ChannelPolicy::DropNewest
        ));
        assert!(matches!(
            ChannelPolicy::from("drop"),
            ChannelPolicy::DropNewest
        ));
        assert!(matches!(
            ChannelPolicy::from("sample:5"),
            ChannelPolicy::Sample(5)
        ));
        assert!(matches!(
            ChannelPolicy::from("sample=20"),
            ChannelPolicy::Sample(20)
        ));
        assert!(matches!(
            ChannelPolicy::from("sample"),
            ChannelPolicy::Sample(10)
        ));
        assert!(matches!(
            ChannelPolicy::from("BACKPRESSURE"),
            ChannelPolicy::Backpressure
        ));
        assert!(matches!(
            ChannelPolicy::from("unknown"),
            ChannelPolicy::Backpressure
        ));
    }

    #[test]
    fn test_runtime_limits_defaults() {
        let limits = RuntimeLimits::default();
        assert_eq!(
            limits.event_channel_capacity,
            DEFAULT_EVENT_CHANNEL_CAPACITY
        );
        assert_eq!(
            limits.event_channel_max_bytes,
            DEFAULT_EVENT_CHANNEL_MAX_BYTES
        );
        assert_eq!(
            limits.pending_genai_max_count,
            DEFAULT_PENDING_GENAI_MAX_COUNT
        );
        assert_eq!(
            limits.pending_genai_max_bytes,
            DEFAULT_PENDING_GENAI_MAX_BYTES
        );
        assert_eq!(limits.pid_cache_size, DEFAULT_PID_CACHE_SIZE);
        assert_eq!(
            limits.max_connection_body_bytes,
            DEFAULT_MAX_CONNECTION_BODY_BYTES
        );
        assert_eq!(
            limits.connection_idle_timeout_secs,
            DEFAULT_CONNECTION_IDLE_TIMEOUT_SECS
        );
        assert_eq!(limits.ring_buffer_mb, DEFAULT_RING_BUFFER_MB);
    }

    #[test]
    fn test_feature_flags_defaults() {
        let flags = FeatureFlags::default();
        assert!(flags.token_stats);
        assert!(!flags.tokenizer_enabled);
        assert_eq!(flags.tokenizer_cache_size, DEFAULT_TOKENIZER_CACHE_SIZE);
        assert!(!flags.session_mapping_enabled);
        assert!(!flags.sqlite_storage_enabled);
        assert!(!flags.resource_sampling_enabled);
        assert!(!flags.interruption_detection_enabled);
        assert!(!flags.audit_enabled);
        assert!(!flags.token_consumption_enabled);
        assert!(!flags.sls_logtail_enabled);
    }

    #[test]
    fn test_load_from_json_runtime_limits() {
        let json = r#"{
            "runtime_limits": {
                "event_channel_capacity": 5000,
                "event_channel_policy": "drop_newest",
                "event_channel_max_bytes_mb": 16,
                "pending_genai_max_count": 500,
                "pending_genai_max_bytes_mb": 32,
                "pid_cache_size": 256,
                "max_connection_body_mb": 4,
                "connection_idle_timeout_secs": 30,
                "ring_buffer_mb": 16
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert_eq!(config.runtime_limits.event_channel_capacity, 5000);
        assert!(matches!(
            config.runtime_limits.event_channel_policy,
            ChannelPolicy::DropNewest
        ));
        assert_eq!(config.runtime_limits.pending_genai_max_count, 500);
        assert_eq!(
            config.runtime_limits.event_channel_max_bytes,
            16 * 1024 * 1024
        );
        assert_eq!(
            config.runtime_limits.pending_genai_max_bytes,
            32 * 1024 * 1024
        );
        assert_eq!(config.runtime_limits.pid_cache_size, 256);
        assert_eq!(
            config.runtime_limits.max_connection_body_bytes,
            4 * 1024 * 1024
        );
        assert_eq!(config.runtime_limits.connection_idle_timeout_secs, 30);
        assert_eq!(config.runtime_limits.ring_buffer_mb, 16);
    }

    #[test]
    fn test_load_from_json_features() {
        let json = r#"{
            "features": {
                "token_stats": true,
                "tokenizer": { "enabled": true, "cache_size": 8 },
                "session_mapping": { "enabled": false, "max_entries": 5000 },
                "sqlite_storage": { "enabled": false },
                "resource_sampling": true,
                "interruption_detection": { "enabled": false },
                "audit": false,
                "token_consumption": true,
                "sls_logtail": true
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(config.features.tokenizer_enabled);
        assert_eq!(config.features.tokenizer_cache_size, 8);
        assert!(!config.features.session_mapping_enabled);
        assert_eq!(config.features.session_mapping_max_entries, 5000);
        assert!(!config.features.sqlite_storage_enabled);
        assert!(config.features.resource_sampling_enabled);
        assert!(!config.features.interruption_detection_enabled);
        assert!(!config.features.audit_enabled);
        assert!(config.features.token_consumption_enabled);
        assert!(config.features.sls_logtail_enabled);
        // trajectory_collection absent -> defaults (disabled, 30s)
        assert!(!config.features.trajectory_collection_enabled);
        assert_eq!(
            config.features.trajectory_scan_interval_secs,
            DEFAULT_TRAJECTORY_SCAN_INTERVAL_SECS
        );
        assert!(config.features.trajectory_scan_dirs.is_none());
    }

    #[test]
    fn test_load_from_json_trajectory_collection() {
        let json = r#"{
            "features": {
                "trajectory_collection": {
                    "enabled": true,
                    "scan_interval_secs": 15,
                    "scan_dirs": ["/tmp/projects"]
                }
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(config.features.trajectory_collection_enabled);
        assert_eq!(config.features.trajectory_scan_interval_secs, 15);
        assert_eq!(
            config.features.trajectory_scan_dirs,
            Some(vec!["/tmp/projects".to_string()])
        );
    }

    #[test]
    fn test_load_from_json_trajectory_collection_disabled() {
        let json = r#"{
            "features": {
                "trajectory_collection": { "enabled": false }
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(!config.features.trajectory_collection_enabled);
        assert_eq!(
            config.features.trajectory_scan_interval_secs,
            DEFAULT_TRAJECTORY_SCAN_INTERVAL_SECS
        );
    }

    #[test]
    fn load_from_json_parses_server_auth_enabled() {
        let json = r#"{
            "server": {
                "auth": {
                    "enabled": true
                }
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(config.server_auth.enabled);
    }

    #[test]
    fn load_from_json_server_auth_defaults_when_absent() {
        let json = r#"{}"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(config.server_auth.enabled); // default is true
    }

    #[test]
    fn load_from_json_server_auth_disabled() {
        let json = r#"{
            "server": {
                "auth": {
                    "enabled": false
                }
            }
        }"#;
        let mut config = AgentsightConfig::new();
        config.load_from_json(json).unwrap();
        assert!(!config.server_auth.enabled);
    }

    // ── schema_version / config upgrade ─────────────────────────────────

    /// Helper: create a unique temp dir under std env temp.
    fn unique_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agentsight-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extract_schema_version_returns_value_when_present() {
        let json = r#"{"schema_version": 2, "features": {}}"#;
        assert_eq!(extract_schema_version(json), Some(2));
    }

    #[test]
    fn extract_schema_version_returns_none_when_absent() {
        let json = r#"{"features": {}}"#;
        assert_eq!(extract_schema_version(json), None);
    }

    #[test]
    fn extract_schema_version_returns_none_on_invalid_json() {
        assert_eq!(extract_schema_version("not json"), None);
    }

    #[test]
    fn ensure_default_agents_config_creates_file_when_missing() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        assert!(!path.exists());
        ensure_default_agents_config(&path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            extract_schema_version(&content),
            Some(CURRENT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn ensure_default_agents_config_upgrades_stale_config() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        std::fs::write(&path, r#"{"cmdline": {"allow": []}}"#).unwrap();
        ensure_default_agents_config(&path).unwrap();
        // The old file should be backed up to a timestamped .bak.* file.
        let parent = path.parent().unwrap();
        let backups: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("agentsight.json.bak.")
            })
            .collect();
        assert_eq!(backups.len(), 1, "expected exactly one backup file");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            r#"{"cmdline": {"allow": []}}"#
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            extract_schema_version(&content),
            Some(CURRENT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn ensure_default_agents_config_preserves_current_config() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        let custom = format!(
            r#"{{"schema_version": {}, "features": {{"token_stats": true}}}}"#,
            CURRENT_SCHEMA_VERSION
        );
        std::fs::write(&path, &custom).unwrap();
        ensure_default_agents_config(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
        // No backup should exist (config was not upgraded).
        let parent = path.parent().unwrap();
        let backups: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("agentsight.json.bak.")
            })
            .collect();
        assert!(backups.is_empty(), "no backup expected for current schema");
    }

    #[test]
    fn ensure_default_agents_config_upgrades_old_schema_version() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        std::fs::write(&path, r#"{"schema_version": 1}"#).unwrap();
        ensure_default_agents_config(&path).unwrap();
        let parent = path.parent().unwrap();
        let backups: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("agentsight.json.bak.")
            })
            .collect();
        assert_eq!(backups.len(), 1, "expected exactly one backup file");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            r#"{"schema_version": 1}"#
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            extract_schema_version(&content),
            Some(CURRENT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn ensure_default_agents_config_leaves_invalid_json_untouched() {
        // Regression for issue #1502: an unparseable config file must NOT be
        // silently backed up + overwritten with the default. The caller's
        // load_from_file → parse_json_rules surfaces "JSON parse error: ..."
        // and AgentSight::new emits its "using embedded defaults" warn log.
        // Auto-upgrade is only meaningful for valid JSON with an outdated
        // schema_version.
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        let invalid = "NOT_VALID_JSON_AT_ALL{{{";
        std::fs::write(&path, invalid).unwrap();
        ensure_default_agents_config(&path).unwrap();
        // File content must be unchanged — no silent overwrite.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
        // No backup file should be created either.
        let parent = path.parent().unwrap();
        let backups: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("agentsight.json.bak.")
            })
            .collect();
        assert!(
            backups.is_empty(),
            "invalid JSON must not trigger backup+overwrite"
        );
    }

    #[test]
    fn ensure_default_agents_config_leaves_truncated_json_untouched() {
        // Same regression, with a truncated-JSON variant (parses as partial
        // JSON, fails serde_json::from_str). Should also be left untouched.
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        let truncated = r#"{"cmdline": {"allow": [{"rule": ["*cosh*""#;
        std::fs::write(&path, truncated).unwrap();
        ensure_default_agents_config(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), truncated);
        let parent = path.parent().unwrap();
        let backups: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("agentsight.json.bak.")
            })
            .collect();
        assert!(backups.is_empty());
    }

    #[test]
    fn ensure_default_agents_config_replaces_outdated_config() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        let user_config = r#"{
            "schema_version": 2,
            "cmdline": {
                "allow": [
                    {"rule": ["*mycustomagent*"], "agent_name": "MyCustomAgent"}
                ]
            }
        }"#;
        std::fs::write(&path, user_config).unwrap();
        ensure_default_agents_config(&path).unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, DEFAULT_AGENTS_JSON);
        assert!(!result.contains("MyCustomAgent"));
    }

    #[test]
    fn ensure_default_agents_config_replacement_contains_storage_section() {
        let dir = unique_temp_dir();
        let path = dir.join("agentsight.json");
        std::fs::write(&path, r#"{"schema_version": 2}"#).unwrap();
        ensure_default_agents_config(&path).unwrap();
        let result: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(result.get("storage").is_some());
        assert_eq!(
            result["schema_version"].as_u64(),
            Some(CURRENT_SCHEMA_VERSION as u64)
        );
    }
}
