//! Instance ID resolution utility
//!
//! Provides a shared function to resolve the current machine's instance ID,
//! used by both SLS PutLogs uploader and Logtail file exporter.
//!
//! `get_instance_id()` is cached via `OnceLock` (it always resolves to a
//! non-empty value thanks to the hostname fallback). `get_owner_account_id()`
//! uses a `CachedUid` that caches ONLY a successful (non-empty) fetch: a
//! transient metadata failure returns empty without being cached, so a later
//! call retries instead of permanently serving an empty owner-account-id.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::ecs_metadata::{METADATA_BASE, metadata_agent, read_plain};

/// ECS metadata 请求超时（连接 + 读取均为 1 秒）
const METADATA_TIMEOUT: Duration = Duration::from_secs(1);

/// 连续失败上限：超过此次数后不再重试，避免非 ECS 环境反复 1s 超时
const MAX_RETRIES: usize = 3;

/// owner-account-id 缓存：只缓存成功（非空）的 fetch 结果。
///
/// 与 `OnceLock` 不同，瞬时失败（返回空）不会被永久缓存——下次调用会重试，
/// 避免一次 metadata 抖动让 owner-account-id 永久变空（SLS 多租户归属 key 丢失）。
struct CachedUid {
    cell: Mutex<Option<String>>,
    failures: AtomicUsize,
}

impl CachedUid {
    const fn new() -> Self {
        Self {
            cell: Mutex::new(None),
            failures: AtomicUsize::new(0),
        }
    }

    /// 命中缓存直接返回；否则运行 `fetch`，仅缓存非空结果，空结果返回但不缓存。
    /// 连续失败达到 `MAX_RETRIES` 后不再调用 `fetch`，直接返回空。
    fn get_or_fetch(&self, fetch: impl FnOnce() -> String) -> String {
        let mut guard = self.cell.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.as_ref() {
            return v.clone();
        }
        if self.failures.load(Ordering::Relaxed) >= MAX_RETRIES {
            return String::new();
        }
        let v = fetch();
        if !v.is_empty() {
            *guard = Some(v.clone());
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        v
    }
}

/// 全局缓存：owner-account-id（仅缓存成功结果）
static OWNER_ACCOUNT_ID: CachedUid = CachedUid::new();
/// 全局缓存：instance-id
static INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// 获取 owner account ID：成功结果缓存，后续直接返回；失败返回空字符串且不缓存，
/// 下次调用重试。请求阿里云 ECS metadata（超时 1 秒）。
pub fn get_owner_account_id() -> String {
    OWNER_ACCOUNT_ID.get_or_fetch(fetch_owner_account_id)
}

/// 实际请求 owner-account-id
fn fetch_owner_account_id() -> String {
    let agent = metadata_agent(METADATA_TIMEOUT);
    match read_plain(&agent, METADATA_BASE, "owner-account-id") {
        Some(uid) => {
            log::info!("Got ECS owner-account-id: {uid}");
            uid
        }
        None => {
            log::warn!("ECS owner-account-id not available");
            String::new()
        }
    }
}

/// 获取实例ID（带缓存）：首次调用请求阿里云 ECS metadata（超时1秒），
/// 失败则回退到 hostname。后续调用直接返回缓存值。
pub fn get_instance_id() -> &'static str {
    INSTANCE_ID.get_or_init(fetch_instance_id)
}

/// 实际请求 instance-id
fn fetch_instance_id() -> String {
    // 尝试从 ECS metadata service 获取 instance-id
    let agent = metadata_agent(METADATA_TIMEOUT);
    if let Some(id) = read_plain(&agent, METADATA_BASE, "instance-id") {
        log::debug!("Got ECS instance-id: {id}");
        return id;
    }
    log::debug!("ECS metadata not available, falling back to hostname");
    // 回退: /etc/hostname -> $HOSTNAME -> "unknown"
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_uid_caches_success_only() {
        let cache = CachedUid::new();
        let calls = AtomicUsize::new(0);
        // Scripted fetcher: 0th attempt fails (empty, simulating a metadata
        // blip), 1st returns a real uid, 2nd returns empty again — the 2nd is
        // only reached if the cache is NOT consulted, so it proves cache hits.
        let fetch = || match calls.fetch_add(1, Ordering::SeqCst) {
            0 => String::new(),
            1 => "uid-42".to_string(),
            _ => String::new(),
        };

        // 1) Transient failure: returns empty, must NOT be cached.
        assert_eq!(cache.get_or_fetch(fetch), "");
        // 2) Recovery: re-fetches (proves the empty wasn't cached) -> real uid,
        //    now cached.
        assert_eq!(cache.get_or_fetch(fetch), "uid-42");
        // 3) Cache hit: even though the fetcher would now return empty, the
        //    cached value short-circuits.
        assert_eq!(cache.get_or_fetch(fetch), "uid-42");
        // Exactly 2 fetches happened (3rd call served from cache).
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cached_uid_stops_after_max_retries() {
        let cache = CachedUid::new();
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            String::new()
        };

        // First MAX_RETRIES calls each invoke fetch and return empty.
        for _ in 0..MAX_RETRIES {
            assert_eq!(cache.get_or_fetch(fetch), "");
        }
        assert_eq!(calls.load(Ordering::SeqCst), MAX_RETRIES);

        // After the cap, the next call MUST short-circuit:
        assert_eq!(cache.get_or_fetch(fetch), "");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            MAX_RETRIES,
            "must not fetch after MAX_RETRIES cap"
        );
    }
}
