//! End-to-end adapter manager tests for the Cosh, Codex, Claude Code, and
//! Qoder drivers.
//!
//! Each test drives the real [`AdapterManager`] against a staged component
//! contract + resource bundle. Codex and Claude Code use shell-script fake
//! CLIs that record their argv (so we can assert the exact framework
//! commands ANOLISA issues) and keep enough state for `status` to verify.
//! Cosh is CLI-less: its enable/disable are pure filesystem operations, so
//! the test asserts it copies and removes only its own extension directory.
//!
//! All three drivers read process-global env (`CODEX_BIN`, `CLAUDE_BIN`,
//! `COSH_HOME`, `XDG_DATA_HOME`, …), so every test serializes on
//! [`ENV_LOCK`], starts from a cleared env, and restores it on exit.
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anolisa_core::adapter::AdapterError;
use anolisa_core::adapter::claim::{
    ClaimResourceKind, ClaimStatus, DRIVER_SCHEMA_VERSION, DriverPayload,
};
use anolisa_core::adapter::driver::{AdapterConditionKind, AdapterSummary, ConditionStatus};
use anolisa_core::adapter::manager::{AdapterManager, EnableOutcome};
use anolisa_core::domain::{PackageIdentity, ProviderBinding};
use anolisa_core::state::{FileOwner, OwnedFile, OwnedFileKind};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_files::{
    PackageFile, PackageFileDigestAlgorithm, PackageFileInventory, PackageFileKind,
    PackageFileQuery,
};
use anolisa_platform::pkg_query::PackageQueryError;
use sha2::{Digest, Sha256};

const COMPONENT: &str = "tokenless";

/// Serializes process-global env mutation across tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Env keys every test clears on entry and restores on drop, so a test
/// never observes another test's half-applied contract.
const MANAGED_ENV: &[&str] = &[
    "CODEX_BIN",
    "CLAUDE_BIN",
    "COSH_BIN",
    "COSH_HOME",
    "XDG_DATA_HOME",
    "FAKE_CODEX_LOG",
    "FAKE_CODEX_STATE",
    "FAKE_CODEX_FAIL",
    "FAKE_CLAUDE_LOG",
    "FAKE_CLAUDE_STATE",
    "QODERCLI_BIN",
    "FAKE_QODER_LOG",
    "FAKE_QODER_CACHE",
    "FAKE_QODER_FAIL",
    "FAKE_QODER_LARGE_INVENTORY",
    "FAKE_QODER_PLUGIN_ID",
    "FAKE_QODER_PROJECT_PLUGIN",
    "QWENPAW_BIN",
    "QWENPAW_WORKING_DIR",
    "COPAW_WORKING_DIR",
    "QWENPAW_HOME",
    "FAKE_QWENPAW_LOG",
    "FAKE_QWENPAW_FAIL",
];

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn acquire() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = MANAGED_ENV
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        let guard = Self { _lock: lock, saved };
        for k in MANAGED_ENV {
            // SAFETY: guard holds ENV_LOCK for the whole test.
            unsafe { std::env::remove_var(k) };
        }
        guard
    }

    fn set(&self, key: &str, value: &Path) {
        // SAFETY: guard holds ENV_LOCK.
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            // SAFETY: guard holds ENV_LOCK until restore completes.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// A staged world: system-mode layout under a temp prefix, a seeded
/// `installed.toml` + component contract for one framework, and the
/// framework's resource bundle.
struct World {
    _root: tempfile::TempDir,
    prefix: PathBuf,
    layout: FsLayout,
    user_home: PathBuf,
    resource_root: PathBuf,
}

impl World {
    fn manager(&self) -> AdapterManager {
        let mut manager = AdapterManager::new(
            self.layout.clone(),
            Some(self.user_home.clone()),
            "tester".to_string(),
        );
        manager.set_package_file_query(Box::new(FixturePackageFiles {
            root: self.prefix.clone(),
        }));
        manager
    }

    fn load_state(&self) -> anolisa_core::state_store::StateStore {
        anolisa_core::state_store::StateStore::load(
            &self.layout.state_dir.join("installed.toml"),
            anolisa_platform::privilege::effective_uid(),
        )
        .expect("load state")
    }
}

struct FixturePackageFiles {
    root: PathBuf,
}

impl PackageFileQuery for FixturePackageFiles {
    fn query_file_inventory(
        &self,
        _package: &str,
    ) -> Result<PackageFileInventory, PackageQueryError> {
        Ok(PackageFileInventory {
            digest_algorithm: PackageFileDigestAlgorithm::Sha256,
            files: fixture_package_files(&self.root),
        })
    }
}

fn fixture_package_files(root: &Path) -> Vec<PackageFile> {
    fn collect(root: &Path, files: &mut Vec<PackageFile>) {
        for entry in std::fs::read_dir(root).expect("read fixture package root") {
            let entry = entry.expect("fixture package entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("fixture file type");
            if file_type.is_dir() {
                collect(&path, files);
            } else if file_type.is_symlink() {
                files.push(PackageFile {
                    path: path.display().to_string(),
                    kind: PackageFileKind::Symlink,
                    digest: None,
                    link_target: Some(
                        std::fs::read_link(&path)
                            .expect("fixture symlink target")
                            .display()
                            .to_string(),
                    ),
                });
            } else if file_type.is_file() {
                files.push(PackageFile {
                    path: path.display().to_string(),
                    kind: PackageFileKind::Regular,
                    digest: Some(format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(&path).expect("fixture file bytes"))
                    )),
                    link_target: None,
                });
            }
        }
    }

    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn record_owned_adapter_files(layout: &FsLayout, root: &Path) {
    let state_path = layout.state_dir.join("installed.toml");
    let mut state = anolisa_core::state_store::StateStore::load(
        &state_path,
        anolisa_platform::privilege::effective_uid(),
    )
    .expect("load fixture state");
    let installation = state
        .find_mut(anolisa_core::state::ObjectKind::Component, COMPONENT)
        .expect("fixture component");
    let ProviderBinding::Owned { artifact } = &mut installation.binding else {
        panic!("fixture component must be raw-owned");
    };
    artifact.files = fixture_package_files(root)
        .into_iter()
        .map(|file| OwnedFile {
            path: PathBuf::from(file.path),
            owner: FileOwner::Anolisa,
            sha256: file.digest,
            kind: match file.kind {
                PackageFileKind::Symlink => OwnedFileKind::Symlink,
                _ => OwnedFileKind::File,
            },
            referent: file.link_target.map(PathBuf::from),
            mode: None,
            capabilities: Vec::new(),
        })
        .collect();
    state.save(&state_path).expect("save fixture state");
}

fn resolve_fixture_rpm_identity(layout: &FsLayout) {
    let state_path = layout.state_dir.join("installed.toml");
    let mut state = anolisa_core::state_store::StateStore::load(
        &state_path,
        anolisa_platform::privilege::effective_uid(),
    )
    .expect("load fixture state");
    let installation = state
        .find_mut(anolisa_core::state::ObjectKind::Component, COMPONENT)
        .expect("fixture component");
    let ProviderBinding::Delegated { package, .. } = &mut installation.binding else {
        panic!("fixture component must be delegated");
    };
    *package = PackageIdentity::Resolved {
        name: COMPONENT.to_string(),
    };
    state.save(&state_path).expect("save fixture state");
}

/// Stage a component contract declaring `framework`/`adapter_type` with the
/// given `dest`, plus the resource bundle written by `stage_bundle`.
fn stage(framework: &str, adapter_type: &str, dest: &str, stage_bundle: impl Fn(&Path)) -> World {
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));
    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");

    // Resolve the resource root the same way the manager will (expand the
    // dest against the system datadir) and populate it.
    let resource_root = expand_dest(dest, &layout.datadir);
    std::fs::create_dir_all(&resource_root).expect("resource root");
    stage_bundle(&resource_root);

    seed_component(&layout, &prefix, framework, adapter_type, dest);

    World {
        _root: root,
        prefix,
        layout,
        user_home,
        resource_root,
    }
}

/// Seed `installed.toml` (component installed) plus the component contract
/// declaring one adapter with the given `framework`/`adapter_type`/`dest`.
fn seed_component(
    layout: &FsLayout,
    prefix: &Path,
    framework: &str,
    adapter_type: &str,
    dest: &str,
) {
    let state_path = layout.state_dir.join("installed.toml");
    std::fs::create_dir_all(state_path.parent().unwrap()).expect("state dir");
    std::fs::write(
        &state_path,
        format!(
            r#"schema_version = 2
updated_at = "2026-07-04T00:00:00Z"
install_mode = "system"
prefix = "{prefix}"
anolisa_version = "0.1.20"

[[objects]]
kind = "component"
name = "{COMPONENT}"
version = "0.6.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
installed_at = "2026-07-04T00:00:00Z"
"#,
            prefix = prefix.display(),
        ),
    )
    .expect("seed state");

    let manifest_path = layout
        .state_dir
        .join("component-manifests")
        .join(COMPONENT)
        .join("component.toml");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).expect("manifest dir");
    std::fs::write(
        &manifest_path,
        format!(
            r#"[component]
name = "{COMPONENT}"
version = "0.6.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "{framework}"
adapter_type = "{adapter_type}"
plugin_id = "{COMPONENT}"
dest = "{dest}"
"#
        ),
    )
    .expect("seed contract");
    let resource_root = expand_dest(dest, &layout.datadir);
    if resource_root.is_dir() {
        record_owned_adapter_files(layout, &resource_root);
    }
}

/// Minimal `{datadir}`/`{component}` expansion for staging (the manager's
/// real expansion is exercised separately).
fn expand_dest(dest: &str, datadir: &Path) -> PathBuf {
    let expanded = dest
        .replace("{datadir}", &datadir.to_string_lossy())
        .replace("{component}", COMPONENT);
    PathBuf::from(expanded)
}

/// Stage a world whose component is RPM-installed (delegated provenance)
/// and whose contract declares an `[adapters.backends.rpm].resource_root`
/// outside every datadir root. The bundle exists only at that RPM root —
/// the raw `dest` was never laid down, exactly like an RPM-only install.
fn stage_rpm_backend(
    framework: &str,
    adapter_type: &str,
    dest: &str,
    stage_bundle: impl Fn(&Path),
) -> World {
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));
    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");

    let resource_root = prefix.join("opt").join("tokenless-plugin");
    std::fs::create_dir_all(&resource_root).expect("rpm root");
    stage_bundle(&resource_root);

    // RPM provenance: adopted object delegated to the rpm backend.
    let state_path = layout.state_dir.join("installed.toml");
    std::fs::create_dir_all(state_path.parent().unwrap()).expect("state dir");
    std::fs::write(
        &state_path,
        format!(
            r#"schema_version = 2
updated_at = "2026-07-04T00:00:00Z"
install_mode = "system"
prefix = "{prefix}"
anolisa_version = "0.1.20"

[[objects]]
kind = "component"
name = "{COMPONENT}"
version = "0.6.0"
status = "adopted"
install_backend = "rpm"
ownership = "rpm_observed"
managed = false
adopted = true
installed_at = "2026-07-04T00:00:00Z"
"#,
            prefix = prefix.display(),
        ),
    )
    .expect("seed state");

    let contract = format!(
        r#"[component]
name = "{COMPONENT}"
version = "0.6.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "{framework}"
adapter_type = "{adapter_type}"
plugin_id = "{COMPONENT}"
dest = "{dest}"

[adapters.backends.rpm]
resource_root = "{rpm_root}/"
"#,
        rpm_root = resource_root.display(),
    );
    // Both discovery paths an adopted component may be read from: the
    // saved manifest snapshot and the datadir contract the RPM ships.
    for path in [
        layout
            .state_dir
            .join("component-manifests")
            .join(COMPONENT)
            .join("component.toml"),
        layout
            .datadir
            .join("components")
            .join(COMPONENT)
            .join("component.toml"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).expect("contract dir");
        std::fs::write(&path, &contract).expect("seed contract");
    }
    resolve_fixture_rpm_identity(&layout);

    World {
        _root: root,
        prefix,
        layout,
        user_home,
        resource_root,
    }
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

// ---------------------------------------------------------------------------
// Cosh
// ---------------------------------------------------------------------------

fn stage_cosh_bundle(root: &Path) {
    std::fs::create_dir_all(root.join("hooks")).expect("hooks");
    std::fs::write(root.join("cosh-extension.json"), br#"{"name":"tokenless"}"#).expect("manifest");
    std::fs::write(root.join("hooks/run-hook.sh"), b"#!/bin/sh\n").expect("hook");
}

#[test]
fn cosh_enable_status_disable_touches_only_extension_dir() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "cosh",
        "extension",
        "{datadir}/adapters/{component}/common/",
        stage_cosh_bundle,
    );
    let cosh_home = world.prefix.join("cosh-home");
    std::fs::create_dir_all(&cosh_home).expect("cosh home");
    guard.set("COSH_HOME", &cosh_home);
    // A sibling extension owned by someone else must survive disable.
    let sibling = cosh_home.join("extensions").join("other");
    std::fs::create_dir_all(&sibling).expect("sibling");
    std::fs::write(sibling.join("keep.txt"), b"keep").expect("keep");

    let manager = world.manager();
    let source_runtime_cache = world.resource_root.join("__pycache__/runtime.pyc");
    std::fs::create_dir_all(
        source_runtime_cache
            .parent()
            .expect("source runtime cache parent"),
    )
    .expect("source runtime cache dir");
    std::fs::write(&source_runtime_cache, b"source-runtime-only")
        .expect("source runtime cache file");
    let claim = match manager
        .enable(COMPONENT, Some("cosh"), false)
        .expect("enable")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    assert_eq!(claim.adapter_type.as_deref(), Some("extension"));

    let ext_dir = cosh_home.join("extensions").join("tokenless");
    assert!(
        ext_dir.join("cosh-extension.json").is_file(),
        "extension copied"
    );
    assert!(ext_dir.join("hooks/run-hook.sh").is_file(), "tree copied");
    assert!(
        !ext_dir.join("__pycache__/runtime.pyc").exists(),
        "unowned source files must not be materialized"
    );

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
    assert!(
        status.entries[0]
            .report
            .conditions
            .iter()
            .any(|c| c.kind == AdapterConditionKind::TreePresent
                && c.status == ConditionStatus::True)
    );

    let runtime_cache = ext_dir.join("__pycache__/runtime.pyc");
    std::fs::create_dir_all(runtime_cache.parent().expect("runtime cache parent"))
        .expect("runtime cache dir");
    std::fs::write(&runtime_cache, b"runtime-only").expect("runtime cache file");
    let status = manager
        .status(Some(COMPONENT))
        .expect("status with runtime file");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    std::fs::write(ext_dir.join("hooks/run-hook.sh"), b"changed").expect("mutate copied hook");
    let status = manager
        .status(Some(COMPONENT))
        .expect("status with changed copy");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    assert!(status.entries[0].report.conditions.iter().any(|condition| {
        condition.kind == AdapterConditionKind::MaterializedBundleMatches
            && condition.status == ConditionStatus::False
    }));
    std::fs::write(ext_dir.join("hooks/run-hook.sh"), b"#!/bin/sh\n").expect("restore copied hook");

    let disabled = manager
        .disable(COMPONENT, Some("cosh"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(!ext_dir.exists(), "extension dir removed");
    assert!(
        sibling.join("keep.txt").is_file(),
        "disable must not touch sibling extensions"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "cosh")
            .is_none(),
        "receipt gone after disable"
    );
}

#[test]
fn cosh_dry_run_enable_writes_nothing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "cosh",
        "extension",
        "{datadir}/adapters/{component}/common/",
        stage_cosh_bundle,
    );
    let cosh_home = world.prefix.join("cosh-home");
    std::fs::create_dir_all(&cosh_home).expect("cosh home");
    guard.set("COSH_HOME", &cosh_home);

    let manager = world.manager();
    let outcome = manager
        .enable(COMPONENT, Some("cosh"), true)
        .expect("dry-run");
    match outcome {
        EnableOutcome::Planned { plan, .. } => {
            assert_eq!(plan.framework, "cosh");
            assert!(
                plan.actions
                    .iter()
                    .any(|a| a.contains("deliver cosh extension"))
            );
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }
    assert!(
        !cosh_home.join("extensions").join("tokenless").exists(),
        "dry-run must not write the extension dir"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "cosh")
            .is_none(),
        "dry-run must not persist a receipt"
    );
}

#[test]
fn cosh_disable_keeps_receipt_when_ownership_marker_missing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "cosh",
        "extension",
        "{datadir}/adapters/{component}/common/",
        stage_cosh_bundle,
    );
    let cosh_home = world.prefix.join("cosh-home");
    std::fs::create_dir_all(&cosh_home).expect("cosh home");
    guard.set("COSH_HOME", &cosh_home);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("cosh"), false)
        .expect("enable");
    let ext_dir = cosh_home.join("extensions").join("tokenless");

    // Simulate the ownership marker going missing (user replaced the
    // extension, or a marker write failed after copy).
    std::fs::remove_file(ext_dir.join(".anolisa-adapter")).expect("remove marker");

    // Status must degrade, not report healthy, when ownership is unprovable.
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);

    // Disable must NOT delete a dir it cannot prove it owns, and must NOT
    // report success (the extension is still on disk / auto-discoverable),
    // so the receipt is kept as cleanup_failed.
    let disabled = manager
        .disable(COMPONENT, Some("cosh"), false)
        .expect("disable runs");
    assert!(
        !disabled.claim_removed,
        "receipt kept when ownership unprovable"
    );
    assert!(!disabled.report.cleanup_complete);
    assert!(
        ext_dir.exists(),
        "non-ANOLISA-owned dir must be left in place"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "cosh")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

fn stage_codex_bundle(root: &Path) {
    std::fs::create_dir_all(root.join(".codex-plugin")).expect("codex-plugin");
    std::fs::write(
        root.join(".codex-plugin/plugin.json"),
        br#"{"name":"tokenless"}"#,
    )
    .expect("plugin.json");
    std::fs::write(root.join("README.md"), b"codex plugin\n").expect("readme");
}

fn stage_codex_hook_bundle(root: &Path) {
    stage_codex_bundle(root);
    std::fs::create_dir_all(root.join("hooks")).expect("hooks directory");
    std::fs::write(
        root.join("hooks/hooks.json"),
        br#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .expect("hooks.json");
}

/// Fake `codex` CLI: appends each argv line to `$FAKE_CODEX_LOG` and keeps
/// marketplace/plugin registries under `$FAKE_CODEX_STATE` so `list`
/// reflects prior `add`/`remove` calls.
fn write_fake_codex(dir: &Path) -> PathBuf {
    let path = dir.join("codex");
    write_exec(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_CODEX_LOG"
st="$FAKE_CODEX_STATE"; mkdir -p "$st" 2>/dev/null
if [ "$1" = "app-server" ]; then
  while IFS= read -r line; do
    case "$line" in
      *'"method":"initialize"'*) printf '{"id":0,"result":{}}\n' ;;
      *'"method":"hooks/list"'*)
        if [ "$FAKE_CODEX_FAIL" = "hooks" ]; then
          printf '{"id":1,"result":{"data":[{"hooks":[],"warnings":[],"errors":[]}]}}\n'
        else
          printf '{"id":1,"result":{"data":[{"hooks":[{"key":"tokenless@anolisa-tokenless:hooks/hooks.json:pre_tool_use:0:0","currentHash":"sha256:trusted","source":"plugin","pluginId":"tokenless@anolisa-tokenless","isManaged":false}],"warnings":[],"errors":[]}]}}\n'
        fi ;;
      *'"method":"config/batchWrite"'*)
        printf '%s\n' "$line" > "$st/trust-request"
        if [ "$FAKE_CODEX_FAIL" = "write-overridden" ]; then
          printf '{"id":1,"result":{"status":"okOverridden","overriddenMetadata":{"message":"hooks.state is managed","overridingLayer":{"name":"sessionFlags","version":"test"},"effectiveValue":{}}}}\n'
        else
          printf '{"id":1,"result":{"status":"ok"}}\n'
        fi ;;
    esac
  done
  exit 0
fi
if [ "$1" = "plugin" ] && [ "$2" = "marketplace" ]; then
  case "$3" in
    add)
      name=$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$4/.agents/plugins/marketplace.json" | head -n1)
      echo "$name" >> "$st/marketplaces" ;;
    remove)
      [ "$FAKE_CODEX_FAIL" = "remove" ] && { echo "remove boom" >&2; exit 1; }
      [ -f "$st/marketplaces" ] && { grep -vx "$4" "$st/marketplaces" > "$st/m.tmp" 2>/dev/null || true; mv "$st/m.tmp" "$st/marketplaces" 2>/dev/null || true; } ;;
    list) cat "$st/marketplaces" 2>/dev/null || true ;;
  esac
  exit 0
fi
if [ "$1" = "plugin" ]; then
  case "$2" in
    add) echo "$3" >> "$st/plugins" ;;
    remove)
      # FAKE_CODEX_FAIL=remove: fail without removing, so the driver's
      # post-remove verification still finds the plugin registered.
      [ "$FAKE_CODEX_FAIL" = "remove" ] && { echo "remove boom" >&2; exit 1; }
      [ -f "$st/plugins" ] && { grep -vx "$3" "$st/plugins" > "$st/p.tmp" 2>/dev/null || true; mv "$st/p.tmp" "$st/plugins" 2>/dev/null || true; } ;;
    list) cat "$st/plugins" 2>/dev/null || true ;;
  esac
  exit 0
fi
exit 0
"#,
    );
    path
}

fn apply_codex_env(guard: &EnvGuard, world: &World, fake_bin: &Path) -> (PathBuf, PathBuf) {
    let xdg = world.prefix.join("xdg-data");
    std::fs::create_dir_all(&xdg).expect("xdg");
    let log = world.prefix.join("codex.log");
    let state = world.prefix.join("codex-state");
    guard.set("CODEX_BIN", fake_bin);
    guard.set("XDG_DATA_HOME", &xdg);
    guard.set("FAKE_CODEX_LOG", &log);
    guard.set("FAKE_CODEX_STATE", &state);
    let marketplace_root = xdg.join("anolisa").join("codex-marketplace");
    (log, marketplace_root)
}

#[test]
fn codex_enable_records_argv_and_builds_marketplace() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    let (log, marketplace_root) = apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };

    // Marketplace layout on disk.
    let manifest = marketplace_root.join(".agents/plugins/marketplace.json");
    assert!(manifest.is_file(), "marketplace.json written");
    let symlink = marketplace_root.join("tokenless");
    assert_eq!(
        std::fs::read_link(&symlink).expect("symlink"),
        world.resource_root,
        "symlink points at the resource root"
    );

    // Recorded argv: exactly the marketplace-add and plugin-add commands.
    let log_text = std::fs::read_to_string(&log).expect("codex log");
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("plugin marketplace add {}", marketplace_root.display())),
        "must run `plugin marketplace add <root>`: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin add tokenless@anolisa-tokenless"),
        "must run `plugin add tokenless@anolisa-tokenless`: {log_text}"
    );

    // Receipt carries the marketplace + symlink + plugin resources.
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::FrameworkMarketplace { marketplace, .. } if marketplace == "anolisa-tokenless"
    )));
    assert!(
        claim
            .resources
            .iter()
            .any(|r| matches!(&r.kind, ClaimResourceKind::Symlink { .. }))
    );

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    let disabled = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(
        !marketplace_root.exists(),
        "marketplace dir removed on disable"
    );
    let log_text = std::fs::read_to_string(&log).expect("codex log");
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin remove tokenless@anolisa-tokenless"),
        "disable must run `plugin remove`: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin marketplace remove anolisa-tokenless"),
        "disable must run `plugin marketplace remove`: {log_text}"
    );
}

#[test]
fn codex_enable_trusts_declared_hooks() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_hook_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);

    world
        .manager()
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    let request: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.prefix.join("codex-state/trust-request"))
            .expect("trust request"),
    )
    .expect("valid request");
    assert_eq!(request["method"], "config/batchWrite");
    assert_eq!(
        request["params"]["edits"][0]["value"]["tokenless@anolisa-tokenless:hooks/hooks.json:pre_tool_use:0:0"]
            ["trusted_hash"],
        "sha256:trusted"
    );
}

#[test]
fn codex_enable_fails_when_declared_hooks_are_not_discovered() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_hook_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);
    guard.set("FAKE_CODEX_FAIL", Path::new("hooks"));

    let error = world
        .manager()
        .enable(COMPONENT, Some("codex"), false)
        .expect_err("missing hooks must fail enable");
    assert!(error.to_string().contains("reported no hooks"), "{error}");
}

#[test]
fn codex_enable_fails_when_hook_trust_is_overridden() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_hook_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);
    guard.set("FAKE_CODEX_FAIL", Path::new("write-overridden"));

    let error = world
        .manager()
        .enable(COMPONENT, Some("codex"), false)
        .expect_err("overridden hook trust must fail enable");
    assert!(
        error.to_string().contains("sessionFlags"),
        "overriding layer must be actionable: {error}"
    );
}

/// Regression for the unified raw/RPM contract: an RPM-installed component
/// whose contract declares `[adapters.backends.rpm].resource_root` outside
/// every datadir must enable through the real production path — the
/// receipt's marketplace symlink targets the external RPM root, and that
/// target validates against contract-derived trust (never the receipt
/// itself). status and disable keep working over the persisted receipt.
#[test]
fn codex_enable_rpm_backend_root_outside_datadir() {
    let guard = EnvGuard::acquire();
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    let (_log, marketplace_root) = apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable must accept the contract-declared RPM root")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };

    // The marketplace symlink targets the external RPM root.
    let symlink = marketplace_root.join(COMPONENT);
    assert_eq!(
        std::fs::read_link(&symlink).expect("symlink"),
        world.resource_root,
        "symlink must point at the RPM-provided root"
    );
    assert!(
        claim.resources.iter().any(|r| matches!(
            &r.kind,
            ClaimResourceKind::Symlink { target, .. } if target == &world.resource_root
        )),
        "receipt must record the RPM root as the symlink target"
    );

    // The persisted receipt keeps validating on the read paths.
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    let disabled = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(
        !marketplace_root.exists(),
        "marketplace dir removed on disable"
    );
    assert!(
        world
            .resource_root
            .join(".codex-plugin/plugin.json")
            .is_file(),
        "disable must never touch the RPM-owned root"
    );
}

/// Regression for RPM root migration: enable on root A, then an RPM update
/// moves the payload to root B and refreshes the contract snapshot (A is
/// removed). The enabled receipt still points at A — it must stay
/// manageable: status keeps reporting (degraded, not an error), re-enable
/// migrates the symlink to B, and disable cleans up. The enable-time trust
/// anchor — not the receipt — is what keeps A trusted.
#[test]
fn codex_receipt_survives_rpm_root_migration() {
    let guard = EnvGuard::acquire();
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    let (_log, marketplace_root) = apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable on root A");

    // RPM update: payload moves A -> B (with updated content), contract
    // snapshots point at B, A disappears.
    let root_b = world.prefix.join("opt").join("tokenless-plugin-v2");
    std::fs::create_dir_all(&root_b).expect("root B");
    stage_codex_bundle(&root_b);
    std::fs::write(root_b.join("README.md"), b"codex plugin v2\n").expect("v2 readme");
    let contract = format!(
        r#"[component]
name = "{COMPONENT}"
version = "0.7.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "codex"
adapter_type = "plugin"
plugin_id = "{COMPONENT}"
dest = "{{datadir}}/adapters/{{component}}/codex/"

[adapters.backends.rpm]
resource_root = "{root_b}/"
"#,
        root_b = root_b.display(),
    );
    for path in [
        world
            .layout
            .state_dir
            .join("component-manifests")
            .join(COMPONENT)
            .join("component.toml"),
        world
            .layout
            .datadir
            .join("components")
            .join(COMPONENT)
            .join("component.toml"),
    ] {
        std::fs::write(&path, &contract).expect("refresh contract");
    }
    std::fs::remove_dir_all(&world.resource_root).expect("remove root A");

    // status must keep reporting the stale receipt, never fail claim
    // validation — the receipt would otherwise be unmanageable. The
    // summary must degrade: the marketplace symlink dangles at the
    // vanished root A, so codex cannot actually serve the plugin no
    // matter what its registration lists say.
    let status = manager
        .status(Some(COMPONENT))
        .expect("status must survive the root migration");
    assert_eq!(status.entries.len(), 1);
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "a receipt pointing at a vanished root must not report Healthy"
    );
    let bundle = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::SourceRevisionMatches)
        .expect("source revision condition present");
    assert_ne!(
        bundle.status,
        ConditionStatus::True,
        "vanished enable-time root must not verify as matching"
    );

    // re-enable migrates the marketplace symlink to root B.
    let claim = match manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("re-enable must migrate to root B")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    assert_eq!(
        std::fs::read_link(marketplace_root.join(COMPONENT)).expect("symlink"),
        root_b,
        "symlink must be rewritten to the new RPM root"
    );
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::Symlink { target, .. } if target == &root_b
    )));

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Healthy,
        "re-enable on root B must restore Healthy"
    );

    let disabled = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(!marketplace_root.exists());
}

#[test]
fn codex_dry_run_enable_writes_nothing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    let (log, marketplace_root) = apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    let outcome = manager
        .enable(COMPONENT, Some("codex"), true)
        .expect("dry-run");
    assert!(matches!(outcome, EnableOutcome::Planned { .. }));
    assert!(
        !marketplace_root.exists(),
        "dry-run must not create marketplace dir"
    );
    assert!(
        !log.exists(),
        "dry-run must not invoke the codex CLI (no log file)"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "codex")
            .is_none(),
        "dry-run must not persist a receipt"
    );
}

#[test]
fn codex_forged_symlink_target_rejected_by_status() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    // Tamper: repoint the symlink resource's target at /etc.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/etc/cron.d/evil");
            }
        }
    }
    state.save(&state_path).expect("save tampered state");

    let err = manager
        .status(Some(COMPONENT))
        .expect_err("forged symlink target must be rejected");
    assert!(
        matches!(err, AdapterError::ClaimValidation(_)),
        "got {err:?}"
    );
}

#[test]
fn codex_forged_resource_root_and_symlink_target_rejected() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    // Forge BOTH the receipt's resource_root and the symlink target to
    // /etc. The symlink target is validated against the trusted layout, not
    // the receipt, so this must still be rejected.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        claim.resource_root = PathBuf::from("/etc");
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/etc/cron.d/evil");
            }
        }
    }
    state.save(&state_path).expect("save tampered state");

    let err = manager
        .status(Some(COMPONENT))
        .expect_err("forged resource_root must not authorize a forged symlink target");
    assert!(
        matches!(err, AdapterError::ClaimValidation(_)),
        "got {err:?}"
    );
}

/// A forged state file that plants both a trust anchor at /etc and a
/// receipt symlink target inside it must still be rejected: the anchor is
/// only honoured when the on-disk contract grants external-root trust for
/// this provenance (RPM provenance + declared RPM root). For a raw
/// component the state file alone is not a trust source.
#[test]
fn codex_forged_anchor_does_not_authorize_forged_target() {
    let guard = EnvGuard::acquire();
    // Real RPM provenance and a contract-declared external root: the one
    // branch where the enable-time anchor is honoured. A forged anchor
    // must still authorize nothing beneath itself — it is an
    // exact-equality allowance, never a root.
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    // Tamper: replace the legitimate anchor with /etc AND point the
    // symlink resource below it.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    state.upsert_adapter_trust_root(COMPONENT, "codex", PathBuf::from("/etc"));
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/etc/cron.d/evil");
            }
        }
    }
    state.save(&state_path).expect("save tampered state");

    let err = manager
        .status(Some(COMPONENT))
        .expect_err("forged anchor must not authorize a target beneath it");
    assert!(
        matches!(err, AdapterError::ClaimValidation(_)),
        "got {err:?}"
    );
    let err = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect_err("disable must refuse the forged target too");
    assert!(
        matches!(err, AdapterError::ClaimValidation(_)),
        "got {err:?}"
    );
}

/// Reverse lifecycle hole: after the RPM component is gone — bundle root,
/// contract snapshots and installation record all removed — the enable-time
/// anchor must keep the stale external-root receipt reportable and
/// disable must clean it up rather than wedge on `ClaimValidation`.
#[test]
fn codex_stale_external_receipt_cleans_up_after_component_removal() {
    let guard = EnvGuard::acquire();
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    let (_log, marketplace_root) = apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    // Simulate `rpm -e` plus `anolisa uninstall`: bundle root, both
    // contract copies and the installation record disappear; only the
    // adapter receipt and its anchor remain.
    std::fs::remove_dir_all(&world.resource_root).expect("remove rpm root");
    for path in [
        world
            .layout
            .state_dir
            .join("component-manifests")
            .join(COMPONENT)
            .join("component.toml"),
        world
            .layout
            .datadir
            .join("components")
            .join(COMPONENT)
            .join("component.toml"),
    ] {
        std::fs::remove_file(&path).expect("remove contract");
    }
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    state.installations.retain(|i| i.name != COMPONENT);
    state.save(&state_path).expect("save uninstalled state");

    // status: reportable, not a validation failure.
    let status = manager
        .status(Some(COMPONENT))
        .expect("status must survive component removal");
    assert_eq!(status.entries.len(), 1);

    // disable: cleans up receipt, anchor and framework registration.
    let outcome = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect("disable must clean up the stale receipt");
    assert!(outcome.claim_removed, "receipt must be removed");
    let state = world.load_state();
    assert!(
        state.find_adapter_claim(COMPONENT, "codex").is_none(),
        "receipt must be gone"
    );
    assert!(
        state.find_adapter_trust_root(COMPONENT, "codex").is_none(),
        "anchor must not outlive its receipt"
    );
    assert!(
        !marketplace_root.join(COMPONENT).exists(),
        "marketplace symlink must be cleaned up"
    );
}

#[test]
fn codex_disable_keeps_receipt_when_cli_removal_fails() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable");

    // Force the codex CLI removal commands to fail without deregistering.
    guard.set("FAKE_CODEX_FAIL", Path::new("remove"));
    let disabled = manager
        .disable(COMPONENT, Some("codex"), false)
        .expect("disable runs");
    assert!(
        !disabled.claim_removed,
        "receipt must be kept when CLI deregistration fails"
    );
    assert!(!disabled.report.cleanup_complete);
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "codex")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

/// Regression: when the resource bundle is resolved from a packaged datadir
/// registered via `push_primary_datadir_root` (exe-sibling `/usr/share`
/// differing from the install prefix's `{datadir}`), the codex plugin
/// symlink target lives outside the primary layout roots. Enable must still
/// succeed — the Manager trusts its configured datadir roots for symlink
/// target validation.
#[test]
fn codex_enable_succeeds_with_bundle_under_packaged_datadir() {
    let guard = EnvGuard::acquire();
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    // Install prefix layout (its datadir is prefix/usr/local/share/anolisa).
    let layout = FsLayout::system(Some(prefix.join("install")));
    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");

    // Packaged datadir distinct from layout.datadir — where the bundle lives.
    let packaged_datadir = prefix.join("pkg").join("usr").join("share").join("anolisa");
    let resource_root = packaged_datadir
        .join("adapters")
        .join(COMPONENT)
        .join("codex");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    stage_codex_bundle(&resource_root);

    // Contract dest expands against {datadir}; the bundle exists only under
    // the packaged datadir, so resolution lands there.
    seed_component(
        &layout,
        &layout.prefix,
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
    );
    record_owned_adapter_files(&layout, &resource_root);

    let fake = write_fake_codex(&prefix);
    let xdg = prefix.join("xdg-data");
    std::fs::create_dir_all(&xdg).expect("xdg");
    guard.set("CODEX_BIN", &fake);
    guard.set("XDG_DATA_HOME", &xdg);
    guard.set("FAKE_CODEX_LOG", &prefix.join("codex.log"));
    guard.set("FAKE_CODEX_STATE", &prefix.join("codex-state"));

    let mut manager = AdapterManager::new(layout, Some(user_home), "tester".to_string());
    manager.push_primary_datadir_root(packaged_datadir);

    let claim = match manager
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable must succeed with bundle under a packaged datadir")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    // The symlink target points at the packaged-datadir bundle, outside the
    // install-prefix layout roots — the very case that previously failed.
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::Symlink { target, .. } if target == &resource_root
    )));

    // status re-validates the receipt; it must not reject the packaged-datadir
    // symlink target either.
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn stage_claude_bundle(root: &Path) {
    std::fs::create_dir_all(root.join(".claude-plugin")).expect("claude-plugin");
    // Written multi-line with the top-level name on its own line so the
    // fake CLI's line-based `sed` reads the marketplace name (not the nested
    // plugin name) — mirrors the real multi-line manifest.
    std::fs::write(
        root.join(".claude-plugin/marketplace.json"),
        b"{\n  \"name\": \"anolisa-tokenless\",\n  \"plugins\": [{ \"name\": \"tokenless\", \"source\": \"./\" }]\n}\n",
    )
    .expect("marketplace.json");
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        br#"{"name":"tokenless","version":"0.6.0"}"#,
    )
    .expect("plugin.json");
}

/// Fake `claude` CLI: records argv and keeps marketplace/plugin registries.
fn write_fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("claude");
    write_exec(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_CLAUDE_LOG"
st="$FAKE_CLAUDE_STATE"; mkdir -p "$st" 2>/dev/null
if [ "$1" = "plugin" ]; then
  case "$2" in
    validate) exit 0 ;;
    marketplace)
      case "$3" in
        add)
          name=$(sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$4/.claude-plugin/marketplace.json" | head -n1)
          echo "$name" >> "$st/marketplaces" ;;
        remove) [ -f "$st/marketplaces" ] && { grep -vx "$4" "$st/marketplaces" > "$st/m.tmp" 2>/dev/null || true; mv "$st/m.tmp" "$st/marketplaces" 2>/dev/null || true; } ;;
        list) cat "$st/marketplaces" 2>/dev/null || true ;;
      esac ;;
    install) echo "$3" >> "$st/plugins" ;;
    uninstall) [ -f "$st/plugins" ] && { grep -vx "$3" "$st/plugins" > "$st/p.tmp" 2>/dev/null || true; mv "$st/p.tmp" "$st/plugins" 2>/dev/null || true; } ;;
    list) cat "$st/plugins" 2>/dev/null || true ;;
  esac
  exit 0
fi
exit 0
"#,
    );
    path
}

fn apply_claude_env(guard: &EnvGuard, world: &World, fake_bin: &Path) -> PathBuf {
    let log = world.prefix.join("claude.log");
    let state = world.prefix.join("claude-state");
    guard.set("CLAUDE_BIN", fake_bin);
    guard.set("FAKE_CLAUDE_LOG", &log);
    guard.set("FAKE_CLAUDE_STATE", &state);
    log
}

#[test]
fn claude_code_enable_records_validate_marketplace_and_install() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "claude-code",
        "plugin",
        "{datadir}/adapters/{component}/claude-code/",
        stage_claude_bundle,
    );
    let fake = write_fake_claude(&world.prefix);
    let log = apply_claude_env(&guard, &world, &fake);

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("claude-code"), false)
        .expect("enable")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    assert_eq!(claim.plugin_id.as_deref(), Some("tokenless"));

    let log_text = std::fs::read_to_string(&log).expect("claude log");
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("plugin validate {}", world.resource_root.display())),
        "must validate the bundle: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("plugin marketplace add {}", world.resource_root.display())),
        "must add the marketplace: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin install tokenless@anolisa-tokenless"),
        "must install the plugin: {log_text}"
    );

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    let disabled = manager
        .disable(COMPONENT, Some("claude-code"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    let log_text = std::fs::read_to_string(&log).expect("claude log");
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin uninstall tokenless@anolisa-tokenless"),
        "disable must uninstall the plugin: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == "plugin marketplace remove anolisa-tokenless"),
        "disable must remove the marketplace: {log_text}"
    );
}

#[test]
fn claude_code_disable_without_cli_keeps_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "claude-code",
        "plugin",
        "{datadir}/adapters/{component}/claude-code/",
        stage_claude_bundle,
    );
    let fake = write_fake_claude(&world.prefix);
    apply_claude_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("claude-code"), false)
        .expect("enable");

    // Point CLAUDE_BIN at a missing path: disable cannot run the CLI and
    // must NOT hand-edit settings.json, so it keeps the receipt.
    guard.set("CLAUDE_BIN", &world.prefix.join("no-such-claude"));
    let disabled = manager
        .disable(COMPONENT, Some("claude-code"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed, "receipt kept when CLI absent");
    assert!(!disabled.report.cleanup_complete);
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "claude-code")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

/// A receipt missing its marketplace resource (malformed/forged) must not
/// drive `plugin uninstall` / `marketplace remove` against a name derived
/// from context: status degrades and disable keeps the receipt without
/// running any CLI.
#[test]
fn claude_code_fails_closed_without_marketplace_resource() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "claude-code",
        "plugin",
        "{datadir}/adapters/{component}/claude-code/",
        stage_claude_bundle,
    );
    let fake = write_fake_claude(&world.prefix);
    let log = apply_claude_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("claude-code"), false)
        .expect("enable");

    // Tamper: drop the FrameworkMarketplace resource, leaving the payload's
    // dangling reference — as a forged/malformed receipt would.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        claim
            .resources
            .retain(|r| !matches!(r.kind, ClaimResourceKind::FrameworkMarketplace { .. }));
    }
    state.save(&state_path).expect("save tampered state");

    // status must not report healthy, and must not run the CLI.
    let log_before = std::fs::read_to_string(&log).unwrap_or_default();
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);

    // disable must keep the receipt and run no CLI removal.
    let disabled = manager
        .disable(COMPONENT, Some("claude-code"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed, "malformed receipt must be kept");
    assert!(!disabled.report.cleanup_complete);
    let log_after = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        log_before, log_after,
        "no framework CLI must run for a receipt with no marketplace resource"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "claude-code")
            .is_some(),
        "receipt kept for manual resolution"
    );
}

// ---------------------------------------------------------------------------
// Qoder
// ---------------------------------------------------------------------------

fn stage_qoder_bundle(root: &Path) {
    std::fs::create_dir_all(root.join(".qoder-plugin")).expect("qoder-plugin");
    std::fs::write(
        root.join(".qoder-plugin/plugin.json"),
        br#"{"name":"tokenless","version":"0.6.0"}"#,
    )
    .expect("plugin.json");
    // Hooks carry the ${QODER_TOKENLESS_HOOKS} placeholder and tokenless-*
    // hook names, mirroring the shipped bundle.
    std::fs::write(
        root.join("hooks.json"),
        br#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": "", "hooks": [
        { "type": "command", "name": "tokenless-rewrite",
          "command": "python3 ${QODER_TOKENLESS_HOOKS}/rewrite_hook.py" } ] }
    ],
    "PostToolUse": [
      { "matcher": "", "hooks": [
        { "type": "command", "name": "tokenless-compress-response",
          "command": "python3 ${QODER_TOKENLESS_HOOKS}/compress_response_hook.py" } ] }
    ]
  }
}
"#,
    )
    .expect("hooks.json");
}

/// Qoder-native fixture using the same nested hook layout as sec-core.
fn stage_native_qoder_bundle(root: &Path) {
    std::fs::create_dir_all(root.join(".qoder-plugin")).expect("qoder-plugin");
    std::fs::create_dir_all(root.join("hooks")).expect("hooks");
    std::fs::write(
        root.join(".qoder-plugin/plugin.json"),
        br#"{"name":"tokenless","version":"0.6.0"}"#,
    )
    .expect("plugin.json");
    std::fs::write(
        root.join("hooks/hooks.json"),
        br#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": "", "hooks": [
        { "type": "command", "command": "python3",
          "args": ["${QODER_PLUGIN_ROOT}/hooks/observability_hook.py"] } ] }
    ]
  }
}
"#,
    )
    .expect("hooks/hooks.json");
    std::fs::write(
        root.join("hooks/observability_hook.py"),
        b"print('observability')\n",
    )
    .expect("observability hook");
}

fn set_native_qoder_plugin_id(world: &World, plugin_id: &str) {
    std::fs::write(
        world.resource_root.join(".qoder-plugin/plugin.json"),
        format!(r#"{{"name":"{plugin_id}","version":"0.6.0"}}"#),
    )
    .expect("replace native plugin id");

    let contract_path = world
        .layout
        .state_dir
        .join("component-manifests")
        .join(COMPONENT)
        .join("component.toml");
    let contract = std::fs::read_to_string(&contract_path).expect("read component contract");
    let prior = format!("plugin_id = \"{COMPONENT}\"");
    assert_eq!(contract.matches(&prior).count(), 1);
    std::fs::write(
        contract_path,
        contract.replace(&prior, &format!("plugin_id = \"{plugin_id}\"")),
    )
    .expect("replace contract plugin id");
}

/// Fake `qodercli`: records argv, mirrors the local plugin cache, and emits
/// the JSON inventory used by the native lifecycle. Failure modes cover
/// read-only preflight, post-install verification, and uninstall retention.
fn write_fake_qodercli(dir: &Path) -> PathBuf {
    let path = dir.join("qodercli");
    write_exec(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_QODER_LOG"
cache="$FAKE_QODER_CACHE"
plugin="${FAKE_QODER_PLUGIN_ID:-tokenless}"
if [ "$1" = "plugins" ]; then
  if [ "$3" = "--help" ]; then
    [ "$FAKE_QODER_FAIL" = "help-$2" ] && { echo "unsupported $2" >&2; exit 1; }
    exit 0
  fi
  case "$2" in
    validate)
      [ "$FAKE_QODER_FAIL" = "validate" ] && { echo "validate boom" >&2; exit 1; } ;;
    install)
      [ "$FAKE_QODER_FAIL" = "install" ] && { echo "install boom" >&2; exit 1; }
      mkdir -p "$cache"
      if [ "$4" = "--scope" ]; then
        rm -rf "$cache/$plugin"
        cp -R "$3" "$cache/$plugin" 2>/dev/null
        : > "$cache/.user-$plugin"
      else
        cp -R "$3" "$cache/" 2>/dev/null
      fi ;;
    uninstall)
      [ "$FAKE_QODER_FAIL" = "uninstall" ] && { echo "uninstall boom" >&2; exit 1; }
      [ "$FAKE_QODER_FAIL" = "uninstall-retain" ] && exit 0
      rm -f "$cache/.user-$3"
      if [ "$FAKE_QODER_PROJECT_PLUGIN" != "1" ]; then
        rm -rf "$cache/$3" 2>/dev/null || true
      fi ;;
    list)
      [ "$FAKE_QODER_FAIL" = "list" ] && { echo "list boom" >&2; exit 1; }
      if [ "$FAKE_QODER_FAIL" = "list-invalid" ]; then
        printf '%s\n' '{invalid'
      elif [ "$FAKE_QODER_FAIL" = "post-list-fail" ] && [ -d "$cache/$plugin" ]; then
        echo "post-install list boom" >&2
        exit 1
      elif [ "$FAKE_QODER_LARGE_INVENTORY" = "1" ]; then
        printf '['
        i=0
        while [ "$i" -lt 900 ]; do
          [ "$i" -gt 0 ] && printf ','
          printf '{"id":"filler-%s@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}' "$i"
          i=$((i + 1))
        done
        if [ -d "$cache/$plugin" ]; then
          printf ',{"id":"%s@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}' "$plugin"
        fi
        printf ']\n'
      elif [ "$FAKE_QODER_PROJECT_PLUGIN" = "1" ]; then
        if [ -f "$cache/.user-$plugin" ]; then
          printf '[{"id":"%s@local","scope":"project","enabled":true,"resources":{"hooks":[{}]}},{"id":"%s@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}]\n' "$plugin" "$plugin"
        else
          printf '[{"id":"%s@local","scope":"project","enabled":true,"resources":{"hooks":[{}]}}]\n' "$plugin"
        fi
      elif [ ! -d "$cache/$plugin" ] || [ "$FAKE_QODER_FAIL" = "post-list-absent" ]; then
        printf '%s\n' '[]'
      elif [ "$FAKE_QODER_FAIL" = "post-list-invalid" ]; then
        printf '%s\n' '{invalid'
      elif [ "$FAKE_QODER_FAIL" = "post-list-disabled" ]; then
        printf '[{"id":"%s@local","scope":"user","enabled":false,"resources":{"hooks":[{}]}}]\n' "$plugin"
      elif [ "$FAKE_QODER_FAIL" = "post-list-no-hooks" ]; then
        printf '[{"id":"%s@local","scope":"user","enabled":true,"resources":{"hooks":[]}}]\n' "$plugin"
      else
        printf '[{"id":"%s@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}]\n' "$plugin"
      fi ;;
  esac
  exit 0
fi
exit 0
"#,
    );
    path
}

/// Returns `(log, settings_path, cache_dir, staging_dir)`.
fn apply_qoder_env(
    guard: &EnvGuard,
    world: &World,
    fake_bin: &Path,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let xdg = world.prefix.join("xdg-data");
    std::fs::create_dir_all(&xdg).expect("xdg");
    let log = world.prefix.join("qoder.log");
    let cache = world
        .user_home
        .join(".qoder")
        .join("plugins")
        .join("cache")
        .join("local");
    guard.set("QODERCLI_BIN", fake_bin);
    guard.set("XDG_DATA_HOME", &xdg);
    guard.set("FAKE_QODER_LOG", &log);
    guard.set("FAKE_QODER_CACHE", &cache);
    let settings = world.user_home.join(".qoder").join("settings.json");
    let staging = xdg.join("anolisa").join("qoder-plugins").join("tokenless");
    (log, settings, cache, staging)
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("read settings.json");
    serde_json::from_str(&text).expect("parse settings.json")
}

fn hook_names(settings: &serde_json::Value, event: &str) -> Vec<String> {
    settings["hooks"][event]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e["hooks"][0]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn hook_command(settings: &serde_json::Value, event: &str, name: &str) -> Option<String> {
    settings["hooks"][event].as_array().and_then(|arr| {
        arr.iter().find_map(|entry| {
            let hook = entry["hooks"].as_array()?.first()?;
            (hook["name"].as_str()? == name)
                .then(|| hook["command"].as_str().map(str::to_string))
                .flatten()
        })
    })
}

fn enabled_plugins(settings: &serde_json::Value) -> Vec<String> {
    settings["plugins"]["enabled"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn qoder_native_enable_status_disable_uses_cli_lifecycle() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, settings, cache, staging) = apply_qoder_env(&guard, &world, &fake);
    let original_settings = b"{\n  \"theme\": \"night\"\n}\n";
    std::fs::create_dir_all(settings.parent().expect("settings parent")).expect("mkdir .qoder");
    std::fs::write(&settings, original_settings).expect("seed settings");

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable native plugin")
    {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };

    // Not a Qoder-specific contract: every driver writes the shared
    // payload schema version, so assert against the constant rather than a
    // literal that every future bump would have to chase.
    assert_eq!(claim.driver_schema, DRIVER_SCHEMA_VERSION);
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    for command in [
        "plugins validate --help".to_string(),
        "plugins install --help".to_string(),
        "plugins list --help".to_string(),
        "plugins uninstall --help".to_string(),
        format!("plugins validate {}", world.resource_root.display()),
        format!(
            "plugins install {} --scope user",
            world.resource_root.display()
        ),
    ] {
        assert!(
            log_text.lines().any(|line| line == command),
            "missing native command {command:?} in {log_text}"
        );
    }
    assert!(
        !staging.exists(),
        "native install must not create a staging copy"
    );
    assert_eq!(
        std::fs::read(&settings).expect("settings unchanged"),
        original_settings,
        "native lifecycle must leave settings.json byte-for-byte unchanged"
    );
    let source_hooks = std::fs::read_to_string(world.resource_root.join("hooks/hooks.json"))
        .expect("source hooks");
    let cached_hooks =
        std::fs::read_to_string(cache.join("tokenless/hooks/hooks.json")).expect("cached hooks");
    assert!(source_hooks.contains("${QODER_PLUGIN_ROOT}"));
    assert!(cached_hooks.contains("${QODER_PLUGIN_ROOT}"));

    assert_eq!(claim.resources.len(), 1);
    assert!(matches!(
        &claim.resources[0].kind,
        ClaimResourceKind::FrameworkPlugin { framework, plugin_id }
            if framework == "qoder" && plugin_id == "tokenless"
    ));
    let DriverPayload::Qoder(payload) = &claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(payload.settings_resource.is_none());
    assert!(!payload.plugin_preexisting);
    assert!(payload.plugin_install_confirmed);
    assert!(payload.managed_hooks.is_empty());
    assert!(payload.managed_hook_specs.is_empty());

    let status = manager.status(Some(COMPONENT)).expect("native status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
    for kind in [
        AdapterConditionKind::PluginRegistered,
        AdapterConditionKind::ActivationEnabled,
        AdapterConditionKind::PluginResourcesLoaded,
        AdapterConditionKind::VerificationSupported,
    ] {
        assert!(status.entries[0].report.conditions.iter().any(|condition| {
            condition.kind == kind && condition.status == ConditionStatus::True
        }));
    }

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable native plugin");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(!cache.join("tokenless").exists());
    assert_eq!(
        std::fs::read(&settings).expect("settings unchanged after disable"),
        original_settings
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user")
    );
}

#[test]
fn qoder_native_enable_tolerates_unavailable_validate_command() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_FAIL", Path::new("help-validate"));

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("native install does not require validate support");
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text
            .lines()
            .any(|line| line == "plugins validate --help")
    );
    assert!(
        !log_text
            .lines()
            .any(|line| { line == format!("plugins validate {}", world.resource_root.display()) }),
        "unsupported validate command must not be invoked: {log_text}"
    );
    assert!(log_text.lines().any(|line| {
        line == format!(
            "plugins install {} --scope user",
            world.resource_root.display()
        )
    }));
    assert!(
        manager
            .disable(COMPONENT, Some("qoder"), false)
            .expect("disable")
            .claim_removed
    );
}

#[test]
fn qoder_native_enable_rejects_same_id_project_scope_with_shared_cache() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_PROJECT_PLUGIN", Path::new("1"));
    let shared_cache = cache.join("tokenless");
    std::fs::create_dir_all(&shared_cache).expect("project plugin shared cache");
    std::fs::write(shared_cache.join("owner.txt"), b"project-owned\n")
        .expect("project ownership marker");

    let manager = world.manager();
    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("project registration must block the shared-cache install");
    assert!(
        matches!(&error, AdapterError::FrameworkCli { reason, .. }
            if reason.contains("matching non-user Qoder registration")
                && reason.contains("project")
                && reason.contains("shares the local plugin cache")),
        "unexpected project-scope conflict: {error:?}"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .is_none(),
        "scope conflict must fail before the write-ahead receipt"
    );
    assert_eq!(
        std::fs::read(shared_cache.join("owner.txt")).expect("project marker retained"),
        b"project-owned\n"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        !log_text.lines().any(|line| {
            (line.starts_with("plugins install ") || line.starts_with("plugins uninstall "))
                && line.ends_with(" --scope user")
        }),
        "scope conflict must not mutate the shared cache: {log_text}"
    );
}

#[test]
fn qoder_native_lifecycle_reads_inventory_larger_than_default_capture() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_LARGE_INVENTORY", Path::new("1"));

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable must parse the target after more than 64 KiB of inventory");
    assert!(cache.join("tokenless").exists());
    assert_eq!(
        manager.status(Some(COMPONENT)).expect("status").entries[0]
            .report
            .summary,
        AdapterSummary::Healthy
    );

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable must verify absence in the large inventory");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(!cache.join("tokenless").exists());
}

#[test]
fn qoder_native_preflight_failures_do_not_write_receipts() {
    let guard = EnvGuard::acquire();
    for failure in ["help-install", "validate", "list", "list-invalid"] {
        let world = stage(
            "qoder",
            "plugin",
            "{datadir}/adapters/{component}/qoder/",
            stage_native_qoder_bundle,
        );
        let fake = write_fake_qodercli(&world.prefix);
        let (_log, settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
        guard.set("FAKE_QODER_FAIL", Path::new(failure));

        world
            .manager()
            .enable(COMPONENT, Some("qoder"), false)
            .expect_err("native preflight must fail");
        assert!(
            world
                .load_state()
                .find_adapter_claim(COMPONENT, "qoder")
                .is_none(),
            "preflight failure {failure} must not persist a receipt"
        );
        assert!(!cache.join("tokenless").exists());
        assert!(!settings.exists());
    }
}

#[test]
fn qoder_native_apply_failures_keep_write_ahead_receipt() {
    let guard = EnvGuard::acquire();
    for failure in [
        "install",
        "post-list-fail",
        "post-list-invalid",
        "post-list-absent",
        "post-list-disabled",
        "post-list-no-hooks",
    ] {
        let world = stage(
            "qoder",
            "plugin",
            "{datadir}/adapters/{component}/qoder/",
            stage_native_qoder_bundle,
        );
        let fake = write_fake_qodercli(&world.prefix);
        let (log, settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
        let original_settings = b"{\"keep\":true}\n";
        std::fs::create_dir_all(settings.parent().expect("settings parent")).expect("mkdir .qoder");
        std::fs::write(&settings, original_settings).expect("seed settings");
        guard.set("FAKE_QODER_FAIL", Path::new(failure));

        world
            .manager()
            .enable(COMPONENT, Some("qoder"), false)
            .expect_err("native apply must fail");
        let claim = world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .cloned()
            .expect("write-ahead receipt kept");
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
        let DriverPayload::Qoder(payload) = &claim.driver_payload else {
            panic!("expected Qoder receipt payload");
        };
        assert_eq!(
            payload.plugin_install_confirmed,
            failure != "install",
            "only a successful install command may confirm ownership"
        );
        assert_eq!(
            std::fs::read(&settings).expect("settings unchanged"),
            original_settings
        );
        assert_eq!(
            cache.join("tokenless").exists(),
            failure != "install",
            "post-install failures keep qodercli's installed plugin"
        );
        let log_text = std::fs::read_to_string(&log).expect("qoder log");
        assert!(
            !log_text
                .lines()
                .any(|line| line == "plugins uninstall tokenless --scope user"),
            "enable verification failure must not guess whether uninstall is safe"
        );
    }
}

#[test]
fn qoder_native_failed_install_never_adopts_later_user_plugin() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_FAIL", Path::new("install"));
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("initial install fails before creating a plugin");

    let failed_claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("failed write-ahead receipt retained");
    assert_eq!(failed_claim.status, ClaimStatus::CleanupFailed);
    let DriverPayload::Qoder(payload) = &failed_claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(!payload.plugin_preexisting);
    assert!(!payload.plugin_install_confirmed);

    let user_plugin = cache.join("tokenless");
    std::fs::create_dir_all(&user_plugin).expect("user-installed plugin");
    std::fs::write(user_plugin.join("owner.txt"), b"user-owned\n").expect("user ownership marker");
    guard.set("FAKE_QODER_FAIL", Path::new(""));

    let retry_error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("retry must not adopt the user's plugin");
    assert!(
        matches!(&retry_error, AdapterError::FrameworkCli { reason, .. }
            if reason.contains("pre-existing user-scope")),
        "unexpected retry error: {retry_error:?}"
    );
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable fails closed for unconfirmed ownership");
    assert!(!disabled.claim_removed);
    assert!(!disabled.report.cleanup_complete);
    assert_eq!(
        std::fs::read(user_plugin.join("owner.txt")).expect("user plugin retained"),
        b"user-owned\n"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert_eq!(
        log_text
            .lines()
            .filter(|line| {
                line.starts_with("plugins install ") && line.ends_with(" --scope user")
            })
            .count(),
        1,
        "retry must not run another install: {log_text}"
    );
    assert!(
        !log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user"),
        "disable must not uninstall an unconfirmed registration: {log_text}"
    );

    std::fs::remove_dir_all(&user_plugin).expect("user removes their plugin");
    let cleaned = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("absence permits receipt cleanup");
    assert!(cleaned.claim_removed);
    assert!(cleaned.report.cleanup_complete);
}

#[test]
fn qoder_native_post_install_failure_keeps_confirmed_cleanup_ownership() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_FAIL", Path::new("post-list-disabled"));
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("post-install verification fails");
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("cleanup receipt retained");
    let DriverPayload::Qoder(payload) = &claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(payload.plugin_install_confirmed);

    guard.set("FAKE_QODER_FAIL", Path::new(""));
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("confirmed install remains eligible for cleanup");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(!cache.join("tokenless").exists());
    assert!(
        std::fs::read_to_string(&log)
            .expect("qoder log")
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user")
    );
}

#[test]
fn qoder_native_v2_preapply_receipt_never_claims_later_user_plugin() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    guard.set("FAKE_QODER_FAIL", Path::new("install"));
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("create an unconfirmed write-ahead receipt without a plugin");

    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    let claim = state
        .adapter_claims
        .iter_mut()
        .find(|claim| claim.component == COMPONENT && claim.framework == "qoder")
        .expect("qoder claim");
    // Schema v2 had no durable post-install checkpoint. `Enabled` is the
    // exact pre-apply state the Manager persisted, so a crash could leave
    // this value even though qodercli never created the registration.
    claim.driver_schema = 2;
    claim.status = ClaimStatus::Enabled;
    let DriverPayload::Qoder(payload) = &mut claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    payload.plugin_install_confirmed = false;
    state.save(&state_path).expect("save v2 receipt fixture");

    let user_plugin = cache.join("tokenless");
    std::fs::create_dir_all(&user_plugin).expect("later user plugin");
    std::fs::write(user_plugin.join("owner.txt"), b"user-owned\n").expect("user ownership marker");
    guard.set("FAKE_QODER_FAIL", Path::new(""));

    let retry_error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("v2 receipt must not authorize replacing the later user plugin");
    assert!(
        matches!(&retry_error, AdapterError::FrameworkCli { reason, .. }
            if reason.contains("pre-existing user-scope")),
        "unexpected v2 retry error: {retry_error:?}"
    );
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("v2 receipt must keep unconfirmed registration untouched");
    assert!(!disabled.claim_removed);
    assert!(!disabled.report.cleanup_complete);
    assert_eq!(
        std::fs::read(user_plugin.join("owner.txt")).expect("user marker retained"),
        b"user-owned\n"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert_eq!(
        log_text
            .lines()
            .filter(|line| {
                line.starts_with("plugins install ") && line.ends_with(" --scope user")
            })
            .count(),
        1,
        "retry must not run a second install: {log_text}"
    );
    assert!(
        !log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user"),
        "disable must not uninstall the later user registration: {log_text}"
    );
}

/// Rewrite the persisted Qoder receipt so it reads as one written by an
/// ANOLISA whose shared `DRIVER_SCHEMA_VERSION` was still 3 — i.e. before an
/// unrelated driver's payload grew a field and the shared number moved on —
/// while keeping the install confirmation that version already recorded.
fn downgrade_qoder_receipt_to_schema_v3(world: &World) {
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    let claim = state
        .adapter_claims
        .iter_mut()
        .find(|claim| claim.component == COMPONENT && claim.framework == "qoder")
        .expect("qoder claim");
    claim.driver_schema = 3;
    let DriverPayload::Qoder(payload) = &mut claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(
        payload.plugin_install_confirmed,
        "fixture must start from a confirmed install"
    );
    state.save(&state_path).expect("save v3 receipt fixture");
}

/// A receipt written at schema 3 already proved ANOLISA installed the Native
/// plugin. Gating that proof on the *shared* write-time version instead of the
/// version that introduced the field would revoke it the moment OpenClaw's
/// payload added `displaced_plugins` and the shared constant moved to 4 —
/// disable would then refuse to uninstall a plugin ANOLISA owns and keep the
/// receipt forever.
#[test]
fn qoder_native_v3_confirmed_receipt_still_owns_its_plugin_on_disable() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("initial enable");
    assert!(cache.join("tokenless").exists(), "plugin installed");

    downgrade_qoder_receipt_to_schema_v3(&world);

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("a v3 confirmed receipt still authorizes cleanup");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(
        !cache.join("tokenless").exists(),
        "ANOLISA-owned plugin must be uninstalled, not stranded behind a kept receipt"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user"),
        "disable must run the uninstall a confirmed receipt authorizes: {log_text}"
    );
}

/// The same revoked-ownership bug on the re-enable side: with the v3 proof
/// ignored, `prior_owned` is false, the still-registered plugin is re-recorded
/// as somebody else's pre-existing one, and a later disable removes the receipt
/// while leaving the plugin installed.
#[test]
fn qoder_native_v3_confirmed_receipt_carries_ownership_across_reenable() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("initial enable");

    downgrade_qoder_receipt_to_schema_v3(&world);

    let claim = match manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("re-enable over a v3 confirmed receipt")
    {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::Qoder(payload) = &claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(
        !payload.plugin_preexisting,
        "ownership crosses the re-enable; the plugin is not re-recorded as somebody else's"
    );
    assert!(payload.plugin_install_confirmed);

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable after re-enable");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(
        !cache.join("tokenless").exists(),
        "the receipt must not be removed while the plugin it owns stays behind"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user"),
        "disable must still own the uninstall: {log_text}"
    );
}

#[test]
fn qoder_native_enable_rejects_unowned_preexisting_user_plugin() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let preexisting = cache.join("tokenless");
    std::fs::create_dir_all(&preexisting).expect("pre-existing plugin cache");
    std::fs::write(preexisting.join("owner.txt"), b"user-owned\n").expect("ownership marker");
    std::fs::write(preexisting.join("version.txt"), b"0.9.0\n").expect("pre-existing version");
    std::fs::write(world.resource_root.join("version.txt"), b"9.9.9\n")
        .expect("replacement version");

    let manager = world.manager();
    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("unowned user plugin must block enable");
    assert!(
        matches!(&error, AdapterError::FrameworkCli { reason, .. } if reason.contains("pre-existing user-scope")),
        "unexpected error: {error:?}"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .is_none(),
        "ownership conflict must fail before receipt persistence"
    );
    assert_eq!(
        std::fs::read(preexisting.join("version.txt")).expect("pre-existing version retained"),
        b"0.9.0\n"
    );
    assert_eq!(
        std::fs::read(preexisting.join("owner.txt")).expect("pre-existing plugin retained"),
        b"user-owned\n"
    );
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable without receipt is a no-op");
    assert!(!disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        !log_text
            .lines()
            .any(|line| line.starts_with("plugins install ") && line.ends_with(" --scope user")),
        "enable must not replace a pre-existing plugin: {log_text}"
    );
    assert!(
        !log_text
            .lines()
            .any(|line| line == "plugins uninstall tokenless --scope user"),
        "disable must not uninstall a pre-existing plugin: {log_text}"
    );
}

#[test]
fn qoder_native_reenable_can_replace_anolisa_owned_user_plugin() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    std::fs::write(world.resource_root.join("generation.txt"), b"first\n")
        .expect("first generation");

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("initial enable");
    std::fs::write(world.resource_root.join("generation.txt"), b"second\n")
        .expect("second generation");
    let claim = match manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("owned re-enable")
    {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::Qoder(payload) = &claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(!payload.plugin_preexisting);
    assert!(payload.plugin_install_confirmed);
    assert_eq!(
        std::fs::read(cache.join("tokenless/generation.txt")).expect("cached generation"),
        b"second\n"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert_eq!(
        log_text
            .lines()
            .filter(|line| {
                line.starts_with("plugins install ") && line.ends_with(" --scope user")
            })
            .count(),
        2
    );

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable owned plugin");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
}

#[test]
fn qoder_native_reenable_rejects_preexisting_different_plugin_id() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("initial native enable");

    let replacement = cache.join("replacement");
    std::fs::create_dir_all(&replacement).expect("pre-existing replacement plugin");
    std::fs::write(replacement.join("owner.txt"), b"user-owned\n")
        .expect("replacement ownership marker");
    set_native_qoder_plugin_id(&world, "replacement");
    record_owned_adapter_files(&world.layout, &world.resource_root);
    guard.set("FAKE_QODER_PLUGIN_ID", Path::new("replacement"));

    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("a different pre-existing plugin id must not inherit ownership");
    assert!(
        matches!(&error, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("changed from 'tokenless' to 'replacement'")),
        "unexpected error: {error:?}"
    );
    let retained = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("original receipt retained");
    assert_eq!(retained.plugin_id.as_deref(), Some("tokenless"));
    assert_eq!(
        std::fs::read(replacement.join("owner.txt")).expect("replacement plugin retained"),
        b"user-owned\n"
    );
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert_eq!(
        log_text
            .lines()
            .filter(|line| {
                line.starts_with("plugins install ") && line.ends_with(" --scope user")
            })
            .count(),
        1,
        "re-enable must not install the replacement plugin: {log_text}"
    );

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("original plugin cleanup remains available");
    assert!(disabled.claim_removed);
    assert!(!cache.join("tokenless").exists());
    assert_eq!(
        std::fs::read(replacement.join("owner.txt")).expect("replacement remains after cleanup"),
        b"user-owned\n"
    );
}

#[test]
fn qoder_native_reenable_rejects_absent_different_plugin_id() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("initial native enable");

    set_native_qoder_plugin_id(&world, "replacement");
    record_owned_adapter_files(&world.layout, &world.resource_root);
    guard.set("FAKE_QODER_PLUGIN_ID", Path::new("replacement"));
    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("an absent different plugin id must not replace the cleanup claim");
    assert!(
        matches!(&error, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("changed from 'tokenless' to 'replacement'")),
        "unexpected error: {error:?}"
    );
    assert!(!cache.join("replacement").exists());
    let retained = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("original receipt retained");
    assert_eq!(retained.plugin_id.as_deref(), Some("tokenless"));
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert_eq!(
        log_text
            .lines()
            .filter(|line| {
                line.starts_with("plugins install ") && line.ends_with(" --scope user")
            })
            .count(),
        1,
        "re-enable must not install the replacement plugin: {log_text}"
    );

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("original plugin cleanup remains available");
    assert!(disabled.claim_removed);
    assert!(!cache.join("tokenless").exists());
    assert!(!cache.join("replacement").exists());
}

#[test]
fn qoder_native_rejects_custom_manifest_before_cli_or_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let manifest_path = world
        .layout
        .state_dir
        .join("component-manifests")
        .join(COMPONENT)
        .join("component.toml");
    let mut contract = std::fs::read_to_string(&manifest_path).expect("read contract");
    contract.push_str("\n[adapters.bundle]\nentry = \"custom.json\"\n");
    std::fs::write(&manifest_path, contract).expect("write custom entry");
    std::fs::remove_file(world.resource_root.join(".qoder-plugin/plugin.json"))
        .expect("remove native manifest");
    std::fs::write(
        world.resource_root.join("custom.json"),
        br#"{"name":"tokenless","version":"9.9.9"}"#,
    )
    .expect("write ignored custom manifest");
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("custom manifest must fail before qodercli install");
    assert!(
        matches!(&error, AdapterError::BundleInvalid { reason, .. } if reason.contains(".qoder-plugin/plugin.json")),
        "unexpected error: {error:?}"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .is_none()
    );
    assert!(!cache.join("tokenless").exists());
    assert!(
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .all(|line| !line.starts_with("plugins install "))
    );
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable without receipt is a no-op");
    assert!(!disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
}

#[test]
fn qoder_native_disable_keeps_receipt_until_absence_is_verified() {
    let guard = EnvGuard::acquire();
    for failure in ["uninstall", "uninstall-retain", "list", "list-invalid"] {
        let world = stage(
            "qoder",
            "plugin",
            "{datadir}/adapters/{component}/qoder/",
            stage_native_qoder_bundle,
        );
        let fake = write_fake_qodercli(&world.prefix);
        apply_qoder_env(&guard, &world, &fake);
        guard.set("FAKE_QODER_FAIL", Path::new(""));
        let manager = world.manager();
        manager
            .enable(COMPONENT, Some("qoder"), false)
            .expect("enable native plugin");
        guard.set("FAKE_QODER_FAIL", Path::new(failure));

        let disabled = manager
            .disable(COMPONENT, Some("qoder"), false)
            .expect("disable returns a cleanup report");
        assert!(!disabled.claim_removed, "receipt kept for {failure}");
        assert!(!disabled.report.cleanup_complete);
        let claim = world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .cloned()
            .expect("receipt kept");
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    }
}

#[test]
fn qoder_cross_layout_reenable_preserves_legacy_receipt_for_cleanup() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("legacy enable");

    std::fs::remove_file(world.resource_root.join("hooks.json")).expect("remove legacy hooks");
    stage_native_qoder_bundle(&world.resource_root);
    record_owned_adapter_files(&world.layout, &world.resource_root);
    let error = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("cross-layout re-enable must be explicit");
    assert!(
        matches!(error, AdapterError::BundleInvalid { .. }),
        "unexpected error: {error:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("legacy receipt retained");
    let DriverPayload::Qoder(payload) = &claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    assert!(payload.settings_resource.is_some());

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("legacy cleanup remains available");
    assert!(disabled.claim_removed);
    let settings = read_json(&settings);
    assert!(!enabled_plugins(&settings).contains(&"tokenless@local".to_string()));
    assert!(hook_names(&settings, "PreToolUse").is_empty());
}

#[test]
fn qoder_native_disable_without_cli_keeps_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable native plugin");
    guard.set("QODERCLI_BIN", &world.prefix.join("missing-qodercli"));

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable returns a cleanup report");
    assert!(!disabled.claim_removed);
    assert!(!disabled.report.cleanup_complete);
    assert_eq!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .expect("receipt kept")
            .status,
        ClaimStatus::CleanupFailed
    );
}

#[test]
fn qoder_native_status_fails_closed_for_unverifiable_or_inconsistent_state() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_native_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable native plugin");

    guard.set("FAKE_QODER_FAIL", Path::new("list-invalid"));
    let status = manager.status(Some(COMPONENT)).expect("status report");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Unknown);
    assert!(status.entries[0].report.conditions.iter().any(|condition| {
        condition.kind == AdapterConditionKind::VerificationSupported
            && condition.status == ConditionStatus::False
    }));

    guard.set("FAKE_QODER_FAIL", Path::new(""));
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    let claim = state
        .adapter_claims
        .iter_mut()
        .find(|claim| claim.component == COMPONENT)
        .expect("claim");
    let DriverPayload::Qoder(payload) = &mut claim.driver_payload else {
        panic!("expected Qoder receipt payload");
    };
    payload.managed_hooks.push("forged-hook".to_string());
    state.save(&state_path).expect("save forged receipt");
    assert!(matches!(
        manager.status(Some(COMPONENT)),
        Err(AdapterError::BundleInvalid { .. })
    ));
}

#[test]
fn qoder_enable_installs_writes_receipt_and_merges_settings() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, settings, cache, staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    assert_eq!(claim.plugin_id.as_deref(), Some("tokenless"));

    // Recorded argv: install from the plugin-named staging copy.
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("plugins install {}", staging.display())),
        "must run `plugins install <staging>`: {log_text}"
    );

    // The verbatim bundle qodercli cached carries the patched hooks.json:
    // consumers that load it directly (the Qoder IDE shares ~/.qoder with
    // qodercli) never expand the placeholder.
    let cached_hooks = cache.join("tokenless").join("hooks.json");
    let cached = std::fs::read_to_string(&cached_hooks).expect("cached hooks.json");
    assert!(
        !cached.contains("${QODER_TOKENLESS_HOOKS}"),
        "cached hooks.json keeps no placeholder: {cached}"
    );
    assert!(
        cached.contains("common/hooks"),
        "cached hooks.json uses the absolute hooks dir: {cached}"
    );

    // settings.json merged: our hooks + tokenless@local, and the placeholder
    // was expanded to an absolute path.
    let cfg = read_json(&settings);
    assert!(hook_names(&cfg, "PreToolUse").contains(&"tokenless-rewrite".to_string()));
    assert!(hook_names(&cfg, "PostToolUse").contains(&"tokenless-compress-response".to_string()));
    assert!(enabled_plugins(&cfg).contains(&"tokenless@local".to_string()));
    let cmd = cfg["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command");
    assert!(
        !cmd.contains("${QODER_TOKENLESS_HOOKS}"),
        "placeholder expanded: {cmd}"
    );

    // Receipt carries the plugin + settings resources, no argv/script.
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::FrameworkPlugin { framework, plugin_id }
            if framework == "qoder" && plugin_id == "tokenless"
    )));
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::ExternalPath { path } if path == &settings
    )));

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    let log_text = std::fs::read_to_string(&log).expect("qoder log");
    assert!(
        log_text.lines().any(|l| l == "plugins uninstall tokenless"),
        "disable must run `plugins uninstall tokenless`: {log_text}"
    );
    // settings.json pruned of our entries; file itself preserved.
    let cfg = read_json(&settings);
    assert!(!enabled_plugins(&cfg).contains(&"tokenless@local".to_string()));
    assert!(hook_names(&cfg, "PreToolUse").is_empty());
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .is_none(),
        "receipt gone after disable"
    );
}

#[test]
fn qoder_enable_preserves_existing_user_settings() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    // Pre-existing user settings.json with the user's own theme, hook, and
    // enabled plugin.
    std::fs::create_dir_all(settings.parent().unwrap()).expect("mkdir .qoder");
    std::fs::write(
        &settings,
        br#"{
  "theme": "dark",
  "hooks": { "PreToolUse": [
    { "hooks": [ { "type": "command", "name": "user-audit" } ] },
    { "hooks": [
      { "type": "command", "name": "tokenless-my-custom-audit",
        "command": "python3 /user/audit.py" } ] } ] },
  "plugins": { "enabled": ["other@local"], "registry": "corp" }
}"#,
    )
    .expect("seed settings");

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    let cfg = read_json(&settings);
    assert_eq!(cfg["theme"], "dark", "user setting preserved");
    assert_eq!(
        cfg["plugins"]["registry"], "corp",
        "user plugin cfg preserved"
    );
    let pre = hook_names(&cfg, "PreToolUse");
    assert!(pre.contains(&"user-audit".to_string()), "user hook kept");
    assert!(
        pre.contains(&"tokenless-my-custom-audit".to_string()),
        "user hook with tokenless prefix kept"
    );
    assert!(
        pre.contains(&"tokenless-rewrite".to_string()),
        "our hook added"
    );
    let enabled = enabled_plugins(&cfg);
    assert!(enabled.contains(&"other@local".to_string()));
    assert!(enabled.contains(&"tokenless@local".to_string()));

    // Disable prunes only ANOLISA-managed entries.
    manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable");
    let cfg = read_json(&settings);
    assert_eq!(cfg["theme"], "dark");
    assert_eq!(cfg["plugins"]["registry"], "corp");
    let pre = hook_names(&cfg, "PreToolUse");
    assert!(
        pre.contains(&"user-audit".to_string()),
        "user hook survives prune"
    );
    assert!(
        pre.contains(&"tokenless-my-custom-audit".to_string()),
        "user tokenless-prefix hook survives prune"
    );
    assert!(
        !pre.contains(&"tokenless-rewrite".to_string()),
        "our hook pruned"
    );
    assert!(enabled_plugins(&cfg).contains(&"other@local".to_string()));
    assert!(!enabled_plugins(&cfg).contains(&"tokenless@local".to_string()));
}

#[test]
fn qoder_enable_replaces_same_named_hook_body() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    std::fs::create_dir_all(settings.parent().unwrap()).expect("mkdir .qoder");
    std::fs::write(
        &settings,
        br#"{
  "hooks": { "PreToolUse": [
    { "matcher": "", "hooks": [
      { "type": "command", "name": "tokenless-rewrite",
        "command": "python3 /user/rewrite.py" } ] } ] },
  "plugins": { "enabled": [] }
}"#,
    )
    .expect("seed settings");

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    let cfg = read_json(&settings);
    let pre = hook_names(&cfg, "PreToolUse");
    assert_eq!(
        pre.iter()
            .filter(|name| *name == "tokenless-rewrite")
            .count(),
        1,
        "same-name hook is replaced instead of duplicated"
    );
    let command = hook_command(&cfg, "PreToolUse", "tokenless-rewrite").expect("command");
    assert!(
        command.contains("rewrite_hook.py"),
        "managed hook body restored: {command}"
    );
    assert_eq!(
        manager.status(Some(COMPONENT)).expect("status").entries[0]
            .report
            .summary,
        AdapterSummary::Healthy
    );
}

#[test]
fn qoder_enable_leaves_non_object_settings_untouched() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    std::fs::create_dir_all(settings.parent().unwrap()).expect("mkdir .qoder");
    std::fs::write(&settings, br#"["user-placeholder"]"#).expect("seed settings");

    let manager = world.manager();
    let err = manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect_err("non-object settings must fail closed");
    assert!(
        matches!(err, AdapterError::SettingsUnparseable { .. }),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings).expect("settings untouched"),
        r#"["user-placeholder"]"#
    );
    assert!(
        !log.exists(),
        "enable must fail before invoking qodercli when settings cannot be merged"
    );
}

#[test]
fn qoder_dry_run_enable_writes_nothing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, settings, _cache, staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    let outcome = manager
        .enable(COMPONENT, Some("qoder"), true)
        .expect("dry-run");
    assert!(matches!(outcome, EnableOutcome::Planned { .. }));
    assert!(!log.exists(), "dry-run must not invoke qodercli (no log)");
    assert!(!settings.exists(), "dry-run must not write settings.json");
    assert!(!staging.exists(), "dry-run must not create the staging dir");
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qoder")
            .is_none(),
        "dry-run must not persist a receipt"
    );
}

#[test]
fn qoder_status_degraded_when_managed_entry_missing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");
    assert_eq!(
        manager.status(Some(COMPONENT)).expect("status").entries[0]
            .report
            .summary,
        AdapterSummary::Healthy
    );

    // Drop tokenless@local from plugins.enabled: status must degrade, not
    // stay healthy off the (unreliable) plugin registry.
    let mut cfg = read_json(&settings);
    cfg["plugins"]["enabled"] = serde_json::json!([]);
    std::fs::write(&settings, serde_json::to_vec_pretty(&cfg).unwrap()).expect("rewrite settings");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    // Plugin registration is reported Unknown (never faked from qodercli list).
    assert!(
        status.entries[0]
            .report
            .conditions
            .iter()
            .any(|c| c.kind == AdapterConditionKind::PluginRegistered
                && c.status == ConditionStatus::Unknown)
    );
}

#[test]
fn qoder_disable_is_idempotent() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    let first = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("first disable");
    assert!(first.claim_removed);
    assert!(first.report.cleanup_complete);

    // Second disable with no receipt is a clean no-op.
    let second = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("second disable");
    assert!(!second.claim_removed);
    assert!(second.report.cleanup_complete, "idempotent no-op");
}

#[test]
fn qoder_disable_keeps_receipt_when_uninstall_fails() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Fail uninstall without clearing the cache: the driver cannot confirm
    // removal, so cleanup is incomplete and the receipt is kept.
    guard.set("FAKE_QODER_FAIL", Path::new("uninstall"));
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed);
    assert!(!disabled.report.cleanup_complete);
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn qoder_disable_without_cli_keeps_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Point QODERCLI_BIN at a missing binary: disable cannot deregister, so
    // it keeps the receipt rather than pruning settings and faking success.
    guard.set("QODERCLI_BIN", &world.prefix.join("no-such-qodercli"));
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed, "receipt kept when CLI absent");
    assert!(!disabled.report.cleanup_complete);
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn qoder_disable_fails_closed_on_unparseable_settings() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Corrupt settings.json: disable must not overwrite it and must report
    // cleanup incomplete, keeping the receipt.
    std::fs::write(&settings, b"{ this is not json").expect("corrupt settings");
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed);
    assert!(!disabled.report.cleanup_complete);
    // The unparseable file was left byte-for-byte untouched.
    assert_eq!(
        std::fs::read_to_string(&settings).expect("read"),
        "{ this is not json"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn qoder_forged_settings_path_rejected_by_status() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Tamper: repoint the settings resource at ~/.ssh, then /etc. Both are
    // outside the driver's allowed roots, so claim validation must reject the
    // receipt before status can act on it.
    for forged in ["/home/attacker/.ssh/authorized_keys", "/etc/cron.d/evil"] {
        let state_path = world.layout.state_dir.join("installed.toml");
        let mut state = world.load_state();
        {
            let claim = state
                .adapter_claims
                .iter_mut()
                .find(|c| c.component == COMPONENT)
                .expect("claim");
            for res in &mut claim.resources {
                if let ClaimResourceKind::ExternalPath { path } = &mut res.kind {
                    *path = PathBuf::from(forged);
                }
            }
        }
        state.save(&state_path).expect("save tampered state");

        let err = manager
            .status(Some(COMPONENT))
            .expect_err("forged settings path must be rejected");
        assert!(
            matches!(err, AdapterError::ClaimValidation(_)),
            "got {err:?} for {forged}"
        );
    }
}

#[test]
fn qoder_status_degraded_when_one_managed_hook_removed() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");
    assert_eq!(
        manager.status(Some(COMPONENT)).expect("status").entries[0]
            .report
            .summary,
        AdapterSummary::Healthy
    );

    // Remove one of the two managed hooks (tokenless-compress-response) while
    // keeping tokenless-rewrite and tokenless@local. Status must degrade:
    // partial hook drift is not healthy.
    let mut cfg = read_json(&settings);
    cfg["hooks"]
        .as_object_mut()
        .expect("hooks obj")
        .remove("PostToolUse");
    std::fs::write(&settings, serde_json::to_vec_pretty(&cfg).unwrap()).expect("rewrite settings");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    // The still-present tokenless@local means plugin entry is fine; the
    // JsonKeysPresent condition is what flipped to False.
    assert!(
        status.entries[0]
            .report
            .conditions
            .iter()
            .any(|c| c.kind == AdapterConditionKind::JsonKeysPresent
                && c.status == ConditionStatus::False)
    );
}

#[test]
fn qoder_status_degraded_when_plugin_resource_missing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Drop the FrameworkPlugin resource, leaving the payload's dangling
    // reference — as a forged/malformed receipt would. Status must fail
    // closed (degraded), never healthy.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        claim
            .resources
            .retain(|r| !matches!(r.kind, ClaimResourceKind::FrameworkPlugin { .. }));
    }
    state.save(&state_path).expect("save tampered state");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
}

#[test]
fn qoder_disable_fails_closed_when_settings_resource_missing() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Drop the settings ExternalPath resource: disable must not run the CLI
    // or touch settings against a ctx-derived default; it keeps the receipt.
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        claim
            .resources
            .retain(|r| !matches!(r.kind, ClaimResourceKind::ExternalPath { .. }));
    }
    state.save(&state_path).expect("save tampered state");

    let log_before = std::fs::read_to_string(&log).unwrap_or_default();
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed, "malformed receipt must be kept");
    assert!(!disabled.report.cleanup_complete);
    let log_after = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        log_before, log_after,
        "no qodercli command may run for a receipt missing its settings resource"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qoder")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn qoder_disable_uses_receipt_hook_specs_when_bundle_removed() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    std::fs::remove_dir_all(&world.resource_root).expect("remove bundle");
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(
        disabled.claim_removed,
        "receipt removed after complete cleanup"
    );
    assert!(disabled.report.cleanup_complete);

    let cfg = read_json(&settings);
    assert!(!hook_names(&cfg, "PreToolUse").contains(&"tokenless-rewrite".to_string()));
    assert!(!hook_names(&cfg, "PostToolUse").contains(&"tokenless-compress-response".to_string()));
    assert!(!enabled_plugins(&cfg).contains(&"tokenless@local".to_string()));
}

#[test]
fn qoder_forged_resource_root_does_not_change_hook_ownership() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (_log, settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    let forged_root = world.prefix.join("forged-qoder-root");
    std::fs::create_dir_all(&forged_root).expect("forged root");
    std::fs::write(
        forged_root.join("hooks.json"),
        br#"{
  "hooks": {
    "PreToolUse": [
      { "hooks": [
        { "type": "command", "name": "tokenless-my-custom-audit",
          "command": "python3 /attacker/audit.py" } ] }
    ]
  }
}"#,
    )
    .expect("forged hooks");

    let mut cfg = read_json(&settings);
    cfg["hooks"]["PreToolUse"]
        .as_array_mut()
        .expect("pre hooks")
        .push(serde_json::json!({
            "hooks": [{
                "type": "command",
                "name": "tokenless-my-custom-audit",
                "command": "python3 /attacker/audit.py"
            }]
        }));
    std::fs::write(&settings, serde_json::to_vec_pretty(&cfg).unwrap()).expect("rewrite settings");

    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        claim.resource_root = forged_root;
    }
    state.save(&state_path).expect("save tampered state");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert!(
        status.entries[0]
            .report
            .conditions
            .iter()
            .any(|c| c.kind == AdapterConditionKind::JsonKeysPresent
                && c.status == ConditionStatus::True),
        "settings verification must use receipt specs, not forged resource_root hooks.json"
    );

    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);

    let cfg = read_json(&settings);
    let pre = hook_names(&cfg, "PreToolUse");
    assert!(
        pre.contains(&"tokenless-my-custom-audit".to_string()),
        "forged resource_root hook is not treated as ANOLISA-owned"
    );
    assert!(!pre.contains(&"tokenless-rewrite".to_string()));
}

#[test]
fn qoder_forged_settings_redirect_within_qoder_home_rejected() {
    let guard = EnvGuard::acquire();
    let world = stage(
        "qoder",
        "plugin",
        "{datadir}/adapters/{component}/qoder/",
        stage_qoder_bundle,
    );
    let fake = write_fake_qodercli(&world.prefix);
    let (log, _settings, _cache, _staging) = apply_qoder_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qoder"), false)
        .expect("enable");

    // Forge the settings resource to another file *inside* ~/.qoder. It still
    // passes the Manager's allowed-root check (the whole ~/.qoder is allowed),
    // so the driver must reject it by pinning the path to settings.json.
    let decoy = world.user_home.join(".qoder").join("other.json");
    std::fs::write(&decoy, b"{\"user\":\"data\"}").expect("seed decoy");
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    {
        let claim = state
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == COMPONENT)
            .expect("claim");
        for res in &mut claim.resources {
            if let ClaimResourceKind::ExternalPath { path } = &mut res.kind {
                *path = decoy.clone();
            }
        }
    }
    state.save(&state_path).expect("save tampered state");

    // status: the redirect is not an outright validation error (same root),
    // but the driver must fail closed to Degraded, never Healthy.
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);

    // disable: must not run the CLI or touch the decoy file.
    let log_before = std::fs::read_to_string(&log).unwrap_or_default();
    let disabled = manager
        .disable(COMPONENT, Some("qoder"), false)
        .expect("disable runs");
    assert!(
        !disabled.claim_removed,
        "receipt kept for redirected settings"
    );
    assert!(!disabled.report.cleanup_complete);
    let log_after = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        log_before, log_after,
        "no qodercli command may run for a redirected settings resource"
    );
    assert_eq!(
        std::fs::read_to_string(&decoy).expect("read decoy"),
        "{\"user\":\"data\"}",
        "the redirected file must be left untouched"
    );
}

/// Anchors are for *external* roots only. An RPM-provenance contract whose
/// `resource_root` lives inside the datadir is already covered by the
/// static trust boundary — writing an anchor for it would needlessly bump
/// the state schema to v6 and lock released 0.2.16 CLIs out of every state
/// command on a path that never needed trust migration. The external-root
/// counterpart must keep anchoring (and bumping to v6) as designed.
#[test]
fn codex_datadir_rpm_root_keeps_state_anchor_free_at_v5() {
    let guard = EnvGuard::acquire();

    // In-datadir declaration: RPM provenance, but the bundle sits under
    // the always-trusted datadir.
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let datadir_root = world
        .layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join("codex");
    std::fs::create_dir_all(&datadir_root).expect("datadir bundle dir");
    stage_codex_bundle(&datadir_root);
    let contract = format!(
        r#"[component]
name = "{COMPONENT}"
version = "0.6.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "codex"
adapter_type = "plugin"
plugin_id = "{COMPONENT}"
dest = "{{datadir}}/adapters/{{component}}/codex/"

[adapters.backends.rpm]
resource_root = "{{datadir}}/adapters/{{component}}/codex/"
"#
    );
    for path in [
        world
            .layout
            .state_dir
            .join("component-manifests")
            .join(COMPONENT)
            .join("component.toml"),
        world
            .layout
            .datadir
            .join("components")
            .join(COMPONENT)
            .join("component.toml"),
    ] {
        std::fs::write(&path, &contract).expect("rewrite contract");
    }
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);
    world
        .manager()
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable with in-datadir rpm root");
    let state = world.load_state();
    assert!(
        state.find_adapter_trust_root(COMPONENT, "codex").is_none(),
        "an in-datadir root must not be anchored"
    );
    let state_text = std::fs::read_to_string(world.layout.state_dir.join("installed.toml"))
        .expect("read state file");
    assert!(
        state_text.contains("schema_version = 5"),
        "anchor-free state must stay at v5, got:\n{}",
        state_text.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

/// External-root counterpart of
/// [`codex_datadir_rpm_root_keeps_state_anchor_free_at_v5`]: a package-owned
/// root outside the datadir still records its anchor and bumps to v6.
#[test]
fn codex_external_rpm_root_still_anchors_state_at_v6() {
    let guard = EnvGuard::acquire();
    let world = stage_rpm_backend(
        "codex",
        "plugin",
        "{datadir}/adapters/{component}/codex/",
        stage_codex_bundle,
    );
    let fake = write_fake_codex(&world.prefix);
    apply_codex_env(&guard, &world, &fake);
    world
        .manager()
        .enable(COMPONENT, Some("codex"), false)
        .expect("enable with external rpm root");
    let state = world.load_state();
    assert_eq!(
        state
            .find_adapter_trust_root(COMPONENT, "codex")
            .map(|p| p.to_path_buf()),
        Some(world.resource_root.clone()),
        "an external root must be anchored"
    );
    let state_text = std::fs::read_to_string(world.layout.state_dir.join("installed.toml"))
        .expect("read state file");
    assert!(
        state_text.contains("schema_version = 6"),
        "anchored state must be written at v6, got:\n{}",
        state_text.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

/// The anchor is consumed only for symlink *targets*
/// (`ClaimResourceKind::Symlink`), which today only Codex receipts
/// contain. A claude-code receipt over the same kind of external RPM root
/// records no symlink — an anchor for it would never be read back, only
/// bump the state schema to v6 and lock released 0.2.16 CLIs out of every
/// state command. It must stay anchor-free at v5.
#[test]
fn claude_code_external_rpm_root_needs_no_anchor_stays_v5() {
    let guard = EnvGuard::acquire();
    let world = stage_rpm_backend(
        "claude-code",
        "plugin",
        "{datadir}/adapters/{component}/claude-code/",
        stage_claude_bundle,
    );
    let fake = write_fake_claude(&world.prefix);
    apply_claude_env(&guard, &world, &fake);
    world
        .manager()
        .enable(COMPONENT, Some("claude-code"), false)
        .expect("enable with external rpm root");
    let state = world.load_state();
    assert!(
        state
            .find_adapter_trust_root(COMPONENT, "claude-code")
            .is_none(),
        "a receipt without symlink resources must not be anchored"
    );
    let state_text = std::fs::read_to_string(world.layout.state_dir.join("installed.toml"))
        .expect("read state file");
    assert!(
        state_text.contains("schema_version = 5"),
        "anchor-free state must stay at v5, got:\n{}",
        state_text.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

// ---------------------------------------------------------------------------
// QwenPaw
// ---------------------------------------------------------------------------

fn stage_qwenpaw_bundle(root: &Path) {
    std::fs::write(
        root.join("plugin.json"),
        br#"{"id":"tokenless","name":"Tokenless","version":"0.6.0","entry":{"backend":"plugin.py"}}"#,
    )
    .expect("plugin.json");
    std::fs::write(root.join("plugin.py"), b"plugin = None\n").expect("plugin.py");
    std::fs::write(root.join("requirements.txt"), b"anolisa-tokenless\n").expect("requirements");
}

/// Fake `qwenpaw` CLI. Like the real one it copies the bundle into
/// `$QWENPAW_WORKING_DIR/plugins/<id>`, asks for confirmation on uninstall,
/// and exits 0 even when an install or uninstall fails
/// (`FAKE_QWENPAW_FAIL=install|uninstall`), leaving the installed tree in
/// place.
fn write_fake_qwenpaw(dir: &Path) -> PathBuf {
    let path = dir.join("qwenpaw");
    write_exec(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_QWENPAW_LOG"
printf 'WORKING_DIR=%s\n' "${QWENPAW_WORKING_DIR:-unset}" >> "$FAKE_QWENPAW_LOG"
plugins="$QWENPAW_WORKING_DIR/plugins"
if [ "$1" = plugin ] && [ "$2" = install ]; then
  if [ "$FAKE_QWENPAW_FAIL" = install ]; then
    echo "❌ Failed to install plugin: simulated"
    exit 0
  fi
  id=$(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$3/plugin.json" | head -n1)
  dst="$plugins/$id"
  if [ -d "$dst" ] && [ "$4" != "--force" ]; then
    echo "❌ Plugin '$id' already installed. Use --force to overwrite."
    exit 0
  fi
  rm -rf "$dst"
  mkdir -p "$plugins"
  cp -R "$3" "$dst"
  exit 0
fi
if [ "$1" = plugin ] && [ "$2" = uninstall ]; then
  read -r answer
  [ "$answer" = y ] || exit 1
  if [ "$FAKE_QWENPAW_FAIL" = uninstall ]; then
    echo "❌ Failed to uninstall plugin: simulated"
    exit 0
  fi
  rm -rf "$plugins/$3"
  exit 0
fi
exit 0
"#,
    );
    path
}

fn apply_qwenpaw_env(guard: &EnvGuard, world: &World, fake_bin: &Path) -> (PathBuf, PathBuf) {
    let log = world.prefix.join("qwenpaw.log");
    guard.set("QWENPAW_BIN", fake_bin);
    guard.set("FAKE_QWENPAW_LOG", &log);
    let plugin_dir = world.user_home.join(".qwenpaw/plugins/tokenless");
    (log, plugin_dir)
}

fn stage_qwenpaw() -> World {
    stage(
        "qwenpaw",
        "plugin",
        "{datadir}/adapters/{component}/qwenpaw/",
        stage_qwenpaw_bundle,
    )
}

#[test]
fn qwenpaw_enable_status_disable_delegates_to_cli() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    let claim = match manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("enable")
    {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    assert_eq!(claim.plugin_id.as_deref(), Some("tokenless"));
    assert!(matches!(claim.driver_payload, DriverPayload::QwenPaw(_)));
    assert!(
        claim.materialized_files.is_empty(),
        "qwenpaw owns the copied tree; ANOLISA must not claim its files"
    );

    let log_text = std::fs::read_to_string(&log).expect("qwenpaw log");
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("plugin install {} --force", world.resource_root.display())),
        "must hand the bundle root to the CLI: {log_text}"
    );
    assert!(
        log_text
            .lines()
            .any(|l| l == format!("WORKING_DIR={}", world.user_home.join(".qwenpaw").display())),
        "must pin the working directory: {log_text}"
    );
    assert!(plugin_dir.join("plugin.json").is_file());
    assert!(plugin_dir.join("requirements.txt").is_file());

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
    assert!(status.entries[0].report.conditions.iter().any(|c| {
        c.kind == AdapterConditionKind::PluginRegistered && c.status == ConditionStatus::True
    }));

    let disabled = manager
        .disable(COMPONENT, Some("qwenpaw"), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    let log_text = std::fs::read_to_string(&log).expect("qwenpaw log");
    assert!(
        log_text.lines().any(|l| l == "plugin uninstall tokenless"),
        "disable must uninstall through the CLI: {log_text}"
    );
    assert!(!plugin_dir.exists(), "plugin directory must be gone");
    assert!(
        world.user_home.join(".qwenpaw").is_dir(),
        "working directory itself must survive"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qwenpaw")
            .is_none()
    );
}

#[test]
fn qwenpaw_enable_fails_closed_when_cli_exits_zero_without_installing() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);
    guard.set("FAKE_QWENPAW_FAIL", Path::new("install"));

    let err = world
        .manager()
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect_err("install that produced no plugin must fail");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "unexpected error: {err:?}"
    );
    assert!(
        err.to_string().contains("simulated"),
        "failure must surface the CLI's message: {err}"
    );
    assert!(!plugin_dir.exists());
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qwenpaw")
        .cloned()
        .expect("write-ahead receipt kept for retry");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    let log_text = std::fs::read_to_string(&log).expect("qwenpaw log");
    assert!(
        !log_text.lines().any(|l| l.starts_with("plugin uninstall")),
        "a failed install must not trigger an uninstall: {log_text}"
    );
}

#[test]
fn qwenpaw_reenable_reinstalls_with_force() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("first enable");
    std::fs::write(plugin_dir.join("stale"), b"x").expect("stale marker");
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("second enable");

    let log_text = std::fs::read_to_string(&log).expect("qwenpaw log");
    let installs = log_text
        .lines()
        .filter(|l| l.starts_with("plugin install ") && l.ends_with(" --force"))
        .count();
    assert_eq!(
        installs, 2,
        "every enable reinstalls with --force: {log_text}"
    );
    assert!(
        !plugin_dir.join("stale").exists(),
        "re-enable replaces the installed tree"
    );
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
}

#[test]
fn qwenpaw_reenable_fails_closed_when_stale_bundle_remains() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (_log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("first enable");
    // The installed tree no longer holds the source bundle (an older
    // install), and the reinstall fails: the CLI exits 0 and leaves that
    // tree, whose manifest still carries the expected id.
    std::fs::write(plugin_dir.join("plugin.py"), b"plugin = 'previous'\n")
        .expect("stale plugin.py");
    guard.set("FAKE_QWENPAW_FAIL", Path::new("install"));

    let err = manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect_err("a stale bundle must not pass as installed");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "unexpected error: {err:?}"
    );
    assert!(
        err.to_string().contains("does not match the source bundle"),
        "failure must name the stale file: {err}"
    );
    assert!(
        plugin_dir.join("plugin.json").is_file(),
        "the previous tree is left for QwenPaw"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qwenpaw")
        .cloned()
        .expect("receipt kept for retry");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn qwenpaw_disable_keeps_receipt_when_uninstall_fails() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (_log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("enable");

    guard.set("FAKE_QWENPAW_FAIL", Path::new("uninstall"));
    let disabled = manager
        .disable(COMPONENT, Some("qwenpaw"), false)
        .expect("disable runs");
    assert!(
        !disabled.claim_removed,
        "receipt kept when hot-unload fails"
    );
    assert!(!disabled.report.cleanup_complete);
    assert!(
        plugin_dir.is_dir(),
        "a failed hot-unload must leave the directory for retry"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qwenpaw")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);

    guard.set("FAKE_QWENPAW_FAIL", Path::new("none"));
    let disabled = manager
        .disable(COMPONENT, Some("qwenpaw"), false)
        .expect("retry runs");
    assert!(disabled.claim_removed, "retry completes the cleanup");
    assert!(disabled.report.cleanup_complete);
    assert!(!plugin_dir.exists());
}

#[test]
fn qwenpaw_disable_uses_working_directory_from_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("enable");
    assert!(plugin_dir.is_dir());

    // ~/.copaw appearing after enable moves the ambient resolution; the
    // receipt still names ~/.qwenpaw and the CLI must be pointed there.
    std::fs::create_dir_all(world.user_home.join(".copaw")).expect("mkdir ~/.copaw");
    let disabled = manager
        .disable(COMPONENT, Some("qwenpaw"), false)
        .expect("disable");
    assert!(
        disabled.claim_removed,
        "cleanup completes: {:?}",
        disabled.report
    );
    assert!(disabled.report.cleanup_complete);
    assert!(!plugin_dir.exists());
    let log_text = std::fs::read_to_string(&log).expect("read fake CLI log");
    let expected = format!("WORKING_DIR={}", world.user_home.join(".qwenpaw").display());
    assert_eq!(
        log_text.lines().rfind(|l| l.starts_with("WORKING_DIR=")),
        Some(expected.as_str()),
        "uninstall must use the receipt's working directory: {log_text}"
    );
}

#[test]
fn qwenpaw_disable_without_cli_keeps_receipt() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (_log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("enable");

    guard.set("QWENPAW_BIN", &world.prefix.join("no-such-qwenpaw"));
    let disabled = manager
        .disable(COMPONENT, Some("qwenpaw"), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed, "receipt kept when CLI absent");
    assert!(!disabled.report.cleanup_complete);
    assert!(
        plugin_dir.is_dir(),
        "without the CLI the plugin directory must be left alone"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, "qwenpaw")
        .cloned()
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::CleanupFailed
    );
}

#[test]
fn qwenpaw_dry_run_enable_writes_nothing() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let outcome = world
        .manager()
        .enable(COMPONENT, Some("qwenpaw"), true)
        .expect("dry-run enable");
    let EnableOutcome::Planned { plan, .. } = outcome else {
        panic!("dry run must only plan");
    };
    let register = plan.register_command.expect("install command in plan");
    assert!(
        register.ends_with(&format!(
            "qwenpaw plugin install {} --force",
            world.resource_root.display()
        )),
        "plan must show the CLI invocation: {register}"
    );
    assert!(!log.exists(), "dry run must not run the CLI");
    assert!(!plugin_dir.exists());
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "qwenpaw")
            .is_none()
    );
}

#[test]
fn qwenpaw_status_degraded_when_plugin_removed_externally() {
    let guard = EnvGuard::acquire();
    let world = stage_qwenpaw();
    let fake = write_fake_qwenpaw(&world.prefix);
    let (_log, plugin_dir) = apply_qwenpaw_env(&guard, &world, &fake);

    let manager = world.manager();
    manager
        .enable(COMPONENT, Some("qwenpaw"), false)
        .expect("enable");
    std::fs::remove_dir_all(&plugin_dir).expect("remove plugin dir");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    assert!(status.entries[0].report.conditions.iter().any(|c| {
        c.kind == AdapterConditionKind::PluginRegistered && c.status == ConditionStatus::False
    }));

    // A different plugin occupying the directory is not ours either.
    std::fs::create_dir_all(&plugin_dir).expect("recreate");
    std::fs::write(plugin_dir.join("plugin.json"), br#"{"id":"someone-else"}"#).expect("other");
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
}
