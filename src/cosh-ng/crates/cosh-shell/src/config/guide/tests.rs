use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, GuideStore, RuntimeState) {
    let (temp, store, runtime) = uninitialized_fixture();
    store.initialize_runtime(&runtime).unwrap();
    (temp, store, runtime)
}

fn uninitialized_fixture() -> (tempfile::TempDir, GuideStore, RuntimeState) {
    #[cfg(target_os = "linux")]
    let temp = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let store = GuideStore::open(root.join("persistent")).unwrap();
    let runtime = RuntimeState::open(&root).unwrap();
    (temp, store, runtime)
}

fn request<'a>(kind: InviteKind, problem: &'a str, session: &'a str) -> ClaimRequest<'a> {
    ClaimRequest {
        kind,
        host: "host",
        problem,
        session,
    }
}

#[test]
fn missing_preferences_have_defaults_but_corrupt_preferences_fail() {
    let (_temp, store, runtime) = fixture();
    assert!(store.load_preferences().unwrap().is_none());
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    assert!(snapshot.preferences.guide_enabled);
    assert!(snapshot.preferences.banner_enabled);
    fs::write(store.root.join("preferences.json"), b"broken").unwrap();
    fs::set_permissions(
        store.root.join("preferences.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(matches!(
        store.snapshot(Some(&runtime)),
        Err(Error::Corrupt)
    ));
}

#[test]
fn banner_and_guide_preferences_are_independent() {
    let (_temp, store, runtime) = fixture();
    store.set_banner(false).unwrap();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    assert!(snapshot.preferences.guide_enabled);
    assert!(!snapshot.preferences.banner_enabled);
    store.set_guide(Some(&runtime), false).unwrap();
    let preferences = store.load_preferences().unwrap().unwrap();
    assert!(!preferences.guide_enabled);
    assert!(!preferences.banner_enabled);
}

#[test]
fn disabled_guide_does_not_consume_claims() {
    let (_temp, store, runtime) = uninitialized_fixture();
    runtime.initialize(false).unwrap();
    let state = runtime.read().unwrap().unwrap();
    let preferences = Preferences {
        guide_enabled: false,
        generation: state.generation.clone(),
        ..Preferences::default()
    };
    store.write_preferences(&preferences).unwrap();
    let snapshot = GuideSnapshot {
        generation: generation_token(&preferences.generation, Some(&state.generation)),
        preferences,
        runtime_generation: true,
    };
    let path = store.root.join("state.lock");
    assert!(!path.exists());
    let original = private_state::open_owner_file(&path, true).unwrap();
    private_state::try_lock_exclusive(&original).unwrap();
    let inherited = original.try_clone().unwrap();
    let ready = std::sync::Barrier::new(2);
    let release = std::sync::Barrier::new(2);
    let (contention, decision) = std::thread::scope(|scope| {
        let ready_child = &ready;
        let release_child = &release;
        scope.spawn(move || {
            ready_child.wait();
            release_child.wait();
            drop(inherited);
        });
        drop(original);
        ready.wait();
        let contention = private_state::LockGuard::try_acquire(&path);
        let decision = store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "problem", "session"),
            100,
        );
        release.wait();
        (contention, decision)
    });
    assert!(matches!(contention, Err(private_state::Error::LockBusy)));
    assert_eq!(decision, Ok(ClaimDecision::Disabled));
    assert!(!store.root.join("claims.json").exists());
    fs::remove_file(&path).unwrap();
    assert_eq!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "problem", "session"),
            101
        ),
        Ok(ClaimDecision::Disabled)
    );
    assert!(!path.exists());
}

#[test]
fn off_on_invalidates_inflight_results_and_retains_quota() {
    let (_temp, store, runtime) = fixture();
    let before = store.snapshot(Some(&runtime)).unwrap();
    let decision = store
        .claim(
            Some(&runtime),
            &before,
            request(InviteKind::LoginEntry, "", "a"),
            100,
        )
        .unwrap();
    let ClaimDecision::Claimed(permit) = decision else {
        panic!("first claim")
    };
    assert!(store.validate_claim(Some(&runtime), &permit).unwrap());
    store.set_guide(Some(&runtime), false).unwrap();
    assert!(!store.validate_claim(Some(&runtime), &permit).unwrap());
    store.set_guide(Some(&runtime), true).unwrap();
    assert!(!store.validate_claim(Some(&runtime), &permit).unwrap());
    assert_eq!(
        store.claim(
            Some(&runtime),
            &before,
            request(InviteKind::Failure, "new", "b"),
            101
        ),
        Ok(ClaimDecision::Stale)
    );
    let after = store.snapshot(Some(&runtime)).unwrap();
    assert_eq!(
        store.claim(
            Some(&runtime),
            &after,
            request(InviteKind::LoginEntry, "", "b"),
            102
        ),
        Ok(ClaimDecision::Suppressed)
    );
}

#[test]
fn health_and_failure_share_problem_dedup_but_have_separate_session_quota() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::LoginHealth, "disk", "s1"),
            100
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert_eq!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s1"),
            101
        ),
        Ok(ClaimDecision::Suppressed)
    );
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "memory", "s1"),
            102
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert_eq!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "build", "s1"),
            103
        ),
        Ok(ClaimDecision::Suppressed)
    );
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s2"),
            100 + DAY_MS
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
}

#[test]
fn generic_and_default_login_have_independent_seven_day_quotas() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    for kind in [InviteKind::LoginEntry, InviteKind::DefaultLogin] {
        assert!(matches!(
            store.claim(Some(&runtime), &snapshot, request(kind, "", "s1"), 100),
            Ok(ClaimDecision::Claimed(_))
        ));
        assert_eq!(
            store.claim(
                Some(&runtime),
                &snapshot,
                request(kind, "", "s2"),
                100 + DAY_MS
            ),
            Ok(ClaimDecision::Suppressed)
        );
        assert!(matches!(
            store.claim(
                Some(&runtime),
                &snapshot,
                request(kind, "", "s3"),
                100 + WEEK_MS
            ),
            Ok(ClaimDecision::Claimed(_))
        ));
    }
}

#[test]
fn busy_and_corrupt_claim_storage_do_not_reset_to_first_use() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let guard = private_state::LockGuard::try_acquire(&store.root.join("state.lock")).unwrap();
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s1"),
            100
        ),
        Err(Error::State(private_state::Error::LockBusy))
    ));
    drop(guard);
    fs::write(store.root.join("claims.json"), b"broken").unwrap();
    fs::set_permissions(
        store.root.join("claims.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s1"),
            100
        ),
        Err(Error::Corrupt)
    ));
}

#[test]
fn runtime_disable_is_published_before_persistent_write_failure() {
    let (_temp, store, runtime) = fixture();
    fs::write(store.root.join("preferences.json"), b"broken").unwrap();
    fs::set_permissions(
        store.root.join("preferences.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let error = store.set_guide(Some(&runtime), false).unwrap_err();
    assert!(matches!(error, SetGuideError::Persistence { .. }));
    assert!(!runtime.read().unwrap().unwrap().enabled);
    assert!(runtime.root.join("disabled").exists());
}

#[test]
fn runtime_without_private_local_directory_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(RuntimeState::open(&root.join("missing")).is_err());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(RuntimeState::open(&root).is_err());
}

#[test]
fn persisted_claims_contain_only_digests() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    store
        .claim(
            Some(&runtime),
            &snapshot,
            request(
                InviteKind::Failure,
                "/private/project:secret-command",
                "session",
            ),
            100,
        )
        .unwrap();
    let bytes = fs::read_to_string(store.root.join("claims.json")).unwrap();
    assert!(!bytes.contains("private/project"));
    assert!(!bytes.contains("secret-command"));
}

#[test]
fn failure_after_disable_marker_reports_effective_runtime_close() {
    let (_temp, store, runtime) = fixture();
    fs::set_permissions(
        runtime.root.join("current"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = store.set_guide(Some(&runtime), false).unwrap_err();
    assert!(matches!(
        error,
        SetGuideError::Runtime {
            disabled_published: true,
            ..
        }
    ));
}

#[test]
fn help_does_not_consume_failure_session_quota() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Help, "clue", "s"),
            100
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert!(matches!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s"),
            101
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
}

fn own_claim_helper(
    command: &mut std::process::Command,
    deadline: crate::process::job::JobDeadline,
) -> crate::process::child::CommandChild<'static> {
    use crate::process::child::{CleanupScope, ReapOwner};
    let permit = ReapOwner::global()
        .unwrap()
        .reserve()
        .expect("guide child capacity");
    let limit = deadline.instant().unwrap();
    crate::process::job::configure_isolated_worker(command, deadline);
    let child = command.spawn().expect("guide helper");
    permit.register(child, CleanupScope::OwnedGroup, limit)
}

fn finish_claim_helper(
    child: &mut crate::process::child::CommandChild<'_>,
    deadline: std::time::Instant,
) -> std::io::Result<std::process::ExitStatus> {
    loop {
        if std::time::Instant::now() >= deadline {
            child.shorten_deadline(deadline);
            child.cleanup()?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "guide helper deadline expired",
            ));
        }
        if child.has_exited()? {
            return child
                .cleanup()?
                .ok_or_else(|| std::io::Error::other("guide helper not reaped"));
        }
        std::thread::sleep(
            crate::process::child::COMMAND_POLL_INTERVAL
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}

#[test]
fn stalled_claim_helper_is_killed_and_reaped_at_the_parent_deadline() {
    use crate::process::child::{child_has_exited, COMMAND_POLL_INTERVAL};
    use crate::process::job::JobDeadline;
    use std::time::{Duration, Instant};
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "config::guide::tests::process_claim_helper"])
        .env("COSH_GUIDE_TEST_STALL", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let permit = crate::process::child::ReapOwner::global()
        .unwrap()
        .reserve()
        .unwrap();
    let worker_deadline = JobDeadline::after(Duration::from_secs(3)).unwrap();
    crate::process::job::configure_isolated_worker(&mut command, worker_deadline);
    let child = command.spawn().unwrap();
    let pid = child.id();
    let mut guard = permit.register(
        child,
        crate::process::child::CleanupScope::OwnedGroup,
        worker_deadline.instant().unwrap(),
    );
    let result = finish_claim_helper(&mut guard, Instant::now() + Duration::from_millis(50));
    let reap_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child_has_exited(pid).is_err_and(|error| error.raw_os_error() == Some(nix::libc::ECHILD))
        {
            break;
        }
        assert!(
            Instant::now() < reap_deadline,
            "guide helper was not reaped"
        );
        std::thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn concurrent_processes_claim_exactly_once() {
    let (_temp, store, runtime) = fixture();
    let executable = std::env::current_exe().unwrap();
    let deadline =
        crate::process::job::JobDeadline::after(std::time::Duration::from_secs(3)).unwrap();
    let limit = deadline.instant().unwrap();
    let mut children = Vec::new();
    for id in 0..4 {
        children.push(own_claim_helper(
            std::process::Command::new(&executable)
                .args(["--exact", "config::guide::tests::process_claim_helper"])
                .env("COSH_GUIDE_TEST_ROOT", &store.root)
                .env("COSH_GUIDE_TEST_RUNTIME", runtime.root.parent().unwrap())
                .env("COSH_GUIDE_TEST_RESULT", id.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null()),
            deadline,
        ));
    }
    for mut child in children {
        assert!(finish_claim_helper(&mut child, limit).unwrap().success());
    }
    let shown = (0..4)
        .filter(|id| fs::read(store.root.join(format!("result-{id}"))).unwrap() == b"1")
        .count();
    assert_eq!(shown, 1);
}

#[test]
fn process_claim_helper() {
    if std::env::var_os("COSH_GUIDE_TEST_STALL").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(1));
        return;
    }
    let Some(root) = std::env::var_os("COSH_GUIDE_TEST_ROOT") else {
        return;
    };
    let store = GuideStore::open(PathBuf::from(root)).unwrap();
    let runtime = RuntimeState::open(&PathBuf::from(
        std::env::var_os("COSH_GUIDE_TEST_RUNTIME").unwrap(),
    ))
    .unwrap();
    let result = store.snapshot(Some(&runtime)).and_then(|snapshot| {
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "shared", "shared-session"),
            100,
        )
    });
    let shown = match result {
        Ok(ClaimDecision::Claimed(_)) => true,
        Ok(ClaimDecision::Suppressed) | Err(Error::State(private_state::Error::LockBusy)) => false,
        other => panic!("unexpected claim result: {other:?}"),
    };
    let id = std::env::var("COSH_GUIDE_TEST_RESULT").unwrap();
    private_state::atomic_write(
        &store.root.join(format!("result-{id}")),
        if shown { b"1" } else { b"0" },
        1,
    )
    .unwrap();
}

#[test]
fn disable_marker_survives_missing_generation_and_reinitialization() {
    let (_temp, store, runtime) = fixture();
    private_state::atomic_write(&runtime.root.join("disabled"), b"", 0).unwrap();
    private_state::remove_owner_file(&runtime.root.join("current")).unwrap();
    store.initialize_runtime(&runtime).unwrap();
    assert!(!runtime.read().unwrap().unwrap().enabled);
    assert!(runtime.root.join("disabled").exists());
}

#[test]
fn persistent_lock_busy_after_off_keeps_runtime_disabled() {
    let (_temp, store, runtime) = fixture();
    let _lock = private_state::LockGuard::try_acquire(&store.root.join("state.lock")).unwrap();
    assert!(matches!(
        store.set_guide(Some(&runtime), false),
        Err(SetGuideError::Persistence {
            runtime: Some(RuntimeSnapshot { enabled: false, .. }),
            error: Error::State(private_state::Error::LockBusy)
        })
    ));
    assert!(!runtime.read().unwrap().unwrap().enabled);
    assert!(store.read_preferences().unwrap().is_none());
}

#[test]
fn unavailable_runtime_still_persists_preference_without_claiming_runtime_close() {
    let (_temp, store, _runtime) = fixture();
    let outcome = store.set_guide(None, false).unwrap();
    assert!(!outcome.preferences.guide_enabled);
    assert!(outcome.runtime.is_none());
    assert!(!store.load_preferences().unwrap().unwrap().guide_enabled);
    let outcome = store.set_guide(None, true).unwrap();
    assert!(outcome.preferences.guide_enabled);
    assert!(outcome.runtime.is_none());
}

#[test]
fn login_claims_without_hook_runtime_keep_persistent_off_on_quota() {
    let (_temp, store, _runtime) = fixture();
    let snapshot = store.snapshot(None).unwrap();
    let decision = store
        .claim(
            None,
            &snapshot,
            request(InviteKind::LoginEntry, "", "a"),
            100,
        )
        .unwrap();
    let ClaimDecision::Claimed(permit) = decision else {
        panic!("first claim")
    };
    assert!(store.validate_claim(None, &permit).unwrap());
    store.set_guide(None, false).unwrap();
    assert!(!store.validate_claim(None, &permit).unwrap());
    store.set_guide(None, true).unwrap();
    assert!(!store.validate_claim(None, &permit).unwrap());
    let snapshot = store.snapshot(None).unwrap();
    assert_eq!(
        store.claim(
            None,
            &snapshot,
            request(InviteKind::LoginEntry, "", "b"),
            101
        ),
        Ok(ClaimDecision::Suppressed)
    );
}

#[test]
fn permit_generation_source_cannot_downgrade_when_runtime_disappears() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let ClaimDecision::Claimed(permit) = store
        .claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "s"),
            100,
        )
        .unwrap()
    else {
        panic!("first claim")
    };
    assert!(matches!(
        store.validate_claim(None, &permit),
        Err(Error::RuntimeUnavailable)
    ));
}

#[test]
fn persistent_only_off_on_invalidates_runtime_permits_and_snapshots() {
    let (_temp, store, runtime) = fixture();
    let snapshot = store.snapshot(Some(&runtime)).unwrap();
    let ClaimDecision::Claimed(permit) = store
        .claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "disk", "a"),
            100,
        )
        .unwrap()
    else {
        panic!("first claim")
    };
    store.set_guide(None, false).unwrap();
    store.set_guide(None, true).unwrap();
    assert!(!store.validate_claim(Some(&runtime), &permit).unwrap());
    assert_eq!(
        store.claim(
            Some(&runtime),
            &snapshot,
            request(InviteKind::Failure, "memory", "b"),
            101
        ),
        Ok(ClaimDecision::Stale)
    );
}

#[test]
fn expired_help_claims_do_not_block_other_invitation_kinds() {
    let (_temp, store, _runtime) = fixture();
    let snapshot = store.snapshot(None).unwrap();
    let claims = Claims {
        version: 1,
        entries: (0..MAX_CLAIMS)
            .map(|index| ClaimEntry {
                kind: InviteKind::Help,
                host: digest(&["host"]),
                problem: digest(&["host", "clue"]),
                scope: digest(&["host", "s"]),
                shown_at_ms: index as u64 * 31_000,
            })
            .collect(),
    };
    private_state::atomic_write(
        &store.root.join("claims.json"),
        &serde_json::to_vec(&claims).unwrap(),
        MAX_BYTES,
    )
    .unwrap();
    let now_ms = MAX_CLAIMS as u64 * 31_000;
    assert!(matches!(
        store.claim(
            None,
            &snapshot,
            request(InviteKind::Failure, "disk", "s"),
            now_ms
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert!(matches!(
        store.claim(
            None,
            &snapshot,
            request(InviteKind::LoginEntry, "", "s"),
            now_ms
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert_eq!(store.read_claims().unwrap().entries.len(), 2);
}

#[test]
fn runtime_operations_stay_on_validated_root_after_path_replacement() {
    let (_temp, store, runtime) = fixture();
    let original = runtime.root.with_file_name("original-runtime");
    fs::rename(&runtime.root, &original).unwrap();
    private_state::ensure_directory(&runtime.root).unwrap();
    store.set_guide(Some(&runtime), false).unwrap();
    assert!(original.join("disabled").exists());
    assert!(!runtime.root.join("disabled").exists());
    assert!(!runtime.root.join("current").exists());
    assert!(!runtime.read().unwrap().unwrap().enabled);
}

#[test]
fn stale_disabled_snapshot_does_not_override_reenabled_preferences() {
    let (_temp, store, _runtime) = fixture();
    store.set_guide(None, false).unwrap();
    let disabled = store.snapshot(None).unwrap();
    store.set_guide(None, true).unwrap();
    let lock = store.lock().unwrap();
    assert!(matches!(
        store.claim(
            None,
            &disabled,
            request(InviteKind::Failure, "disk", "s"),
            100
        ),
        Err(Error::State(private_state::Error::LockBusy))
    ));
    drop(lock);
    assert_eq!(
        store.claim(
            None,
            &disabled,
            request(InviteKind::Failure, "disk", "s"),
            100
        ),
        Ok(ClaimDecision::Stale)
    );
    assert!(!store.root.join("claims.json").exists());
}

#[test]
fn user_state_opener_only_opens_shared_paths_without_initializing() {
    let (_temp, _store, runtime) = uninitialized_fixture();
    let base = runtime.directory().parent().unwrap();
    let cosh_dir = base.join("cosh");
    let (store, opened_runtime) = open_user_state_at(Some(&cosh_dir), Some(base)).unwrap();
    assert_eq!(store.root, cosh_dir.join("guide"));
    assert_eq!(opened_runtime.directory(), runtime.directory());
    for file in ["preferences.json", "claims.json", "state.lock"] {
        assert!(!store.root.join(file).exists());
    }
    for file in ["current", "disabled", "runtime.lock"] {
        assert!(!opened_runtime.directory().join(file).exists());
    }
}

#[test]
fn user_state_opener_missing_runtime_does_not_create_fallback_or_store() {
    let (_temp, _store, runtime) = uninitialized_fixture();
    let base = runtime.directory().parent().unwrap();
    let cosh_dir = base.join("cosh");
    assert!(matches!(
        open_user_state_at(Some(&cosh_dir), None),
        Err(Error::RuntimeUnavailable)
    ));
    assert!(!cosh_dir.exists());
    let missing = base.join("missing-runtime");
    assert!(matches!(
        open_user_state_at(Some(&cosh_dir), Some(&missing)),
        Err(Error::State(private_state::Error::Io(
            std::io::ErrorKind::NotFound
        )))
    ));
    assert!(!cosh_dir.exists());
    assert!(!missing.exists());
}

#[test]
fn user_store_opener_keeps_banner_independent_of_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let store = open_user_store_at(Some(&root.join("cosh"))).unwrap();
    assert_eq!(store.root, root.join("cosh/guide"));
    assert!(!store.root.join("preferences.json").exists());
    assert!(!store.root.join("claims.json").exists());
    assert!(!store.root.join("state.lock").exists());
    assert!(!root.join("cosh-guide").exists());
    assert!(matches!(
        open_user_store_at(None),
        Err(Error::State(private_state::Error::Io(
            std::io::ErrorKind::NotFound
        )))
    ));
}

#[test]
fn failure_context_references_keep_exact_events_and_expire_without_fallback() {
    use crate::insight::guidance::{BashEvent, CommandIdentity, ExecutionScope};
    let (_temp, store, _runtime) = fixture();
    let event = BashEvent {
        expression: "cargo test --test 'my suite'".into(),
        status: 101,
        cwd: "/original project".into(),
        observed_at_ms: 1000,
        scope: ExecutionScope::ObservedCommandFragment,
        identity: CommandIdentity::ExternalUnshadowed,
    };
    let first = uuid::Uuid::new_v4().simple().to_string();
    store.save_failure_context(&first, &event, 1000).unwrap();
    let mut second_event = event.clone();
    second_event.cwd = "/other terminal".into();
    let second = uuid::Uuid::new_v4().simple().to_string();
    store
        .save_failure_context(&second, &second_event, 1001)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(store.load_failure_context(&first, 1002).unwrap(), event);
    assert_eq!(
        store.load_failure_context(&second, 1002).unwrap(),
        second_event
    );
    assert!(store.load_failure_context(&first, 3_601_001).is_err());
    assert!(store
        .load_failure_context("../preferences.json", 1002)
        .is_err());
    assert!(store.load_failure_context(&"0".repeat(32), 1002).is_err());
    assert!(store.load_failure_context(&first, 999).is_err());
}

#[test]
fn failure_context_capacity_is_bounded_and_busy_or_unsafe_state_fails_quietly() {
    use crate::insight::guidance::{BashEvent, CommandIdentity, ExecutionScope};
    let (_temp, store, _runtime) = fixture();
    let event = BashEvent {
        expression: "cargo build".into(),
        status: 101,
        cwd: "/project".into(),
        observed_at_ms: 1000,
        scope: ExecutionScope::ObservedCommandFragment,
        identity: CommandIdentity::ExternalUnshadowed,
    };
    let first = uuid::Uuid::new_v4().simple().to_string();
    store.save_failure_context(&first, &event, 1000).unwrap();
    for _ in 0..16 {
        store
            .save_failure_context(&uuid::Uuid::new_v4().simple().to_string(), &event, 1000)
            .unwrap();
    }
    assert!(store.load_failure_context(&first, 1000).is_err());
    let lease = store.lock().unwrap();
    assert!(store.save_failure_context(&first, &event, 1000).is_err());
    drop(lease);
    let path = store.root.join("failure-contexts.json");
    assert!(fs::metadata(&path).unwrap().len() < 128 * 1024);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.save_failure_context(&first, &event, 1000).is_err());
}
