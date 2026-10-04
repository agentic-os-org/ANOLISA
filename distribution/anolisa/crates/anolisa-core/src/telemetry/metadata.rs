//! Unified client for querying ECS instance metadata API and cloud-init datasource.
//!
//! Consolidates the two repeating access patterns (curl metadata API +
//! `cloud-init query ds`) previously duplicated in `RegionProbe` and
//! `InstanceProber`.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Client for querying Alibaba Cloud instance metadata and cloud-init datasource.
///
/// Once a metadata transfer fails, subsequent
/// `query_metadata` calls short-circuit to `None` without spawning curl again,
/// so a non-ECS host pays the timeout cost only once instead of once per key.
///
/// Cheap to clone: the cloud-init cache and the unreachable latch are both
/// shared, so a transfer failure observed through *any* clone — the
/// uploader's region probe receives a clone — short-circuits every other
/// probe in the same uploader round, and the uploader's per-round
/// `clear_unreachable` re-arms them all at once.
#[derive(Clone)]
pub struct MetadataClient {
    metadata_url_base: String,
    cloud_init_all: Arc<OnceLock<Option<serde_json::Value>>>,
    /// Set after a curl transfer failure, not an HTTP error or empty value.
    /// All further `query_metadata` calls skip curl entirely. Shared by
    /// every clone: probes that receive a cloned client must observe and
    /// set the same latch as the caller's own probes, or each clone pays
    /// its own bounded curl attempt per round.
    metadata_unreachable: Arc<AtomicBool>,
}

impl MetadataClient {
    /// Create from a metadata base URL, e.g.
    /// `http://100.100.100.200/latest/meta-data`.
    pub fn new(metadata_url_base: &str) -> Self {
        Self {
            metadata_url_base: metadata_url_base.trim_end_matches('/').to_string(),
            cloud_init_all: Arc::new(OnceLock::new()),
            metadata_unreachable: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create from a full metadata key URL, e.g.
    /// `http://100.100.100.200/latest/meta-data/region-id`.
    ///
    /// The trailing key segment is stripped to obtain the base path so that
    /// `query_metadata("instance-id")` resolves to
    /// `http://100.100.100.200/latest/meta-data/instance-id`.
    pub fn from_key_url(key_url: &str) -> Self {
        let url = key_url.trim_end_matches('/');
        let base = url
            .rsplit_once('/')
            .map(|(prefix, _)| prefix)
            .unwrap_or(url);
        Self::new(base)
    }

    /// Clear the unreachable latch so the next `query_metadata` probes the
    /// endpoint again.
    ///
    /// The latch exists to collapse the several keys probed *within* one
    /// uploader round (region-id, desktop-id, instance/instance-type) into
    /// a single curl attempt after a transfer failure. It must not outlive
    /// the round: the uploader documents "probe the region once per round",
    /// and a transient failure (early-boot race, maintenance blip) on an
    /// ECS host would otherwise pin every later round to the cn-hangzhou
    /// public fallback and misattribute the region dimension for the
    /// daemon's lifetime. Callers that start a new round — only the
    /// uploader loop — clear it; one extra bounded curl attempt per round
    /// is the intended cost.
    pub fn clear_unreachable(&self) {
        self.metadata_unreachable.store(false, Ordering::Relaxed);
    }

    /// Query a metadata API key via curl.
    ///
    /// Uses `--connect-timeout 1` (connect phase) and `--max-time 2` (total)
    /// so an unreachable metadata endpoint fails fast without blocking the
    /// caller. Returns `None` on curl failure, non-200 status, or empty response.
    /// Only curl transfer failures disable subsequent metadata queries.
    pub fn query_metadata(&self, key: &str) -> Option<String> {
        // Short-circuit: once the metadata endpoint is known unreachable,
        // skip curl for all subsequent keys.
        if self.metadata_unreachable.load(Ordering::Relaxed) {
            return None;
        }

        let url = format!("{}/{}", self.metadata_url_base, key);
        let output = Command::new("curl")
            .args([
                "-s",
                "--connect-timeout",
                "1",
                "--max-time",
                "2",
                "-w",
                "\n%{http_code}",
                &url,
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            self.metadata_unreachable.store(true, Ordering::Relaxed);
            return None;
        }

        // A missing key does not make the other metadata keys unreachable.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (body, status) = stdout.rsplit_once('\n')?;
        let value = body.trim();
        (status == "200" && !value.is_empty()).then(|| value.to_string())
    }

    /// Query cloud-init datasource via `cloud-init query ds`.
    ///
    /// Returns the raw JSON value, or `None` on any failure.
    pub fn query_cloud_init_ds(&self) -> Option<serde_json::Value> {
        let output = Command::new("cloud-init")
            .args(["query", "ds"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout).ok()
    }

    /// Unified lookup of an instance attribute.
    ///
    /// Tries the ECS metadata API first, then falls back to the cloud-init
    /// datasource (`ds.meta_data.<key>`) and finally to `cloud-init query --all`
    /// full JSON lookup. Returns the first non-empty value, or `None` if all
    /// sources fail.
    ///
    /// Use this when the caller only cares about the value, not its source.
    /// For cloud-init-specific fields outside `meta_data` (e.g. `v1.cloud_id`),
    /// use [`query_cloud_init_ds`](Self::query_cloud_init_ds) directly.
    pub fn query(&self, key: &str) -> Option<String> {
        if let Some(v) = self.query_metadata(key) {
            return Some(v);
        }
        self.query_cloud_init(key)
    }

    /// Look up `key` using cloud-init query.
    ///
    /// Resolution order (most specific first):
    /// 1. Mapped cloud-init path (if any) via `cloud-init query <path>`.
    /// 2. `cloud-init query ds.meta_data.<key>`.
    /// 3. Cached `cloud-init query --all` JSON, but only under the
    ///    `ds.meta_data` subtree to avoid matching unrelated keys elsewhere
    ///    in the cloud-init datasource.
    fn query_cloud_init(&self, key: &str) -> Option<String> {
        // 1. Mapped path (e.g. instance/instance-type).
        if let Some(path) = cloud_init_path_for_metadata_key(key)
            && let Some(v) = self.cloud_init_query_path(path)
        {
            return Some(v);
        }

        // 2. Direct ds.meta_data.<key> query.
        let path = format!("ds.meta_data.{key}");
        if let Some(v) = self.cloud_init_query_path(&path) {
            return Some(v);
        }

        // 3. Fallback to --all JSON, scoped to ds.meta_data.
        self.cloud_init_query_all_key(key)
    }

    /// Run `cloud-init query <path>` and return a trimmed non-empty string.
    fn cloud_init_query_path(&self, path: &str) -> Option<String> {
        let output = Command::new("cloud-init")
            .args(["query", path])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if val.is_empty() { None } else { Some(val) }
    }

    /// Look up `key` in the cached `cloud-init query --all` JSON output,
    /// scoped to the `ds.meta_data` subtree.
    fn cloud_init_query_all_key(&self, key: &str) -> Option<String> {
        let json = self.cloud_init_all()?;
        let meta_data = json.get("ds")?.get("meta_data")?;

        if let Some(v) = find_string_by_key(meta_data, key) {
            return Some(v);
        }

        // For slash-containing keys (e.g. "instance/instance-type"), try the last segment.
        if let Some(short_key) = key.rsplit('/').next()
            && short_key != key
        {
            return find_string_by_key(meta_data, short_key);
        }
        None
    }

    /// Return cached `cloud-init query --all` JSON output.
    fn cloud_init_all(&self) -> Option<&serde_json::Value> {
        self.cloud_init_all
            .get_or_init(|| {
                let output = Command::new("cloud-init")
                    .args(["query", "--all"])
                    .output()
                    .ok()?;

                if !output.status.success() {
                    return None;
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                serde_json::from_str(&stdout).ok()
            })
            .as_ref()
    }
}

/// Map a metadata API key to the corresponding cloud-init query path.
///
/// Some metadata keys do not translate literally to `ds.meta_data.<key>`
/// because cloud-init uses dotted object paths instead of slashes.
fn cloud_init_path_for_metadata_key(key: &str) -> Option<&'static str> {
    match key {
        "instance/instance-type" => Some("ds.meta_data.instance.instance-type"),
        _ => None,
    }
}

/// Recursively find the first non-empty string value matching `key` in JSON.
fn find_string_by_key(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(s) = map.get(key).and_then(|v| v.as_str()) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            map.values().find_map(|v| find_string_by_key(v, key))
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(|v| find_string_by_key(v, key)),
        _ => None,
    }
}

/// Test helper: run `f` with a fake `cloud-init` binary that always fails.
///
/// This is placed first in PATH so that `Command::new("cloud-init")` resolves
/// to it, letting tests exercise the fallback path when cloud-init is
/// unavailable. Other commands (e.g. `curl`) still resolve via the original
/// PATH.
#[cfg(test)]
pub(crate) fn with_cloud_init_disabled<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    with_cloud_init_responses(&[], f)
}

/// Test helper: run `f` with a fake `cloud-init` binary that returns canned
/// responses for specific query paths.
///
/// `responses` is a list of `(query_path, stdout)` pairs. When the fake binary
/// is invoked as `cloud-init query <query_path>` it prints `stdout` and exits
/// 0; otherwise it exits 1.
#[cfg(test)]
pub(crate) fn with_cloud_init_responses<F, R>(responses: &[(&str, &str)], f: F) -> R
where
    F: FnOnce() -> R,
{
    use std::ffi::OsString;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let fake = tmp.path().join("cloud-init");

    let mut script = String::from("#!/bin/sh\n");
    script.push_str("case \"$2\" in\n");
    for (path, response) in responses {
        script.push_str(&format!("  \"{path}\") echo \"{response}\" ; exit 0 ;;\n"));
    }
    script.push_str("  *) exit 1 ;;\n");
    script.push_str("esac\n");

    std::fs::write(&fake, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake, perms).unwrap();
    }

    let old_path = std::env::var_os("PATH");
    let mut new_path = OsString::from(tmp.path());
    if let Some(old) = old_path.as_ref() {
        new_path.push(":");
        new_path.push(old);
    }
    // SAFETY: The static Mutex ensures no other test is executing concurrently
    // when we mutate the PATH environment variable. All tests using this helper
    // are serialized through the lock.
    unsafe { std::env::set_var("PATH", &new_path) };

    let result = f();

    // SAFETY: Same Mutex invariant as above — no concurrent access during restore.
    match old_path {
        Some(p) => unsafe { std::env::set_var("PATH", p) },
        None => unsafe { std::env::remove_var("PATH") },
    }

    result
}

// ── Unit tests ───────────────────────────────────────────────────────

/// Serve an ordered metadata response sequence on a temporary loopback port.
#[cfg(test)]
pub(crate) fn with_metadata_responses<T>(
    responses: &[(&str, u16, &str)],
    f: impl FnOnce(&str) -> T,
) -> T {
    with_metadata_script(&[], responses, f)
}

/// Like [`with_metadata_responses`], but the FIRST connection is accepted
/// and then dropped without an HTTP response — a genuine curl transfer
/// failure, which is what sets the client's unreachable latch — after
/// which the scripted responses are served in order.
#[cfg(test)]
pub(crate) fn with_metadata_dropping_first<T>(
    responses: &[(&str, u16, &str)],
    f: impl FnOnce(&str) -> T,
) -> T {
    with_metadata_script(&["region-id"], responses, f)
}

/// Scripted metadata server: the listed `drops` are accepted and closed
/// without an HTTP response (curl transfer failures), then the scripted
/// `(path, status, body)` responses are served in order.
#[cfg(test)]
pub(crate) fn with_metadata_script<T>(
    drops: &[&str],
    responses: &[(&str, u16, &str)],
    f: impl FnOnce(&str) -> T,
) -> T {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let accept_conn = || {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "no connection arrived in time");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("metadata server accept failed: {e}"),
                    }
                }
            };
            let read_request = |stream: &mut std::net::TcpStream, want_path: &str| {
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, format!("GET /{want_path} HTTP/1.1\r\n"));
                loop {
                    assert!(Instant::now() < deadline, "metadata request headers timed out");
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
            };

            for drop_path in drops {
                // Swallow the request, then close without a response — the
                // server-died-mid-transfer shape that makes curl exit
                // non-zero and the client latch.
                let mut stream = accept_conn();
                read_request(&mut stream, drop_path);
                drop(stream);
            }

            for (path, status, body) in responses {
                let mut stream = accept_conn();
                read_request(&mut stream, path);
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        f(&base)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_trims_trailing_slash() {
        let client = MetadataClient::new("http://example.com/meta-data/");
        assert_eq!(client.metadata_url_base, "http://example.com/meta-data");
    }

    #[test]
    fn test_from_key_url_strips_last_segment() {
        let client =
            MetadataClient::from_key_url("http://100.100.100.200/latest/meta-data/region-id");
        assert_eq!(
            client.metadata_url_base,
            "http://100.100.100.200/latest/meta-data"
        );
    }

    #[test]
    fn test_from_key_url_handles_trailing_slash() {
        let client =
            MetadataClient::from_key_url("http://100.100.100.200/latest/meta-data/region-id/");
        assert_eq!(
            client.metadata_url_base,
            "http://100.100.100.200/latest/meta-data"
        );
    }

    #[test]
    fn test_query_metadata_unreachable_returns_none() {
        let client = MetadataClient::new("http://127.0.0.1:19999/no-such-endpoint");
        assert!(client.query_metadata("instance-id").is_none());
        assert!(client.metadata_unreachable.load(Ordering::Relaxed));
        assert!(client.query_metadata("image-id").is_none());
    }

    #[test]
    fn cleared_latch_lets_the_next_query_probe_again() {
        with_metadata_dropping_first(&[("region-id", 200, "cn-beijing\n")], |base| {
            let client = MetadataClient::new(base);
            assert!(client.query_metadata("region-id").is_none());
            assert!(
                client.metadata_unreachable.load(Ordering::Relaxed),
                "a dropped transfer must latch"
            );
            client.clear_unreachable();
            assert_eq!(
                client.query_metadata("region-id").as_deref(),
                Some("cn-beijing"),
                "a cleared latch must probe the endpoint again"
            );
        });
    }

    #[test]
    fn latch_still_collapses_keys_within_one_probe_series() {
        with_metadata_dropping_first(&[("image-id", 200, "img-test\n")], |base| {
            let client = MetadataClient::new(base);
            assert!(client.query_metadata("region-id").is_none());
            // Without clearing, the staged 200 is never fetched: a second
            // key in the same probe series short-circuits to None.
            assert!(client.query_metadata("image-id").is_none());
            // Only after the round boundary (the clear) does it probe.
            client.clear_unreachable();
            assert_eq!(
                client.query_metadata("image-id").as_deref(),
                Some("img-test")
            );
        });
    }

    #[test]
    fn clones_share_one_latch_in_both_directions() {
        // The uploader hands a clone to the region probe and keeps the
        // original for the product-type probes. A transfer failure seen
        // through the clone must latch the caller's client too (one
        // bounded curl attempt per round, not one per clone), and the
        // per-round clear must re-arm the clone (a shared false), not
        // leave it pinned to a stale copy of the flag.
        with_metadata_dropping_first(&[("region-id", 200, "cn-beijing\n")], |base| {
            let client = MetadataClient::new(base);
            let probe = client.clone();

            // Round 1: the probe's transfer failure latches the shared
            // flag — visible on the caller's client.
            assert!(probe.query_metadata("region-id").is_none());
            assert!(
                client.metadata_unreachable.load(Ordering::Relaxed),
                "a failure through the clone must latch the caller's client"
            );
            // The caller's own next key short-circuits without another
            // connection: the staged 200 stays unconsumed.
            assert!(client.query_metadata("desktop-id").is_none());

            // Round boundary: the caller clears, and the *clone* re-arms —
            // its next query actually reaches the endpoint and consumes
            // the staged 200.
            client.clear_unreachable();
            assert_eq!(
                probe.query_metadata("region-id").as_deref(),
                Some("cn-beijing"),
                "the per-round clear must re-arm the clone, not only the clearing client"
            );
        });
    }

    #[test]
    fn test_http_responses_do_not_disable_later_metadata_queries() {
        for (status, body) in [
            (404, "missing"),
            (500, "failed"),
            (302, "redirect"),
            (200, ""),
            (200, " \n"),
        ] {
            with_metadata_responses(
                &[
                    ("desktop-id", status, body),
                    ("image-id", 200, " img-test\n"),
                ],
                |base| {
                    let client = MetadataClient::new(base);
                    assert_eq!(client.query_metadata("desktop-id"), None);
                    assert_eq!(
                        client.query_metadata("image-id").as_deref(),
                        Some("img-test")
                    );
                },
            );
        }
    }

    #[test]
    fn test_query_unknown_key_returns_none() {
        let client = MetadataClient::new("http://127.0.0.1:19999/no-such-endpoint");
        // Metadata API is unreachable and this key should not exist in cloud-init.
        assert!(client.query("__this_key_should_not_exist__").is_none());
    }

    #[test]
    fn test_find_string_by_key_finds_nested_value() {
        let json = serde_json::json!({
            "v1": { "cloud_id": "aliyun" },
            "ds": {
                "meta_data": {
                    "owner-account-id": "1644215368948677"
                }
            }
        });
        assert_eq!(
            find_string_by_key(&json, "owner-account-id"),
            Some("1644215368948677".to_string())
        );
        assert_eq!(
            find_string_by_key(&json, "cloud_id"),
            Some("aliyun".to_string())
        );
        assert_eq!(find_string_by_key(&json, "missing"), None);
        assert_eq!(find_string_by_key(&json, "v1"), None); // not a string
    }
}
