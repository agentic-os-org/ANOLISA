//! Persisted consent changes govern subsequent consolidation across clients.

use agent_memory::tools::memory_observe::memory_observe;
use agent_memory::tools::memory_sovereignty::memory_consent;
use agent_memory::{config::AppConfig, service::MemoryService};

fn config(tmp: &tempfile::TempDir) -> AppConfig {
    let mut config = AppConfig::default();
    config.global.user_id = "consent-probe".into();
    config.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    config.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    config.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    config.memory.index.enabled = false;
    config.memory.git.enabled = false;
    config.memory.cgroup.enabled = false;
    config.memory.audit.journald = false;
    config.memory.consolidation.enabled = true;
    config.memory.consolidation.min_tool_calls = 1;
    config.memory.consolidation.episodic_enabled = false;
    config.memory.consolidation.conflict_detection = false;
    config.memory.consolidation.incremental_interval = 0;
    config
}

fn seed(svc: &MemoryService) {
    assert!(svc.session.is_some());
    for path in ["notes/one.md", "notes/two.md", "notes/three.md"] {
        svc.write(path, "A persistent decision for this project", false)
            .unwrap();
    }
}

#[test]
fn revoked_consolidation_is_observed_by_existing_service() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    let peer = MemoryService::new(config(&tmp)).unwrap();
    assert_eq!(svc.mount.root, peer.mount.root);
    memory_consent(&svc, Some("allow"), Some("consolidation")).unwrap();
    seed(&svc);
    assert!(svc.consolidate() > 0, "healthy initial control must write");
    memory_consent(&peer, Some("deny"), Some("consolidation")).unwrap();
    let before = std::fs::read(svc.mount.root.join("facts/facts.jsonl")).unwrap();
    assert_eq!(
        svc.consolidate(),
        0,
        "other client's denial must stop further facts"
    );
    assert_eq!(
        std::fs::read(svc.mount.root.join("facts/facts.jsonl")).unwrap(),
        before
    );
}

#[test]
fn restored_consolidation_is_observed_by_existing_service() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    let peer = MemoryService::new(config(&tmp)).unwrap();
    memory_consent(&svc, Some("deny"), Some("consolidation")).unwrap();
    seed(&svc);
    assert_eq!(svc.consolidate(), 0);
    memory_consent(&peer, Some("allow"), Some("consolidation")).unwrap();
    assert!(
        svc.consolidate() > 0,
        "other client's grant must restore consolidation"
    );
}

#[test]
fn invalid_consent_does_not_authorize_consolidation() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::write(
        svc.mount.meta_dir.join("consent.toml"),
        "auto_consolidation = nope",
    )
    .unwrap();
    assert_eq!(
        svc.consolidate(),
        0,
        "invalid settings must not mean consent"
    );
    assert!(!svc.mount.root.join("facts").exists());
}

#[test]
fn unreadable_consent_does_not_authorize_consolidation() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::create_dir(svc.mount.meta_dir.join("consent.toml")).unwrap();
    assert_eq!(svc.consolidate(), 0, "read failure must not mean consent");
    assert!(!svc.mount.root.join("facts").exists());
}

#[test]
fn absent_consent_retains_default_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    assert!(svc.consolidate() > 0);
}

#[test]
fn removed_settings_restore_the_missing_file_default() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    memory_consent(&svc, Some("deny"), Some("all")).unwrap();
    seed(&svc);
    assert_eq!(svc.consolidate(), 0);
    std::fs::remove_file(svc.mount.meta_dir.join("consent.toml")).unwrap();
    assert!(svc.consolidate() > 0);
}

#[test]
fn capture_only_denial_does_not_disable_consolidation() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    memory_consent(&svc, Some("deny"), Some("capture")).unwrap();
    seed(&svc);
    assert!(svc.consolidate() > 0);
}

#[test]
fn malformed_settings_do_not_block_explicit_observations() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    std::fs::write(svc.mount.meta_dir.join("consent.toml"), "invalid settings").unwrap();
    let path = memory_observe(
        &svc,
        "Explicit user-directed memory",
        None,
        None,
        &svc.config.memory,
    )
    .unwrap();
    assert!(
        svc.read(&path)
            .unwrap()
            .contains("Explicit user-directed memory")
    );
}

#[test]
fn repaired_settings_restore_consolidation() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    let peer = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::write(svc.mount.meta_dir.join("consent.toml"), "invalid settings").unwrap();
    assert_eq!(svc.consolidate(), 0);
    memory_consent(&peer, Some("allow"), Some("consolidation")).unwrap();
    assert!(svc.consolidate() > 0);
}

#[test]
fn empty_toml_retains_default_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::write(svc.mount.meta_dir.join("consent.toml"), "").unwrap();
    assert!(svc.consolidate() > 0);
}

#[test]
fn unknown_consent_keys_preserve_existing_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::write(
        svc.mount.meta_dir.join("consent.toml"),
        "auto_consolidaton = \"deny\"\n",
    )
    .unwrap();
    assert!(svc.consolidate() > 0);
}

#[test]
fn symlinked_consent_is_rejected_without_touching_target() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    let target = tmp.path().join("external-consent.toml");
    let original = "auto_consolidation = \"allow\"\n";
    std::fs::write(&target, original).unwrap();
    std::os::unix::fs::symlink(&target, svc.mount.meta_dir.join("consent.toml")).unwrap();
    assert_eq!(svc.consolidate(), 0);
    assert!(!svc.mount.root.join("facts").exists());
    assert_eq!(std::fs::read_to_string(target).unwrap(), original);
}

#[test]
fn symlinked_settings_directory_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    let original = svc.mount.root.join(".anolisa-original");
    std::fs::rename(&svc.mount.meta_dir, &original).unwrap();
    std::fs::write(
        original.join("consent.toml"),
        "auto_consolidation = \"allow\"\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&original, &svc.mount.meta_dir).unwrap();
    assert_eq!(svc.consolidate(), 0);
    assert!(!svc.mount.root.join("facts").exists());
}

#[test]
fn invalid_consent_is_visible_with_default_log_filter() {
    const CASE: &str = "invalid_consent_is_visible_with_default_log_filter";
    if std::env::var("MEMORY_CONSENT_LOG_PROBE").as_deref() != Ok(CASE) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env_remove("RUST_LOG")
            .env("MEMORY_CONSENT_LOG_PROBE", CASE)
            .args(["--exact", CASE, "--nocapture"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("consent-log-probe-completed"));
        assert!(
            stderr.contains("ERROR") && stderr.contains("cannot read consent"),
            "operator must see unreadable settings with RUST_LOG unset: {stderr}"
        );
        assert!(!stderr.contains("private-consent-sentinel"));
        return;
    }
    assert!(std::env::var_os("RUST_LOG").is_none());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .without_time()
        .with_ansi(false)
        .init();
    let tmp = tempfile::tempdir().unwrap();
    let svc = MemoryService::new(config(&tmp)).unwrap();
    seed(&svc);
    std::fs::write(
        svc.mount.meta_dir.join("consent.toml"),
        "private-consent-sentinel = [",
    )
    .unwrap();
    assert_eq!(svc.consolidate(), 0);
    assert!(!svc.mount.root.join("facts").exists());
    println!("consent-log-probe-completed");
}
