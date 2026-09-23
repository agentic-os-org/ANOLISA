//! End-to-end adapter manager tests driving a fake OpenClaw CLI.
//!
//! These exercise the full enable → status → disable lifecycle through the
//! real [`AdapterManager`] and [`OpenClawDriver`], using a shell script as
//! a stand-in for the `openclaw` binary. They cover the P3 acceptance
//! cases: install/list/uninstall success and failure, "CLI missing must
//! not clean up arbitrary paths", and forged-receipt rejection.
//!
//! The fake CLI is controlled entirely through the same env contract the
//! real driver uses (`OPENCLAW_BIN`, `OPENCLAW_STATE_DIR`, `OPENCLAW_HOME`,
//! plus a test-only `FAKE_OPENCLAW_FAIL` knob). Because those are
//! process-global, every test serializes on [`ENV_LOCK`], starts from a clean
//! env contract, and restores the prior environment on exit.
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anolisa_core::adapter::AdapterError;
use anolisa_core::adapter::claim::{
    AdapterClaim, ClaimResourceKind, ClaimStatus, ConfigApplyState, DriverPayload,
};
use anolisa_core::adapter::driver::{
    AdapterConditionKind, AdapterStatusReport, AdapterSummary, ConditionStatus,
};
use anolisa_core::adapter::manager::{
    AdapterManager, AdapterSourceStatus, EnableOptions, EnableOutcome, ScanEntry, ScanReport,
};
use anolisa_core::central_log::LogRecord;
use anolisa_core::domain::ProviderBinding;
use anolisa_core::manifest::{NoticeLevel, NoticeWhen};
use anolisa_core::state::{
    FileOwner, InstallMode as StateInstallMode, ObjectKind, OwnedFile, OwnedFileKind,
};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use sha2::{Digest, Sha256};

/// Serializes the process-global env mutation across tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const COMPONENT: &str = "tokenless";
const FRAMEWORK: &str = "openclaw";

/// A staged test world: a prefix-rooted layout, openclaw home, fake CLI,
/// and a seeded `installed.toml`.
struct World {
    _root: tempfile::TempDir,
    layout: FsLayout,
    user_home: PathBuf,
    openclaw_home: PathBuf,
    fake_bin: PathBuf,
    resource_root: PathBuf,
}

impl World {
    fn manager(&self) -> AdapterManager {
        record_owned_adapter_files(&self.layout, &self.resource_root);
        AdapterManager::new(
            self.layout.clone(),
            Some(self.user_home.clone()),
            "tester".to_string(),
        )
    }

    /// Apply this world's env contract through the process-env guard.
    fn apply_env(&self, guard: &OpenClawEnvGuard, fail: Option<&str>) {
        guard.apply(&self.fake_bin, &self.openclaw_home, fail);
    }

    fn load_state(&self) -> StateStore {
        load_state_at(&self.layout.state_dir.join("installed.toml"))
    }

    /// Path the fake CLI appends each invocation's argv to (test-only).
    fn argv_log(&self) -> PathBuf {
        self.openclaw_home
            .parent()
            .expect("prefix")
            .join("argv.log")
    }

    /// Whether the openclaw registry marker for the component exists.
    fn registry_marker_exists(&self) -> bool {
        self.openclaw_home.join("registry").join(COMPONENT).exists()
    }

    fn config_marker_exists(&self, key: &str) -> bool {
        self.openclaw_home.join("config").join(key).exists()
    }

    fn has_claim(&self) -> bool {
        self.load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_some()
    }
}

fn record_owned_adapter_files(layout: &FsLayout, root: &Path) {
    fn collect(root: &Path, files: &mut Vec<OwnedFile>) {
        for entry in std::fs::read_dir(root).expect("read adapter fixture") {
            let entry = entry.expect("adapter fixture entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("adapter fixture file type");
            if file_type.is_dir() {
                collect(&path, files);
            } else if file_type.is_symlink() {
                files.push(OwnedFile {
                    path: path.clone(),
                    owner: FileOwner::Anolisa,
                    sha256: None,
                    kind: OwnedFileKind::Symlink,
                    referent: Some(std::fs::read_link(path).expect("adapter fixture symlink")),
                    mode: None,
                    capabilities: Vec::new(),
                });
            } else if file_type.is_file() {
                files.push(OwnedFile {
                    path: path.clone(),
                    owner: FileOwner::Anolisa,
                    sha256: Some(format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(path).expect("adapter fixture bytes"))
                    )),
                    kind: OwnedFileKind::File,
                    referent: None,
                    mode: None,
                    capabilities: Vec::new(),
                });
            }
        }
    }

    if !root.is_dir() {
        return;
    }
    let state_path = layout.state_dir.join("installed.toml");
    let mut state = load_state_at(&state_path);
    let installation = state
        .find_mut(ObjectKind::Component, COMPONENT)
        .expect("fixture component");
    let ProviderBinding::Owned { artifact } = &mut installation.binding else {
        panic!("fixture component must be raw-owned");
    };
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    artifact.files = files;
    state.save(&state_path).expect("save fixture state");
}

fn recorded_openclaw_state_dir(claim: &AdapterClaim) -> &Path {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt");
    };
    let resource = claim
        .resource(&payload.state_dir_resource)
        .expect("state directory resource");
    match &resource.kind {
        ClaimResourceKind::ExternalPath { path } => path,
        other => panic!("expected external state directory, got {other:?}"),
    }
}

/// Lines the fake CLI recorded, in invocation order (empty when unset/absent).
fn argv_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The real `plugins install` invocation (excluding the `--help` probe).
fn install_argv(lines: &[String]) -> Option<&String> {
    lines
        .iter()
        .find(|l| l.starts_with("plugins install ") && !l.contains("--help"))
}

/// The real `plugins inspect` invocation (excluding the `--help` probe).
fn inspect_argv(lines: &[String]) -> Option<&String> {
    lines
        .iter()
        .find(|l| l.starts_with("plugins inspect ") && !l.contains("--help"))
}

/// Overwrite the component's installed manifest with a custom `[[adapters]]`
/// block, keeping the component recorded as installed. `adapters_block` is a
/// substituted string, so `{datadir}`/`{component}` placeholders inside it
/// reach the manifest verbatim.
fn write_openclaw_manifest(layout: &FsLayout, adapters_block: &str) {
    let manifest_path = layout
        .state_dir
        .join("component-manifests")
        .join(COMPONENT)
        .join("component.toml");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).expect("manifest dir");
    let toml = format!(
        r#"[component]
name = "{COMPONENT}"
version = "0.1.0"

[component.layout]
modes = ["system"]

{adapters_block}
"#
    );
    std::fs::write(&manifest_path, toml).expect("seed component manifest");
}

/// A plain OpenClaw plugin adapter block with an optional adapter-level
/// framework version requirement.
fn plugin_adapter_block(compat_req: Option<&str>) -> String {
    let compat = compat_req
        .map(|r| format!("\n[adapters.compat]\nframework_version = \"{r}\"\n"))
        .unwrap_or_default();
    format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"
{compat}"#
    )
}

fn configure_plugin_with_skill(world: &World, skill_name: &str) {
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.openclaw]
skills = ["{skill_name}"]
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    let skill_source = world.resource_root.join("skills").join(skill_name);
    std::fs::create_dir_all(&skill_source).expect("skill source");
    std::fs::write(skill_source.join("marker.txt"), b"skill").expect("skill marker");
}

/// Every environment variable this test binary's fake OpenClaw contract
/// owns. The guard saves and restores exactly these so no test leaks state
/// into another. `FAKE_OC_*` are the capability knobs the fake CLI reads.
const OWNED_ENV: &[&str] = &[
    "OPENCLAW_BIN",
    "OPENCLAW_STATE_DIR",
    "OPENCLAW_HOME",
    "FAKE_OPENCLAW_FAIL",
    "FAKE_OC_VERSION",
    "FAKE_OC_INSTALL_FORCE",
    "FAKE_OC_INSTALL_ACCEPT",
    "FAKE_OC_ENABLE_ACCEPT",
    "FAKE_OC_INSTALL_UNSAFE",
    "FAKE_OC_INSTALL_UNSAFE_NOOP",
    "FAKE_OC_INSPECT_JSON",
    "FAKE_OC_INSPECT_RUNTIME",
    "FAKE_OC_RUNTIME_STATUS",
    "FAKE_OC_INSPECT_DIAG",
    "FAKE_OC_ARGV_LOG",
    "FAKE_OC_PROBE_FAIL",
    "FAKE_OC_LIST_JSON",
    "FAKE_OC_VERSION_PREAMBLE",
    "FAKE_OC_CONFIG_FAIL_KEY",
    "FAKE_OC_CONFIG_FAIL_AFTER_KEY",
    "HERMES_BIN",
    "HERMES_HOME",
];

struct OpenClawEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl OpenClawEnvGuard {
    fn acquire() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = OWNED_ENV
            .iter()
            .map(|&k| (k, std::env::var_os(k)))
            .collect();
        let guard = Self { _lock: lock, saved };
        guard.clear();
        guard
    }

    fn clear(&self) {
        // SAFETY: this guard holds ENV_LOCK, so tests in this binary cannot
        // observe a half-mutated OpenClaw env contract.
        unsafe {
            for &key in OWNED_ENV {
                std::env::remove_var(key);
            }
        }
    }

    fn apply(&self, fake_bin: &Path, openclaw_home: &Path, fail: Option<&str>) {
        // SAFETY: this guard holds ENV_LOCK, so no other test thread in this
        // binary reads these vars concurrently.
        unsafe {
            std::env::set_var("OPENCLAW_BIN", fake_bin);
            std::env::set_var("OPENCLAW_HOME", openclaw_home);
            match fail {
                Some(stage) => std::env::set_var("FAKE_OPENCLAW_FAIL", stage),
                None => std::env::remove_var("FAKE_OPENCLAW_FAIL"),
            }
        }
    }

    fn set_openclaw_bin(&self, value: &Path) {
        // SAFETY: this guard holds ENV_LOCK.
        unsafe {
            std::env::set_var("OPENCLAW_BIN", value);
        }
    }

    /// Set one of the owned fake-CLI knobs (or `OsStr`-valued path).
    fn set(&self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
        assert!(
            OWNED_ENV.contains(&key),
            "env key {key} must be guard-owned"
        );
        // SAFETY: this guard holds ENV_LOCK.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn unset(&self, key: &str) {
        assert!(
            OWNED_ENV.contains(&key),
            "env key {key} must be guard-owned"
        );
        // SAFETY: this guard holds ENV_LOCK.
        unsafe {
            std::env::remove_var(key);
        }
    }
}

impl Drop for OpenClawEnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            restore_env(key, value.as_ref());
        }
    }
}

fn restore_env(key: &str, value: Option<&OsString>) {
    // SAFETY: callers hold ENV_LOCK until after the saved values are restored.
    unsafe {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

/// Build a fully staged world: layout under a temp prefix, an openclaw
/// home, a fake CLI, the adapter resource bundle, and a seeded state file
/// recording the component as installed.
fn stage() -> World {
    stage_in(tempfile::tempdir().expect("tempdir"))
}

/// The same staged world rooted at a caller-chosen temp dir, for tests that
/// need to pin the shape of the prefix path itself.
fn stage_in(root: tempfile::TempDir) -> World {
    let prefix = root.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));

    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");

    let openclaw_home = prefix.join("openclaw-home");
    std::fs::create_dir_all(&openclaw_home).expect("openclaw home");

    // Adapter resource bundle with the same native manifest shape shipped by
    // tokenless' OpenClaw plugin.
    let resource_root = layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join(FRAMEWORK);
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("openclaw.plugin.json"),
        format!(r#"{{"id":"{COMPONENT}","name":"Tokenless"}}"#),
    )
    .expect("plugin manifest");

    let fake_bin = write_fake_openclaw(&prefix);
    seed_state(&layout, &prefix);

    World {
        _root: root,
        layout,
        user_home,
        openclaw_home,
        fake_bin,
        resource_root,
    }
}

/// Write a fake `openclaw` CLI honoring the driver's argv/env contract.
///
/// Read-only probes, defaulting to a modern, force-capable, JSON-capable host:
/// - `--version` prints `openclaw $FAKE_OC_VERSION` (default `2026.4.14`) and
///   creates NO registry/config/state.
/// - `plugins install --help` lists `--force` unless `FAKE_OC_INSTALL_FORCE=0`
///   and `--dangerously-force-unsafe-install` when `FAKE_OC_INSTALL_UNSAFE=1`;
///   `FAKE_OC_INSTALL_UNSAFE_NOOP=1` marks that option as a deprecated no-op.
/// - `plugins enable --help` advertises consent when `FAKE_OC_ENABLE_ACCEPT=1`.
/// - `plugins inspect --help` lists `--json` unless `FAKE_OC_INSPECT_JSON=0`
///   and `--runtime` when `FAKE_OC_INSPECT_RUNTIME=1`.
///
/// Mutations / runtime state:
/// - `plugins install <root> ...` reads `<root>/openclaw.plugin.json` and
///   touches a marker in `$OPENCLAW_STATE_DIR/registry/<id>`.
/// - `plugins inspect <id> [--runtime] --json` prints an optional legacy
///   diagnostic line (when `FAKE_OC_INSPECT_DIAG` is set) followed by the JSON
///   `{"plugin":{"id":..,"status":"$FAKE_OC_RUNTIME_STATUS"}}` (default
///   `loaded` unless uninstall left a persistent disabled marker).
/// - `plugins uninstall <id> ...` removes registration and persists disabled state;
///   `plugins enable <id>` clears it. `plugins list` prints registry markers.
/// - `FAKE_OPENCLAW_FAIL=untracked` refuses uninstall without changing the
///   registry; `FAKE_OC_LIST_JSON` overrides JSON listing, and
///   `FAKE_OC_PROBE_FAIL=list` makes listing fail.
/// - `FAKE_OPENCLAW_FAIL=install|install_after_register|uninstall` forces that
///   verb to exit non-zero; `FAKE_OC_CONFIG_FAIL_KEY` fails `config set`
///   before mutation, while `FAKE_OC_CONFIG_FAIL_AFTER_KEY` fails after
///   writing a marker for one exact key.
///
/// When `FAKE_OC_ARGV_LOG` names a file, every invocation appends its full
/// argv (one line) — test instrumentation, not OpenClaw state.
fn write_fake_openclaw(dir: &Path) -> PathBuf {
    let script = r#"#!/bin/sh
if [ -n "${FAKE_OC_ARGV_LOG:-}" ]; then printf '%s\n' "$*" >> "$FAKE_OC_ARGV_LOG"; fi

ver="${FAKE_OC_VERSION:-2026.4.14}"
if [ "$1" = "--version" ]; then
  [ -n "${FAKE_OC_VERSION_PREAMBLE:-}" ] && echo "$FAKE_OC_VERSION_PREAMBLE"
  echo "openclaw $ver"
  [ "${FAKE_OC_PROBE_FAIL:-}" = "version" ] && exit 3
  exit 0
fi

sub="$1"; action="$2"; arg3="$3"

if [ "$sub" = "config" ] && [ "$action" = "set" ]; then
  if [ -n "${FAKE_OC_CONFIG_FAIL_KEY:-}" ] && [ "$arg3" = "$FAKE_OC_CONFIG_FAIL_KEY" ]; then
    echo "boom-config-$arg3" >&2
    exit 13
  fi
  config_dir="$OPENCLAW_STATE_DIR/config"; mkdir -p "$config_dir" 2>/dev/null
  printf '%s' "$4" > "$config_dir/$arg3"
  if [ -n "${FAKE_OC_CONFIG_FAIL_AFTER_KEY:-}" ] && [ "$arg3" = "$FAKE_OC_CONFIG_FAIL_AFTER_KEY" ]; then
    echo "boom-after-config-$arg3" >&2
    exit 14
  fi
  echo "config set $arg3 $4"
  exit 0
fi
if [ "$sub" != "plugins" ]; then echo "unknown subcommand: $sub" >&2; exit 2; fi

case "$action" in
  install)
    if [ "$arg3" = "--help" ]; then
      echo "Usage: openclaw plugins install <path> [options]"
      [ "${FAKE_OC_INSTALL_FORCE:-1}" = "1" ] && echo "  --force                             overwrite an existing plugin"
      [ "${FAKE_OC_INSTALL_ACCEPT:-0}" = "1" ] && echo "  --accept-capabilities               accept declared capabilities"
      [ "${FAKE_OC_INSTALL_ACCEPT:-0}" = "near_match" ] && echo "  --accept-capabilities-only          unrelated option"
      if [ "${FAKE_OC_INSTALL_UNSAFE:-0}" = "1" ]; then
        if [ "${FAKE_OC_INSTALL_UNSAFE_NOOP:-0}" = "1" ]; then
          echo "  --dangerously-force-unsafe-install  Deprecated no-op; security.installPolicy may still block"
        else
          echo "  --dangerously-force-unsafe-install  bypass plugin safety checks"
        fi
      fi
      [ "${FAKE_OC_PROBE_FAIL:-}" = "install_help" ] && exit 4
      exit 0
    fi
    reg="$OPENCLAW_STATE_DIR/registry"; mkdir -p "$reg" 2>/dev/null
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "install" ]; then echo "boom-install" >&2; exit 7; fi
    case "${FAKE_OPENCLAW_FAIL:-}" in
      install_consent*)
        [ "$FAKE_OPENCLAW_FAIL" = "install_consent_warning" ] && echo "--dangerously-force-unsafe-install is deprecated and no longer affects plugin installs"
        echo 'Plugin requires capability consent. Use --accept-capabilities, then retry.' >&2
        exit 15 ;;
    esac
    accepted=0
    for option in "$@"; do [ "$option" = "--accept-capabilities" ] && accepted=1; done
    if [ "${FAKE_OC_INSTALL_ACCEPT:-0}" = "1" ]; then
      if [ "$accepted" != 1 ]; then echo "Plugin requires capability consent" >&2; exit 15; fi
    elif [ "$accepted" = 1 ]; then
      echo "unknown option --accept-capabilities" >&2; exit 2
    fi
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "install_unsafe_policy" ]; then
      echo "refusing install: plugin failed safety checks (pass --dangerously-force-unsafe-install to override)" >&2
      exit 11
    fi
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "install_unsafe_policy_stdout" ]; then
      echo "SECURITY FINDING: plugin failed safety review; pass --dangerously-force-unsafe-install to override"
      exit 12
    fi
    id=$(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$arg3/openclaw.plugin.json" | head -n 1)
    if [ -z "$id" ]; then echo "missing plugin id" >&2; exit 9; fi
    : > "$reg/$id"
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "install_after_register" ]; then echo "boom-after-register" >&2; exit 10; fi
    echo "installed $id"
    ;;
  enable)
    if [ "$arg3" = "--help" ]; then
      echo "Usage: openclaw plugins enable [options] <id>"
      [ "${FAKE_OC_ENABLE_ACCEPT:-0}" = "1" ] && echo "  --accept-capabilities  accept declared capabilities"
      [ "${FAKE_OC_ENABLE_ACCEPT:-0}" = "near_match" ] && echo "  --accept-capabilities-only  unrelated option"
      [ "${FAKE_OC_PROBE_FAIL:-}" = "enable_help" ] && exit 4
      exit 0
    fi
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "enable" ]; then echo "boom-enable" >&2; exit 16; fi
    accepted=0
    for option in "$@"; do [ "$option" = "--accept-capabilities" ] && accepted=1; done
    if [ "${FAKE_OC_ENABLE_ACCEPT:-0}" = "1" ]; then
      if [ "$accepted" != 1 ]; then echo "Plugin requires capability consent" >&2; exit 15; fi
    elif [ "$accepted" = 1 ]; then
      echo "unknown option --accept-capabilities" >&2; exit 2
    fi
    if [ ! -e "$OPENCLAW_STATE_DIR/registry/$arg3" ]; then echo "Plugin not found: $arg3" >&2; exit 1; fi
    rm -f "$OPENCLAW_STATE_DIR/disabled/$arg3"
    echo "enabled $arg3"
    ;;
  inspect)
    if [ "$arg3" = "--help" ]; then
      echo "Usage: openclaw plugins inspect <id> [options]"
      [ "${FAKE_OC_INSPECT_JSON:-1}" = "1" ] && echo "  --json      machine-readable output"
      [ "${FAKE_OC_INSPECT_RUNTIME:-0}" = "1" ] && echo "  --runtime   include live runtime status"
      [ "${FAKE_OC_PROBE_FAIL:-}" = "inspect_help" ] && exit 5
      exit 0
    fi
    status="${FAKE_OC_RUNTIME_STATUS:-loaded}"
    [ -e "$OPENCLAW_STATE_DIR/disabled/$arg3" ] && status=disabled
    [ -n "${FAKE_OC_INSPECT_DIAG:-}" ] && echo "legacy: reading plugin registry for $arg3 ..."
    echo "{\"plugin\":{\"id\":\"$arg3\",\"status\":\"$status\"}}"
    ;;
  uninstall)
    reg="$OPENCLAW_STATE_DIR/registry"; mkdir -p "$reg" 2>/dev/null
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "uninstall" ]; then echo "boom-uninstall" >&2; exit 8; fi
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "untracked" ]; then
      echo "Plugin \"$arg3\" is not associated with a tracked package install. Refresh the plugin registry, then reinstall the package or run openclaw doctor before retrying." >&2
      exit 1
    fi
    if [ ! -e "$reg/$arg3" ]; then echo "Plugin not found: $arg3" >&2; exit 1; fi
    rm -f "$reg/$arg3"
    mkdir -p "$OPENCLAW_STATE_DIR/disabled"
    : > "$OPENCLAW_STATE_DIR/disabled/$arg3"
    echo "uninstalled $arg3"
    ;;
  list)
    reg="$OPENCLAW_STATE_DIR/registry"
    if [ "${FAKE_OC_PROBE_FAIL:-}" = "list" ]; then echo "boom-list" >&2; exit 6; fi
    if [ "$arg3" = "--json" ]; then
      if [ "${FAKE_OC_LIST_JSON+x}" = x ]; then printf '%s\n' "$FAKE_OC_LIST_JSON"; exit 0; fi
      printf '{"plugins":['
      sep=""
      for plugin in "$reg"/*; do
        [ -f "$plugin" ] || continue
        printf '%s{"id":"%s"}' "$sep" "${plugin##*/}"
        sep=,
      done
      printf '],"diagnostics":[]}\n'
      exit 0
    fi
    ls "$reg" 2>/dev/null || true
    ;;
  *)
    echo "unknown action: $action" >&2; exit 2 ;;
esac
exit 0
"#;
    let path = dir.join("openclaw");
    std::fs::write(&path, script).expect("write fake cli");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// Seed `installed.toml` with the component recorded as installed so
/// `enable`'s precondition passes.
fn load_state_at(path: &Path) -> StateStore {
    StateStore::load(path, anolisa_platform::privilege::effective_uid()).expect("load state")
}

fn seed_state(layout: &FsLayout, prefix: &Path) {
    let state_path = layout.state_dir.join("installed.toml");
    std::fs::create_dir_all(state_path.parent().unwrap()).expect("state dir");
    let toml = format!(
        r#"schema_version = 2
updated_at = "2026-06-15T00:00:00Z"
install_mode = "system"
prefix = "{prefix}"
anolisa_version = "0.1.7"

[[objects]]
kind = "component"
name = "{COMPONENT}"
version = "0.1.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
installed_at = "2026-06-15T00:00:00Z"
"#,
        prefix = prefix.display(),
    );
    std::fs::write(&state_path, toml).expect("seed state");
    write_installed_manifest(layout, FRAMEWORK);
    record_owned_adapter_files(
        layout,
        &layout
            .datadir
            .join("adapters")
            .join(COMPONENT)
            .join(FRAMEWORK),
    );
}

fn write_installed_manifest(layout: &FsLayout, framework: &str) {
    let manifest_path = layout
        .state_dir
        .join("component-manifests")
        .join(COMPONENT)
        .join("component.toml");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).expect("manifest dir");
    std::fs::write(
        manifest_path,
        format!(
            r#"[component]
name = "{COMPONENT}"
version = "0.1.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "{framework}"
source = "adapters/{COMPONENT}/{framework}"
dest = "{{datadir}}/adapters/{{component}}/{framework}/"
"#
        ),
    )
    .expect("seed component manifest");
}

#[test]
fn enable_status_disable_happy_path() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    // enable
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let claim = match outcome {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled, got plan"),
    };
    assert!(
        world.layout.lock_file.is_file(),
        "apply must retain the existing install-lock boundary"
    );
    assert_eq!(claim.component, COMPONENT);
    assert_eq!(claim.framework, FRAMEWORK);
    assert_eq!(claim.plugin_id.as_deref(), Some(COMPONENT));
    assert_eq!(claim.status, ClaimStatus::Enabled);
    // Receipt records the external home + the plugin, no owned paths.
    assert!(claim.resources.iter().any(|r| matches!(
        &r.kind,
        ClaimResourceKind::FrameworkPlugin { plugin_id, .. } if plugin_id == COMPONENT
    )));

    // Persisted to state.
    let state = world.load_state();
    assert!(state.find_adapter_claim(COMPONENT, FRAMEWORK).is_some());

    // The framework CLI invocation reached the central log.
    let log = std::fs::read_to_string(&world.layout.central_log).expect("central log");
    assert!(
        log.contains("framework cli"),
        "central log should record the CLI invocation: {log}"
    );

    // status → healthy (framework detected + plugin registered).
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
    // The plugin-registered condition must be verified True.
    assert!(status.entries[0].report.conditions.iter().any(|c| matches!(
        c.kind,
        anolisa_core::adapter::driver::AdapterConditionKind::PluginRegistered
    ) && c.status
        == ConditionStatus::True));

    // disable → removes receipt.
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert!(disabled.report.cleanup_complete);
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "receipt must be gone after successful disable"
    );
}

#[test]
fn enable_after_disable_restores_loaded_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("first enable");
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let disabled = world.openclaw_home.join("disabled").join(COMPONENT);
    assert!(
        disabled.exists(),
        "uninstall preserves explicit disabled state"
    );
    for _ in 0..2 {
        let outcome = manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("re-enable");
        let EnableOutcome::Enabled(claim) = outcome else {
            panic!("expected enabled")
        };
        assert_eq!(claim.status, ClaimStatus::Enabled);
        assert!(!disabled.exists(), "explicit enable clears disabled state");
    }
}

#[test]
fn enable_rejects_modified_package_owned_source() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    std::fs::write(
        world.resource_root.join("openclaw.plugin.json"),
        br#"{"id":"modified","name":"Modified"}"#,
    )
    .expect("modify package-owned manifest after recording its digest");

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("modified package-owned bytes must block enable");
    assert!(matches!(
        err,
        AdapterError::InvalidAdapterInput { reason, .. } if reason.contains("content changed")
    ));
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

#[test]
fn enable_honors_explicit_openclaw_state_dir() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let state_dir = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &state_dir);
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable in configured state directory");
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled, got plan"),
    };

    assert!(state_dir.join("registry").join(COMPONENT).exists());
    assert!(
        !world.registry_marker_exists(),
        "OPENCLAW_HOME must not override an explicit OPENCLAW_STATE_DIR"
    );
    assert!(claim.resources.iter().any(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::ExternalPath { path } if path == &state_dir
    )));

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
}

#[test]
fn blank_openclaw_state_dir_falls_back_to_openclaw_home() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("OPENCLAW_STATE_DIR", "  \t  ");
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with a blank state override");
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled, got plan"),
    };

    assert!(world.registry_marker_exists());
    assert!(claim.resources.iter().any(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::ExternalPath { path } if path == &world.openclaw_home
    )));
}

#[test]
fn skill_bundle_expands_tilde_in_openclaw_state_dir() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.openclaw]
skills = ["sec-audit"]
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    let skill_source = world.resource_root.join("skills/sec-audit");
    std::fs::create_dir_all(&skill_source).expect("skill source");
    std::fs::write(skill_source.join("marker.txt"), b"skill").expect("skill marker");

    world.apply_env(&guard, None);
    guard.unset("OPENCLAW_HOME");
    guard.set("OPENCLAW_STATE_DIR", "~/.openclaw-work");
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable skill bundle with a tilde state directory");
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled, got plan"),
    };
    let expected = world.user_home.join(".openclaw-work");

    assert!(expected.join("skills/sec-audit/marker.txt").is_file());
    assert!(claim.resources.iter().any(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::ExternalPath { path } if path == &expected
    )));

    std::fs::write(expected.join("skills/sec-audit/runtime.log"), b"runtime")
        .expect("runtime file");
    let status = manager
        .status(Some(COMPONENT))
        .expect("status with runtime file");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    std::fs::write(expected.join("skills/sec-audit/marker.txt"), b"changed")
        .expect("mutate materialized skill");
    let status = manager
        .status(Some(COMPONENT))
        .expect("status with changed skill");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    assert!(status.entries[0].report.conditions.iter().any(|condition| {
        condition.kind
            == anolisa_core::adapter::driver::AdapterConditionKind::MaterializedBundleMatches
            && condition.status == ConditionStatus::False
    }));
}

#[test]
fn reenable_prunes_removed_managed_skill_files_but_keeps_runtime_extras() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    world.apply_env(&guard, None);
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable initial skill revision");
    let destination = world.openclaw_home.join("skills/sec-audit");
    let removed = destination.join("marker.txt");
    let runtime_extra = destination.join("runtime.log");
    assert!(removed.is_file());
    std::fs::write(&runtime_extra, b"runtime").expect("runtime-created extra");

    let source = world.resource_root.join("skills/sec-audit");
    std::fs::remove_file(source.join("marker.txt")).expect("remove old managed source");
    std::fs::write(source.join("renamed.txt"), b"skill-v2").expect("write renamed source");
    record_owned_adapter_files(&world.layout, &world.resource_root);

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable updated skill revision");

    assert!(destination.join("renamed.txt").is_file());
    assert!(
        !removed.exists(),
        "a file owned only by the prior receipt must be removed"
    );
    assert_eq!(
        std::fs::read(&runtime_extra).expect("runtime extra must survive"),
        b"runtime"
    );
    let status = manager
        .status(Some(COMPONENT))
        .expect("status after re-enable");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
}

#[test]
fn reenable_prunes_empty_ancestors_before_directory_to_file_change() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    let source = world.resource_root.join("skills/sec-audit");
    std::fs::create_dir_all(source.join("hook.py/sub")).expect("old nested source directory");
    std::fs::write(source.join("hook.py/sub/managed.txt"), b"v1").expect("old nested managed file");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable initial directory-shaped output");

    std::fs::remove_dir_all(source.join("hook.py")).expect("remove old source directory");
    std::fs::write(source.join("hook.py"), b"v2").expect("new file-shaped source");
    record_owned_adapter_files(&world.layout, &world.resource_root);

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("empty ancestors are pruned before replacing the directory");

    let destination = world.openclaw_home.join("skills/sec-audit/hook.py");
    assert_eq!(
        std::fs::read(destination).expect("new file-shaped output"),
        b"v2"
    );
}

#[test]
fn hermes_reenable_prunes_removed_managed_skill_files() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let hermes_root = world
        .layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join("hermes");
    let source = hermes_root.join("skills/sec-audit");
    std::fs::create_dir_all(&source).expect("Hermes skill source");
    std::fs::write(source.join("marker.txt"), b"skill-v1").expect("Hermes skill marker");
    write_openclaw_manifest(
        &world.layout,
        &format!(
            r#"[[adapters]]
framework = "hermes"
adapter_type = "skill_bundle"
source = "adapters/{COMPONENT}/hermes"
dest = "{{datadir}}/adapters/{{component}}/hermes/"

[adapters.hermes]
skills = ["sec-audit"]
"#
        ),
    );
    record_owned_adapter_files(&world.layout, &hermes_root);
    let hermes_home = world._root.path().join("hermes-home");
    guard.set("HERMES_BIN", &world.fake_bin);
    guard.set("HERMES_HOME", &hermes_home);
    let manager = AdapterManager::new(
        world.layout.clone(),
        Some(world.user_home.clone()),
        "tester".to_string(),
    );

    manager
        .enable(COMPONENT, Some("hermes"), false)
        .expect("enable initial Hermes skill revision");
    let destination = hermes_home.join("skills/sec-audit");
    assert!(destination.join("marker.txt").is_file());

    std::fs::remove_file(source.join("marker.txt")).expect("remove old Hermes source");
    std::fs::write(source.join("renamed.txt"), b"skill-v2").expect("rename Hermes source");
    record_owned_adapter_files(&world.layout, &hermes_root);
    manager
        .enable(COMPONENT, Some("hermes"), false)
        .expect("re-enable updated Hermes skill revision");

    assert!(destination.join("renamed.txt").is_file());
    assert!(!destination.join("marker.txt").exists());
    let status = manager.status(Some(COMPONENT)).expect("Hermes status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);
}

#[test]
fn reenable_refuses_directory_to_file_change_when_runtime_content_would_be_lost() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    let source = world.resource_root.join("skills/sec-audit");
    std::fs::create_dir_all(source.join("hook.py")).expect("old source directory");
    std::fs::write(source.join("hook.py/managed.txt"), b"v1").expect("old managed file");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable initial directory-shaped output");

    let destination = world.openclaw_home.join("skills/sec-audit");
    std::fs::write(destination.join("hook.py/runtime.log"), b"runtime")
        .expect("runtime content under old directory");
    std::fs::remove_dir_all(source.join("hook.py")).expect("remove old source directory");
    std::fs::write(source.join("hook.py"), b"v2").expect("new file-shaped source");
    record_owned_adapter_files(&world.layout, &world.resource_root);

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("runtime content must not be recursively removed");
    assert!(matches!(
        err,
        AdapterError::ReenableCleanupIncomplete { .. }
    ));
    assert_eq!(
        std::fs::read(destination.join("hook.py/runtime.log")).expect("runtime content survives"),
        b"runtime"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("prior receipt remains durable");
    assert!(
        claim
            .materialized_files
            .iter()
            .any(|file| file.relative_path == Path::new("hook.py/managed.txt"))
    );
}

#[test]
fn pre_fix_receipt_remains_visible_and_disable_cleans_recorded_state() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed a receipt using the pre-fix state directory");
    assert!(world.registry_marker_exists());

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);

    let status = manager.status(Some(COMPONENT)).expect("status old receipt");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable old receipt");
    assert!(disabled.claim_removed);
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

#[test]
fn reenable_migrates_pre_fix_receipt_to_configured_state_dir() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");

    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed a receipt using the pre-fix state directory");
    assert!(world.registry_marker_exists());
    assert!(
        world
            .openclaw_home
            .join("skills/sec-audit/marker.txt")
            .is_file()
    );

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable old receipt in configured state directory");
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled, got plan"),
    };

    assert!(configured_state.join("registry").join(COMPONENT).exists());
    assert!(
        configured_state
            .join("skills/sec-audit/marker.txt")
            .is_file()
    );
    assert!(!world.registry_marker_exists());
    assert!(
        !world
            .openclaw_home
            .join("skills/sec-audit/marker.txt")
            .exists()
    );
    assert!(claim.resources.iter().any(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::ExternalPath { path } if path == &configured_state
    )));
    let status = manager
        .status(Some(COMPONENT))
        .expect("status migrated receipt");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Healthy);

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable migrated receipt");
    assert!(disabled.claim_removed);
    assert!(!world.registry_marker_exists());
    assert!(!configured_state.join("registry").join(COMPONENT).exists());
    assert!(
        !configured_state
            .join("skills/sec-audit/marker.txt")
            .exists()
    );
    assert!(!world.has_claim());
}

#[test]
fn migration_cleanup_retry_tolerates_already_missing_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed old registry and skill");

    let old_skill = world.openclaw_home.join("skills/sec-audit");
    let mut blocked = std::fs::metadata(&old_skill)
        .expect("old skill metadata")
        .permissions();
    blocked.set_mode(0o000);
    std::fs::set_permissions(&old_skill, blocked).expect("block old skill cleanup");

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("skill cleanup failure must keep the prior receipt");
    assert!(matches!(
        err,
        AdapterError::ReenableCleanupIncomplete { .. }
    ));
    assert!(
        !world.registry_marker_exists(),
        "the first cleanup already unregistered the old plugin"
    );
    assert!(old_skill.exists());
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("prior receipt retained for retry");
    assert_eq!(recorded_openclaw_state_dir(claim), world.openclaw_home);

    let mut retryable = std::fs::metadata(&old_skill)
        .expect("blocked skill metadata")
        .permissions();
    retryable.set_mode(0o755);
    std::fs::set_permissions(&old_skill, retryable).expect("allow cleanup retry");

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry treats the missing old plugin as already clean");
    assert!(!old_skill.exists());
    assert!(configured_state.join("registry").join(COMPONENT).exists());
    assert!(
        configured_state
            .join("skills/sec-audit/marker.txt")
            .is_file()
    );
}

#[test]
fn migration_dry_run_previews_cleanup_without_mutation() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed old registry and skill");
    let state_path = world.layout.state_dir.join("installed.toml");
    let state_before = std::fs::read(&state_path).expect("state before dry-run");

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("plan state-directory migration");
    let plan = match outcome {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };

    let action_index = |needle: &str| {
        plan.actions
            .iter()
            .position(|action| action.contains(needle))
            .unwrap_or_else(|| panic!("missing '{needle}' in plan: {:?}", plan.actions))
    };
    assert!(
        action_index("unregister prior openclaw plugin") < action_index("register openclaw plugin")
    );
    assert!(action_index("remove prior openclaw skill") < action_index("deliver openclaw skill"));

    assert_eq!(
        std::fs::read(&state_path).expect("state after dry-run"),
        state_before
    );
    assert!(world.registry_marker_exists());
    assert!(
        world
            .openclaw_home
            .join("skills/sec-audit/marker.txt")
            .is_file()
    );
    assert!(!configured_state.join("registry").join(COMPONENT).exists());
    assert!(
        argv_lines(&world.argv_log())
            .iter()
            .all(|line| !line.starts_with("plugins uninstall "))
    );
}

#[test]
fn reenable_dry_run_previews_stale_materialized_file_cleanup() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable initial skill revision");

    let source = world.resource_root.join("skills/sec-audit");
    std::fs::remove_file(source.join("marker.txt")).expect("remove old managed source");
    std::fs::write(source.join("renamed.txt"), b"skill-v2").expect("write renamed source");
    record_owned_adapter_files(&world.layout, &world.resource_root);

    let destination = world.openclaw_home.join("skills/sec-audit");
    let stale = destination.join("marker.txt");
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("plan stale materialized-file cleanup");
    let plan = match outcome {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    let cleanup = format!("remove stale materialized file {}", stale.display());
    assert!(
        plan.actions.iter().any(|action| action == &cleanup),
        "missing '{cleanup}' in plan: {:?}",
        plan.actions
    );
    assert!(stale.is_file(), "dry-run must not remove the stale output");
    assert!(
        !destination.join("renamed.txt").exists(),
        "dry-run must not deliver the replacement output"
    );
}

#[test]
fn reenable_cleanup_failure_keeps_prior_receipt_and_installation() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed a receipt using the pre-fix state directory");

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    guard.set("FAKE_OPENCLAW_FAIL", "uninstall");
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("failed prior cleanup must block receipt replacement");

    assert!(matches!(
        err,
        AdapterError::ReenableCleanupIncomplete { .. }
    ));
    assert!(world.registry_marker_exists());
    assert!(!configured_state.join("registry").join(COMPONENT).exists());
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("prior receipt must remain durable");
    assert_eq!(recorded_openclaw_state_dir(claim), world.openclaw_home);
    assert_eq!(claim.status, ClaimStatus::Enabled);
}

#[test]
fn failed_install_after_state_migration_tracks_only_new_state() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed a receipt using the pre-fix state directory");

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    guard.set("FAKE_OPENCLAW_FAIL", "install");
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("new-state install must fail");
    assert!(matches!(err, AdapterError::FrameworkCli { .. }));

    assert!(!world.registry_marker_exists());
    assert!(!configured_state.join("registry").join(COMPONENT).exists());
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("new-state cleanup receipt");
    assert_eq!(recorded_openclaw_state_dir(claim), configured_state);
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);

    guard.unset("FAKE_OPENCLAW_FAIL");
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("clean failed new-state install");
    assert!(disabled.claim_removed);
    assert!(!world.registry_marker_exists());
    assert!(!configured_state.join("registry").join(COMPONENT).exists());
    assert!(!world.has_claim());
}

#[test]
fn legacy_home_must_be_restored_when_it_cannot_be_reconstructed() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed a receipt using the pre-fix state directory");

    let configured_state = world._root.path().join("configured-openclaw-state");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    guard.unset("OPENCLAW_HOME");
    let err = manager
        .status(Some(COMPONENT))
        .expect_err("unknown legacy root must not self-authorize from receipt data");
    assert!(matches!(err, AdapterError::ClaimValidation(_)));

    guard.set("OPENCLAW_HOME", &world.openclaw_home);
    let status = manager
        .status(Some(COMPONENT))
        .expect("restored legacy home validates the old receipt");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
}

#[test]
fn user_layout_enable_accepts_system_installed_component() {
    let guard = OpenClawEnvGuard::acquire();
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    let system_prefix = prefix.join("system");
    let system_layout = FsLayout::system(Some(system_prefix.clone()));
    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");
    let user_layout =
        FsLayout::user_with_overrides(user_home.clone(), None, None, None, None, None);

    let openclaw_home = prefix.join("openclaw-home");
    std::fs::create_dir_all(&openclaw_home).expect("openclaw home");
    let resource_root = system_layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join(FRAMEWORK);
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("openclaw.plugin.json"),
        format!(r#"{{"id":"{COMPONENT}","name":"Tokenless"}}"#),
    )
    .expect("plugin manifest");
    seed_state(&system_layout, &system_prefix);
    let fake_bin = write_fake_openclaw(&prefix);
    guard.apply(&fake_bin, &openclaw_home, None);

    let mut manager =
        AdapterManager::new(user_layout.clone(), Some(user_home), "tester".to_string());
    manager.push_visible_root(anolisa_core::adapter::manager::VisibleRoot {
        state_dir: system_layout.state_dir.clone(),
        contract_datadir_roots: vec![system_layout.datadir.clone()],
    });

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable system component from user layout");

    let user_state = load_state_at(&user_layout.state_dir.join("installed.toml"));
    assert_eq!(user_state.install_mode, StateInstallMode::User);
    assert_eq!(user_state.prefix, user_layout.prefix);
    assert!(
        user_state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_some(),
        "receipt is written to the invoking user's state"
    );
    let system_state = load_state_at(&system_layout.state_dir.join("installed.toml"));
    assert!(
        system_state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "system install state is read as a source, not used for user receipts"
    );
}

#[test]
fn enable_rejects_resource_directory_not_declared_by_manifest() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_installed_manifest(&world.layout, "hermes");
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("directory discovery alone must not authorize enable");
    assert!(
        matches!(err, AdapterError::AdapterNotDeclared { .. }),
        "got {err:?}"
    );
    assert!(
        !world
            .openclaw_home
            .join("registry")
            .join(COMPONENT)
            .exists(),
        "framework driver must not run when manifest does not declare it"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "no receipt should be created for an undeclared adapter"
    );
}

#[test]
fn failed_enable_keeps_cleanup_receipt_for_retry() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install"));
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("install failure must surface");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("failed enable receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn failed_enable_after_framework_side_effect_keeps_visible_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install_after_register"));
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("install failure must surface");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );

    assert!(
        world
            .openclaw_home
            .join("registry")
            .join(COMPONENT)
            .exists(),
        "fake framework registered the plugin before returning failure"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt must remain visible for disable/status");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn dry_run_enable_does_not_register_or_persist() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    assert!(!world.layout.lock_file.exists());
    assert!(!world.layout.central_log.exists());

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run enable");
    match outcome {
        EnableOutcome::Planned { plan, .. } => {
            assert_eq!(plan.component, COMPONENT);
            assert!(plan.register_command.is_some());
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }

    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "dry-run must not persist a receipt"
    );
    // Nothing should have been written into the openclaw registry.
    assert!(
        !world
            .openclaw_home
            .join("registry")
            .join(COMPONENT)
            .exists()
    );
    assert!(
        !world.layout.lock_file.exists(),
        "dry-run must not create the install lock file"
    );
    assert!(
        !world.layout.central_log.exists(),
        "dry-run probes must not create operation records"
    );
}

#[test]
fn disable_keeps_receipt_when_uninstall_fails() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    // Now force uninstall to fail.
    world.apply_env(&guard, Some("uninstall"));
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable runs");
    assert!(
        !disabled.claim_removed,
        "receipt must be kept on cleanup failure"
    );
    assert!(!disabled.report.cleanup_complete);

    // Receipt is kept and marked cleanup_failed for retry.
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn disable_untracked_plugin_requires_verified_absence_in_recorded_state_dir() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill(&world, "sec-audit");
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    // Include the persisted anchor used by externally rooted receipts.
    let mut state = world.load_state();
    state.upsert_adapter_trust_root(COMPONENT, FRAMEWORK, world.resource_root.clone());
    state
        .save(&world.layout.state_dir.join("installed.toml"))
        .expect("persist trust anchor");
    let skill = world.openclaw_home.join("skills/sec-audit");
    guard.set("FAKE_OPENCLAW_FAIL", "untracked");

    // An empty active instance must not hide the recorded instance's plugin.
    let active_home = world._root.path().join("other-openclaw");
    guard.set("OPENCLAW_STATE_DIR", &active_home);
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable untracked but registered plugin");
    assert!(!disabled.report.cleanup_complete);
    assert!(!disabled.claim_removed);
    assert!(world.registry_marker_exists());
    assert!(skill.is_dir());

    std::fs::remove_file(world.openclaw_home.join("registry").join(COMPONENT))
        .expect("remove plugin out of band");
    guard.set("FAKE_OC_PROBE_FAIL", "list");
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("failed list probe");
    assert!(!disabled.report.cleanup_complete);
    assert!(!disabled.claim_removed);
    guard.unset("FAKE_OC_PROBE_FAIL");

    for json in [
        "",
        "No plugins found",
        "{}",
        r#"{"plugins":[{}],"diagnostics":[]}"#,
        r#"{"plugins":[{"id":"tokenless","status":"disabled"}],"diagnostics":[]}"#,
        r#"{"plugins":[],"diagnostics":[{"level":"error","message":"discovery failed"}]}"#,
        r#"{"plugins":[],"diagnostics":[],"registry":{"diagnostics":[{"level":"warn","message":"stale registry"}]}}"#,
    ] {
        guard.set("FAKE_OC_LIST_JSON", json);
        let disabled = manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("uncertain absence keeps receipt");
        assert!(!disabled.report.cleanup_complete, "list output: {json}");
        assert!(!disabled.claim_removed, "list output: {json}");
        let state = world.load_state();
        assert_eq!(
            state
                .find_adapter_claim(COMPONENT, FRAMEWORK)
                .unwrap()
                .status,
            ClaimStatus::CleanupFailed
        );
        assert!(
            state
                .find_adapter_trust_root(COMPONENT, FRAMEWORK)
                .is_some()
        );
        assert!(skill.is_dir());
    }
    guard.unset("FAKE_OC_LIST_JSON");

    // A similar ID in the recorded instance and the exact ID in another
    // instance must not prevent recovery of this receipt.
    std::fs::write(world.openclaw_home.join("registry/tokenless-other"), b"")
        .expect("other plugin");
    std::fs::create_dir_all(active_home.join("registry")).expect("active registry");
    let active_marker = active_home.join("registry").join(COMPONENT);
    std::fs::write(&active_marker, b"").expect("active instance plugin");

    let argv_log = world.argv_log();
    guard.set("FAKE_OC_ARGV_LOG", &argv_log);
    let preview = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("preview recovery");
    assert!(!preview.claim_removed);
    assert!(!argv_log.exists(), "dry-run must not invoke the CLI");
    assert!(world.has_claim());

    // An unexpected file at a managed directory makes remove_tree fail.
    std::fs::remove_dir_all(&skill).expect("replace skill directory");
    std::fs::write(&skill, b"unexpected file").expect("block directory cleanup");
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("skill cleanup failure");
    assert!(!disabled.report.cleanup_complete);
    assert!(!disabled.claim_removed);
    assert!(
        disabled
            .report
            .messages
            .iter()
            .any(|message| message.contains("failed to remove skill dir"))
    );
    std::fs::remove_file(&skill).expect("repair skill path");
    std::fs::create_dir(&skill).expect("restore skill directory");

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("recover missing plugin");
    assert!(disabled.report.cleanup_complete, "{:?}", disabled.report);
    assert!(disabled.claim_removed);
    assert!(!world.has_claim());
    assert!(
        world
            .load_state()
            .find_adapter_trust_root(COMPONENT, FRAMEWORK)
            .is_none()
    );
    assert!(!skill.exists());
    assert!(active_marker.exists());
    assert!(
        manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("repeated disable")
            .report
            .cleanup_complete
    );
}

#[test]
fn disable_untracked_plugin_verifies_large_json_inventory() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    guard.set("FAKE_OPENCLAW_FAIL", "untracked");

    for registered in [true, false] {
        let mut plugins = vec![serde_json::json!({
            "id": "other-plugin",
            "description": "x".repeat(80 * 1024),
        })];
        if registered {
            plugins.push(serde_json::json!({"id": COMPONENT}));
        } else {
            std::fs::remove_file(world.openclaw_home.join("registry").join(COMPONENT))
                .expect("remove plugin out of band");
        }
        let json = serde_json::json!({"plugins": plugins, "diagnostics": []}).to_string();
        assert!(json.len() > 64 * 1024);
        guard.set("FAKE_OC_LIST_JSON", json);

        let disabled = manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("disable with a large inventory");
        assert_eq!(disabled.report.cleanup_complete, !registered);
        assert_eq!(disabled.claim_removed, !registered);
        assert_eq!(world.has_claim(), registered);
    }
}

#[test]
fn disable_without_cli_keeps_receipt_for_retry() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    // Point OPENCLAW_BIN at a path that does not exist: disable cannot run
    // the CLI, so it must keep the receipt for a later retry instead of
    // pretending cleanup completed.
    let missing = world._root.path().join("no-such-openclaw");
    guard.set_openclaw_bin(&missing);
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(!disabled.claim_removed, "receipt kept when CLI absent");
    assert!(!disabled.report.cleanup_complete);
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt kept");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

#[test]
fn forged_external_path_receipt_is_rejected_by_status() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    // Tamper with the persisted receipt: repoint the external-path resource
    // at /etc, outside the driver's allowed roots.
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
                *path = PathBuf::from("/etc/cron.d/evil");
            }
        }
    }
    state.save(&state_path).expect("save tampered state");

    let err = manager
        .status(Some(COMPONENT))
        .expect_err("forged receipt must be rejected");
    assert!(
        matches!(err, AdapterError::ClaimValidation(_)),
        "got {err:?}"
    );
}

#[test]
fn scan_includes_manifest_declaration_without_resource_directory() {
    let _guard = OpenClawEnvGuard::acquire();
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    let layout = FsLayout::system(Some(prefix.clone()));
    seed_state(&layout, &prefix);
    let manager = AdapterManager::new(
        layout.clone(),
        Some(prefix.join("home")),
        "tester".to_string(),
    );

    let report = manager.scan().expect("scan");
    let entry = report
        .entries
        .iter()
        .find(|e| e.component == COMPONENT && e.framework == FRAMEWORK)
        .expect("manifest declaration entry");
    assert!(entry.declared);
    assert!(entry.resource_root.is_none());
    assert!(entry.driver_available);
    assert!(!entry.enabled);
}

#[test]
fn user_scan_includes_system_state_declaration() {
    let _guard = OpenClawEnvGuard::acquire();
    let root = tempfile::tempdir().expect("tempdir");
    let prefix = root.path().to_path_buf();
    let system_prefix = prefix.join("system");
    let system_layout = FsLayout::system(Some(system_prefix.clone()));
    seed_state(&system_layout, &system_prefix);

    let user_home = prefix.join("home");
    std::fs::create_dir_all(&user_home).expect("home");
    let user_layout =
        FsLayout::user_with_overrides(user_home.clone(), None, None, None, None, None);
    let mut manager = AdapterManager::new(user_layout, Some(user_home), "tester".to_string());
    manager.push_visible_root(anolisa_core::adapter::manager::VisibleRoot {
        state_dir: system_layout.state_dir.clone(),
        contract_datadir_roots: vec![system_layout.datadir.clone()],
    });

    let report = manager.scan().expect("scan");
    let entry = report
        .entries
        .iter()
        .find(|e| e.component == COMPONENT && e.framework == FRAMEWORK)
        .expect("system declaration entry");
    assert!(entry.declared);
    assert!(entry.resource_root.is_none());
    assert!(!entry.enabled);
}

#[test]
fn scan_lists_resource_with_detection_and_receipt_state() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    // Before enable: discovered, driver available, detected, not enabled.
    let report = manager.scan().expect("scan");
    let entry = report
        .entries
        .iter()
        .find(|e| e.component == COMPONENT && e.framework == FRAMEWORK)
        .expect("entry");
    assert!(entry.driver_available);
    assert!(entry.framework_detected);
    assert!(!entry.enabled);
    assert!(entry.declared);
    assert_eq!(entry.resource_root.as_ref(), Some(&world.resource_root));

    // After enable: reported as enabled.
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let report = manager.scan().expect("scan again");
    let entry = report
        .entries
        .iter()
        .find(|e| e.component == COMPONENT)
        .expect("entry");
    assert!(entry.enabled);
    assert_eq!(entry.claim_status, Some(ClaimStatus::Enabled));
}

// ---------------------------------------------------------------------------
// dry-run disable regression tests (#1251)
// ---------------------------------------------------------------------------

/// Dry-run disable must leave `InstalledState` completely unchanged and
/// must not invoke framework CLI operations. A following real disable
/// must still clean up exactly once.
#[test]
fn dry_run_disable_leaves_state_unchanged() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    // Enable the adapter for real.
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let state_path = world.layout.state_dir.join("installed.toml");
    let state_bytes_before = std::fs::read(&state_path).expect("read state file");
    let log_bytes_before = std::fs::read(&world.layout.central_log).expect("read central log");
    std::fs::remove_file(&world.layout.lock_file).expect("remove released seed lock file");
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_some(),
        "pre-condition: receipt must exist after enable"
    );
    // The fake OpenClaw CLI wrote a registry marker for the plugin —
    // framework-side state we must prove dry-run does not touch.
    let registry_marker = world.openclaw_home.join("registry").join(COMPONENT);
    assert!(
        registry_marker.exists(),
        "pre-condition: openclaw registry marker must exist after enable"
    );

    // Dry-run disable.
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run disable");
    assert!(outcome.dry_run, "outcome must be flagged dry-run");
    assert!(
        !outcome.claim_removed,
        "dry-run must not remove the receipt"
    );
    assert!(
        outcome.report.cleanup_complete,
        "dry-run plan reports as complete"
    );
    assert!(
        !outcome.report.messages.is_empty(),
        "dry-run must describe planned actions"
    );

    // State file must be byte-identical — no writes at all.
    let state_bytes_after = std::fs::read(&state_path).expect("read state file after dry-run");
    assert_eq!(
        state_bytes_before, state_bytes_after,
        "installed.toml must be byte-identical after dry-run disable"
    );
    assert!(
        !world.layout.lock_file.exists(),
        "dry-run disable must not recreate the install lock file"
    );
    assert_eq!(
        std::fs::read(&world.layout.central_log).expect("read central log after dry-run"),
        log_bytes_before,
        "dry-run disable must not append operation records"
    );
    // Double-check: receipt still present and status unchanged.
    let state_after = world.load_state();
    let claim_after = state_after
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt must still exist after dry-run disable");
    assert_eq!(
        claim_after.status,
        anolisa_core::adapter::claim::ClaimStatus::Enabled,
        "receipt status must remain Enabled, not cleanup_failed"
    );
    // Framework state must be untouched: the plugin registry marker must
    // still exist (a real disable would have unregistered it).
    assert!(
        registry_marker.exists(),
        "openclaw registry marker must still exist after dry-run disable"
    );

    // Following real disable cleans up exactly once.
    let real = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("real disable");
    assert!(!real.dry_run);
    assert!(real.claim_removed, "real disable must remove receipt");
    assert!(world.layout.lock_file.is_file());
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "receipt must be gone after real disable"
    );
}

/// Dry-run disable must report meaningful planned actions for a plugin
/// adapter (one with a `FrameworkPlugin` resource).
#[test]
fn dry_run_disable_reports_plugin_unregister() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run disable");
    assert!(outcome.dry_run);

    let has_unregister = outcome
        .report
        .messages
        .iter()
        .any(|m| m.contains("would unregister"));
    assert!(
        has_unregister,
        "dry-run must describe the plugin unregister: {:?}",
        outcome.report.messages
    );

    let has_receipt = outcome
        .report
        .messages
        .iter()
        .any(|m| m.contains("would remove adapter receipt"));
    assert!(
        has_receipt,
        "dry-run must note receipt removal: {:?}",
        outcome.report.messages
    );
}

/// Dry-run disable of a component with no receipt is a no-op, same as
/// a real disable, and the outcome carries the dry_run flag.
#[test]
fn dry_run_disable_no_receipt_is_noop() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run disable no receipt");
    assert!(outcome.dry_run);
    assert!(!outcome.claim_removed);
    assert!(outcome.report.cleanup_complete);
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("no receipt")),
        "must report no receipt: {:?}",
        outcome.report.messages
    );
}

// ---------------------------------------------------------------------------
// Adapter operation notices
// ---------------------------------------------------------------------------

/// An OpenClaw plugin adapter block declaring both a `post_enable` and a
/// `post_disable` notice.
fn notices_adapter_block() -> String {
    format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.notices]]
when = "post_enable"
level = "info"
text = "Restart the framework to load the plugin."
command = "openclaw restart"

[[adapters.notices]]
when = "post_disable"
level = "warning"
text = "Cached tokens remain until the framework restarts."
"#
    )
}

#[test]
fn enable_persists_all_declared_notices_in_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &notices_adapter_block());
    world.apply_env(&guard, None);
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let claim = match outcome {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    // Both triggers are persisted so a later receipt-only disable can show
    // the post_disable notice.
    assert_eq!(claim.notices.len(), 2);

    let state = world.load_state();
    let persisted = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt persisted");
    assert_eq!(persisted.notices.len(), 2);
    assert!(
        persisted
            .notices
            .iter()
            .any(|n| n.when == NoticeWhen::PostEnable)
    );
    assert!(
        persisted
            .notices
            .iter()
            .any(|n| n.when == NoticeWhen::PostDisable)
    );
}

#[test]
fn dry_run_enable_previews_only_post_enable_notices() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &notices_adapter_block());
    world.apply_env(&guard, None);
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run enable");
    match outcome {
        EnableOutcome::Planned { notices, .. } => {
            assert_eq!(notices.len(), 1, "preview only post_enable notices");
            assert_eq!(notices[0].when, NoticeWhen::PostEnable);
            assert_eq!(notices[0].level, NoticeLevel::Info);
            assert_eq!(notices[0].text, "Restart the framework to load the plugin.");
            assert_eq!(notices[0].command.as_deref(), Some("openclaw restart"));
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .is_none(),
        "dry-run must not persist a receipt"
    );
}

#[test]
fn disable_returns_post_disable_notices_from_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &notices_adapter_block());
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(disabled.claim_removed);
    assert_eq!(disabled.notices.len(), 1);
    assert_eq!(disabled.notices[0].when, NoticeWhen::PostDisable);
    assert_eq!(disabled.notices[0].level, NoticeLevel::Warning);
    assert_eq!(
        disabled.notices[0].text,
        "Cached tokens remain until the framework restarts."
    );
}

#[test]
fn dry_run_disable_previews_post_disable_notices() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &notices_adapter_block());
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run disable");
    assert!(outcome.dry_run);
    assert!(!outcome.claim_removed);
    assert_eq!(outcome.notices.len(), 1);
    assert_eq!(outcome.notices[0].when, NoticeWhen::PostDisable);
    // The receipt is untouched: a real disable still shows the notice once.
    assert!(world.has_claim());
}

#[test]
fn failed_disable_shows_no_notices() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &notices_adapter_block());
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    world.apply_env(&guard, Some("uninstall"));
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable runs");
    assert!(!disabled.claim_removed);
    assert!(
        disabled.notices.is_empty(),
        "a degraded disable must not display post_disable notices"
    );
}

#[test]
fn notice_text_is_preserved_verbatim_in_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.notices]]
when = "post_enable"
text = "run {{datadir}}/bin/tool; echo $HOME `id`"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let claim = match outcome {
        EnableOutcome::Enabled(c) => *c,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    // Inert text: placeholders and shell metacharacters survive unchanged.
    assert_eq!(
        claim.notices[0].text,
        "run {datadir}/bin/tool; echo $HOME `id`"
    );
}

#[test]
fn framework_specific_notices_take_precedence() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.notices]]
when = "post_enable"
text = "generic notice"

[[adapters.openclaw.notices]]
when = "post_enable"
text = "openclaw-specific notice"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run enable");
    match outcome {
        EnableOutcome::Planned { notices, .. } => {
            assert_eq!(notices.len(), 1);
            assert_eq!(notices[0].text, "openclaw-specific notice");
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }
}

// ---------------------------------------------------------------------------
// Issue #1534: version gating, install policy, and runtime verification
// ---------------------------------------------------------------------------

/// 1. Host below the adapter minimum: no plugin install and no receipt.
#[test]
fn host_below_adapter_minimum_blocks_enable() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some(">=2026.5.0")));
    world.apply_env(&guard, None); // FAKE_OC_VERSION defaults to 2026.4.14
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("host below minimum must block enable");
    assert!(
        matches!(err, AdapterError::FrameworkVersionMismatch { .. }),
        "got {err:?}"
    );
    assert!(
        !world.registry_marker_exists(),
        "no plugin install before the version gate"
    );
    assert!(
        !world.has_claim(),
        "no receipt persisted on version mismatch"
    );
}

/// 2. Host version cannot be parsed: fail before any mutation.
#[test]
fn unparseable_host_version_blocks_enable() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some(">=2026.4.14")));
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_VERSION", "unreleased-nightly");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("unparseable version must block enable");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// 3. Install help does not expose `--force`: fail before mutation and before
///    the receipt is persisted.
#[test]
fn missing_install_force_blocks_before_mutation() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_FORCE", "0");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("missing --force must block enable");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(
        !world.has_claim(),
        "the force-capability gate runs before the receipt is persisted"
    );
}

/// 4. Unsafe flag supported but not authorized: the install argv omits it.
#[test]
fn unsafe_supported_without_authorization_omits_flag() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    let install = install_argv(&lines).expect("install argv recorded");
    assert!(
        install.contains("--force"),
        "install must pass --force: {install}"
    );
    assert!(
        !install.contains("--dangerously-force-unsafe-install"),
        "unsafe flag must be absent without authorization: {install}"
    );
}

/// 5. Unsafe flag supported and explicitly authorized: the single install
///    argv carries it exactly once.
#[test]
fn authorized_unsafe_supported_includes_flag_once() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect("authorized unsafe enable");
    let lines = argv_lines(&world.argv_log());
    let install = install_argv(&lines).expect("install argv recorded");
    assert_eq!(
        install
            .matches("--dangerously-force-unsafe-install")
            .count(),
        1,
        "unsafe flag must appear exactly once in the single install argv: {install}"
    );
    // No second install invocation.
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("plugins install ") && !l.contains("--help"))
            .count(),
        1,
        "exactly one real install must run"
    );
}

/// 6. Unsafe authorized but the host does not expose the flag: fail before
///    mutation, no receipt.
#[test]
fn authorized_unsafe_unsupported_blocks() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None); // FAKE_OC_INSTALL_UNSAFE defaults to 0
    let manager = world.manager();

    let err = manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect_err("authorized-but-unsupported unsafe must block");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// An advertised unsafe option that is a deprecated no-op is not an effective
/// capability. Explicit authorization fails before mutation and points the
/// operator at OpenClaw's policy configuration instead.
#[test]
fn authorized_unsafe_deprecated_noop_blocks() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    guard.set("FAKE_OC_INSTALL_UNSAFE_NOOP", "1");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let err = manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect_err("a deprecated no-op cannot satisfy unsafe authorization");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(reason.contains("deprecated no-op"), "{reason}");
            assert!(reason.contains("security.installPolicy"), "{reason}");
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    assert!(
        install_argv(&argv_lines(&world.argv_log())).is_none(),
        "preflight must block before a real install"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// 7. Config entry whose version condition is unmet: not set, and left out
///    of the receipt.
#[test]
fn config_version_mismatch_skips_config_and_claim() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "plugins.entries.tokenless.hooks.allowConversationAccess"
value = true
framework_version = ">=2026.5.0"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None); // host 2026.4.14 < 2026.5.0
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !lines.iter().any(|l| l.starts_with("config set")),
        "a config entry with an unmet version condition must not be applied: {lines:?}"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("claim");
    assert!(
        !claim
            .resources
            .iter()
            .any(|r| matches!(r.kind, ClaimResourceKind::FrameworkConfig { .. })),
        "skipped config must not appear in the receipt"
    );
}

/// 8. Config entry whose version condition is met: set, and recorded in the
///    receipt.
#[test]
fn config_version_match_applies_and_records() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let key = "plugins.entries.tokenless.hooks.allowConversationAccess";
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "{key}"
value = true
framework_version = ">=2026.4.0"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None); // host 2026.4.14 satisfies >=2026.4.0
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("config set") && l.contains(key)),
        "a config entry with a met version condition must be applied: {lines:?}"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("claim");
    assert!(
        claim.resources.iter().any(|r| matches!(
            &r.kind,
            ClaimResourceKind::FrameworkConfig { key: k, .. } if k == key
        )),
        "applied config must be recorded in the receipt"
    );
}

/// A failed re-enable must not discard config facts from the last successful
/// enable because those keys remain present on the host.
#[test]
fn reenable_install_failure_preserves_applied_config_facts() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "preserved.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("initial enable");
    world.apply_env(&guard, Some("install"));
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("re-enable install must fail");

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("cleanup receipt");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    assert!(
        claim.resources.iter().any(|resource| matches!(
            &resource.kind,
            ClaimResourceKind::FrameworkConfig {
                key,
                state: ConfigApplyState::Applied,
                ..
            } if key == "preserved.key"
        )),
        "the successful enable's config fact must survive failed re-enable"
    );
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw payload");
    };
    assert_eq!(payload.config_resources.len(), 1);
}

/// Successful re-enable reuses the matching applied fact without duplicating
/// either the resource or its payload reference.
#[test]
fn successful_reenable_keeps_config_receipt_idempotent() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "idempotent.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("initial enable");
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable");

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("enabled receipt");
    let config_resources: Vec<_> = claim
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .collect();
    assert_eq!(config_resources.len(), 1);
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw payload");
    };
    assert_eq!(payload.config_resources.len(), 1);
    assert_eq!(payload.config_resources[0], config_resources[0].id);
}

/// A command that mutates and then exits non-zero must leave a typed pending
/// fact rather than falsely claiming success or omitting uncertain host state.
#[test]
fn first_config_failure_after_mutation_records_pending_intent() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "first.key"
value = true

[[adapters.openclaw.config]]
key = "second.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "first.key");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("the first config write must fail enable");

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("cleanup receipt");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    assert!(
        world.config_marker_exists("first.key"),
        "fake host must reproduce mutation before failure"
    );
    let config_resources: Vec<_> = claim
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .collect();
    assert_eq!(config_resources.len(), 1);
    assert!(matches!(
        &config_resources[0].kind,
        ClaimResourceKind::FrameworkConfig {
            key,
            state: ConfigApplyState::Pending,
            ..
        } if key == "first.key"
    ));
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw payload");
    };
    assert!(payload.config_resources.is_empty());

    let config_sets: Vec<String> = argv_lines(&world.argv_log())
        .into_iter()
        .filter(|line| line.starts_with("config set "))
        .collect();
    assert_eq!(config_sets.len(), 1);
    assert!(config_sets[0].contains("first.key"));
    assert!(!config_sets[0].contains("second.key"));

    world.apply_env(&guard, None);
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "");
    world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable must replay and confirm the pending entry");
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("recovered receipt");
    assert_eq!(claim.status, ClaimStatus::Enabled);
    let config_resources: Vec<_> = claim
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .collect();
    assert_eq!(config_resources.len(), 2);
    assert!(config_resources.iter().all(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::FrameworkConfig {
            state: ConfigApplyState::Applied,
            ..
        }
    )));
    assert_eq!(
        config_resources
            .iter()
            .filter(|resource| matches!(
                &resource.kind,
                ClaimResourceKind::FrameworkConfig { key, .. } if key == "first.key"
            ))
            .count(),
        1,
        "the recovered pending key must not be duplicated"
    );
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw payload");
    };
    assert_eq!(payload.config_resources.len(), 2);
}

/// A pending config that the replacement manifest no longer selects cannot be
/// reconciled, so re-enable must fail before another framework mutation.
#[test]
fn reenable_rejects_pending_config_removed_from_manifest() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "removed.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "removed.key");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("the first config write must fail enable");
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(None));
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "");

    let err = world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an unselected pending config must block re-enable");
    let message = err.to_string();
    assert!(message.contains("removed.key"), "got {message}");
    assert!(message.contains("disable"), "got {message}");

    let lines = argv_lines(&world.argv_log());
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("plugins install ") && !line.contains("--help"))
            .count(),
        1,
        "the blocked re-enable must fail before another plugin install"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("config set removed.key "))
            .count(),
        1,
        "the removed pending key cannot be replayed"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("pending receipt must remain visible");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    assert!(claim.resources.iter().any(|resource| matches!(
        &resource.kind,
        ClaimResourceKind::FrameworkConfig {
            key,
            state: ConfigApplyState::Pending,
            ..
        } if key == "removed.key"
    )));
}

/// Explicit disable reports uncertain config that may remain on the host
/// before removing the receipt.
#[test]
fn disable_reports_pending_config_left_in_place() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "uncertain.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "uncertain.key");

    world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("the config write must leave pending state");
    guard.set("FAKE_OC_CONFIG_FAIL_AFTER_KEY", "");
    let outcome = world
        .manager()
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("explicit disable");

    assert!(outcome.claim_removed);
    assert!(outcome.report.cleanup_complete);
    assert!(
        outcome.report.messages.iter().any(|message| {
            message.contains("1 openclaw config entry")
                && message.contains("uncertain")
                && message.contains("left in place")
        }),
        "disable must disclose uncertain config before discarding the receipt: {:?}",
        outcome.report.messages
    );
    assert!(!world.has_claim());
}

/// A mid-sequence failure keeps the successful prefix confirmed, the failed
/// entry pending, and later unattempted entries absent.
#[test]
fn mid_sequence_config_failure_records_applied_prefix() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "first.key"
value = true

[[adapters.openclaw.config]]
key = "second.key"
value = true

[[adapters.openclaw.config]]
key = "third.key"
value = true
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_CONFIG_FAIL_KEY", "second.key");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    world
        .manager()
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("the second config write must fail enable");

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("cleanup receipt");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    let config_resources: Vec<_> = claim
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .collect();
    assert_eq!(config_resources.len(), 2);
    assert_eq!(config_resources[0].id, "openclaw_config_0");
    assert!(matches!(
        &config_resources[0].kind,
        ClaimResourceKind::FrameworkConfig {
            key,
            state: ConfigApplyState::Applied,
            ..
        } if key == "first.key"
    ));
    assert_eq!(config_resources[1].id, "openclaw_config_1");
    assert!(matches!(
        &config_resources[1].kind,
        ClaimResourceKind::FrameworkConfig {
            key,
            state: ConfigApplyState::Pending,
            ..
        } if key == "second.key"
    ));
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw payload");
    };
    assert_eq!(payload.config_resources, ["openclaw_config_0"]);

    let config_sets: Vec<String> = argv_lines(&world.argv_log())
        .into_iter()
        .filter(|line| line.starts_with("config set "))
        .collect();
    assert_eq!(config_sets.len(), 2);
    assert!(config_sets[0].contains("first.key"));
    assert!(config_sets[1].contains("second.key"));
    assert!(!config_sets.iter().any(|line| line.contains("third.key")));
}

/// 9. Inspect help exposes `--runtime`: runtime verification uses it.
#[test]
fn runtime_verification_uses_runtime_flag_when_supported() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSPECT_RUNTIME", "1");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    let inspect = inspect_argv(&lines).expect("inspect argv recorded");
    assert!(
        inspect.contains("--runtime") && inspect.contains("--json"),
        "runtime-capable host must inspect with --runtime --json: {inspect}"
    );
}

/// 10. Inspect help lacks `--runtime`: verification falls back to `--json`.
#[test]
fn runtime_verification_falls_back_to_json_only() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None); // FAKE_OC_INSPECT_RUNTIME defaults to 0
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    let inspect = inspect_argv(&lines).expect("inspect argv recorded");
    assert!(
        inspect.contains("--json"),
        "must inspect with --json: {inspect}"
    );
    assert!(
        !inspect.contains("--runtime"),
        "must not pass --runtime when unsupported: {inspect}"
    );
}

/// 11. Legacy diagnostics before the JSON must still parse.
#[test]
fn runtime_verification_tolerates_leading_diagnostics() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSPECT_DIAG", "1");
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable succeeds despite legacy diagnostics before the JSON");
    assert!(world.has_claim());
}

/// 12. Runtime status is not `loaded`: enable fails with diagnostics and the
///     receipt is kept for cleanup retry.
#[test]
fn runtime_status_error_fails_and_keeps_cleanup_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail enable");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                reason.contains("error") && reason.contains("loaded"),
                "diagnostics must surface the observed and expected status: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    assert!(
        world.registry_marker_exists(),
        "install ran before the failed runtime verification"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt kept for cleanup retry");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
}

/// 13. Dry-run: probes are allowed but nothing is mutated, and the plan shows
///     the single install command.
#[test]
fn dry_run_probes_but_does_not_mutate() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("dry-run enable");
    match outcome {
        EnableOutcome::Planned { plan, .. } => {
            let cmd = plan
                .register_command
                .expect("plan shows the install command");
            assert!(cmd.contains("--force"), "plan must show --force: {cmd}");
            assert!(
                !cmd.contains("--dangerously-force-unsafe-install"),
                "unauthorized dry-run plan must not show the unsafe flag: {cmd}"
            );
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }
    // Read-only probes may have run, but nothing was installed or persisted.
    let lines = argv_lines(&world.argv_log());
    assert!(
        install_argv(&lines).is_none(),
        "dry-run must not run a real install: {lines:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// 15. An unsafe authorization for a skill-only adapter is rejected before
///     any work.
#[test]
fn unsafe_authorization_rejected_for_skill_bundle() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect_err("unsafe authorization must be rejected for skill_bundle");
    assert!(
        matches!(err, AdapterError::UnsafeInstallNotApplicable { .. }),
        "got {err:?}"
    );
    assert!(!world.has_claim());
}

/// 1 (extended). A skill_bundle also honors the adapter-level version gate:
/// an incompatible host blocks enable with no receipt.
#[test]
fn skill_bundle_honors_adapter_version_gate() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.compat]
framework_version = ">=2026.5.0"

[adapters.openclaw]
skills = ["sec-audit"]
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None); // host 2026.4.14 < 2026.5.0
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("skill_bundle below the adapter minimum must be blocked");
    assert!(
        matches!(err, AdapterError::FrameworkVersionMismatch { .. }),
        "got {err:?}"
    );
    assert!(!world.has_claim());
}

/// P1 fail-closed: an unparseable `--version` blocks a plugin enable even
/// when the manifest declares no version condition.
#[test]
fn unparseable_version_blocks_even_without_condition() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage(); // default manifest declares no compat requirement
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_VERSION", "nightly-build");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an unreadable version must fail closed before install");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P1 fail-closed: a host whose inspect help exposes no `--json` is rejected
/// before the first mutation (the full profile, including inspect help, is
/// probed during prepare), so no plugin is installed and no receipt is left.
#[test]
fn missing_inspect_json_blocks_before_mutation() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSPECT_JSON", "0");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("no --json inspect support must fail closed before install");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                reason.contains("--json"),
                "must explain the missing --json capability: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    assert!(
        !world.registry_marker_exists(),
        "install must not run when runtime verification cannot be performed"
    );
    assert!(!world.has_claim());
}

/// P1 fail-closed: every read-only probe — `--version`, install/enable/inspect
/// `--help` — is performed before the first mutation, so a non-zero
/// exit from any of them blocks enable with no install and no receipt, even
/// when the output would otherwise look like a capability answer.
#[test]
fn nonzero_probe_exit_blocks_enable_before_mutation() {
    for (stage_label, note) in [
        ("version", "a non-zero `--version` with parseable output"),
        (
            "install_help",
            "a non-zero install --help still mentioning --force",
        ),
        (
            "inspect_help",
            "a non-zero inspect --help still mentioning --json",
        ),
        ("enable_help", "a non-zero enable --help"),
    ] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_PROBE_FAIL", stage_label);
        let manager = world.manager();

        let err = manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect_err(note);
        assert!(
            matches!(err, AdapterError::FrameworkCli { .. }),
            "{note}: got {err:?}"
        );
        assert!(!world.registry_marker_exists(), "{note}: no install");
        assert!(!world.has_claim(), "{note}: no receipt");
    }
}

/// Each probe runs exactly once in a real enable (`--version`,
/// install/enable/inspect `--help`) happen in prepare, and apply re-probes
/// nothing (it reuses the prepared capabilities).
#[test]
fn each_probe_runs_exactly_once_per_enable() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    let count = |pred: &dyn Fn(&&String) -> bool| lines.iter().filter(|l| pred(l)).count();
    assert_eq!(
        count(&|l| l.as_str() == "--version"),
        1,
        "one --version probe: {lines:?}"
    );
    assert_eq!(
        count(&|l| l.as_str() == "plugins install --help"),
        1,
        "one install --help probe: {lines:?}"
    );
    assert_eq!(
        count(&|l| l.as_str() == "plugins enable --help"),
        1,
        "one enable --help probe: {lines:?}"
    );
    assert_eq!(
        count(&|l| l.as_str() == "plugins inspect --help"),
        1,
        "one inspect --help probe: {lines:?}"
    );
}

#[test]
fn enable_accepts_capabilities_per_subcommand_help() {
    let guard = OpenClawEnvGuard::acquire();
    for (support, enable_support) in [
        ("1", "1"),
        ("1", "0"),
        ("0", "1"),
        ("0", "0"),
        ("near_match", "near_match"),
    ] {
        let world = stage();
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_INSTALL_ACCEPT", support);
        guard.set("FAKE_OC_ENABLE_ACCEPT", enable_support);
        let argv_log = world.argv_log();
        guard.set("FAKE_OC_ARGV_LOG", &argv_log);
        let manager = world.manager();
        let preview = manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect("preview");
        let EnableOutcome::Planned { plan, .. } = preview else {
            panic!("expected preview")
        };
        assert_eq!(
            plan.register_command
                .unwrap()
                .contains("--accept-capabilities"),
            support == "1"
        );
        let activation = plan.actions.last().expect("activation preview");
        assert!(activation.contains("plugins enable tokenless"));
        assert_eq!(
            activation.contains("--accept-capabilities"),
            enable_support == "1"
        );
        assert!(!world.has_claim());
        assert!(!world.registry_marker_exists());
        assert!(
            argv_lines(&argv_log)
                .iter()
                .all(|line| line == "--version" || line.ends_with("--help"))
        );
        std::fs::write(&argv_log, "").expect("reset probe log");
        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("enable");
        let log = std::fs::read_to_string(&argv_log).expect("argv log");
        assert_eq!(
            log.lines()
                .filter(|line| *line == "plugins install --help")
                .count(),
            1
        );
        let install = log
            .lines()
            .find(|line| line.starts_with("plugins install ") && !line.ends_with("--help"))
            .expect("install argv");
        assert_eq!(install.contains("--accept-capabilities"), support == "1");
        assert!(!install.contains("--dangerously-force-unsafe-install"));
        let activation = log
            .lines()
            .find(|line| line.starts_with("plugins enable tokenless"))
            .expect("activation argv");
        assert_eq!(
            activation.contains("--accept-capabilities"),
            enable_support == "1"
        );
        assert!(!activation.contains("--dangerously-force-unsafe-install"));
        assert!(world.registry_marker_exists());
    }
}

#[test]
fn explicit_enable_failure_keeps_receipt_for_cleanup() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("enable"));
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("activation failure");
    assert!(
        matches!(err, AdapterError::FrameworkCli { reason, .. } if reason.contains("plugins enable") && reason.contains("boom-enable"))
    );
    assert!(world.registry_marker_exists());
    assert_eq!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .expect("cleanup receipt")
            .status,
        ClaimStatus::CleanupFailed
    );
    assert!(
        inspect_argv(&argv_lines(&world.argv_log())).is_none(),
        "do not verify after activation fails"
    );
    guard.unset("FAKE_OPENCLAW_FAIL");
    assert!(
        manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("cleanup")
            .claim_removed
    );
}

#[test]
fn capability_consent_failure_takes_precedence_over_safety_warning() {
    let guard = OpenClawEnvGuard::acquire();
    for failure in ["install_consent", "install_consent_warning"] {
        let world = stage();
        world.apply_env(&guard, Some(failure));
        guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
        let err = world
            .manager()
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect_err("consent rejection");
        let reason = err.to_string();
        assert!(reason.contains("OpenClaw capability consent"), "{reason}");
        assert!(
            !reason.contains("--allow-unsafe-plugin-install"),
            "{reason}"
        );
        assert_eq!(
            world
                .load_state()
                .find_adapter_claim(COMPONENT, FRAMEWORK)
                .unwrap()
                .status,
            ClaimStatus::CleanupFailed
        );
    }
}

/// P2 (negative): when the host does NOT expose the unsafe flag, a plain
/// safety-rejected install must not dangle the `--allow-unsafe-plugin-install`
/// hint (retrying would just fail in prepare), and there is no auto-retry.
#[test]
fn safety_rejection_without_unsafe_support_omits_hint() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install_unsafe_policy"));
    // FAKE_OC_INSTALL_UNSAFE defaults to 0 → host does not expose the flag.
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("safety-rejected install must fail");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                !reason.contains("--allow-unsafe-plugin-install"),
                "must not suggest an unsafe retry the host cannot honor: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    let lines = argv_lines(&world.argv_log());
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("plugins install ") && !l.contains("--help"))
            .count(),
        1,
        "must not auto-retry the install: {lines:?}"
    );
}

/// An advertised deprecated no-op is equivalent to no effective unsafe
/// capability for retry guidance.
#[test]
fn safety_rejection_with_deprecated_noop_omits_hint() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install_unsafe_policy"));
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    guard.set("FAKE_OC_INSTALL_UNSAFE_NOOP", "1");
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("safety-rejected install must fail");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                !reason.contains("--allow-unsafe-plugin-install"),
                "must not suggest retrying with a no-op option: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
}

/// P2: plugin-safety findings printed to stdout are surfaced in the failure,
/// alongside the explicit-retry hint.
#[test]
fn safety_rejection_on_stdout_is_surfaced() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install_unsafe_policy_stdout"));
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1"); // host supports the unsafe flag
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("stdout safety rejection must fail");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                reason.contains("SECURITY FINDING"),
                "stdout findings must be surfaced to the operator: {reason}"
            );
            assert!(
                reason.contains("--allow-unsafe-plugin-install"),
                "must hint the explicit retry: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
}

/// P1: two config entries sharing a key but gated on different versions —
/// only the entry whose condition the host satisfies is applied and recorded.
#[test]
fn same_key_config_applies_only_selected_version() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "shared.key"
value = "future"
framework_version = ">=2026.5.0"

[[adapters.openclaw.config]]
key = "shared.key"
value = "current"
framework_version = ">=2026.4.0"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None); // host 2026.4.14: only the ">=2026.4.0" entry matches
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    let config_sets: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("config set "))
        .collect();
    assert_eq!(
        config_sets.len(),
        1,
        "only the version-selected same-key entry must be applied: {config_sets:?}"
    );
    assert!(
        config_sets[0].contains("current") && !config_sets[0].contains("future"),
        "the applied value must be the selected version's, not the skipped one: {config_sets:?}"
    );
    // The receipt records exactly one config resource.
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("claim");
    assert_eq!(
        claim
            .resources
            .iter()
            .filter(|r| matches!(r.kind, ClaimResourceKind::FrameworkConfig { .. }))
            .count(),
        1,
        "only the selected config entry must appear in the receipt"
    );
}

/// P2: when a normal install is rejected by OpenClaw's plugin-safety policy
/// and the host exposes the unsafe flag, the error points the operator at the
/// explicit `--allow-unsafe-plugin-install` retry — without auto-retrying.
#[test]
fn safety_rejection_surfaces_explicit_retry_hint() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, Some("install_unsafe_policy"));
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1"); // host supports the unsafe flag
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("safety-rejected install must fail");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                reason.contains("--allow-unsafe-plugin-install"),
                "must hint the explicit retry authorization: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    // No automatic unsafe retry: only the one failed (safe) install ran.
    let lines = argv_lines(&world.argv_log());
    let installs: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("plugins install ") && !l.contains("--help"))
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "must not auto-retry the install: {installs:?}"
    );
    assert!(
        !installs[0].contains("--dangerously-force-unsafe-install"),
        "the failed attempt must have been the safe one: {installs:?}"
    );
}

/// P1: an explicitly empty adapter-level version requirement is a manifest
/// error, not a silent "no requirement" — it must not fall through to enable.
#[test]
fn empty_compat_framework_version_is_invalid() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.compat]
framework_version = ""
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an empty compat.framework_version must be rejected");
    assert!(
        matches!(err, AdapterError::InvalidAdapterInput { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P1: an explicitly empty per-config version condition is a manifest error;
/// no config is applied and no receipt is written.
#[test]
fn empty_config_framework_version_is_invalid() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "some.key"
value = true
framework_version = ""
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an empty config framework_version must be rejected");
    assert!(
        matches!(err, AdapterError::InvalidAdapterInput { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P1: a malformed version constraint is a typed input error (not a generic
/// framework-CLI error), and enable stops before any mutation.
#[test]
fn malformed_config_constraint_is_invalid_input() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "some.key"
value = true
framework_version = ">=not.a.version"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a malformed constraint must be rejected");
    assert!(
        matches!(err, AdapterError::InvalidAdapterInput { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// Every config condition clause is validated before the condition is
/// evaluated. A malformed later clause cannot hide behind an earlier
/// non-match and silently skip the config.
#[test]
fn malformed_later_config_clause_is_invalid_input() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.config]]
key = "some.key"
value = true
framework_version = ">=2027.0.0, >=not.a.version"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None); // host fails the first clause
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a malformed later clause must still be rejected");
    assert!(
        matches!(err, AdapterError::InvalidAdapterInput { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P1: a malformed adapter-level constraint (an empty `-` suffix here) is a
/// typed input error and stops enable before any mutation — it must not be
/// silently treated as the well-formed `>=2026.4.14`.
#[test]
fn malformed_compat_constraint_is_invalid_input() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some(">=2026.4.14-")));
    world.apply_env(&guard, None); // host 2026.4.14 would satisfy >=2026.4.14
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a malformed compat constraint must be rejected");
    assert!(
        matches!(err, AdapterError::InvalidAdapterInput { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P1: a `--version` output carrying only an unrelated (non-calendar) number
/// is treated as unknown; with a declared requirement, enable fails closed.
#[test]
fn unrelated_numeric_version_is_not_accepted() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some(">=2026.4.0")));
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_VERSION", "22.14.0"); // not calendar-shaped
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-calendar version must not satisfy the gate");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "got {err:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// A calendar-shaped token in a warning must not be selected ahead of the
/// explicit OpenClaw version line.
#[test]
fn version_warning_date_does_not_override_openclaw_version() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some("<2027.0.0")));
    world.apply_env(&guard, None);
    guard.set(
        "FAKE_OC_VERSION_PREAMBLE",
        "warning: certificate expires on 2099.1.1",
    );
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the explicit 2026.4.14 version satisfies the gate");
    assert!(world.registry_marker_exists());
    assert!(world.has_claim());
}

/// P2: when `--version` output is multi-line and unparseable, the failure
/// preserves the full trimmed output (not just the first line), so the real
/// version text is actionable even behind a leading warning line.
#[test]
fn unparseable_version_error_keeps_full_output() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(Some(">=2026.4.0")));
    world.apply_env(&guard, None);
    guard.set(
        "FAKE_OC_VERSION_PREAMBLE",
        "warning: config migration pending",
    );
    guard.set("FAKE_OC_VERSION", "nightly-build"); // unparseable, on the 2nd line
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("unparseable version must block");
    match err {
        AdapterError::FrameworkCli { reason, .. } => {
            assert!(
                reason.contains("nightly-build"),
                "the real (unparseable) version line must survive, not just the warning: {reason}"
            );
        }
        other => panic!("expected FrameworkCli, got {other:?}"),
    }
    assert!(!world.has_claim());
}

/// P2 acceptance: an authorized-unsafe dry-run shows the unsafe flag in the
/// planned install command and mutates nothing.
#[test]
fn authorized_unsafe_dry_run_shows_flag_without_mutation() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let outcome = manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            true,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect("dry-run enable");
    match outcome {
        EnableOutcome::Planned { plan, .. } => {
            let cmd = plan
                .register_command
                .expect("plan shows the install command");
            assert!(
                cmd.contains("--dangerously-force-unsafe-install"),
                "authorized dry-run plan must show the unsafe flag: {cmd}"
            );
        }
        EnableOutcome::Enabled(_) => panic!("dry-run must not enable"),
    }
    let lines = argv_lines(&world.argv_log());
    assert!(
        install_argv(&lines).is_none(),
        "dry-run must not run a real install: {lines:?}"
    );
    assert!(!world.registry_marker_exists());
    assert!(!world.has_claim());
}

/// P2 acceptance: an authorized real enable records the exact install command,
/// including the unsafe flag, in the live central operation log.
#[test]
fn central_log_records_authorized_unsafe_install_argv() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_INSTALL_UNSAFE", "1");
    let manager = world.manager();

    manager
        .enable_with_options(
            COMPONENT,
            Some(FRAMEWORK),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect("authorized unsafe enable");
    let log = std::fs::read_to_string(&world.layout.central_log).expect("central log");
    assert!(
        log.contains("plugins install")
            && log.contains("--force")
            && log.contains("--dangerously-force-unsafe-install"),
        "central log must record the exact install argv incl. the unsafe flag: {log}"
    );
}

/// 15. An unsafe authorization for a non-OpenClaw framework is rejected.
#[test]
fn unsafe_authorization_rejected_for_non_openclaw_framework() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    let block = format!(
        r#"[[adapters]]
framework = "hermes"
source = "adapters/{COMPONENT}/hermes"
dest = "{{datadir}}/adapters/{{component}}/hermes/"
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    world.apply_env(&guard, None);
    let manager = world.manager();

    let err = manager
        .enable_with_options(
            COMPONENT,
            Some("hermes"),
            false,
            EnableOptions {
                allow_unsafe_plugin_install: true,
                profiles: Vec::new(),
            },
        )
        .expect_err("unsafe authorization must be rejected for a non-OpenClaw framework");
    assert!(
        matches!(err, AdapterError::UnsafeInstallNotApplicable { .. }),
        "got {err:?}"
    );
    assert!(
        world
            .load_state()
            .find_adapter_claim(COMPONENT, "hermes")
            .is_none(),
        "no receipt for a rejected unsafe authorization"
    );
}

// ---------------------------------------------------------------------------
// Adapter observation contracts (#3310, first slice of #3309)
//
// Behavior-preserving regression coverage for the two read-only adapter
// collectors — `AdapterManager::scan` (candidate and receipt rows) and
// `AdapterManager::status` (per-receipt condition projection). These pin the
// shared facts the evidence-pipeline consolidation in #3309 must not change:
// source authority, candidate versus receipt sets, integrity precedence, and
// the read-only boundary (including which framework queries each collector is
// allowed to issue).
// ---------------------------------------------------------------------------

/// Every path under `root` with a content digest for regular files, sorted.
/// A read-only collector must leave this identical.
fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Option<String>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Option<String>)>) {
        let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("read snapshot dir")
            .map(|entry| entry.expect("snapshot dir entry").path())
            .collect();
        children.sort();
        for path in children {
            let metadata = std::fs::symlink_metadata(&path).expect("snapshot metadata");
            if metadata.file_type().is_dir() {
                out.push((path.clone(), None));
                walk(&path, out);
            } else {
                let digest = std::fs::read(&path)
                    .ok()
                    .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
                out.push((path, digest));
            }
        }
    }

    let mut out = Vec::new();
    if root.is_dir() {
        out.push((root.to_path_buf(), None));
        walk(root, &mut out);
    }
    out
}

/// The `(component, framework)` scan row, failing with the whole report when
/// the collector dropped it.
fn scan_row<'a>(report: &'a ScanReport, component: &str, framework: &str) -> &'a ScanEntry {
    report
        .entries
        .iter()
        .find(|entry| entry.component == component && entry.framework == framework)
        .unwrap_or_else(|| {
            panic!(
                "no scan row for {component}/{framework}; got {:?}",
                report
                    .entries
                    .iter()
                    .map(|entry| (
                        entry.component.clone(),
                        entry.framework.clone(),
                        entry.declared,
                        entry.enabled
                    ))
                    .collect::<Vec<_>>()
            )
        })
}

/// The tri-state result of one condition kind, failing with the projected
/// condition list when the collector omitted it.
fn condition(report: &AdapterStatusReport, kind: AdapterConditionKind) -> ConditionStatus {
    report
        .conditions
        .iter()
        .find(|condition| condition.kind == kind)
        .unwrap_or_else(|| {
            panic!(
                "condition {kind:?} missing; got {:?}",
                report
                    .conditions
                    .iter()
                    .map(|condition| condition.kind)
                    .collect::<Vec<_>>()
            )
        })
        .status
}

/// How many times `kind` appears in a status report. Manager-owned conditions
/// must never be duplicated by whatever the driver reported.
fn condition_count(report: &AdapterStatusReport, kind: AdapterConditionKind) -> usize {
    report
        .conditions
        .iter()
        .filter(|condition| condition.kind == kind)
        .count()
}

/// Whether one recorded framework argv is a read-only query: a version probe,
/// a registry/runtime listing, or a `--help` capability probe. Every verb that
/// could change framework state (install, uninstall, enable, disable, config)
/// is a mutation unless it is the `--help` form the drivers use to discover
/// host capabilities.
fn read_only_framework_query(argv: &str) -> bool {
    let mut tokens = argv.split_whitespace();
    match tokens.next() {
        Some("--version") => tokens.next().is_none(),
        Some("plugins") => match tokens.next() {
            Some("list") | Some("inspect") => true,
            Some("install") | Some("uninstall") | Some("enable") | Some("disable") => {
                tokens.next() == Some("--help")
            }
            _ => false,
        },
        _ => false,
    }
}

/// Byte length of the central log, or 0 while the file does not exist yet.
/// Lets a test scope an audit assertion to the records one phase appended
/// instead of matching text an earlier phase already wrote.
fn central_log_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// The framework argv of every `framework cli` record appended to the central
/// log after `offset`, in log order. The exact `program` token the driver
/// recorded is stripped, so the result is directly comparable with the fake
/// CLI's own argv log: one entry per spawned query. Stripping at the first
/// space instead would mis-parse any program path that contains one, which is
/// what a `TMPDIR` with a space in it hands every staged world.
fn central_log_framework_argv_after(path: &Path, offset: u64, program: &Path) -> Vec<String> {
    const PREFIX: &str = "framework cli: ";
    let program = program.to_string_lossy();
    let bytes = std::fs::read(path).expect("read central log");
    assert!(
        u64::try_from(bytes.len()).expect("log length") >= offset,
        "central log shrank below the recorded offset {offset}"
    );
    let appended =
        String::from_utf8(bytes[offset as usize..].to_vec()).expect("central log is utf-8");
    appended
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<LogRecord>(line)
                .unwrap_or_else(|error| panic!("central log line is not a record: {error}: {line}"))
        })
        .filter(|record| record.message.starts_with(PREFIX))
        .map(|record| {
            // `<program> <args…>` → `<args…>`, which is what the fake CLI
            // writes for `$*`.
            let command = &record.message[PREFIX.len()..];
            let args = command
                .strip_prefix(program.as_ref())
                .unwrap_or_else(|| {
                    panic!(
                        "audited command does not start with the staged CLI {program}: {command}"
                    )
                })
                .strip_prefix(' ')
                .unwrap_or_default();
            args.to_string()
        })
        .collect()
}

/// A candidate row — declared by the contract and/or discovered on disk, but
/// with no receipt — carries no source verdict at all, and produces no
/// `status` row. Source health is a persisted-state concern, so it only exists
/// once a receipt does; receipt-only status stays distinct from candidate scan.
#[test]
fn scan_candidate_row_carries_no_source_verdict_or_status_row() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();

    let report = manager.scan().expect("scan");
    let row = scan_row(&report, COMPONENT, FRAMEWORK);
    assert!(row.declared, "the installed manifest declares this adapter");
    assert!(!row.enabled, "a candidate row has no receipt");
    assert_eq!(row.claim_status, None);
    assert_eq!(
        row.source_status, None,
        "a candidate row must not carry a source verdict"
    );
    assert_eq!(row.source_reason, None);
    assert_eq!(row.resource_root.as_ref(), Some(&world.resource_root));

    let status = manager.status(None).expect("status");
    assert!(
        status.entries.is_empty(),
        "status must not re-project the candidate set; got {:?}",
        status
            .entries
            .iter()
            .map(|entry| (entry.component.clone(), entry.framework.clone()))
            .collect::<Vec<_>>()
    );
}

/// Once a receipt exists the same row gains the source-authority verdict: the
/// component is still visibly installed and its contract still resolves to a
/// valid bundle, so the source is `Available` with no operator explanation.
#[test]
fn scan_receipt_row_reports_available_source() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let report = manager.scan().expect("scan");
    let row = scan_row(&report, COMPONENT, FRAMEWORK);
    assert!(row.enabled);
    assert_eq!(row.claim_status, Some(ClaimStatus::Enabled));
    assert_eq!(row.source_status, Some(AdapterSourceStatus::Available));
    assert_eq!(
        row.source_reason, None,
        "an available source needs no operator explanation"
    );
    assert_eq!(row.resource_root.as_ref(), Some(&world.resource_root));
}

/// A receipt outlives its source. When the package-owned bundle disappears the
/// row stays visible — there is still something to clean up — but flips to
/// `Missing` with an operator-facing reason, and the vanished directory is no
/// longer offered as a usable resource root.
#[test]
fn scan_receipt_row_reports_missing_source_after_bundle_loss() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    std::fs::remove_dir_all(&world.resource_root).expect("remove package-owned bundle");

    let report = manager.scan().expect("scan");
    let row = scan_row(&report, COMPONENT, FRAMEWORK);
    assert!(row.enabled, "the receipt survives its source");
    assert_eq!(row.claim_status, Some(ClaimStatus::Enabled));
    assert_eq!(row.source_status, Some(AdapterSourceStatus::Missing));
    let reason = row
        .source_reason
        .clone()
        .expect("a missing source explains itself");
    assert!(
        reason.contains(COMPONENT),
        "the reason names the component whose source vanished: {reason}"
    );
    assert_eq!(
        row.resource_root, None,
        "a lost bundle must not be reported as a usable resource root"
    );
}

/// `status` projects source authority ahead of integrity: the source verdict
/// is always the first condition, the package-owned integrity signals follow
/// in a fixed order, and the manager's own copies are never duplicated by
/// whatever the driver reported.
#[test]
fn status_orders_source_authority_before_integrity() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries.len(), 1);
    let report = &status.entries[0].report;
    assert_eq!(report.summary, AdapterSummary::Healthy);

    let kinds = report
        .conditions
        .iter()
        .map(|condition| condition.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds[0],
        AdapterConditionKind::SourceAvailable,
        "source authority leads the projection: {kinds:?}"
    );
    assert_eq!(kinds[1], AdapterConditionKind::ManagedBundleMatches);
    assert_eq!(kinds[2], AdapterConditionKind::SourceRevisionMatches);

    for kind in [
        AdapterConditionKind::SourceAvailable,
        AdapterConditionKind::ManagedBundleMatches,
        AdapterConditionKind::SourceRevisionMatches,
    ] {
        assert_eq!(
            condition_count(report, kind),
            1,
            "{kind:?} must appear exactly once: {kinds:?}"
        );
    }
    assert!(
        condition_count(report, AdapterConditionKind::MaterializedBundleMatches) <= 1,
        "materialized integrity is driver-optional but must never be duplicated: {kinds:?}"
    );

    assert_eq!(
        condition(report, AdapterConditionKind::SourceAvailable),
        ConditionStatus::True
    );
    assert_eq!(
        condition(report, AdapterConditionKind::ManagedBundleMatches),
        ConditionStatus::True
    );
    assert_eq!(
        condition(report, AdapterConditionKind::SourceRevisionMatches),
        ConditionStatus::True
    );
}

/// A lost source degrades the receipt, but the integrity signals that can no
/// longer be read stay `Unknown` — "could not verify", never "verified
/// absent". Conflating the two would tell an operator the package files were
/// proven gone when in fact nothing could be proven at all.
#[test]
fn status_missing_source_degrades_without_faking_integrity() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    std::fs::remove_dir_all(&world.resource_root).expect("remove package-owned bundle");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries.len(), 1);
    let report = &status.entries[0].report;
    assert_eq!(report.summary, AdapterSummary::Degraded);
    assert_eq!(
        condition(report, AdapterConditionKind::SourceAvailable),
        ConditionStatus::False
    );
    assert_eq!(
        condition(report, AdapterConditionKind::ManagedBundleMatches),
        ConditionStatus::Unknown,
        "an unreadable source is unverified, not verified-absent"
    );
    assert_eq!(
        condition(report, AdapterConditionKind::SourceRevisionMatches),
        ConditionStatus::Unknown,
        "an unreadable source is unverified, not verified-absent"
    );
}

/// Tampering a package-owned source file is *verified* drift, not missing
/// evidence, and it is attributed to exactly one of the two integrity signals.
/// `ManagedBundleMatches` reads the bytes on disk, so it goes `False`;
/// `SourceRevisionMatches` compares the authoritative package metadata
/// revision captured at enable, which on-disk tampering does not change, so it
/// stays `True` — a tampered file is not a package upgrade. `False` outranks
/// `Unknown`, so the summary is Degraded while source authority stays `True`.
#[test]
fn status_tampered_package_source_degrades_with_false_integrity() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    // Still a valid bundle marker with the same plugin id, but different
    // bytes: the package-owned file no longer matches the recorded inventory.
    std::fs::write(
        world.resource_root.join("openclaw.plugin.json"),
        format!(r#"{{"id":"{COMPONENT}","name":"Tokenless (tampered)"}}"#),
    )
    .expect("tamper package-owned source");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries.len(), 1);
    let report = &status.entries[0].report;
    assert_eq!(report.summary, AdapterSummary::Degraded);
    assert_eq!(
        condition(report, AdapterConditionKind::SourceAvailable),
        ConditionStatus::True,
        "the source is still there; only its bytes drifted"
    );
    assert_eq!(
        condition(report, AdapterConditionKind::ManagedBundleMatches),
        ConditionStatus::False,
        "the bytes on disk no longer match the recorded package inventory"
    );
    assert_eq!(
        condition(report, AdapterConditionKind::SourceRevisionMatches),
        ConditionStatus::True,
        "tampering is not a package revision change; the two signals stay independent"
    );
}

/// The audited argv must survive a staged prefix whose path contains a space.
/// `tempfile` inherits `TMPDIR`, so a developer machine or CI runner with a
/// space in its temp root stages a fake CLI whose absolute path has one too;
/// stripping the program token at the first space then leaves a path fragment
/// in the audited argv and the comparison fails on a host where both the
/// production call and the audit record are correct.
#[test]
fn central_log_argv_audit_survives_a_staged_prefix_with_spaces() {
    let guard = OpenClawEnvGuard::acquire();
    let parent = tempfile::tempdir().expect("parent tempdir");
    let spaced = parent.path().join("prefix with spaces");
    std::fs::create_dir_all(&spaced).expect("spaced prefix");
    let root = tempfile::Builder::new()
        .tempdir_in(&spaced)
        .expect("tempdir under a spaced prefix");
    let world = stage_in(root);
    assert!(
        world.fake_bin.to_string_lossy().contains(' '),
        "pre-condition: the staged CLI path must contain a space, got {:?}",
        world.fake_bin
    );
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let argv_log = world.argv_log();
    let _ = std::fs::remove_file(&argv_log);
    guard.set("FAKE_OC_ARGV_LOG", &argv_log);
    let offset = central_log_len(&world.layout.central_log);

    manager.status(None).expect("status");

    let queries = argv_lines(&argv_log);
    assert!(
        !queries.is_empty(),
        "status must have queried the framework to verify the receipt"
    );
    assert_eq!(
        central_log_framework_argv_after(&world.layout.central_log, offset, &world.fake_bin),
        queries,
        "the central log must record exactly the framework queries status \
         issued, spaces in the staged CLI path included"
    );
}

/// The read-only boundary, asserted three ways: `scan` never spawns the
/// framework at all, `status` only issues read-only framework queries, and
/// neither collector mutates installed state, the package-owned source tree or
/// framework-side state, nor takes the install lock. The queries themselves
/// stay auditable through the existing central log and framework argv log.
#[test]
fn scan_and_status_stay_inside_the_read_only_boundary() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    world.apply_env(&guard, None);
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let state_path = world.layout.state_dir.join("installed.toml");
    std::fs::remove_file(&world.layout.lock_file).expect("remove released seed lock file");
    let state_before = std::fs::read(&state_path).expect("read state");
    let datadir_before = tree_snapshot(&world.layout.datadir);
    let framework_before = tree_snapshot(&world.openclaw_home);
    assert!(
        world.registry_marker_exists(),
        "pre-condition: the plugin must be registered after enable"
    );

    // Record the framework argv — and the central-log offset — for the
    // read-only phase only: `enable` above already wrote its own records.
    let argv_log = world.argv_log();
    let _ = std::fs::remove_file(&argv_log);
    guard.set("FAKE_OC_ARGV_LOG", &argv_log);
    let central_log_offset = central_log_len(&world.layout.central_log);

    // Detection is side-effect-free by contract: it inspects PATH and the
    // filesystem, so scan never spawns the framework.
    let scan = manager.scan().expect("scan");
    assert!(!scan.entries.is_empty());
    let scan_queries = argv_lines(&argv_log);
    assert!(
        scan_queries.is_empty(),
        "scan must not spawn a framework query; got {scan_queries:?}"
    );

    // status may query the framework, but only through read-only verbs.
    let status = manager.status(None).expect("status");
    assert_eq!(status.entries.len(), 1);
    let queries = argv_lines(&argv_log);
    assert!(
        !queries.is_empty(),
        "status must have queried the framework to verify the receipt"
    );
    for argv in &queries {
        assert!(
            read_only_framework_query(argv),
            "status issued a mutating framework command: {argv}"
        );
    }

    assert_eq!(
        std::fs::read(&state_path).expect("read state after the collectors"),
        state_before,
        "installed.toml must be byte-identical after scan+status"
    );
    assert_eq!(
        tree_snapshot(&world.layout.datadir),
        datadir_before,
        "the package-owned source tree changed under a read-only collector"
    );
    assert_eq!(
        tree_snapshot(&world.openclaw_home),
        framework_before,
        "framework-side state changed under a read-only collector"
    );
    assert!(
        !world.layout.lock_file.exists(),
        "read-only collectors must not take the install lock"
    );
    assert!(
        world.registry_marker_exists(),
        "the plugin registration must survive a read-only status"
    );
    assert!(
        world.has_claim(),
        "the receipt must survive a read-only scan+status"
    );

    // The framework queries stay auditable, and only the read-only phase can
    // prove it: every query the fake CLI saw must have its own central-log
    // record appended after `enable`, in the same order. Matching the whole
    // file instead would be satisfied by the install phase's records, so the
    // assertion would survive `status` going silent entirely.
    assert_eq!(
        central_log_framework_argv_after(
            &world.layout.central_log,
            central_log_offset,
            &world.fake_bin,
        ),
        queries,
        "the central log must record exactly the framework queries status issued"
    );
}
