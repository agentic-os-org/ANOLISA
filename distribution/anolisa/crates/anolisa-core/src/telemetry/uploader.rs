//! Self-hosted telemetry uploader.
//!
//! Tails the ops `.jsonl` files, batches new lines, and ships them to
//! Alibaba Cloud SLS via the anonymous **PutWebtracking** endpoint (zero
//! auth, `POST /logstores/{logstore}/track`, matching `sls-sts-demo`).
//!
//! Design points:
//! - **Opt-out gated**: every round re-stats the opt-out marker; when it
//!   appears the loop self-exits so `telemetry disable` takes effect without a
//!   restart. Absent marker means collection is on (the default).
//! - **Offset tracking** (`offsets.json`, `component -> {inode, offset}`)
//!   with rotation resilience: on inode change we drain the rotated
//!   `.jsonl.1` residue before switching to the fresh file; a shrunk file
//!   (size < offset) is treated as truncation and reset.
//! - **Offsets advance only after a successful POST** so a failed round is
//!   retried next tick with no data loss.
//! - **Single instance** via a non-blocking `flock` on `uploader.lock`.
//! - **Routing**: a single SLS project; `link_id` (when present) is embedded
//!   per log for correlation but does not change the destination.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::register::RegistrationManager;
use crate::telemetry::channel::DISABLE_MARKER_PATH;
use crate::telemetry::instance::Identity;
use crate::telemetry::metadata::MetadataClient;
use crate::telemetry::{RegionInfo, RegionProbe};

/// HTTP connect timeout for PutWebtracking requests.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// HTTP read timeout for PutWebtracking requests.
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Default poll interval between upload rounds.
const DEFAULT_SLEEP_SECS: u64 = 120;
/// Maximum lines shipped in a single PutWebtracking request per component.
/// SLS caps the uncompressed body at 10 MB; this keeps memory and request
/// size bounded even when a log file grows quickly between rounds.
const MAX_LINES_PER_ROUND: usize = 1000;

// ── Endpoint ─────────────────────────────────────────────────────────

/// An SLS PutWebtracking destination.
///
/// Holds only the `project`: the `region` is probed at runtime (see
/// [`RegionProbe`]) and the `logstore` is the component name (e.g.
/// `agent-sec-core.jsonl` → logstore `agent-sec-core`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub project: String,
}

impl Endpoint {
    /// Build the PutWebtracking URL.
    ///
    /// `use_internal` selects the Aliyun internal vs public host (per the
    /// region probe); `logstore` is the component name.
    ///
    /// `self.project` is a project prefix (e.g. `anolisa`); the actual
    /// SLS project name is `{prefix}-{region}` (e.g. `anolisa-cn-hangzhou`).
    pub fn track_url(&self, region: &str, use_internal: bool, logstore: &str) -> String {
        let project = format!("{}-{region}", self.project);
        let host = if use_internal {
            format!("{project}.{region}-internal.log.aliyuncs.com")
        } else {
            format!("{project}.{region}.log.aliyuncs.com")
        };
        format!("https://{host}/logstores/{logstore}/track")
    }
}

// ── Offsets ──────────────────────────────────────────────────────────

/// Per-component tail position (inode + byte offset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffset {
    pub inode: u64,
    pub offset: u64,
}

type Offsets = BTreeMap<String, FileOffset>;

// ── Configuration ────────────────────────────────────────────────────

/// Uploader configuration. Production paths come from [`Default`]; unit
/// tests build one directly with temp paths.
#[derive(Debug, Clone)]
pub struct UploaderConfig {
    /// Directory holding component `.jsonl` files.
    pub ops_dir: PathBuf,
    /// Persistent tail offsets.
    pub offsets_path: PathBuf,
    /// Single-instance lock file.
    pub lock_path: PathBuf,
    /// register.json (read for `link_id` routing).
    pub register_path: PathBuf,
    /// `/etc/anolisa-release` (read for product type dimension).
    pub release_path: PathBuf,
    /// Opt-out marker; present → uploader stays idle / exits.
    pub disable_marker_path: PathBuf,
    /// Persisted personal identity (`instance_id` / `uid`); read each round
    /// only when a `link_id` is present, then merged into every log line.
    pub identity_cache_path: PathBuf,
    /// Instance metadata URL used to probe the region-id (and internal vs
    /// public network reachability).
    pub metadata_url: String,
    /// SLS project destination (single project for all uploads).
    pub endpoint: Endpoint,
    /// Poll interval between rounds.
    pub sleep_secs: u64,
    /// `__topic__` for every batch.
    pub topic: String,
    /// `__source__` for every batch.
    pub source: String,
    /// Persistent telemetry id file (UUID generated once, reused across reboots).
    pub telemetry_id_path: PathBuf,
}

impl Default for UploaderConfig {
    fn default() -> Self {
        Self {
            ops_dir: PathBuf::from("/var/log/anolisa/sls/ops"),
            offsets_path: PathBuf::from("/var/lib/anolisa/telemetry/offsets.json"),
            lock_path: PathBuf::from("/var/lib/anolisa/telemetry/uploader.lock"),
            register_path: PathBuf::from("/etc/anolisa/register.json"),
            release_path: PathBuf::from("/etc/anolisa-release"),
            disable_marker_path: PathBuf::from(DISABLE_MARKER_PATH),
            identity_cache_path: PathBuf::from("/var/lib/anolisa/telemetry/identity.json"),
            metadata_url: "http://100.100.100.200/latest/meta-data/region-id".to_string(),
            endpoint: Endpoint {
                project: env_or("SLS_PROJECT_PREFIX", "anolisa"),
            },
            sleep_secs: DEFAULT_SLEEP_SECS,
            topic: "anolisa-telemetry".to_string(),
            source: "anolisa".to_string(),
            telemetry_id_path: PathBuf::from("/var/lib/anolisa/telemetry/telemetry-id"),
        }
    }
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

// ── Errors ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum UploaderError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("PutWebtracking returned HTTP {code} for {url}")]
    Http { code: u16, url: String },
    #[error("network error: {reason}")]
    Network { reason: String },
    #[error("uploader lock error: {0}")]
    Lock(String),
    #[error("platform unsupported: {0}")]
    Unsupported(String),
    #[error("failed to serialize request body: {0}")]
    Serialize(#[from] serde_json::Error),
}

// ── Uploader ─────────────────────────────────────────────────────────

/// Self-hosted uploader over the ops `.jsonl` channel.
pub struct Uploader {
    config: UploaderConfig,
    client: MetadataClient,
    /// Reused across all POSTs so HTTP keep-alive connections are pooled
    /// instead of being re-established on every component upload.
    agent: ureq::Agent,
}

impl Default for Uploader {
    fn default() -> Self {
        Self::new(UploaderConfig::default())
    }
}

impl Uploader {
    pub fn new(config: UploaderConfig) -> Self {
        let client = MetadataClient::from_key_url(&config.metadata_url);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(HTTP_CONNECT_TIMEOUT)
            .timeout_read(HTTP_READ_TIMEOUT)
            .build();
        Self {
            config,
            client,
            agent,
        }
    }

    /// Whether collection is currently enabled (single opt-out-marker stat).
    ///
    /// Enabled by default; only an explicit opt-out marker disables it.
    fn collection_enabled(&self) -> bool {
        !self.config.disable_marker_path.exists()
    }

    /// Resolve `(region, use_internal)` for this round.
    ///
    /// Delegates to [`RegionProbe`], which falls back to `cn-hangzhou` on the
    /// public host when the ECS metadata region-id is unreachable. Reuses the
    /// uploader's [`MetadataClient`] so the metadata-unreachable short-circuit
    /// flag is shared with `product_type()`.
    fn resolve_region(&self) -> (String, bool) {
        let info = RegionProbe::with_client(self.client.clone())
            .probe()
            .unwrap_or_else(|_| RegionInfo {
                region_id: "cn-hangzhou".to_string(),
                use_internal: false,
            });
        (info.region_id, info.use_internal)
    }

    /// Resolve the raw product type string for the common dimensions.
    fn product_type(&self) -> String {
        crate::telemetry::common::probe_product_type(&self.config.release_path, &self.client)
    }

    /// Read (or lazily create + persist) the telemetry id.
    ///
    /// Uses an atomic temp-file + rename so a crash never leaves a partial
    /// file that would cause a different telemetry id on next boot.
    /// Returns an error if persistence fails, so the caller can retry instead
    /// of silently using a volatile id.
    fn telemetry_id(&self) -> Result<String, UploaderError> {
        let path = &self.config.telemetry_id_path;
        if let Ok(existing) = fs::read_to_string(path) {
            let trimmed = existing.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

        let id = uuid::Uuid::new_v4().to_string();
        self.persist_telemetry_id(path, &id)?;
        Ok(id)
    }

    fn persist_telemetry_id(&self, path: &Path, id: &str) -> Result<(), UploaderError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            f.write_all(id.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Build the common dimensions injected into every log line.
    ///
    /// `telemetry_id` (a persistent UUID) is always present so every logstore's
    /// lines carry it for scale counting and deduplication. `identity` is
    /// `Some` only when named reporting is authorized (a `link_id` is present);
    /// when so, its `instance_id` / `uid` fields are added so every logstore's
    /// lines carry the identity, not just `instance.jsonl`. Anonymous rounds
    /// pass `None` and stay identity-free.
    fn common_dimensions(
        &self,
        region: &str,
        identity: Option<&Identity>,
        product_type: &str,
        telemetry_id: &str,
    ) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        map.insert(
            "version".to_string(),
            Value::String(env!("CARGO_PKG_VERSION").to_string()),
        );
        map.insert(
            "product_type".to_string(),
            Value::String(product_type.to_string()),
        );
        map.insert("region".to_string(), Value::String(region.to_string()));
        // telemetry_id is always injected (L1-compatible, no authorization needed).
        map.insert(
            "telemetry_id".to_string(),
            Value::String(telemetry_id.to_string()),
        );
        if let Some(id) = identity {
            if let Some(instance_id) = &id.instance_id {
                map.insert(
                    "instance_id".to_string(),
                    Value::String(instance_id.clone()),
                );
            }
            if let Some(uid) = &id.uid {
                map.insert("uid".to_string(), Value::String(uid.clone()));
            }
        }
        map
    }

    // ── Offsets persistence ──────────────────────────────────────────

    fn load_offsets(&self) -> Offsets {
        fs::read_to_string(&self.config.offsets_path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    fn save_offsets(&self, offsets: &Offsets) -> io::Result<()> {
        if let Some(parent) = self.config.offsets_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content =
            serde_json::to_string_pretty(offsets).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = self
            .config
            .offsets_path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.config.offsets_path)?;
        Ok(())
    }

    // ── Component tailing ────────────────────────────────────────────

    /// Discover component names from `*.jsonl` files (excludes rotated
    /// `.jsonl.1`). Returns a sorted, de-duplicated list.
    fn discover_components(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.config.ops_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    fn jsonl_path(&self, component: &str) -> PathBuf {
        self.config.ops_dir.join(format!("{component}.jsonl"))
    }

    fn rotated_path(&self, component: &str) -> PathBuf {
        self.config.ops_dir.join(format!("{component}.jsonl.1"))
    }

    /// Collect new complete lines for a component, honoring rotation.
    ///
    /// Returns `Ok(None)` when the file is missing or has no new complete
    /// lines. On success returns the new lines plus the [`FileOffset`] to
    /// persist **after** a successful upload.
    fn collect_component(
        &self,
        component: &str,
        stored: Option<&FileOffset>,
    ) -> io::Result<Option<(Vec<String>, FileOffset)>> {
        let path = self.jsonl_path(component);
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let cur_inode = inode_of(&meta);
        let cur_len = meta.len();

        let mut lines: Vec<String> = Vec::new();

        // Determine where to start reading the current file, and whether we
        // first need to drain a rotated residue.
        let start_offset = match stored {
            Some(o) if o.inode == cur_inode => {
                if cur_len < o.offset {
                    // Truncated in place → restart from the beginning.
                    0
                } else {
                    o.offset
                }
            }
            Some(o) => {
                // Inode changed → rotation. Drain residue of the rotated file
                // starting at the last consumed offset. The residue is capped
                // at MAX_LINES_PER_ROUND: if the rotated file accumulated a
                // lot of data (e.g. the uploader was down), this prevents a
                // single oversized POST that would exceed the SLS 10 MB body
                // limit and retry forever. When the cap is hit the offset stays
                // on the rotated file so the remainder drains next round.
                let rotated = self.rotated_path(component);
                if rotated.exists()
                    && let Ok((mut residue, res_consumed)) =
                        read_from(&rotated, o.offset, MAX_LINES_PER_ROUND)
                {
                    lines.append(&mut residue);
                    if lines.len() >= MAX_LINES_PER_ROUND {
                        // Cap hit: keep the offset on the rotated file so
                        // the remainder is drained next round instead of
                        // being skipped.
                        return Ok(Some((
                            lines,
                            FileOffset {
                                inode: o.inode,
                                offset: o.offset + res_consumed,
                            },
                        )));
                    }
                }
                0
            }
            None => 0,
        };

        // Fresh file: cap the remaining budget so residue + fresh together
        // never exceed MAX_LINES_PER_ROUND in a single POST.
        let remaining_cap = MAX_LINES_PER_ROUND.saturating_sub(lines.len());
        let (mut fresh, consumed) = read_from(&path, start_offset, remaining_cap)?;
        lines.append(&mut fresh);

        if lines.is_empty() {
            return Ok(None);
        }

        let new_offset = FileOffset {
            inode: cur_inode,
            offset: start_offset + consumed,
        };
        Ok(Some((lines, new_offset)))
    }

    // ── HTTP ─────────────────────────────────────────────────────────

    fn post(&self, url: &str, body: &str) -> Result<(), UploaderError> {
        match self
            .agent
            .post(url)
            .set("Content-Type", "application/json")
            .send_string(body)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(UploaderError::Http {
                code,
                url: url.to_string(),
            }),
            Err(ureq::Error::Transport(t)) => Err(UploaderError::Network {
                reason: t.to_string(),
            }),
        }
    }

    // ── One round ────────────────────────────────────────────────────

    /// Execute one upload round. No-op (returns `Ok`) when the opt-out marker
    /// is present. Offsets advance only for components whose POST succeeded.
    pub fn run_once(&self) -> Result<(), UploaderError> {
        if !self.collection_enabled() {
            return Ok(());
        }

        let link_id = RegistrationManager::with_paths(
            self.config.register_path.clone(),
            self.config.release_path.clone(),
        )
        .read_link_id();
        let endpoint = self.config.endpoint.clone();

        // Personal identity rides on every line only while linked; unlinked
        // rounds never read it, so the anonymous stream stays identity-free.
        let identity = link_id
            .as_deref()
            .and_then(|_| Identity::read(&self.config.identity_cache_path));

        // Probe the region once per round: detected → internal host; not
        // detected → cn-hangzhou + public host (see `RegionProbe`).
        let (region, use_internal) = self.resolve_region();
        let product_type = self.product_type();
        let telemetry_id = self.telemetry_id()?;
        let common =
            self.common_dimensions(&region, identity.as_ref(), &product_type, &telemetry_id);

        let mut offsets = self.load_offsets();
        let mut dirty = false;
        let mut last_err: Option<UploaderError> = None;

        for component in self.discover_components() {
            // Abort between components when SIGTERM arrives so the loop exits
            // promptly instead of blocking on every component's HTTP round.
            #[cfg(unix)]
            if TERM.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let stored = offsets.get(&component).cloned();
            let collected = match self.collect_component(&component, stored.as_ref()) {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(e) => {
                    last_err = Some(UploaderError::Io(e));
                    continue;
                }
            };
            let (lines, new_offset) = collected;
            // logstore is the component name (e.g. agent-sec-core.jsonl →
            // logstore agent-sec-core).
            let url = endpoint.track_url(&region, use_internal, &component);
            let body = build_body(
                &lines,
                link_id.as_deref(),
                &common,
                &self.config.topic,
                &self.config.source,
            )?;
            match self.post(&url, &body) {
                Ok(()) => {
                    offsets.insert(component, new_offset);
                    dirty = true;
                }
                Err(UploaderError::Http { code: 404, .. }) => {
                    // Logstore does not exist (or WebTracking not enabled);
                    // advance the offset so we don't retry this component
                    // forever, and continue with the remaining components.
                    eprintln!("[anolisa] telemetry: logstore `{component}` not found, skipping");
                    offsets.insert(component, new_offset);
                    dirty = true;
                }
                Err(UploaderError::Http { code, .. }) if (400..500).contains(&code) => {
                    // Client error: the request itself is invalid (e.g., malformed
                    // body or unsupported content). Retrying the same payload will
                    // never succeed, so advance the offset to avoid blocking the
                    // pipeline indefinitely. The error is still logged above for
                    // visibility.
                    eprintln!(
                        "[anolisa] telemetry: logstore `{component}` rejected request with HTTP {code}, skipping"
                    );
                    offsets.insert(component, new_offset);
                    dirty = true;
                }
                Err(e) => {
                    // Do not advance offset; retry next round.
                    last_err = Some(e);
                }
            }
        }

        if dirty {
            self.save_offsets(&offsets)?;
        }

        match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ── Body assembly (free fn for unit testing) ─────────────────────────

/// Assemble the PutWebtracking request body.
///
/// Each source line is parsed as a JSON object (falling back to
/// `{"raw": <line>}`), then enriched with `__time__` (Unix seconds), the
/// common dimensions, and — when linked — the `link_id`. Common dimensions
/// never overwrite fields the component already set. The originating
/// component is conveyed by the destination logstore (and any `component`
/// field the source line already carries), so it is not injected here.
///
/// PutWebtracking requires every field value inside `__logs__` to be a
/// string; non-string values produce `PostBodyInvalid: Value in log is not
/// string data type`. Values are therefore normalized to strings here.
pub fn build_body(
    lines: &[String],
    link_id: Option<&str>,
    common: &BTreeMap<String, Value>,
    topic: &str,
    source: &str,
) -> Result<String, serde_json::Error> {
    let now = unix_now();
    let logs: Vec<Value> = lines
        .iter()
        .map(|line| {
            let mut obj: Map<String, Value> = match serde_json::from_str::<Value>(line) {
                Ok(Value::Object(m)) => m,
                _ => {
                    let mut m = Map::new();
                    m.insert("raw".to_string(), Value::String(line.clone()));
                    m
                }
            };
            obj.insert("__time__".to_string(), Value::from(now));
            for (k, v) in common {
                obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if let Some(id) = link_id {
                obj.insert("link_id".to_string(), Value::String(id.to_string()));
            }
            Value::Object(stringify_log_values(obj))
        })
        .collect();

    let body = serde_json::json!({
        "__logs__": logs,
        "__topic__": topic,
        "__source__": source,
    });
    serde_json::to_string(&body)
}

/// Normalize a log object's field values to strings.
///
/// SLS PutWebtracking only accepts string values inside `__logs__`; numbers,
/// booleans, and nested structures must be serialized as strings. `null`
/// values are dropped because they have no string representation that SLS
/// accepts.
fn stringify_log_values(obj: Map<String, Value>) -> Map<String, Value> {
    obj.into_iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k, Value::String(s))),
            Value::Null => None,
            other => Some((k, Value::String(other.to_string()))),
        })
        .collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read complete (newline-terminated) lines from `path` starting at byte
/// `offset`. Returns the trimmed non-empty lines plus the number of bytes
/// consumed (up to and including the last newline). A trailing partial line
/// is left for the next round.
///
/// `max_lines` bounds the number of returned lines (`0` means unlimited).
/// Use this to cap a single PutWebtracking request when a file grows fast.
///
/// Reads line-by-line via a `BufReader` so a large file does not have to be
/// fully loaded into memory before `max_lines` takes effect.
fn read_from(path: &Path, offset: u64, max_lines: usize) -> io::Result<(Vec<String>, u64)> {
    use std::io::{BufRead, BufReader};
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(f);

    let mut lines = Vec::new();
    let mut consumed: u64 = 0;
    let mut raw = String::new();
    loop {
        raw.clear();
        let n = reader.read_line(&mut raw)?;
        if n == 0 {
            break; // EOF
        }
        // Only complete lines (terminated by '\n') are consumed; a trailing
        // partial line is left for the next round.
        if !raw.ends_with('\n') {
            break;
        }
        consumed += n as u64;
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        if !trimmed.is_empty() {
            lines.push(trimmed.to_string());
        }
        if max_lines > 0 && lines.len() >= max_lines {
            break;
        }
    }
    Ok((lines, consumed))
}

#[cfg(unix)]
fn inode_of(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode_of(_meta: &fs::Metadata) -> u64 {
    0
}

// ── Signal handling (unix) ───────────────────────────────────────────

/// Set by the SIGTERM handler so [`Uploader::run_once`] can abort mid-round
/// instead of blocking until every component has been uploaded.
#[cfg(unix)]
static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// SIGTERM handler: flips [`TERM`] so the upload loop exits promptly.
#[cfg(unix)]
extern "C" fn handle_term(_: i32) {
    TERM.store(true, std::sync::atomic::Ordering::SeqCst);
}

// ── Loop + single-instance + spawn (unix) ────────────────────────────

#[cfg(unix)]
mod unix_runtime {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use nix::fcntl::{Flock, FlockArg};

    // Bring the signal flag + handler (defined in the parent module) into
    // scope so the rest of this module can use them unqualified.
    use super::{TERM, handle_term};

    /// Time to hold the lock after spawning the child so the child enters its
    /// own wait loop before we release it. This closes the race where another
    /// `ensure_running` call observes the lock as free between our drop and the
    /// child's acquisition.
    const LOCK_HANDOVER_MILLIS: Duration = Duration::from_millis(100);
    /// Maximum time a freshly spawned loop waits for the parent to release the
    /// lock before giving up.
    const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
    /// Polling interval while waiting for the single-instance lock.
    const LOCK_WAIT_INTERVAL: Duration = Duration::from_millis(20);

    impl Uploader {
        /// Run the upload loop until the opt-out marker appears or SIGTERM is
        /// received. Waits briefly for the single-instance lock so a parent
        /// `ensure_running` can hand it over; if another instance owns it,
        /// returns `Ok` immediately (self-heal, no duplicate loop).
        pub fn run_loop(&self) -> Result<(), UploaderError> {
            let _lock = match self.wait_for_lock(LOCK_WAIT_TIMEOUT)? {
                Some(lock) => lock,
                None => return Ok(()), // already running elsewhere
            };

            // Defensive: ensure the ops dir exists so components can write even
            // if a prior authoritative `enable` did not pre-create it. This is
            // non-authoritative; the opt-out marker and 0666 jsonl files remain
            // `enable`/`ensure_ops_channel`'s responsibility.
            let _ = fs::create_dir_all(&self.config.ops_dir);

            // SIGTERM → flush once then exit gracefully.
            unsafe {
                let _ = nix::sys::signal::signal(
                    nix::sys::signal::Signal::SIGTERM,
                    nix::sys::signal::SigHandler::Handler(handle_term),
                );
            }

            loop {
                // Flush on start and every round; a failed round is logged,
                // not fatal.
                if let Err(e) = self.run_once() {
                    eprintln!("[anolisa] telemetry upload round failed: {e}");
                }

                if self.config.disable_marker_path.exists() {
                    break; // disable took effect
                }
                if TERM.load(Ordering::SeqCst) {
                    // Final flush already happened above; exit.
                    break;
                }

                if !self.sleep_interruptible() {
                    break;
                }
            }
            Ok(())
        }

        /// Ensure a loop is running: probe the lock non-blocking. If idle we
        /// spawn a detached loop and hold the lock for a short handover window
        /// so the child enters its own wait loop before we release it; if held,
        /// one is already running.
        pub fn ensure_running(&self) -> Result<(), UploaderError> {
            match self.try_lock()? {
                Some(_lock) => {
                    self.spawn_detached()?;
                    // Hold the lock briefly so the spawned child has time to
                    // start waiting before we release it. This prevents another
                    // concurrent `ensure_running` from observing a free lock.
                    std::thread::sleep(LOCK_HANDOVER_MILLIS);
                    Ok(())
                }
                None => Ok(()), // already running
            }
        }

        /// Acquire the single-instance lock non-blocking. `Ok(Some)` when
        /// acquired, `Ok(None)` when already held.
        fn try_lock(&self) -> Result<Option<Flock<File>>, UploaderError> {
            if let Some(parent) = self.config.lock_path.parent() {
                fs::create_dir_all(parent)?;
            }
            // flock does not depend on file contents; do not truncate so a
            // concurrent opener (e.g. `ensure_running` probing the lock) cannot
            // clobber the file while another instance holds it.
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.config.lock_path)?;
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(lock) => Ok(Some(lock)),
                Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
                Err((_, e)) => Err(UploaderError::Lock(e.to_string())),
            }
        }

        /// Acquire the single-instance lock, polling until `timeout` elapses.
        /// `Ok(Some)` when acquired, `Ok(None)` if the lock was held for the
        /// entire timeout period.
        fn wait_for_lock(&self, timeout: Duration) -> Result<Option<Flock<File>>, UploaderError> {
            let deadline = Instant::now() + timeout;
            loop {
                match self.try_lock()? {
                    Some(lock) => return Ok(Some(lock)),
                    None if Instant::now() < deadline => {
                        std::thread::sleep(LOCK_WAIT_INTERVAL);
                    }
                    None => return Ok(None),
                }
            }
        }

        fn spawn_detached(&self) -> Result<(), UploaderError> {
            let exe = std::env::current_exe()?;
            let mut cmd = Command::new(exe);
            cmd.args(["telemetry", "upload", "--loop"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            unsafe {
                cmd.pre_exec(|| {
                    nix::unistd::setsid()
                        .map(|_| ())
                        .map_err(|e| io::Error::from_raw_os_error(e as i32))
                });
            }
            cmd.spawn()?;
            Ok(())
        }

        /// Sleep for `sleep_secs`, waking early on SIGTERM. Returns `false`
        /// when a term was observed mid-sleep.
        fn sleep_interruptible(&self) -> bool {
            let mut remaining = self.config.sleep_secs;
            while remaining > 0 {
                if TERM.load(Ordering::SeqCst) {
                    return false;
                }
                std::thread::sleep(Duration::from_secs(1));
                remaining -= 1;
            }
            !TERM.load(Ordering::SeqCst)
        }
    }
}

#[cfg(not(unix))]
impl Uploader {
    pub fn run_loop(&self) -> Result<(), UploaderError> {
        Err(UploaderError::Unsupported(
            "telemetry uploader loop requires a unix platform".to_string(),
        ))
    }

    pub fn ensure_running(&self) -> Result<(), UploaderError> {
        Err(UploaderError::Unsupported(
            "telemetry uploader spawn requires a unix platform".to_string(),
        ))
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_endpoint(name: &str) -> Endpoint {
        Endpoint {
            project: format!("{name}-proj"),
        }
    }

    fn test_uploader(dir: &TempDir) -> Uploader {
        let ops = dir.path().join("ops");
        fs::create_dir_all(&ops).unwrap();
        Uploader::new(UploaderConfig {
            ops_dir: ops,
            offsets_path: dir.path().join("offsets.json"),
            lock_path: dir.path().join("uploader.lock"),
            register_path: dir.path().join("register.json"),
            release_path: dir.path().join("anolisa-release"),
            disable_marker_path: dir.path().join(".telemetry_disabled"),
            identity_cache_path: dir.path().join("identity.json"),
            metadata_url: "http://127.0.0.1:19999/no-such-endpoint".to_string(),
            endpoint: test_endpoint("anon"),
            sleep_secs: 1,
            topic: "topic".to_string(),
            source: "source".to_string(),
            telemetry_id_path: dir.path().join("telemetry-id"),
        })
    }

    fn write_lines(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_track_url_public_and_internal() {
        let ep = Endpoint {
            project: "proj".into(),
        };
        // Prefix + region → full project name in the host.
        assert_eq!(
            ep.track_url("cn-hangzhou", false, "agent-sec-core"),
            "https://proj-cn-hangzhou.cn-hangzhou.log.aliyuncs.com/logstores/agent-sec-core/track"
        );
        // Detected → internal host.
        assert_eq!(
            ep.track_url("cn-beijing", true, "cosh"),
            "https://proj-cn-beijing.cn-beijing-internal.log.aliyuncs.com/logstores/cosh/track"
        );
    }

    #[test]
    fn test_read_from_leaves_partial() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.jsonl");
        fs::write(&path, "a\nb\npartial").unwrap();
        let (lines, consumed) = read_from(&path, 0, 0).unwrap();
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(consumed, 4); // "a\nb\n"
    }

    #[test]
    fn test_read_from_skips_blank() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.jsonl");
        fs::write(&path, "a\n\nb\n").unwrap();
        let (lines, consumed) = read_from(&path, 0, 0).unwrap();
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_read_from_respects_max_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.jsonl");
        fs::write(&path, "a\nb\nc\nd\n").unwrap();
        let (lines, consumed) = read_from(&path, 0, 2).unwrap();
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(consumed, 4); // "a\nb\n"
    }

    #[test]
    fn test_build_body_shape_and_time() {
        let common = {
            let mut m = BTreeMap::new();
            m.insert("version".to_string(), Value::String("1.2.3".into()));
            m
        };
        let lines = vec![r#"{"k":"v"}"#.to_string(), "not-json".to_string()];
        let body = build_body(&lines, Some("lk"), &common, "topic", "src").unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["__topic__"], "topic");
        assert_eq!(parsed["__source__"], "src");
        let logs = parsed["__logs__"].as_array().unwrap();
        assert_eq!(logs.len(), 2);

        // First line: object preserved + enriched. No injected `component`.
        assert_eq!(logs[0]["k"], "v");
        assert!(logs[0].get("component").is_none());
        assert_eq!(logs[0]["version"], "1.2.3");
        assert_eq!(logs[0]["link_id"], "lk");
        // PutWebtracking only accepts string values, so `__time__` is stringified.
        assert!(logs[0]["__time__"].is_string());

        // Second line: non-JSON wrapped as raw.
        assert_eq!(logs[1]["raw"], "not-json");
    }

    #[test]
    fn test_build_body_stringifies_non_string_values() {
        let common = BTreeMap::new();
        let lines = vec![r#"{"n":42,"b":true,"null":null,"s":"keep"}"#.to_string()];
        let body = build_body(&lines, None, &common, "t", "s").unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        let log = &parsed["__logs__"][0];

        assert_eq!(log["n"], "42");
        assert_eq!(log["b"], "true");
        assert!(log.get("null").is_none());
        assert_eq!(log["s"], "keep");
    }

    #[test]
    fn test_build_body_common_does_not_override() {
        let mut common = BTreeMap::new();
        common.insert("region".to_string(), Value::String("cn-common".into()));
        let lines = vec![r#"{"region":"cn-line"}"#.to_string()];
        let body = build_body(&lines, None, &common, "t", "s").unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        // Component-provided field wins.
        assert_eq!(parsed["__logs__"][0]["region"], "cn-line");
        // No link_id when unlinked.
        assert!(parsed["__logs__"][0].get("link_id").is_none());
    }

    #[test]
    fn test_common_dimensions_injects_identity_only_when_linked() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let telemetry_id = up.telemetry_id().unwrap();

        // Unlinked (identity = None): no personal fields, but telemetry_id is always present.
        let anon = up.common_dimensions("cn-hangzhou", None, "unknown", &telemetry_id);
        assert!(!anon.contains_key("instance_id"));
        assert!(!anon.contains_key("uid"));
        assert!(anon.contains_key("telemetry_id"));
        assert!(!anon["telemetry_id"].as_str().unwrap().is_empty());

        // Linked: instance_id + uid ride on the common dimensions.
        let identity = Identity {
            instance_id: Some("i-abc".into()),
            uid: Some("1644215368948677".into()),
        };
        let named = up.common_dimensions("cn-hangzhou", Some(&identity), "unknown", &telemetry_id);
        assert_eq!(named["instance_id"], Value::String("i-abc".into()));
        assert_eq!(named["uid"], Value::String("1644215368948677".into()));
    }

    #[test]
    fn test_telemetry_id_persists() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let id1 = up.telemetry_id().unwrap();
        let id2 = up.telemetry_id().unwrap();
        assert_eq!(id1, id2);
        let content = fs::read_to_string(&up.config.telemetry_id_path).unwrap();
        assert_eq!(content.trim(), id1);
    }

    #[test]
    fn test_run_once_skips_when_disabled_marker_present() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        write_lines(&up.jsonl_path("cosh"), "{\"a\":1}\n");
        // Opt-out marker present → no-op, no offsets file written.
        fs::write(&up.config.disable_marker_path, "").unwrap();
        up.run_once().unwrap();
        assert!(!up.config.offsets_path.exists());
    }

    #[test]
    fn test_collect_component_fresh_and_incremental() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let path = up.jsonl_path("cosh");
        write_lines(&path, "{\"a\":1}\n{\"b\":2}\n");

        let (lines, off) = up.collect_component("cosh", None).unwrap().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(off.offset, 16); // two 8-byte lines incl newline

        // No new data from the persisted offset.
        assert!(up.collect_component("cosh", Some(&off)).unwrap().is_none());

        // Append one line → only the new line is collected.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"c\":3}\n").unwrap();
        let (lines2, off2) = up.collect_component("cosh", Some(&off)).unwrap().unwrap();
        assert_eq!(lines2, vec!["{\"c\":3}".to_string()]);
        assert_eq!(off2.offset, 24);
    }

    #[test]
    fn test_collect_component_rotation_drains_residue() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let path = up.jsonl_path("cosh");
        write_lines(&path, "{\"a\":1}\n{\"b\":2}\n");

        // Consume the first line only.
        let stored = FileOffset {
            inode: inode_of(&fs::metadata(&path).unwrap()),
            offset: 8,
        };

        // Simulate logrotate rename mode: move active file to .1, create a
        // fresh active file with a NEW inode + one new line.
        fs::rename(&path, up.rotated_path("cosh")).unwrap();
        write_lines(&path, "{\"c\":3}\n");

        let (lines, off) = up
            .collect_component("cosh", Some(&stored))
            .unwrap()
            .unwrap();
        // Residue of rotated file (second line) + new file line.
        assert_eq!(
            lines,
            vec!["{\"b\":2}".to_string(), "{\"c\":3}".to_string()]
        );
        // New offset tracks the fresh file's inode + consumed bytes.
        assert_eq!(off.inode, inode_of(&fs::metadata(&path).unwrap()));
        assert_eq!(off.offset, 8);
    }

    #[test]
    fn test_collect_component_truncation_resets() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let path = up.jsonl_path("cosh");
        write_lines(&path, "{\"a\":1}\n{\"b\":2}\n");
        let inode = inode_of(&fs::metadata(&path).unwrap());

        // Offset points past a now-shrunk file → treat as truncation.
        let stored = FileOffset { inode, offset: 999 };
        write_lines(&path, "{\"x\":9}\n");
        let (lines, off) = up
            .collect_component("cosh", Some(&stored))
            .unwrap()
            .unwrap();
        assert_eq!(lines, vec!["{\"x\":9}".to_string()]);
        assert_eq!(off.offset, 8);
    }

    #[test]
    fn test_discover_components_excludes_rotated() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        write_lines(&up.jsonl_path("cosh"), "");
        write_lines(&up.jsonl_path("skillfs"), "");
        write_lines(&up.rotated_path("cosh"), "");
        let components = up.discover_components();
        assert_eq!(components, vec!["cosh".to_string(), "skillfs".to_string()]);
    }

    #[test]
    fn test_offsets_round_trip() {
        let dir = TempDir::new().unwrap();
        let up = test_uploader(&dir);
        let mut offsets = Offsets::new();
        offsets.insert(
            "cosh".to_string(),
            FileOffset {
                inode: 42,
                offset: 100,
            },
        );
        up.save_offsets(&offsets).unwrap();
        let loaded = up.load_offsets();
        assert_eq!(loaded, offsets);
    }
}
