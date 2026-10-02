//! FUSE integration tests for the read-time OS-adapter transform stage.
//!
//! These pin the issue #1450 contract: activation target selection happens
//! before transformation; only `SKILL.md` is adapted; the adapter is opt-in and
//! byte-transparent when disabled; `getattr` size and partial reads agree with
//! the full transformed bytes; source and snapshot bytes are never mutated by a
//! read; Current/Snapshot/Hidden all behave; flat and Hermes layouts share the
//! pipeline; and an open handle stays pinned to its selected target.

#![allow(clippy::too_many_arguments)]

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::os_adapter::{OsAdapterStage, OsTarget, TargetSelector};
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, ActiveTarget, InMemoryEventSink, SkillEventAction, SkillEventKind,
    SkillEventSink, serialize_event_jsonl,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::MountFixture;

// ─────────────────────────────────────────────────────────────────────────────
// Rule artifact + stage helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Representative bidirectional rules covering package-manager verbs, a
/// `-dev`/`-devel` package name, a service unit, and a filesystem path.
const RULES_YAML: &str = r#"
- ubuntu: "sudo apt-get install -y "
  alinux: "sudo dnf install -y "
  direction: bidirectional
  auto_apply: always
- ubuntu: "apt-get install -y "
  alinux: "dnf install -y "
  direction: bidirectional
  auto_apply: always
- ubuntu: "libssl-dev"
  alinux: "openssl-devel"
  direction: bidirectional
  auto_apply: always
- ubuntu: "apache2.service"
  alinux: "httpd.service"
  direction: bidirectional
  auto_apply: always
- ubuntu: "/etc/apt/sources.list.d/"
  alinux: "/etc/yum.repos.d/"
  direction: bidirectional
  auto_apply: always
"#;

/// SKILL.md body written in Ubuntu style. Contains no `<!-- @if -->` directives
/// and none of the compiler's heuristic triggers (pip/venv/npm), so the
/// directive stage is a no-op and only the OS adapter changes bytes.
const UBUNTU_SKILL_MD: &str = "# Setup\n\nRun `sudo apt-get install -y libssl-dev`.\n\
     Enable apache2.service. Repos live in /etc/apt/sources.list.d/.\n";

/// The same content in Alinux style.
const ALINUX_SKILL_MD: &str = "# Setup\n\nRun `sudo dnf install -y openssl-devel`.\n\
     Enable httpd.service. Repos live in /etc/yum.repos.d/.\n";

/// Write the rule artifact into a dedicated tempdir (kept alive by the caller)
/// and build a stage for `target`.
fn stage_for(target: OsTarget) -> (tempfile::TempDir, OsAdapterStage) {
    let dir = tempfile::tempdir().expect("rules tempdir");
    let path = dir.path().join("os-rules.yaml");
    std::fs::write(&path, RULES_YAML).expect("write rules");
    let selector = match target {
        OsTarget::Alinux => TargetSelector::Alinux,
        OsTarget::Ubuntu => TargetSelector::Ubuntu,
    };
    let stage = OsAdapterStage::load(&path, selector).expect("load stage");
    (dir, stage)
}

/// Seed a flat skill dir with a specific SKILL.md body (no frontmatter needed —
/// the transform operates on the raw served bytes).
fn seed_skill(src: &Path, name: &str, skill_md: &str) {
    let dir = src.join(name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("SKILL.md");
}

// ─────────────────────────────────────────────────────────────────────────────
// Local fixture: resolver + OS adapter, normal mode
// ─────────────────────────────────────────────────────────────────────────────

/// Normal-mode mount that injects both an [`ActiveSkillResolver`] and an
/// [`OsAdapterStage`]. Exposes the resolver `Arc` so tests can flip activation
/// targets after the mount is live (pinned-open test).
struct AdapterMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    resolver: Option<Arc<ActiveSkillResolver>>,
    handle: Option<MountHandle>,
    _rules: tempfile::TempDir,
}

impl AdapterMount {
    fn new<S, R>(target: OsTarget, seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        let (rules_dir, stage) = stage_for(target);
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: resolver.clone(),
                os_adapter: Some(stage),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            resolver,
            handle: Some(handle),
            _rules: rules_dir,
        }
    }

    fn skill_md(&self, name: &str) -> PathBuf {
        self.mountpoint
            .path()
            .join("skills")
            .join(name)
            .join("SKILL.md")
    }

    fn source_skill_md(&self, name: &str) -> PathBuf {
        self.source.path().join(name).join("SKILL.md")
    }
}

impl Drop for AdapterMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

fn current_result(skill: &str) -> skillfs_fuse::security::LedgerResolveResult {
    let json = format!(
        r#"{{"schemaVersion":1,"skillName":"{skill}","status":"pass","decision":"current","currentVersion":"v000001","trustedVersion":"v000001"}}"#
    );
    skillfs_fuse::security::LedgerResolveResult::from_json_str(&json).expect("current json")
}

fn fallback_result(skill: &str, segment: &str) -> skillfs_fuse::security::LedgerResolveResult {
    let json = format!(
        r#"{{"schemaVersion":1,"skillName":"{skill}","status":"deny","decision":"fallback","currentVersion":"v000003","trustedVersion":"{segment}","target":".skill-meta/versions/{segment}","targetKind":"relative_to_skill_dir","reason":"risk"}}"#
    );
    skillfs_fuse::security::LedgerResolveResult::from_json_str(&json).expect("fallback json")
}

fn hidden_result(skill: &str) -> skillfs_fuse::security::LedgerResolveResult {
    let json = format!(
        r#"{{"schemaVersion":1,"skillName":"{skill}","status":"deny","decision":"hidden","reason":"no trusted version"}}"#
    );
    skillfs_fuse::security::LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

fn write_snapshot(source: &Path, skill: &str, version: &str, skill_md: &str) {
    let dir = source
        .join(skill)
        .join(".skill-meta/versions")
        .join(version);
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("snapshot SKILL.md");
}

// ─────────────────────────────────────────────────────────────────────────────
// Local fixture: resolver + OS adapter, in-place Hermes mode
// ─────────────────────────────────────────────────────────────────────────────

/// In-place Hermes mount with both an [`ActiveSkillResolver`] and an
/// [`OsAdapterStage`], exposing the resolver so tests can flip a nested skill's
/// activation target after a handle is open.
struct HermesAdapterMount {
    source: tempfile::TempDir,
    resolver: Arc<ActiveSkillResolver>,
    handle: Option<MountHandle>,
    _rules: tempfile::TempDir,
}

impl HermesAdapterMount {
    fn new<S>(seed: S, resolver_builder: impl FnOnce(&Path) -> Arc<ActiveSkillResolver>) -> Self
    where
        S: FnOnce(&Path),
    {
        let (rules_dir, stage) = stage_for(OsTarget::Alinux);
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            source.path(),
            source.path(),
            shared,
            MountOptions::default(),
            true,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                os_adapter: Some(stage),
                skill_layout: Some(skillfs_fuse::SkillLayout::Hermes),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            resolver,
            handle: Some(handle),
            _rules: rules_dir,
        }
    }

    fn nested_skill_md(&self, category: &str, skill: &str) -> PathBuf {
        self.source
            .path()
            .join(category)
            .join(skill)
            .join("SKILL.md")
    }
}

impl Drop for HermesAdapterMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        let mp = self.source.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

/// Seed a Hermes nested skill: `<category>/<skill>/SKILL.md`.
fn seed_nested(src: &Path, category: &str, skill: &str, skill_md: &str) {
    let dir = src.join(category).join(skill);
    std::fs::create_dir_all(&dir).expect("nested dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("nested SKILL.md");
}

// ─────────────────────────────────────────────────────────────────────────────
// Opt-in / disabled behavior
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn os_adapter_disabled_by_default_preserves_skill_md() {
    skip_if_no_fuse!();
    let fx = MountFixture::normal(|src| seed_skill(src, "web", UBUNTU_SKILL_MD));
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    // No adapter configured: Ubuntu content survives untouched.
    assert_eq!(got, UBUNTU_SKILL_MD);
    assert!(!got.contains("dnf"));
}

#[test]
fn ubuntu_to_alinux_transforms_skill_md() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let fx =
        MountFixture::normal_with_os_adapter(|src| seed_skill(src, "web", UBUNTU_SKILL_MD), stage);
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    assert!(
        got.contains("sudo dnf install -y openssl-devel"),
        "got: {got}"
    );
    assert!(got.contains("httpd.service"));
    assert!(got.contains("/etc/yum.repos.d/"));
    assert!(!got.contains("apt-get"));
    assert!(!got.contains("libssl-dev"));
}

#[test]
fn alinux_to_ubuntu_transforms_skill_md() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Ubuntu);
    let fx =
        MountFixture::normal_with_os_adapter(|src| seed_skill(src, "web", ALINUX_SKILL_MD), stage);
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    assert!(
        got.contains("sudo apt-get install -y libssl-dev"),
        "got: {got}"
    );
    assert!(got.contains("apache2.service"));
    assert!(got.contains("/etc/apt/sources.list.d/"));
    assert!(!got.contains("dnf"));
    assert!(!got.contains("openssl-devel"));
}

#[test]
fn passthrough_non_skill_md_is_not_transformed() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let fx = MountFixture::normal_with_os_adapter(
        |src| {
            seed_skill(src, "web", UBUNTU_SKILL_MD);
            // A sibling Markdown + shell file must NOT be adapted.
            std::fs::write(src.join("web/notes.md"), UBUNTU_SKILL_MD).unwrap();
            std::fs::write(src.join("web/setup.sh"), "apt-get install -y libssl-dev\n").unwrap();
        },
        stage,
    );
    let notes = std::fs::read_to_string(fx.skill_path("web").join("notes.md")).expect("notes");
    let sh = std::fs::read_to_string(fx.skill_path("web").join("setup.sh")).expect("sh");
    assert_eq!(notes, UBUNTU_SKILL_MD);
    assert!(sh.contains("apt-get install -y libssl-dev"));
}

#[test]
fn adapter_only_pipeline_skips_directive_compilation() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    // A directive block plus an apt-get command. With the directive stage
    // disabled, the markers survive and only the OS adapter rewrites bytes.
    let body = "<!-- @if os == linux -->\nrun apt-get install -y libssl-dev\n<!-- @endif -->\n";
    let fx = MountFixture::normal_with_transforms(
        |src| seed_skill(src, "web", body),
        Some(false),
        Some(stage),
    );
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    assert!(
        got.contains("<!-- @if os == linux -->"),
        "directive not stripped: {got}"
    );
    assert!(
        got.contains("run dnf install -y openssl-devel"),
        "adapter not applied: {got}"
    );
}

#[test]
fn both_stages_disabled_serves_raw_content() {
    skip_if_no_fuse!();
    // Directive off and no adapter: the selected bytes are served verbatim,
    // including directive markers and Ubuntu-style commands.
    let body = "<!-- @if os == linux -->\napt-get install -y libssl-dev\n<!-- @endif -->\n";
    let fx =
        MountFixture::normal_with_transforms(|src| seed_skill(src, "web", body), Some(false), None);
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    assert_eq!(got, body);
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in bundled catalog (no external rules_path)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a stage from the bundled catalog embedded in the binary, exactly as
/// `enabled = true` with an absent `rules_path` would produce.
fn builtin_stage(target: OsTarget) -> OsAdapterStage {
    let selector = match target {
        OsTarget::Alinux => TargetSelector::Alinux,
        OsTarget::Ubuntu => TargetSelector::Ubuntu,
    };
    OsAdapterStage::load_default(selector).expect("load built-in catalog")
}

#[test]
fn builtin_catalog_transforms_skill_md_without_rules_path() {
    skip_if_no_fuse!();
    let stage = builtin_stage(OsTarget::Alinux);
    // The bundled catalog carries all 311 rules.
    assert_eq!(stage.total_rules(), 311);
    let body = "# Setup\n\nRun `sudo apt-get install -y nginx`.\n";
    let fx = MountFixture::normal_with_os_adapter(|src| seed_skill(src, "web", body), stage);
    let got = std::fs::read_to_string(fx.skill_path("web").join("SKILL.md")).expect("read");
    // A representative high-confidence rule from the built-in catalog applied.
    assert!(got.contains("sudo dnf install -y nginx"), "got: {got}");
    assert!(!got.contains("apt-get"), "got: {got}");
    // The physical source SKILL.md is never mutated by the transformed read.
    let src = std::fs::read_to_string(fx.source_skill_path("web").join("SKILL.md")).unwrap();
    assert_eq!(src, body, "source bytes must remain unchanged");
}

// ─────────────────────────────────────────────────────────────────────────────
// getattr size and partial reads agree with transformed bytes
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn getattr_size_matches_transformed_bytes() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let fx =
        MountFixture::normal_with_os_adapter(|src| seed_skill(src, "web", UBUNTU_SKILL_MD), stage);
    let path = fx.skill_path("web").join("SKILL.md");
    let content = std::fs::read(&path).expect("read");
    let size = std::fs::metadata(&path).expect("stat").len();
    assert_eq!(
        size as usize,
        content.len(),
        "getattr size must equal read len"
    );
    // Adaptation shrinks the content, so it must differ from the source size.
    let src_size = std::fs::metadata(fx.source_skill_path("web").join("SKILL.md"))
        .unwrap()
        .len();
    assert_ne!(size, src_size, "transformed size should differ from source");
}

#[test]
fn offset_and_partial_reads_match_full_read() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let fx =
        MountFixture::normal_with_os_adapter(|src| seed_skill(src, "web", UBUNTU_SKILL_MD), stage);
    let path = fx.skill_path("web").join("SKILL.md");
    let full = std::fs::read(&path).expect("full read");

    // Reassemble via small offset reads to exercise the FUSE offset path.
    let mut file = std::fs::File::open(&path).expect("open");
    let mut assembled = Vec::new();
    let mut buf = [0u8; 7];
    loop {
        let n = file.read(&mut buf).expect("chunk read");
        if n == 0 {
            break;
        }
        assembled.extend_from_slice(&buf[..n]);
    }
    assert_eq!(
        assembled, full,
        "chunked reads must reconstruct full content"
    );

    // Explicit mid-content offset read.
    let off = full.len() / 2;
    file.seek(SeekFrom::Start(off as u64)).expect("seek");
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).expect("tail read");
    assert_eq!(tail, full[off..], "offset read must match slice of full");
}

// ─────────────────────────────────────────────────────────────────────────────
// Activation: Current / Snapshot / Hidden with the adapter enabled
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn current_activation_transforms_live_source() {
    skip_if_no_fuse!();
    let mount = AdapterMount::new(
        OsTarget::Alinux,
        |src| seed_skill(src, "web", UBUNTU_SKILL_MD),
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set_from_resolve(&current_result("web")).unwrap();
            Some(Arc::new(r))
        },
    );
    let got = std::fs::read_to_string(mount.skill_md("web")).expect("read");
    assert!(got.contains("dnf install -y openssl-devel"), "got: {got}");
    // Live source bytes are untouched by the read.
    let src = std::fs::read_to_string(mount.source_skill_md("web")).unwrap();
    assert_eq!(src, UBUNTU_SKILL_MD);
}

#[test]
fn snapshot_activation_transforms_snapshot_never_live_source() {
    skip_if_no_fuse!();
    // Live SKILL.md carries a marker that must never surface through a
    // fallback read; the snapshot carries the Ubuntu content to transform.
    let live = "# LIVE-ONLY apt-get install -y libssl-dev\n";
    let mount = AdapterMount::new(
        OsTarget::Alinux,
        |src| {
            seed_skill(src, "web", live);
            write_snapshot(src, "web", "v000002", UBUNTU_SKILL_MD);
        },
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set_from_resolve(&fallback_result("web", "v000002"))
                .unwrap();
            Some(Arc::new(r))
        },
    );
    let got = std::fs::read_to_string(mount.skill_md("web")).expect("read");
    // Snapshot content, transformed.
    assert!(got.contains("dnf install -y openssl-devel"), "got: {got}");
    // The live-only marker must never appear (no snapshot->live fallback).
    assert!(
        !got.contains("LIVE-ONLY"),
        "must not read live source: {got}"
    );
    // Snapshot bytes on disk stay raw (Ubuntu) after the transformed read.
    let snap = std::fs::read_to_string(
        mount
            .source
            .path()
            .join("web/.skill-meta/versions/v000002/SKILL.md"),
    )
    .unwrap();
    assert_eq!(snap, UBUNTU_SKILL_MD);
}

#[test]
fn hidden_skill_returns_enoent_and_skips_transform() {
    skip_if_no_fuse!();
    let mount = AdapterMount::new(
        OsTarget::Alinux,
        |src| seed_skill(src, "web", UBUNTU_SKILL_MD),
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set_from_resolve(&hidden_result("web")).unwrap();
            Some(Arc::new(r))
        },
    );
    let err = std::fs::read_to_string(mount.skill_md("web")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn pinned_open_target_stable_after_activation_update() {
    skip_if_no_fuse!();
    let mount = AdapterMount::new(
        OsTarget::Alinux,
        |src| seed_skill(src, "web", UBUNTU_SKILL_MD),
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set_from_resolve(&current_result("web")).unwrap();
            Some(Arc::new(r))
        },
    );

    // Open the handle while the skill is Current.
    let mut file = std::fs::File::open(mount.skill_md("web")).expect("open");

    // Flip the resolver to Hidden after the handle is open.
    if let Some(resolver) = &mount.resolver {
        resolver.set_from_resolve(&hidden_result("web")).unwrap();
    }

    // The pinned handle still serves the transformed Current content.
    let mut got = String::new();
    file.read_to_string(&mut got).expect("read pinned");
    assert!(got.contains("dnf install -y openssl-devel"), "got: {got}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Hermes nested layout shares the same pipeline
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hermes_nested_skill_md_is_transformed() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let fx = MountFixture::in_place_hermes_with_os_adapter(
        |src| {
            // category/skill/SKILL.md nested layout.
            let dir = src.join("cloud").join("deploy");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), UBUNTU_SKILL_MD).unwrap();
        },
        stage,
    );
    let nested = fx
        .mountpoint()
        .join("cloud")
        .join("deploy")
        .join("SKILL.md");
    let got = std::fs::read_to_string(&nested).expect("read nested");
    assert!(got.contains("dnf install -y openssl-devel"), "got: {got}");
    assert!(!got.contains("apt-get"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Content-free Open audit metadata
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn flat_transformed_open_audits_target_and_digest() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let digest = stage.rule_digest().to_string();
    let sink = Arc::new(InMemoryEventSink::new());
    let fx = MountFixture::normal_with_transform_audit(
        |src| seed_skill(src, "web", UBUNTU_SKILL_MD),
        Some(stage),
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    let _file = std::fs::File::open(fx.skill_path("web").join("SKILL.md")).expect("open");
    let event = sink
        .events()
        .into_iter()
        .find(|event| {
            event.kind == SkillEventKind::Open
                && event.action == Some(SkillEventAction::Allowed)
                && event.skill_name.as_deref() == Some("web")
                && event.relative_path.as_deref() == Some(Path::new("SKILL.md"))
        })
        .expect("flat transformed Open event");
    let expected = format!("transform=os_adapter target_os=alinux rule_digest={digest}");
    assert_eq!(event.detail.as_deref(), Some(expected.as_str()));

    let json: serde_json::Value =
        serde_json::from_str(&serialize_event_jsonl(&event)).expect("audit JSONL");
    assert_eq!(json["detail"].as_str(), Some(expected.as_str()));
}

#[test]
fn hermes_transformed_open_audits_target_and_digest() {
    skip_if_no_fuse!();
    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let digest = stage.rule_digest().to_string();
    let sink = Arc::new(InMemoryEventSink::new());
    let fx = MountFixture::in_place_hermes_with_transform_audit(
        |src| seed_nested(src, "cloud", "deploy", UBUNTU_SKILL_MD),
        stage,
        sink.clone() as Arc<dyn SkillEventSink>,
    );

    let _file = std::fs::File::open(
        fx.mountpoint()
            .join("cloud")
            .join("deploy")
            .join("SKILL.md"),
    )
    .expect("open nested");
    let event = sink
        .events()
        .into_iter()
        .find(|event| {
            event.kind == SkillEventKind::Open
                && event.action == Some(SkillEventAction::Allowed)
                && event.skill_name.as_deref() == Some("cloud/deploy")
                && event.relative_path.as_deref() == Some(Path::new("SKILL.md"))
        })
        .expect("Hermes transformed Open event");
    let expected = format!("transform=os_adapter target_os=alinux rule_digest={digest}");
    assert_eq!(event.detail.as_deref(), Some(expected.as_str()));
}

#[test]
fn adapter_detail_is_absent_for_disabled_nonvirtual_and_write_opens() {
    skip_if_no_fuse!();

    let disabled_sink = Arc::new(InMemoryEventSink::new());
    let disabled = MountFixture::normal_with_transform_audit(
        |src| seed_skill(src, "raw", UBUNTU_SKILL_MD),
        None,
        disabled_sink.clone() as Arc<dyn SkillEventSink>,
    );
    let _file = std::fs::File::open(disabled.skill_path("raw").join("SKILL.md")).expect("open");
    let disabled_opens = disabled_sink.of_kind(SkillEventKind::Open);
    assert!(disabled_opens.iter().all(|event| {
        event
            .detail
            .as_deref()
            .is_none_or(|detail| !detail.contains("transform=os_adapter"))
    }));
    assert!(
        disabled_opens.iter().any(|event| {
            event.skill_name.as_deref() == Some("raw")
                && event.relative_path.as_deref() == Some(Path::new("SKILL.md"))
        }),
        "adapter-disabled virtual Open event missing"
    );

    let (_rules, stage) = stage_for(OsTarget::Alinux);
    let enabled_sink = Arc::new(InMemoryEventSink::new());
    let enabled = MountFixture::normal_with_transform_audit(
        |src| {
            seed_skill(src, "web", UBUNTU_SKILL_MD);
            std::fs::write(src.join("web/notes.md"), UBUNTU_SKILL_MD).unwrap();
        },
        Some(stage),
        enabled_sink.clone() as Arc<dyn SkillEventSink>,
    );
    let _passthrough =
        std::fs::File::open(enabled.skill_path("web").join("notes.md")).expect("open passthrough");
    let _write = std::fs::OpenOptions::new()
        .write(true)
        .open(enabled.skill_path("web").join("SKILL.md"))
        .expect("open SKILL.md for write");
    let opens = enabled_sink.of_kind(SkillEventKind::Open);
    for event in opens.iter().filter(|event| {
        event.relative_path.as_deref() == Some(Path::new("notes.md"))
            || event.relative_path.as_deref() == Some(Path::new("SKILL.md"))
    }) {
        assert!(
            event
                .detail
                .as_deref()
                .is_none_or(|detail| !detail.contains("transform=os_adapter")),
            "nonvirtual/write Open was mislabeled: {event:?}"
        );
    }
    assert!(
        opens
            .iter()
            .any(|event| event.relative_path.as_deref() == Some(Path::new("notes.md"))),
        "passthrough Open event missing"
    );
    assert!(
        opens
            .iter()
            .any(|event| event.relative_path.as_deref() == Some(Path::new("SKILL.md"))),
        "write Open event missing"
    );
}

#[test]
fn hermes_current_to_hidden_after_open_stays_readable() {
    skip_if_no_fuse!();
    let mount = HermesAdapterMount::new(
        |src| seed_nested(src, "cloud", "deploy", UBUNTU_SKILL_MD),
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set(
                "cloud/deploy",
                ActiveTarget::Current {
                    source_dir: root.join("cloud/deploy"),
                },
            );
            Arc::new(r)
        },
    );

    // Open while Current.
    let mut file = std::fs::File::open(mount.nested_skill_md("cloud", "deploy")).expect("open");
    // Flip the nested skill to Hidden after the handle is open.
    mount.resolver.set(
        "cloud/deploy",
        ActiveTarget::Hidden {
            reason: "test hidden".to_string(),
        },
    );

    // The pinned handle still serves the transformed Current content.
    let mut got = String::new();
    file.read_to_string(&mut got).expect("read pinned");
    assert!(got.contains("dnf install -y openssl-devel"), "got: {got}");

    // A fresh open now observes the Hidden decision (ENOENT), confirming the
    // resolver flip took effect and the earlier handle was genuinely pinned.
    let err = std::fs::File::open(mount.nested_skill_md("cloud", "deploy")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn hermes_snapshot_to_current_after_open_stays_snapshot() {
    skip_if_no_fuse!();
    let live = "# LIVE-ONLY apt-get install -y libssl-dev\n";
    let mount = HermesAdapterMount::new(
        |src| {
            seed_nested(src, "cloud", "deploy", live);
            write_snapshot(src, "cloud/deploy", "v000002", UBUNTU_SKILL_MD);
        },
        |root| {
            let r = ActiveSkillResolver::new(root.to_path_buf());
            r.set(
                "cloud/deploy",
                ActiveTarget::Snapshot {
                    snapshot_dir: root.join("cloud/deploy/.skill-meta/versions/v000002"),
                    version: "v000002".to_string(),
                },
            );
            Arc::new(r)
        },
    );

    // While still resolving to Snapshot: getattr size equals the transformed
    // snapshot length, and a partial (offset) read matches the full read.
    let path = mount.nested_skill_md("cloud", "deploy");
    let full = std::fs::read(&path).expect("full read");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len() as usize,
        full.len(),
        "getattr size must match transformed bytes"
    );
    let mut probe = std::fs::File::open(&path).expect("probe open");
    probe.seek(SeekFrom::Start(3)).expect("seek");
    let mut tail = Vec::new();
    probe.read_to_end(&mut tail).expect("tail");
    assert_eq!(tail, full[3..], "offset read must match slice of full");

    // Open the handle we will keep pinned across the flip.
    let mut file = std::fs::File::open(&path).expect("open");

    // Flip to Current after the handle is open.
    mount.resolver.set(
        "cloud/deploy",
        ActiveTarget::Current {
            source_dir: mount.source.path().join("cloud/deploy"),
        },
    );

    // The pinned handle still serves Snapshot content, not the live source.
    // Read a bounded prefix (valid for both snapshot and live sizes) so the
    // assertion is immune to the kernel re-stat'ing the inode to the shorter
    // Current size after the flip — the point under test is *which target* the
    // handle reads from, which the leading marker distinguishes: the snapshot
    // begins with "# Setup" while the live source begins with "# LIVE-ONLY".
    let mut prefix = [0u8; 7];
    file.read_exact(&mut prefix).expect("read pinned prefix");
    assert_eq!(
        &prefix, b"# Setup",
        "pinned handle must read snapshot, not live"
    );

    // A fresh open now observes Current: the live-only marker appears, proving
    // the flip took effect and the earlier handle stayed pinned to the snapshot.
    let fresh = std::fs::read_to_string(&path).expect("fresh read");
    assert!(
        fresh.contains("LIVE-ONLY"),
        "fresh open should read live: {fresh}"
    );
}

#[test]
fn captured_current_survives_edits_and_replacement() {
    use std::os::unix::fs::FileExt;
    skip_if_no_fuse!();
    for layout in [
        skillfs_fuse::SkillLayout::Flat,
        skillfs_fuse::SkillLayout::Hermes,
    ] {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        let id = if layout == skillfs_fuse::SkillLayout::Flat {
            "web"
        } else {
            "cloud/web"
        };
        let original = UBUNTU_SKILL_MD.repeat(200);
        seed_skill(source.path(), id, &original);
        let physical = source.path().join(id).join("SKILL.md");
        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let (_rules, stage) = stage_for(OsTarget::Alinux);
        let expected = ALINUX_SKILL_MD.repeat(200);
        let _mount = mount_background_configured(
            mountpoint.path(),
            source.path(),
            Arc::new(RwLock::new(store)),
            MountOptions::default(),
            false,
            MountConfig {
                skill_layout: Some(layout),
                os_adapter: Some(stage),
                ..MountConfig::default()
            },
        )
        .unwrap();
        let path = mountpoint.path().join("skills").join(id).join("SKILL.md");
        let old = std::fs::File::open(&path).unwrap();
        let mut first = [0; 17];
        assert_eq!(old.read_at(&mut first, 0).unwrap(), first.len());
        assert_eq!(&first, &expected.as_bytes()[..17]);
        // Same-size in-place modification, then shrink, then atomic growth.
        for (replacement, atomic) in [
            ("X".repeat(original.len()), false),
            ("short\n".into(), false),
            ("long\n".repeat(10000), true),
        ] {
            if atomic {
                let next = physical.with_file_name("next");
                std::fs::write(&next, &replacement).unwrap();
                std::fs::rename(next, &physical).unwrap();
            } else {
                std::fs::write(&physical, &replacement).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), replacement);
            // A fresh open must not overwrite the old handle's cached pages.
            let mut got = Vec::new();
            let mut block = [0; 997];
            for _ in 0..100 {
                let n = old.read_at(&mut block, got.len() as u64).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&block[..n]);
            }
            assert_eq!(got, expected.as_bytes());
            assert_eq!(
                old.read_at(&mut block, expected.len() as u64 + 10).unwrap(),
                0
            );
            assert_eq!(old.read_at(&mut [], 0).unwrap(), 0);
        }
    }
}

#[test]
fn captured_snapshot_survives_removal_without_live_fallback() {
    skip_if_no_fuse!();
    let mount = AdapterMount::new(
        OsTarget::Alinux,
        |src| {
            seed_skill(src, "web", "LIVE\n");
            seed_skill(src, "web/.skill-meta/versions/v1", UBUNTU_SKILL_MD);
        },
        |root| {
            let resolver = Arc::new(ActiveSkillResolver::new(root.to_path_buf()));
            resolver.set(
                "web",
                ActiveTarget::Snapshot {
                    snapshot_dir: root.join("web/.skill-meta/versions/v1"),
                    version: "v1".into(),
                },
            );
            Some(resolver)
        },
    );
    let mut old = std::fs::File::open(mount.skill_md("web")).unwrap();
    std::fs::remove_file(
        mount
            .source
            .path()
            .join("web/.skill-meta/versions/v1/SKILL.md"),
    )
    .unwrap();
    assert!(std::fs::File::open(mount.skill_md("web")).is_err());
    mount.resolver.as_ref().unwrap().set(
        "web",
        ActiveTarget::Current {
            source_dir: mount.source.path().join("web"),
        },
    );
    assert_eq!(
        std::fs::read_to_string(mount.skill_md("web")).unwrap(),
        "LIVE\n"
    );
    let mut got = String::new();
    old.read_to_string(&mut got).unwrap();
    assert_eq!(got, ALINUX_SKILL_MD);
}

#[test]
fn repeated_versions_hit_captured_budget_then_recover_after_close() {
    use std::os::unix::fs::FileExt;
    skip_if_no_fuse!();
    // Stay just below the LRU per-entry ceiling, but keep more historical
    // versions alive than the LRU can retain. Only eight fit the handle budget.
    let payload_size = 8 * 1024 * 1024 - 64;
    let fixture = MountFixture::normal(|src| {
        seed_skill(src, "web", "# Initial\n");
        seed_skill(src, "other", "# Initial\n");
    });
    let physical = fixture.source_skill_path("web").join("SKILL.md");
    let path = fixture.skill_path("web").join("SKILL.md");
    std::fs::write(
        fixture.source_skill_path("other").join("SKILL.md"),
        "Z".repeat(payload_size),
    )
    .unwrap();
    let mut held = Vec::new();
    for version in 0..24u8 {
        let payload = vec![b'A' + version; payload_size];
        if version % 2 == 0 {
            std::fs::write(&physical, &payload).unwrap();
        } else {
            let next = physical.with_file_name("next");
            std::fs::write(&next, &payload).unwrap();
            std::fs::rename(next, &physical).unwrap();
        }
        match std::fs::File::open(&path) {
            Ok(file) => {
                assert!(version < 8);
                held.push(file);
            }
            Err(error) => {
                assert!(version >= 8, "version {version}: {error}");
                assert_eq!(error.raw_os_error(), Some(libc::ENOMEM));
            }
        }
    }
    assert_eq!(held.len(), 8);
    assert_eq!(
        std::fs::File::open(fixture.skill_path("other").join("SKILL.md"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOMEM)
    );
    for (version, file) in held.iter().enumerate() {
        let mut byte = [0];
        assert_eq!(
            file.read_at(&mut byte, (payload_size - 1) as u64).unwrap(),
            1
        );
        assert_eq!(byte[0], b'A' + version as u8);
    }
    drop(held.pop());
    // FUSE release can be asynchronous to close; allow a bounded dispatch delay.
    let mut reopened = None;
    for _ in 0..100 {
        match std::fs::File::open(&path) {
            Ok(file) => {
                reopened = Some(file);
                break;
            }
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::ENOMEM)),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let file = reopened.expect("closing a captured handle restores admission capacity");
    let mut byte = [0];
    assert_eq!(file.read_at(&mut byte, 0).unwrap(), 1);
    assert_eq!(byte[0], b'X');
}
