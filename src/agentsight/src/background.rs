use crate::genai::GenAIExporter;
use crate::genai::logtail::LogtailExporter;
use crate::storage::sqlite::GenAISqliteStore;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) fn start_stale_scanner(store: Arc<GenAISqliteStore>, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("genai-stale-scanner".to_string())
        .spawn(move || {
            log::info!("GenAI stale-pending scanner started (interval=60s, timeout=300s)");
            stale_scanner_loop(&store, &stop, 60);
            log::info!("GenAI stale-pending scanner stopped");
        })
        .ok();
}

/// Marks stale pending calls as interrupted every `interval_secs`, until `stop`
/// is cleared. `interval_secs` is a parameter so tests can exercise the loop
/// body without a 60-second wait; production always passes 60.
pub(crate) fn stale_scanner_loop(store: &GenAISqliteStore, stop: &AtomicBool, interval_secs: u64) {
    while crate::utils::thread::sleep_or_stop(stop, interval_secs) {
        if let Err(e) = store.mark_interrupted_stale(300) {
            log::warn!("Stale-pending scan failed: {e}");
        }
    }
}

/// True if a filesystem event is a "file fully written" (CloseWrite) event.
///
/// Pure helper extracted so the config-watcher's event filtering is unit-testable
/// without spawning a real inotify watcher.
pub(crate) fn is_close_write(kind: &notify::EventKind) -> bool {
    matches!(
        kind,
        notify::EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Write
        ))
    )
}

/// True if any of `paths` has a file name equal to `target`.
///
/// Pure helper extracted from the config-watcher's path filtering.
pub(crate) fn path_matches_target(paths: &[PathBuf], target: &Option<OsString>) -> bool {
    paths
        .iter()
        .any(|p| p.file_name().map(|f| f.to_os_string()) == *target)
}

/// Decision produced by [`decide_sls_config_change`]: what the config watcher
/// should do in response to a parsed `runtime.sls_logtail_path` value.
///
/// Side effects (exporter construction, mailbox write, dynamic logtail-path
/// update) are carried out by the thread shell so the decision logic stays
/// pure and testable.
#[derive(Debug, PartialEq)]
pub(crate) enum SlsConfigAction {
    /// Field missing / parse error, or empty path while already inactive: no-op.
    NoChange,
    /// Empty path while active: SLS was just deactivated (dynamic path cleared).
    Deactivated,
    /// Non-empty path but uid fetch failed: SLS activation deferred until
    /// the next config event (when the metadata endpoint may be reachable).
    UidUnavailable,
    /// Non-empty path, first activation: shell should build a LogtailExporter for
    /// `path` and deposit it into the mailbox.
    Activate { path: String },
    /// Non-empty path, already active: dynamic path swapped, no new exporter.
    Reactivated { path: String },
}

/// Decide how to react to a parsed `runtime.sls_logtail_path`. Mutates only the
/// caller-owned `sls_activated` flag (the test-and-set that distinguishes first
/// activation from reactivation); the process-global dynamic logtail path is
/// updated by the caller (`handle_config_event`) from the returned action, so
/// this function touches no cross-module global state.
///
/// `uid` is passed in (not fetched here) because `get_owner_account_id` blocks on
/// ECS metadata and `process::exit`s the test harness; the shell fetches it and
/// this function only inspects whether it is empty.
pub(crate) fn decide_sls_config_change(
    parsed: Option<Option<String>>,
    sls_activated: &AtomicBool,
    uid: &str,
) -> SlsConfigAction {
    match parsed {
        None => SlsConfigAction::NoChange,
        Some(None) => {
            if sls_activated.swap(false, Ordering::SeqCst) {
                SlsConfigAction::Deactivated
            } else {
                SlsConfigAction::NoChange
            }
        }
        Some(Some(new_path)) => {
            if uid.is_empty() {
                return SlsConfigAction::UidUnavailable;
            }
            if !sls_activated.swap(true, Ordering::SeqCst) {
                SlsConfigAction::Activate { path: new_path }
            } else {
                SlsConfigAction::Reactivated { path: new_path }
            }
        }
    }
}

/// Handle one config-file change: parse `runtime.sls_logtail_path`, decide the
/// SLS reaction, and carry out the in-process side effects (update the
/// process-global dynamic logtail path, and on first activation build a
/// LogtailExporter and deposit it into the mailbox).
///
/// Returns the [`SlsConfigAction`] for logging/tracing. `fetch_uid` is injected
/// so tests can supply a uid without invoking `get_owner_account_id`, which
/// blocks on ECS metadata.
pub(crate) fn handle_config_event(
    content: &str,
    sls_activated: &AtomicBool,
    fetch_uid: impl Fn() -> String,
    encryption_pem: Option<&str>,
    trace_enabled: bool,
    pending_logtail: &Mutex<Option<Box<dyn GenAIExporter>>>,
) -> SlsConfigAction {
    let parsed = crate::config::parse_runtime_sls_path(content);
    let uid: String = match &parsed {
        Some(Some(_)) => fetch_uid(),
        _ => String::new(),
    };
    let action = decide_sls_config_change(parsed, sls_activated, &uid);
    match &action {
        SlsConfigAction::NoChange => {}
        SlsConfigAction::Deactivated => {
            crate::genai::logtail::set_dynamic_logtail_path("");
            log::info!(
                "Config watcher: SLS Logtail deactivated \
                 (runtime.sls_logtail_path cleared)"
            );
        }
        SlsConfigAction::UidUnavailable => {
            log::warn!(
                "Config watcher: SLS activation deferred — uid fetch failed \
                 (metadata endpoint unreachable). Will retry on next config event."
            );
        }
        SlsConfigAction::Activate { path } => {
            crate::genai::logtail::set_dynamic_logtail_path(path);
            let exporter = LogtailExporter::new_with_path(path, encryption_pem, trace_enabled);
            log::info!("Config watcher: LogtailExporter created (path={path}, uid={uid})");
            if let Ok(mut guard) = pending_logtail.lock() {
                *guard = Some(Box::new(exporter));
            }
            log::info!("Config watcher: SLS Logtail activated dynamically");
        }
        SlsConfigAction::Reactivated { path } => {
            crate::genai::logtail::set_dynamic_logtail_path(path);
            log::info!("Config watcher: SLS Logtail re-activated with path={path}");
        }
    }
    action
}

pub(crate) fn start_config_watcher(
    config_path: PathBuf,
    sls_activated: Arc<AtomicBool>,
    pending_logtail: Arc<Mutex<Option<Box<dyn GenAIExporter>>>>,
    encryption_pem: Option<String>,
    trace_enabled: bool,
    stop: Arc<AtomicBool>,
) {
    use notify::{Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};

    let watch_path = config_path.clone();
    std::thread::Builder::new()
        .name("config-watcher".to_string())
        .spawn(move || {
            log::info!("Config watcher started for {watch_path:?}");

            let (tx, rx) = std::sync::mpsc::channel::<notify::Result<NotifyEvent>>();

            let mut watcher: RecommendedWatcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    log::warn!("Failed to create config file watcher: {e}");
                    return;
                }
            };

            let watch_dir = watch_path.parent().unwrap_or(Path::new("/"));
            if let Err(e) = watcher.watch(watch_dir, RecursiveMode::NonRecursive) {
                log::warn!("Failed to watch config directory {watch_dir:?}: {e}");
                return;
            }

            let target_filename = watch_path.file_name().map(|f| f.to_os_string());

            while stop.load(Ordering::SeqCst) {
                let event = match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(event) => event,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let event = match event {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("Config watcher error: {e}");
                        continue;
                    }
                };

                if !is_close_write(&event.kind) {
                    continue;
                }
                if !path_matches_target(&event.paths, &target_filename) {
                    continue;
                }

                let content = match std::fs::read_to_string(&watch_path) {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!("Config watcher: failed to read {watch_path:?}: {e}");
                        continue;
                    }
                };

                let action = handle_config_event(
                    &content,
                    &sls_activated,
                    crate::genai::instance_id::get_owner_account_id,
                    encryption_pem.as_deref(),
                    trace_enabled,
                    &pending_logtail,
                );
                // UidUnavailable is logged inside handle_config_event;
                // the process continues and retries on the next config event.
                let _ = action;
            }

            log::info!("Config watcher exiting");
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn tmp_dir(tag: &str) -> PathBuf {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("bg-test-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_stale_scanner_loop_returns_when_stopped() {
        // stop already false -> loop must exit promptly without running the body.
        let stop = Arc::new(AtomicBool::new(false));
        let dir = tmp_dir("stale1");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &dir.join("test.db"),
                crate::config::PeriodicStoragePolicy::default(),
            )
            .unwrap(),
        );
        let start = std::time::Instant::now();
        stale_scanner_loop(&store, &stop, 1);
        // First sleep_or_stop call sleeps ~1s then sees stop=false and returns.
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn test_stale_scanner_loop_runs_body_then_stops() {
        use crate::storage::sqlite::{PendingCallInfo, PendingOrigin};

        let dir = tmp_dir("stale2");
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &dir.join("test.db"),
                crate::config::PeriodicStoragePolicy::default(),
            )
            .unwrap(),
        );

        // Seed a pending row with an old timestamp so it counts as stale.
        let old_ts_ns = 1_000_000_000u64; // ~1970, definitely older than 300s ago
        store
            .insert_pending(&PendingCallInfo {
                call_id: "stale-1".to_string(),
                trace_id: None,
                conversation_id: None,
                session_id: None,
                start_timestamp_ns: old_ts_ns,
                pid: 1234,
                process_name: "test".to_string(),
                agent_name: None,
                http_method: None,
                http_path: None,
                input_messages: None,
                system_instructions: None,
                user_query: None,
                is_sse: false,
                model: None,
                provider: None,
                call_kind: "main".to_string(),
                pending_origin: PendingOrigin::RequestCapture,
                pending_match_key: None,
            })
            .unwrap();

        // Run the loop body via a 1s interval; stop after one iteration.
        let stop = Arc::new(AtomicBool::new(true));
        let stop_clone = Arc::clone(&stop);
        let store_clone = Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            stale_scanner_loop(&store_clone, &stop_clone, 1);
        });
        std::thread::sleep(std::time::Duration::from_millis(2500));
        stop.store(false, Ordering::SeqCst);
        handle.join().unwrap();

        // Discriminating signal: the loop body must have marked the seeded row
        // interrupted. If the body never ran, the row is still pending and this
        // call would mark it now, returning 1. So it MUST return 0.
        assert_eq!(
            store.mark_interrupted_stale(0).unwrap(),
            0,
            "loop body should have already marked the stale pending row"
        );
    }

    // ── is_close_write ──────────────────────────────────────────────
    #[test]
    fn test_is_close_write() {
        use notify::EventKind;
        use notify::event::{AccessKind, AccessMode};
        assert!(is_close_write(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
        // Other access modes / kinds are not "fully written".
        assert!(!is_close_write(&EventKind::Access(AccessKind::Close(
            AccessMode::Read
        ))));
        assert!(!is_close_write(&EventKind::Access(AccessKind::Open(
            AccessMode::Write
        ))));
        assert!(!is_close_write(&EventKind::Modify(
            notify::event::ModifyKind::Any
        )));
    }

    // ── path_matches_target ─────────────────────────────────────────
    #[test]
    fn test_path_matches_target() {
        let target = Some(OsString::from("agentsight.json"));
        assert!(path_matches_target(
            &[PathBuf::from("/etc/anolisa/agentsight.json")],
            &target
        ));
        // Non-matching file name.
        assert!(!path_matches_target(
            &[PathBuf::from("/etc/anolisa/other.json")],
            &target
        ));
        // Matches when any path in the list matches.
        assert!(path_matches_target(
            &[
                PathBuf::from("/etc/anolisa/other.json"),
                PathBuf::from("/etc/anolisa/agentsight.json"),
            ],
            &target
        ));
        // Empty list never matches.
        assert!(!path_matches_target(&[], &target));
        // None target never matches a named file.
        assert!(!path_matches_target(
            &[PathBuf::from("/etc/anolisa/agentsight.json")],
            &None
        ));
    }

    // ── decide_sls_config_change ────────────────────────────────────
    #[test]
    fn test_decide_sls_none_is_nochange() {
        let flag = AtomicBool::new(false);
        assert_eq!(
            decide_sls_config_change(None, &flag, "uid"),
            SlsConfigAction::NoChange
        );
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_decide_sls_empty_while_inactive_is_nochange() {
        let flag = AtomicBool::new(false);
        assert_eq!(
            decide_sls_config_change(Some(None), &flag, "uid"),
            SlsConfigAction::NoChange
        );
    }

    #[test]
    fn test_decide_sls_empty_while_active_deactivates() {
        let flag = AtomicBool::new(true);
        assert_eq!(
            decide_sls_config_change(Some(None), &flag, "uid"),
            SlsConfigAction::Deactivated
        );
        assert!(!flag.load(Ordering::SeqCst), "flag cleared on deactivation");
    }

    #[test]
    fn test_decide_sls_path_but_no_uid_defers() {
        let flag = AtomicBool::new(false);
        assert_eq!(
            decide_sls_config_change(Some(Some("/p.log".into())), &flag, ""),
            SlsConfigAction::UidUnavailable
        );
        // Flag must NOT be set when uid is missing.
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_decide_sls_first_activation() {
        let flag = AtomicBool::new(false);
        let action = decide_sls_config_change(Some(Some("/p.log".into())), &flag, "ecs-uid");
        assert_eq!(
            action,
            SlsConfigAction::Activate {
                path: "/p.log".to_string()
            }
        );
        assert!(flag.load(Ordering::SeqCst), "flag set on activation");
    }

    #[test]
    fn test_decide_sls_reactivation() {
        let flag = AtomicBool::new(true); // already active
        let action = decide_sls_config_change(Some(Some("/p2.log".into())), &flag, "ecs-uid");
        assert_eq!(
            action,
            SlsConfigAction::Reactivated {
                path: "/p2.log".to_string()
            }
        );
        assert!(flag.load(Ordering::SeqCst));
    }

    // ── handle_config_event (dispatch + exporter construction) ───────
    fn empty_mailbox() -> Mutex<Option<Box<dyn GenAIExporter>>> {
        Mutex::new(None)
    }

    // Serializes tests that read or write the process-global SLS_LOGTAIL_FILE
    // env var or dynamic logtail path. Cargo runs tests in parallel and would
    // otherwise let them clobber each other's path assertions.
    fn lock_sls_path() -> std::sync::MutexGuard<'static, ()> {
        // Recover from poisoning so one failing test does not cascade-panic the rest.
        crate::genai::logtail::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_dynamic_path_side_effect_is_in_handler_not_decider() {
        let _guard = lock_sls_path();
        let reset = || crate::genai::logtail::set_dynamic_logtail_path("");
        reset();
        assert_eq!(
            crate::genai::logtail::logtail_path(),
            None,
            "precondition: no SLS_LOGTAIL_FILE env in the test process"
        );

        // (1) decide_sls_config_change must be PURE w.r.t. the global dynamic
        //     path. Reverting the fix (set_dynamic_logtail_path back inside
        //     decide) makes this assertion fail.
        let flag = AtomicBool::new(false);
        let action =
            decide_sls_config_change(Some(Some("/decide-only.log".into())), &flag, "ecs-uid");
        assert_eq!(
            action,
            SlsConfigAction::Activate {
                path: "/decide-only.log".to_string()
            }
        );
        assert_eq!(
            crate::genai::logtail::logtail_path(),
            None,
            "decide must NOT touch the global dynamic path"
        );

        // (2) handle_config_event MUST set the global dynamic path on
        //     activation. Forgetting to move the side effect into the handler
        //     makes this assertion fail.
        reset();
        let flag = AtomicBool::new(false);
        let mailbox = empty_mailbox();
        handle_config_event(
            r#"{"runtime":{"sls_logtail_path":"/handler-set.log"}}"#,
            &flag,
            || "ecs-uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(
            crate::genai::logtail::logtail_path(),
            Some("/handler-set.log".to_string()),
            "handler must set the global dynamic path on activation"
        );

        // (3) handle_config_event MUST clear it on deactivation.
        let flag = AtomicBool::new(true);
        let mailbox = empty_mailbox();
        handle_config_event(
            r#"{"runtime":{"sls_logtail_path":""}}"#,
            &flag,
            || "uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(
            crate::genai::logtail::logtail_path(),
            None,
            "handler must clear the global dynamic path on deactivation"
        );

        reset();
    }

    #[test]
    fn test_handle_event_none_is_nochange() {
        let flag = AtomicBool::new(false);
        let mailbox = empty_mailbox();
        // Content without runtime.sls_logtail_path -> parse None -> NoChange.
        let action = handle_config_event(
            r#"{"deadloop":{"enabled":false}}"#,
            &flag,
            || "uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(action, SlsConfigAction::NoChange);
        assert!(mailbox.lock().unwrap().is_none());
    }

    #[test]
    fn test_handle_event_deactivate() {
        let _guard = lock_sls_path();
        let flag = AtomicBool::new(true); // currently active
        let mailbox = empty_mailbox();
        let action = handle_config_event(
            r#"{"runtime":{"sls_logtail_path":""}}"#,
            &flag,
            || "uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(action, SlsConfigAction::Deactivated);
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_handle_event_defers_when_uid_missing() {
        let flag = AtomicBool::new(false);
        let mailbox = empty_mailbox();
        // Non-empty path but uid fetch returns empty -> UidUnavailable.
        let action = handle_config_event(
            r#"{"runtime":{"sls_logtail_path":"/p.log"}}"#,
            &flag,
            String::new, // empty uid
            None,
            false,
            &mailbox,
        );
        assert_eq!(action, SlsConfigAction::UidUnavailable);
        // No exporter built, flag not set.
        assert!(mailbox.lock().unwrap().is_none());
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_handle_event_activate_builds_exporter() {
        let _guard = lock_sls_path();
        let flag = AtomicBool::new(false);
        let mailbox = empty_mailbox();
        let action = handle_config_event(
            r#"{"runtime":{"sls_logtail_path":"/var/log/sls/a.log"}}"#,
            &flag,
            || "ecs-uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(
            action,
            SlsConfigAction::Activate {
                path: "/var/log/sls/a.log".to_string()
            }
        );
        assert!(flag.load(Ordering::SeqCst));
        // Exporter was built and deposited into the mailbox.
        assert!(mailbox.lock().unwrap().is_some());
        crate::genai::logtail::set_dynamic_logtail_path("");
    }

    #[test]
    fn test_handle_event_reactivate_no_new_exporter() {
        let _guard = lock_sls_path();
        let flag = AtomicBool::new(true); // already active
        let mailbox = empty_mailbox();
        // Seed a different active path: an active->active change must OVERWRITE
        // it, not leave the stale value.
        crate::genai::logtail::set_dynamic_logtail_path("/stale.log");
        let action = handle_config_event(
            r#"{"runtime":{"sls_logtail_path":"/p2.log"}}"#,
            &flag,
            || "ecs-uid".to_string(),
            None,
            false,
            &mailbox,
        );
        assert_eq!(
            action,
            SlsConfigAction::Reactivated {
                path: "/p2.log".to_string()
            }
        );
        // Reactivation does NOT build a new exporter.
        assert!(mailbox.lock().unwrap().is_none());
        // ...but the Reactivated arm MUST overwrite the global dynamic path to
        // the new value (load-bearing: dropping its set_dynamic_logtail_path
        // would silently keep writing GenAI events to the stale path).
        assert_eq!(
            crate::genai::logtail::logtail_path(),
            Some("/p2.log".to_string()),
            "reactivation must overwrite the dynamic path to the new value"
        );
        crate::genai::logtail::set_dynamic_logtail_path("");
    }
}
