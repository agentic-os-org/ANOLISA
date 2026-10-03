use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

fn snapshot(
    session: &str,
    generation: u64,
    marker_sequence: u64,
    path: &str,
) -> ShellEnvironmentSnapshot {
    ShellEnvironmentSnapshot {
        session_id: session.to_string(),
        marker_sequence,
        generation,
        path: path.to_string(),
    }
}

fn resolve_block_with_diagnostic(
    catalog: &[&str],
    command: &str,
    output: Option<&str>,
) -> Option<String> {
    let service = ShellRewriteCatalogService::default();
    service
        .cache
        .publish_for_test("session", 1, catalog.iter().copied());
    let block = CommandBlock {
        id: "command-1".to_string(),
        session_id: "session".to_string(),
        command: command.to_string(),
        origin: crate::types::CommandOrigin::UserInteractive,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 127,
        status: crate::types::CommandStatus::Failed,
        output: crate::types::OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: Some(1),
        audit_identity: None,
    };

    resolve_shell_rewrite(
        &service.cache,
        &block.session_id,
        block.shell_environment_generation,
        &block.command,
        output,
        &FixedClock,
    )
}

fn resolve_with_matching_diagnostic(
    cache: &ReadyCatalogCache,
    session_id: &str,
    generation: Option<u64>,
    command: &str,
    clock: &dyn DeadlineClock,
) -> Option<String> {
    let program = command.split(' ').next().unwrap_or_default();
    let diagnostic = format!("bash: {program}: command not found");
    resolve_shell_rewrite(
        cache,
        session_id,
        generation,
        command,
        Some(&diagnostic),
        clock,
    )
}

#[test]
fn resolver_accepts_supported_shell_command_not_found_diagnostics() {
    for diagnostic in [
        "bash: grpe: command not found\n",
        "zsh: command not found: grpe\n",
        "sh: grpe: not found\n",
        "sh: 3: grpe: not found\n",
        "sh: line 3: grpe: not found\n",
    ] {
        assert_eq!(
            resolve_block_with_diagnostic(&["grep"], "grpe file", Some(diagnostic)),
            Some("grep file".to_string()),
            "{diagnostic:?}"
        );
    }
}

#[test]
fn resolver_requires_a_parseable_supported_diagnostic() {
    for diagnostic in [
        None,
        Some(""),
        Some("command not found: grpe\n"),
        Some("bash: grpe: commande introuvable\n"),
        Some("fish: grpe: command not found\n"),
        Some("custom-handler: grpe: not found\n"),
    ] {
        assert_eq!(
            resolve_block_with_diagnostic(&["grep"], "grpe", diagnostic),
            None,
            "{diagnostic:?}"
        );
    }
}

#[test]
fn resolver_rejects_unsafe_ambiguous_or_inner_missing_tokens() {
    for diagnostic in [
        "bash: grpe;rm: command not found\n",
        "bash: grpe: command not found\nzsh: command not found: inner\n",
        "bash: inner: command not found\n",
    ] {
        assert_eq!(
            resolve_block_with_diagnostic(&["grep"], "grpe", Some(diagnostic)),
            None,
            "{diagnostic:?}"
        );
    }
}

#[test]
fn resolver_accepts_repeated_identical_missing_token() {
    assert_eq!(
        resolve_block_with_diagnostic(
            &["grep"],
            "grpe",
            Some("bash: grpe: command not found\nzsh: command not found: grpe\n"),
        ),
        Some("grep".to_string())
    );
}

#[test]
fn resolver_rejects_unbounded_diagnostic_tail() {
    let oversized_bytes = format!(
        "{}\nbash: grpe: command not found\n",
        "x".repeat(DIAGNOSTIC_TAIL_MAX_BYTES)
    );
    let oversized_lines = format!(
        "{}bash: grpe: command not found\n",
        "noise\n".repeat(DIAGNOSTIC_TAIL_MAX_LINES)
    );

    for diagnostic in [&oversized_bytes, &oversized_lines] {
        assert_eq!(
            resolve_block_with_diagnostic(&["grep"], "grpe", Some(diagnostic)),
            None
        );
    }
}

#[test]
fn resolver_is_silent_when_original_argv_zero_is_in_catalog() {
    assert_eq!(
        resolve_block_with_diagnostic(
            &["grep", "grpe"],
            "grpe",
            Some("bash: grpe: command not found\n"),
        ),
        None
    );
}

#[test]
fn resolver_rewrites_only_argv_zero_and_preserves_suffix() {
    let cache = ReadyCatalogCache::default();
    cache.publish_for_test("session", 1, ["grep"]);
    let clock = FixedClock;

    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), "grpe foo.txt", &clock,),
        Some("grep foo.txt".to_string())
    );
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), "grpe --color  foo", &clock,),
        Some("grep --color  foo".to_string())
    );
}

#[test]
fn resolver_rejects_unsafe_or_indirect_commands() {
    let cache = ReadyCatalogCache::default();
    cache.publish_for_test("session", 1, ["grep", "sudo"]);
    let clock = FixedClock;

    for command in [
        "sudo grpe",
        "ssh host grpe",
        "FOO=bar grpe",
        "grpe x | head",
        "grpe\tx",
        "grpe\nx",
        "grpe;head",
        "grpe > out",
        "'grpe' x",
        "grpe $HOME",
    ] {
        assert_eq!(
            resolve_with_matching_diagnostic(&cache, "session", Some(1), command, &clock),
            None,
            "{command:?}"
        );
    }
}

#[test]
fn resolver_requires_matching_ready_generation_and_unique_distance_one_candidate() {
    let cache = ReadyCatalogCache::default();
    cache.publish_for_test("session", 1, ["foa", "fob"]);
    let clock = FixedClock;

    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), "foo", &clock),
        None
    );
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(2), "foo", &clock),
        None
    );
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", None, "foo", &clock),
        None
    );
    cache.publish_for_test("session", 3, ["sudo"]);
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(3), "sudp arg", &clock),
        None
    );
}

#[test]
fn resolver_never_starts_or_invokes_filesystem_scanner() {
    let scanner = Arc::new(CountingScanner::default());
    let service = ShellRewriteCatalogService::with_scanner(scanner.clone());

    assert_eq!(
        resolve_with_matching_diagnostic(&service.cache(), "session", Some(1), "grpe", &FixedClock,),
        None
    );
    assert_eq!(scanner.scans.load(Ordering::SeqCst), 0);
}

#[test]
fn ready_cache_is_bounded_and_expired_generation_is_silent() {
    let cache = ReadyCatalogCache::default();
    for generation in 1..=(READY_CACHE_ENTRY_LIMIT as u64 + 1) {
        cache.publish_for_test("session", generation, [format!("tool{generation}")]);
    }

    assert!(!cache.contains("session", 1));
    assert!(cache.contains("session", READY_CACHE_ENTRY_LIMIT as u64 + 1));
}

#[derive(Default)]
struct FixedClock;

impl DeadlineClock for FixedClock {
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
}

struct AdvancingClock {
    calls: AtomicUsize,
}

impl DeadlineClock for AdvancingClock {
    fn elapsed(&self) -> Duration {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Duration::from_millis((call * 11) as u64)
    }
}

#[test]
fn resolver_stops_when_injected_clock_crosses_deadline() {
    let cache = ReadyCatalogCache::default();
    let names = (0..128).map(|index| format!("tool{index}"));
    cache.publish_for_test("session", 1, names);
    let clock = AdvancingClock {
        calls: AtomicUsize::new(0),
    };

    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), "grpe", &clock),
        None
    );
}

struct FinalCheckClock {
    calls: AtomicUsize,
}

impl DeadlineClock for FinalCheckClock {
    fn elapsed(&self) -> Duration {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= 3 {
            Duration::from_millis(11)
        } else {
            Duration::ZERO
        }
    }
}

#[test]
fn resolver_checks_deadline_after_final_catalog_chunk() {
    let cache = ReadyCatalogCache::default();
    cache.publish_for_test("session", 1, ["grep"]);
    let clock = FinalCheckClock {
        calls: AtomicUsize::new(0),
    };

    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), "grpe", &clock),
        None
    );
}

#[test]
fn resolver_fails_silent_immediately_when_cache_lock_is_busy() {
    let cache = ReadyCatalogCache::default();
    cache.publish_for_test("session", 1, ["grep"]);
    let guard = cache.state.lock().expect("hold cache lock");
    let resolver_cache = cache.clone();
    let (sender, receiver) = mpsc::channel();
    let resolver = std::thread::spawn(move || {
        sender
            .send(resolve_shell_rewrite(
                &resolver_cache,
                "session",
                Some(1),
                "grpe",
                Some("bash: grpe: command not found"),
                &FixedClock,
            ))
            .expect("resolver result");
    });

    let result = receiver.recv_timeout(Duration::from_millis(20));
    drop(guard);
    resolver.join().expect("resolver thread");

    assert_eq!(result.expect("busy cache must not block"), None);
}

#[test]
fn filesystem_scanner_keeps_only_safe_executables_and_builtins() {
    let root = std::env::temp_dir().join(format!("cosh-shell-rewrite-scan-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scan dir");
    let executable = root.join("grep");
    fs::write(&executable, "#!/bin/sh\n").expect("executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("chmod");
    fs::write(root.join("not-executable"), "plain").expect("plain file");
    let no_execute_bits = root.join("no-execute-bits");
    fs::write(&no_execute_bits, "#!/bin/sh\n").expect("non-executable file");
    fs::set_permissions(&no_execute_bits, fs::Permissions::from_mode(0o000))
        .expect("remove execute bits");
    let unsafe_name = root.join("bad\nname");
    fs::write(&unsafe_name, "#!/bin/sh\n").expect("unsafe executable");
    fs::set_permissions(&unsafe_name, fs::Permissions::from_mode(0o700)).expect("chmod unsafe");
    let scanner = FilesystemCatalogScanner;

    let names = scanner
        .scan(&snapshot("session", 1, 1, &root.to_string_lossy()))
        .expect("scan");

    assert!(names.iter().any(|name| name == "grep"));
    assert!(names.iter().any(|name| name == "cd"));
    assert!(!names.iter().any(|name| name == "not-executable"));
    assert!(!names.iter().any(|name| name == "no-execute-bits"));
    assert!(!names.iter().any(|name| name.contains('\n')));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn filesystem_scanner_caps_directories_and_examined_names() {
    let root =
        std::env::temp_dir().join(format!("cosh-shell-rewrite-bounds-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("bounds root");
    let mut directories = Vec::new();
    for index in 0..33 {
        let directory = root.join(format!("d{index}"));
        fs::create_dir(&directory).expect("path directory");
        let executable = directory.join(format!("tool{index}"));
        fs::write(&executable, "#!/bin/sh\n").expect("tool");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("chmod");
        directories.push(directory);
    }
    let many_names = directories[0].clone();
    for index in 0..8192 {
        let executable = many_names.join(format!("name{index}"));
        fs::write(&executable, "").expect("bounded name");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    let path = directories
        .iter()
        .map(|directory| directory.to_string_lossy())
        .collect::<Vec<_>>()
        .join(":");

    let names = FilesystemCatalogScanner
        .scan(&snapshot("session", 1, 1, &path))
        .expect("bounded scan");

    assert!(!names.iter().any(|name| name == "tool32"));
    assert!(
        names
            .iter()
            .filter(|name| name.starts_with("name") || name.starts_with("tool"))
            .count()
            <= PATH_NAME_LIMIT
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn filesystem_scanner_fails_silent_for_unreadable_path_entry() {
    let missing =
        std::env::temp_dir().join(format!("cosh-shell-rewrite-missing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&missing);

    assert!(FilesystemCatalogScanner
        .scan(&snapshot("session", 1, 1, &missing.to_string_lossy()))
        .is_err());
}

#[test]
fn filesystem_scanner_skips_missing_path_entry_when_valid_directory_exists() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-rewrite-partial-path-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scan dir");
    let executable = root.join("grep");
    fs::write(&executable, "#!/bin/sh\n").expect("executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("chmod");
    let missing = root.join("missing");
    let path = format!("{}:{}", missing.to_string_lossy(), root.to_string_lossy());

    let names = FilesystemCatalogScanner
        .scan(&snapshot("session", 1, 1, &path))
        .expect("scan valid directory");

    assert!(names.iter().any(|name| name == "grep"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolver_enforces_basename_and_rewrite_byte_caps() {
    let cache = ReadyCatalogCache::default();
    let candidate = format!("{}b", "a".repeat(127));
    let typo = format!("{}c", "a".repeat(127));
    cache.publish_for_test("session", 1, [candidate.clone()]);
    let clock = FixedClock;
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(1), &typo, &clock),
        Some(candidate)
    );

    let oversized_candidate = format!("{}b", "a".repeat(128));
    let oversized_typo = format!("{}c", "a".repeat(128));
    cache.publish_for_test("session", 2, [oversized_candidate]);
    assert_eq!(
        resolve_with_matching_diagnostic(&cache, "session", Some(2), &oversized_typo, &clock,),
        None
    );

    let suffix = format!(" {}", "x".repeat(REWRITE_MAX_BYTES));
    cache.publish_for_test("session", 3, ["grep"]);
    assert_eq!(
        resolve_with_matching_diagnostic(
            &cache,
            "session",
            Some(3),
            &format!("grpe{suffix}"),
            &clock,
        ),
        None
    );
}

struct ControlledScanner {
    started: mpsc::Sender<u64>,
    release_first: Mutex<mpsc::Receiver<()>>,
    scans: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl CatalogScanner for ControlledScanner {
    fn scan(&self, snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.scans.fetch_add(1, Ordering::SeqCst);
        self.started.send(snapshot.generation).expect("started");
        if snapshot.generation == 1 {
            self.release_first.lock().unwrap().recv().expect("release");
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(vec![if snapshot.generation == 1 {
            "grep-old".to_string()
        } else {
            "grep".to_string()
        }])
    }
}

#[test]
fn single_worker_discards_in_flight_stale_result_and_scans_latest_generation() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let scanner = Arc::new(ControlledScanner {
        started: started_tx,
        release_first: Mutex::new(release_rx),
        scans: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
    });
    let mut service = ShellRewriteCatalogService::with_scanner(scanner.clone());
    let publisher = service.start_worker();

    publisher.publish(snapshot("session", 1, 1, "/one"));
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
    publisher.publish(snapshot("session", 2, 2, "/two"));
    release_tx.send(()).expect("release first scan");
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
    service.wait_until_ready("session", 2, Duration::from_secs(1));

    assert!(!service.cache().contains("session", 1));
    assert!(service.cache().contains("session", 2));
    assert_eq!(scanner.max_active.load(Ordering::SeqCst), 1);
}

struct BlockingScanner {
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    returned: mpsc::Sender<()>,
}

impl CatalogScanner for BlockingScanner {
    fn scan(&self, _snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()> {
        self.started.send(()).expect("scan started");
        self.release.lock().unwrap().recv().expect("release scan");
        self.returned.send(()).expect("scan returned");
        Ok(vec!["grep".to_string()])
    }
}

#[test]
fn shutdown_does_not_wait_for_an_in_flight_filesystem_scan() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let scanner = Arc::new(BlockingScanner {
        started: started_tx,
        release: Mutex::new(release_rx),
        returned: returned_tx,
    });
    let mut service = ShellRewriteCatalogService::with_scanner(scanner);
    let cache = service.cache();
    let publisher = service.start_worker();
    publisher.publish(snapshot("session", 1, 1, "/slow"));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scan must start");
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = std::thread::spawn(move || {
        service.shutdown();
        shutdown_tx.send(()).expect("shutdown completed");
    });

    let shutdown_result = shutdown_rx.recv_timeout(Duration::from_secs(1));
    publisher.publish(snapshot("session", 2, 2, "/ignored"));
    release_tx.send(()).expect("release scan");
    returned_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scan must return");
    shutdown_thread.join().expect("shutdown thread");

    assert!(shutdown_result.is_ok(), "shutdown waited for the scanner");
    assert!(!cache.contains("session", 1));
    assert!(!cache.contains("session", 2));
}

#[test]
fn shutdown_waits_for_an_in_progress_publication_before_returning() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let scanner = Arc::new(BlockingScanner {
        started: started_tx,
        release: Mutex::new(release_rx),
        returned: returned_tx,
    });
    let mut service = ShellRewriteCatalogService::with_scanner(scanner);
    let cache = service.cache();
    let publisher = service.start_worker();
    publisher.publish(snapshot("session", 1, 1, "/slow"));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scan must start");
    let cache_guard = cache.state.lock().expect("hold cache publication");
    release_tx.send(()).expect("release scan");
    returned_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scan must return");
    let publication_started = Instant::now();
    loop {
        match publisher.publication_gate.try_lock() {
            Ok(publication) => drop(publication),
            Err(std::sync::TryLockError::WouldBlock) => break,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("publication gate poisoned")
            }
        }
        assert!(
            publication_started.elapsed() < Duration::from_secs(1),
            "worker did not enter the publication gate"
        );
        std::thread::yield_now();
    }
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = std::thread::spawn(move || {
        service.shutdown();
        shutdown_tx.send(()).expect("shutdown completed");
    });

    let returned_before_publication_finished =
        shutdown_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    drop(cache_guard);
    if !returned_before_publication_finished {
        shutdown_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown must finish after publication");
    }
    shutdown_thread.join().expect("shutdown thread");

    assert!(
        !returned_before_publication_finished,
        "shutdown returned while a cache publication was still in progress"
    );
}

#[test]
fn worker_reuses_ready_generation_without_rescanning() {
    let scanner = Arc::new(CountingScanner::default());
    let mut service = ShellRewriteCatalogService::with_scanner(scanner.clone());
    let first_publisher = service.start_worker();
    let publisher = service.start_worker();
    let same = snapshot("session", 7, 1, "/same");

    first_publisher.publish(same.clone());
    service.wait_until_ready("session", 7, Duration::from_secs(1));
    publisher.publish(same);
    std::thread::sleep(Duration::from_millis(20));

    assert_eq!(scanner.scans.load(Ordering::SeqCst), 1);
}

#[test]
fn worker_caches_failed_generation_without_repeated_scans() {
    let scanner = Arc::new(FailingScanner::default());
    let mut service = ShellRewriteCatalogService::with_scanner(scanner.clone());
    let publisher = service.start_worker();
    let failed = snapshot("session", 9, 1, "/missing");

    publisher.publish(failed.clone());
    service.wait_until_processed("session", 9, Duration::from_secs(1));
    publisher.publish(failed);
    std::thread::sleep(Duration::from_millis(20));

    assert!(!service.cache().contains("session", 9));
    assert_eq!(scanner.scans.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_disables_surviving_publishers_after_a_ready_scan() {
    let scanner = Arc::new(CountingScanner::default());
    let mut service = ShellRewriteCatalogService::with_scanner(scanner.clone());
    let publisher = service.start_worker();

    publisher.publish(snapshot("session", 1, 1, "/one"));
    service.wait_until_ready("session", 1, Duration::from_secs(1));
    service.shutdown();
    publisher.publish(snapshot("session", 2, 2, "/two"));
    std::thread::sleep(Duration::from_millis(20));

    assert_eq!(scanner.scans.load(Ordering::SeqCst), 1);
    assert!(!service.cache().contains("session", 2));
}

#[derive(Default)]
struct CountingScanner {
    scans: AtomicUsize,
}

impl CatalogScanner for CountingScanner {
    fn scan(&self, _snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        Ok(vec!["grep".to_string()])
    }
}

#[derive(Default)]
struct FailingScanner {
    scans: AtomicUsize,
}

impl CatalogScanner for FailingScanner {
    fn scan(&self, _snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        Err(())
    }
}
