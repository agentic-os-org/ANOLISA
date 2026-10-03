//! Regression: `compact()` must not overflow on an out-of-range
//! `[memory.index].cold_after_days`.
//!
//! `BM25Store::compact` computes `now_ms - (cold_after_days as i64 * 86_400_000)`
//! with unchecked `i64` arithmetic. The config field is an unbounded `u64`
//! (TOML file or `MEMORY_INDEX_COLD_AFTER_DAYS` env), so any value above
//! `i64::MAX / 86_400_000` overflows the multiply: a debug build panics
//! inside `mem_compact`, and a release build wraps to a cutoff that marks
//! every never-accessed file — including brand-new ones — cold.

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use tempfile::tempdir;

#[test]
fn compact_rejects_or_survives_huge_cold_after_days() {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    // 2e14 days * 86_400_000 ms/day overflows i64.
    cfg.memory.index.cold_after_days = 200_000_000_000_000;
    let svc = MemoryService::new(cfg).unwrap();

    // Observe so the note is synchronously indexed (warm, access_count = 0).
    let path = svc
        .memory_observe("the deploy codename is zephyr", None, None)
        .unwrap();

    // On main this panics in debug and wraps in release; the wrapped cutoff
    // marks the fresh note cold, so the non-deep search below misses it.
    let compacted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| svc.compact()));
    match compacted {
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!("mem_compact panicked on cold_after_days overflow: {msg}");
        }
        Ok(Err(e)) => {
            // A clean validation error is acceptable.
            eprintln!("compact rejected the config: {e}");
        }
        Ok(Ok(compacted)) => {
            assert_eq!(
                compacted, 0,
                "an out-of-range cold_after_days must not mark any file cold"
            );
            let hits = svc
                .memory_search("zephyr", 5, Some("bm25"), None, None)
                .unwrap();
            let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
            assert_eq!(
                paths,
                vec![path.as_str()],
                "the fresh note was marked cold by an overflowing cutoff"
            );
        }
    }
}
