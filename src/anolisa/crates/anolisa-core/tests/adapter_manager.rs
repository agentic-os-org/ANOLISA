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
    AdapterClaim, ClaimResourceKind, ClaimStatus, ConfigApplyState, DisplacedPluginRef,
    DriverPayload,
};
use anolisa_core::adapter::driver::{
    AdapterCondition, AdapterConditionKind, AdapterSummary, ConditionStatus,
};
use anolisa_core::adapter::manager::{AdapterManager, EnableOptions, EnableOutcome, StatusReport};
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

    /// The staged prefix, so a world can be handed to a child process that
    /// reopens it from disk instead of inheriting it in memory.
    fn root(&self) -> &Path {
        self._root.path()
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
    "FAKE_OC_OPERATOR_DISABLES_ON_INSTALL",
    "FAKE_OC_CONFIG_GET_FAIL_ONCE",
    "FAKE_OC_CONFIG_GET_FAIL_ON_NTH",
    "FAKE_OC_ENABLE_FAIL_ID",
    "FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID",
    "FAKE_OC_KILL_PID_AFTER_UNINSTALL",
    "FAKE_OC_CONFIG_GET_FAIL_KEY",
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
    let root = tempfile::tempdir().expect("tempdir");
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
///   `plugins enable <id>` clears it, and `plugins disable <id>` persists it
///   without touching registration. `plugins list` prints registry markers.
///   Uninstall also applies OpenClaw's config rewrites — the plugin leaves
///   `plugins.allow`, and a memory slot it owned is reset to `memory-core`; see
///   `fake_uninstall_config_rewrite` in the script for the upstream references.
/// - `config get <key>` echoes the value `config set` (or a test) persisted for
///   that key, or an empty line when the key is absent;
///   `FAKE_OC_PROBE_FAIL=config_get` makes the probe fail.
/// - `FAKE_OPENCLAW_FAIL=untracked` refuses uninstall without changing the
///   registry; `FAKE_OC_LIST_JSON` overrides JSON listing, and
///   `FAKE_OC_PROBE_FAIL=list` makes listing fail;
/// - `FAKE_OC_OPERATOR_DISABLES_ON_INSTALL=<id>` persists `plugins.entries.<id>.enabled
///   = false` as part of a successful `plugins install`, i.e. somebody else turned
///   that plugin off inside the window between `prepare_enable` probing it and
///   `apply_displacements` mutating it.
///   `FAKE_OC_CONFIG_GET_FAIL_ONCE=<key>` fails `config get` for that key exactly
///   once and then answers normally, which is how a *transient* probe failure is
///   expressed — the difference between "the host could not answer this time" and
///   "the host cannot answer" is the whole point of the tests that use it.
///   `FAKE_OC_CONFIG_GET_FAIL_ON_NTH=<key>:<n>` fails the *n*-th `config get` of
///   that key (counted per state directory, from 1) and answers every other one
///   normally. `FAIL_ONCE` is `n = 1`; the point of the general form is reaching a
///   *later* read, which is how "prepare could read it, apply could not" is
///   expressed at all — neither a one-shot nor a persistent failure can put the two
///   probes into different states.
///   `FAKE_OC_CONFIG_GET_FAIL_KEY=<key>` fails `config get` for that one key and
///   no other, so a receipt with several displacements can be put into a *mixed*
///   state — one readable, one not — which a global probe failure cannot express.
///   `FAKE_OC_ENABLE_FAIL_ID=<id>` fails `plugins enable` for that one plugin id
///   and no other, which is how a *partial* restore is expressed: a receipt with
///   several displacements then has some handed back and some not. Neither a global
///   `FAKE_OPENCLAW_FAIL=enable` nor one of the restore vetoes can produce that
///   split — a veto reports the entry as released, not as failed.
///   `FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID=<id>` deletes the fake binary itself right
///   after that plugin's `plugins enable` succeeds, so the *next* CLI call cannot
///   be spawned at all. This is the only knob that expresses "the driver errored
///   out in the middle of a cleanup": a non-zero exit is a report, and it takes the
///   `cleanup_complete` path instead. It models an install broken or a package
///   removed while a disable is running.
///   `FAKE_OC_KILL_PID_AFTER_UNINSTALL=<pid>` SIGKILLs that process from inside a
///   successful `plugins uninstall`, immediately after the config rewrite. Where
///   the knob above reaches the driver as an error the Manager can report and
///   persist around, this one runs no handler at all in the process it kills, so it
///   is the only way to express "the process died between a destructive host
///   mutation and the save that follows it". It needs a process to aim at: see
///   `disable_child_killed_between_the_uninstall_and_the_save`.
/// - `FAKE_OPENCLAW_FAIL=install|install_after_register|uninstall` forces that
///   verb to exit non-zero; `FAKE_OC_CONFIG_FAIL_KEY` fails `config set`
///   before mutation, while `FAKE_OC_CONFIG_FAIL_AFTER_KEY` fails after
///   writing a marker for one exact key.
///
/// Independently of any knob, `plugins install` and `plugins enable` apply
/// OpenClaw's exclusive slot selection when the plugin declares a slot `kind`: the
/// slot comes to name it and every other plugin of that kind is written
/// `enabled = false`. A bundle's kind comes from its `openclaw.plugin.json`
/// (`declare_bundle_plugin_kind`), a bundled plugin's from its registry marker
/// (`seed_bundled_plugin_with_kind`); fixtures that declare neither see no slot
/// selection at all, exactly as before.
///
/// When `FAKE_OC_ARGV_LOG` names a file, every invocation appends its full
/// argv (one line) — test instrumentation, not OpenClaw state.
fn write_fake_openclaw(dir: &Path) -> PathBuf {
    let script = r#"#!/bin/sh
if [ -n "${FAKE_OC_ARGV_LOG:-}" ]; then printf '%s\n' "$*" >> "$FAKE_OC_ARGV_LOG"; fi

# OpenClaw applies exclusive slot selection on both `plugins install` and
# `plugins enable` (v2026.4.14: src/cli/plugins-install-persist.ts and
# src/cli/plugins-cli.ts, both through applySlotSelectionForPlugin ->
# applyExclusiveSlotSelection): the slot for the selected plugin's kind comes to
# name it, and every *other* plugin of that kind is written `enabled = false`.
# Modelled because a driver has to tell that side effect apart from an operator's
# own `plugins disable`, and a fake that omits it makes the two indistinguishable
# in every lifecycle test. Not modelled: the upstream guard that spares a plugin
# still owning a different slot — no fixture declares a multi-kind plugin.
fake_slot_selection() {
  sel_id="$1"; sel_kind="$2"
  [ -n "$sel_kind" ] || return 0
  case "$sel_kind" in
    memory) sel_slot=memory ;;
    context-engine) sel_slot=contextEngine ;;
    *) return 0 ;;
  esac
  sel_cfg="$OPENCLAW_STATE_DIR/config"; mkdir -p "$sel_cfg" 2>/dev/null
  printf '%s' "$sel_id" > "$sel_cfg/plugins.slots.$sel_slot"
  sel_reg="$OPENCLAW_STATE_DIR/registry"
  [ -d "$sel_reg" ] || return 0
  for sel_other in "$sel_reg"/*; do
    [ -e "$sel_other" ] || continue
    sel_oid=$(basename "$sel_other")
    [ "$sel_oid" = "$sel_id" ] && continue
    [ "$(cat "$sel_other" 2>/dev/null)" = "$sel_kind" ] || continue
    printf '%s' "false" > "$sel_cfg/plugins.entries.$sel_oid.enabled"
  done
}

# `plugins uninstall` is not registry-only: OpenClaw's `removePluginFromConfig`
# rewrites config too (v2026.4.14 src/plugins/uninstall.ts). The uninstalled
# plugin leaves `plugins.allow`, and the key is dropped entirely once nothing is
# left in it (L126-133); a memory slot it owned is reset to that slot's default,
# which `DEFAULT_SLOT_BY_KEY` in src/plugins/slots.ts:17-20 fixes at
# `memory-core` (L147-155).
# Modelled because a `disable` reads both *after* running this command — the slot
# to attribute a hand-off it may never have marked, and `plugins.allow` to decide
# whether the host would refuse the restore — so a fake that leaves them alone
# hides from the driver exactly the state its own cleanup creates. The reset is
# what makes the unapplied-entry restore fallback testable at all: without it the
# slot still names this adapter's plugin when the restore reads it, and the
# attribution cannot fail.
fake_uninstall_config_rewrite() {
  uns_id="$1"
  uns_cfg="$OPENCLAW_STATE_DIR/config"
  [ -d "$uns_cfg" ] || return 0
  uns_slot="$uns_cfg/plugins.slots.memory"
  if [ -f "$uns_slot" ] && [ "$(cat "$uns_slot")" = "$uns_id" ]; then
    printf '%s' "memory-core" > "$uns_slot"
  fi
  uns_allow="$uns_cfg/plugins.allow"
  if [ -f "$uns_allow" ]; then
    grep -v "\"$uns_id\"" "$uns_allow" 2>/dev/null | grep -v "^$uns_id$" > "$uns_allow.tmp"
    # Upstream drops the key when the filter empties the array; a rendering with
    # no plugin id left in it is the same answer.
    if grep -q '[A-Za-z0-9]' "$uns_allow.tmp"; then
      mv "$uns_allow.tmp" "$uns_allow"
    else
      rm -f "$uns_allow" "$uns_allow.tmp"
    fi
  fi
}

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
if [ "$sub" = "config" ] && [ "$action" = "get" ]; then
  if [ "${FAKE_OC_PROBE_FAIL:-}" = "config_get" ]; then echo "boom-config-get" >&2; exit 17; fi
  if [ -n "${FAKE_OC_CONFIG_GET_FAIL_KEY:-}" ] && [ "$arg3" = "$FAKE_OC_CONFIG_GET_FAIL_KEY" ]; then echo "boom-config-get-$arg3" >&2; exit 21; fi
  if [ -n "${FAKE_OC_CONFIG_GET_FAIL_ONCE:-}" ] && [ "$arg3" = "$FAKE_OC_CONFIG_GET_FAIL_ONCE" ]; then
    once_marker="$OPENCLAW_STATE_DIR/.fake-config-get-once"
    if [ ! -e "$once_marker" ]; then
      : > "$once_marker"
      echo "boom-config-get-once-$arg3" >&2
      exit 22
    fi
  fi
  if [ -n "${FAKE_OC_CONFIG_GET_FAIL_ON_NTH:-}" ]; then
    nth_key="${FAKE_OC_CONFIG_GET_FAIL_ON_NTH%%:*}"
    nth_n="${FAKE_OC_CONFIG_GET_FAIL_ON_NTH##*:}"
    if [ "$arg3" = "$nth_key" ]; then
      count_file="$OPENCLAW_STATE_DIR/.fake-config-get-count-$(printf '%s' "$arg3" | tr './' '__')"
      seen=0
      [ -f "$count_file" ] && seen=$(cat "$count_file")
      seen=$((seen + 1))
      printf '%s' "$seen" > "$count_file"
      if [ "$seen" = "$nth_n" ]; then
        echo "boom-config-get-nth-$arg3" >&2
        exit 23
      fi
    fi
  fi
  key_file="$OPENCLAW_STATE_DIR/config/$arg3"
  if [ -f "$key_file" ]; then cat "$key_file"; echo; else echo ""; fi
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
    kind=$(sed -n 's/.*"kind"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$arg3/openclaw.plugin.json" | head -n 1)
    : > "$reg/$id"
    [ -n "$kind" ] && printf '%s' "$kind" > "$reg/$id"
    fake_slot_selection "$id" "$kind"
    # Model somebody else acting on the host *during* this enable: install has
    # succeeded, config and runtime verification still have to run, and only then
    # does the driver reach `plugins disable`. The Manager's lock serializes
    # ANOLISA against itself, not against a directly invoked `openclaw`.
    if [ -n "${FAKE_OC_OPERATOR_DISABLES_ON_INSTALL:-}" ]; then
      cfg_dir="$OPENCLAW_STATE_DIR/config"; mkdir -p "$cfg_dir" 2>/dev/null
      printf '%s' "false" > "$cfg_dir/plugins.entries.${FAKE_OC_OPERATOR_DISABLES_ON_INSTALL}.enabled"
    fi
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
    if [ -n "${FAKE_OC_ENABLE_FAIL_ID:-}" ] && [ "$arg3" = "$FAKE_OC_ENABLE_FAIL_ID" ]; then
      echo "boom-enable-$arg3" >&2; exit 24
    fi
    accepted=0
    for option in "$@"; do [ "$option" = "--accept-capabilities" ] && accepted=1; done
    if [ "${FAKE_OC_ENABLE_ACCEPT:-0}" = "1" ]; then
      if [ "$accepted" != 1 ]; then echo "Plugin requires capability consent" >&2; exit 15; fi
    elif [ "$accepted" = 1 ]; then
      echo "unknown option --accept-capabilities" >&2; exit 2
    fi
    if [ ! -e "$OPENCLAW_STATE_DIR/registry/$arg3" ]; then echo "Plugin not found: $arg3" >&2; exit 1; fi
    rm -f "$OPENCLAW_STATE_DIR/disabled/$arg3"
    # The real host persists the enablement flag it just changed, and that flag
    # is the only thing a later `config get plugins.entries.<id>.enabled` probe
    # can distinguish a transition from a no-op by. Model it, or a re-enable
    # dry-run would read a host no real disable ever leaves behind.
    cfg_dir="$OPENCLAW_STATE_DIR/config"; mkdir -p "$cfg_dir" 2>/dev/null
    printf '%s' "true" > "$cfg_dir/plugins.entries.$arg3.enabled"
    fake_slot_selection "$arg3" "$(cat "$OPENCLAW_STATE_DIR/registry/$arg3" 2>/dev/null)"
    echo "enabled $arg3"
    if [ -n "${FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID:-}" ] && [ "$arg3" = "$FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID" ]; then
      rm -f "$0"
    fi
    ;;
  disable)
    if [ "${FAKE_OPENCLAW_FAIL:-}" = "disable" ]; then echo "boom-disable" >&2; exit 18; fi
    mkdir -p "$OPENCLAW_STATE_DIR/disabled"
    : > "$OPENCLAW_STATE_DIR/disabled/$arg3"
    cfg_dir="$OPENCLAW_STATE_DIR/config"; mkdir -p "$cfg_dir" 2>/dev/null
    printf '%s' "false" > "$cfg_dir/plugins.entries.$arg3.enabled"
    echo "disabled $arg3"
    ;;
  inspect)
    if [ "$arg3" = "--help" ]; then
      echo "Usage: openclaw plugins inspect <id> [options]"
      [ "${FAKE_OC_INSPECT_JSON:-1}" = "1" ] && echo "  --json      machine-readable output"
      [ "${FAKE_OC_INSPECT_RUNTIME:-0}" = "1" ] && echo "  --runtime   include live runtime status"
      [ "${FAKE_OC_PROBE_FAIL:-}" = "inspect_help" ] && exit 5
      exit 0
    fi
    # Deliberately derived from the persisted config, not from any long-lived
    # process: that is what the real `plugins inspect --runtime` does. It reads
    # current metadata and inspects runtime inside the CLI process it just
    # started, so it echoes the config back and says nothing about what a running
    # gateway actually loaded. A driver must not read gateway liveness from it.
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
    fake_uninstall_config_rewrite "$arg3"
    # Test-only, and deliberately placed after the rewrite above: the caller is
    # killed at the exact moment the host stops holding the state a hand-off
    # attribution was read from, with every save the Manager would have performed
    # still ahead of it.
    if [ -n "${FAKE_OC_KILL_PID_AFTER_UNINSTALL:-}" ]; then
      kill -9 "$FAKE_OC_KILL_PID_AFTER_UNINSTALL" 2>/dev/null
      exit 0
    fi
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

/// A stale-file prune that cannot complete must fail the re-enable *before* the
/// driver has changed anything on the host.
///
/// The reported sequence: the first enable materializes a directory-shaped skill
/// output and displaces `memory-core`; the contract then drops the displacement
/// declaration and replaces that directory with a file, while a runtime-created
/// file is still inside it. The prune has to remove the now-stale directory with a
/// non-recursive `remove_dir`, which fails `ENOTEMPTY` — and it used to run *after*
/// the driver's cleanup, so `plugins enable memory-core` had already handed the
/// plugin back when the re-enable returned `ReenableCleanupIncomplete`, with the
/// prior receipt still on disk claiming `applied` ownership of a displacement it no
/// longer held. The old adapter then reported a tool-name collision it did not have,
/// and an operator who disabled the plugin themselves had it re-enabled by a retry
/// reading that stale ownership.
///
/// The failure is injected structurally, through the shape of the managed output,
/// rather than through directory permissions: `rmdir` on a non-empty directory
/// fails for every uid including root, whereas a `chmod 000` injection succeeds
/// under `CAP_DAC_OVERRIDE` and would turn this test red on such a runner. It also
/// reaches the code path the fix is actually about — a permission error fails on
/// `symlink_metadata` before any `remove_dir`, so it never exercises the prune's
/// directory branch at all.
///
/// Dropping the displacement declaration is what gives the forbidden-verb
/// assertions teeth: it makes `dropped_displaced_plugins` non-empty, so
/// `cleanup_replaced_claim` really would run `plugins enable memory-core`. A
/// skill-only contract makes that hook return without issuing a single framework
/// verb, and then this test would still pass with the two steps in the old order.
#[test]
fn reenable_materialized_cleanup_failure_mutates_nothing_on_the_host() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill_and_displacement(
        &world,
        "sec-audit",
        "memory-core",
        Some("memory"),
    );
    let source = world.resource_root.join("skills/sec-audit");
    std::fs::create_dir_all(source.join("hook.py")).expect("old source directory");
    std::fs::write(source.join("hook.py/managed.txt"), b"v1").expect("old managed file");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable initial directory-shaped output with a displacement");
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "fixture must start from a claimed, performed displacement"
    );

    // Runtime content inside the managed directory, then a contract that replaces
    // that directory with a file and no longer displaces anything.
    let destination = world.openclaw_home.join("skills/sec-audit");
    std::fs::write(destination.join("hook.py/runtime.log"), b"runtime")
        .expect("runtime content under old directory");
    std::fs::remove_dir_all(source.join("hook.py")).expect("remove old source directory");
    std::fs::write(source.join("hook.py"), b"v2").expect("new file-shaped source");
    configure_plugin_with_skill(&world, "sec-audit");
    record_owned_adapter_files(&world.layout, &world.resource_root);
    let logged_before = argv_lines(&world.argv_log()).len();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("runtime content must not be recursively removed");
    assert!(
        matches!(err, AdapterError::ReenableCleanupIncomplete { .. }),
        "{err:?}"
    );

    // The fix: no framework verb ran at all, so the prior receipt still matches the
    // host exactly.
    let appended = argv_appended(&world, logged_before);
    for forbidden in [
        "plugins enable memory-core",
        "plugins uninstall tokenless --force",
    ] {
        assert!(
            !argv_contains(&appended, forbidden),
            "a prune failure must happen before any host mutation ('{forbidden}'): \
             {appended:?}"
        );
    }
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the plugin is still displaced, exactly as the receipt says"
    );
    assert!(
        world.registry_marker_exists(),
        "and this adapter's own plugin is still installed"
    );
    let claim = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("prior receipt retained for retry")
    };
    assert_eq!(recorded_openclaw_state_dir(&claim), world.openclaw_home);
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        payload.displaced_plugins[0].applied,
        "the ownership must still be true, because the host really is still \
         displaced: {:?}",
        payload.displaced_plugins
    );

    // Clear the runtime content and retry: the whole cleanup then runs, in the new
    // order, and the dropped displacement is really handed back.
    std::fs::remove_file(destination.join("hook.py/runtime.log")).expect("clear runtime content");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry succeeds once the stale directory can be pruned");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the retry performs the restore the contract dropped: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "and the plugin really comes back"
    );
    assert!(
        destination.join("hook.py").is_file(),
        "the new file-shaped output is materialized"
    );
    assert!(
        !destination.join("hook.py/managed.txt").exists(),
        "the stale directory-shaped output is pruned"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("the retry swapped in the new receipt");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "the new contract declares no displacement: {:?}",
        payload.displaced_plugins
    );
}

/// A migration cleanup that got as far as unregistering the old plugin must stay
/// retryable: the second attempt finds that plugin already gone and has to treat it
/// as clean rather than as an error.
///
/// The failure is injected into the driver's own cleanup — the prior home's
/// displacement restore — not into the Manager's stale-file prune, because the prune
/// now runs *first* (see
/// `reenable_materialized_cleanup_failure_mutates_nothing_on_the_host`). A prune
/// failure therefore never reaches the uninstall and can no longer produce the
/// half-done cleanup this test is about.
#[test]
fn migration_cleanup_retry_tolerates_already_missing_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill_and_displacement(
        &world,
        "sec-audit",
        "memory-core",
        Some("memory"),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("seed old registry, skill and displacement");
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "fixture must start from a claimed displacement in the old home"
    );

    let configured_state = world._root.path().join("configured-openclaw-state");
    // The new instance has to ship the bundled plugin too, or `prepare_enable`
    // correctly refuses the contract before any cleanup runs.
    for dir in ["registry", "config"] {
        std::fs::create_dir_all(configured_state.join(dir)).expect("new home dir");
    }
    std::fs::write(configured_state.join("registry/memory-core"), b"").expect("bundled plugin");
    guard.set("OPENCLAW_STATE_DIR", &configured_state);
    // The migration branch runs a full `disable` on the prior receipt, which
    // uninstalls first and restores afterwards, so failing the restore leaves the
    // old plugin already unregistered.
    guard.set("FAKE_OPENCLAW_FAIL", "enable");
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a failed restore must keep the prior receipt");
    assert!(
        matches!(err, AdapterError::ReenableCleanupIncomplete { .. }),
        "{err:?}"
    );
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins uninstall tokenless --force"),
        "the uninstall ran before the restore failed: {appended:?}"
    );
    assert!(
        !world.registry_marker_exists(),
        "the first cleanup already unregistered the old plugin"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the restore really did not happen"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("prior receipt retained for retry");
    assert_eq!(recorded_openclaw_state_dir(claim), world.openclaw_home);
    drop(state);

    guard.unset("FAKE_OPENCLAW_FAIL");
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry treats the missing old plugin as already clean");
    assert!(configured_state.join("registry").join(COMPONENT).exists());
    assert!(
        configured_state
            .join("skills/sec-audit/marker.txt")
            .is_file()
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the retry completes the prior home's restore"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("the retry swapped in the new receipt");
    assert_eq!(recorded_openclaw_state_dir(claim), configured_state);
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

/// A disable whose cleanup only *partly* succeeded must not keep the ownership it
/// already handed back.
///
/// The reported sequence: the adapter declares both a materialized skill and a
/// displacement, and the skill's directory is replaced by a plain file, so the
/// driver's `remove_tree` fails on it — `remove_dir_all` on a regular file is
/// `ENOTDIR`, which no uid bypasses. That sets `cleanup_complete = false` but does
/// **not** return early, so the displacement restore still runs and really issues
/// `plugins enable memory-core`. The Manager then keeps the receipt for retry; if
/// that receipt still claims the displacement, an operator who closes `memory-core`
/// themselves before retrying has it re-enabled by the retry — a choice made *after*
/// the ownership was already given up, undone with nothing left to show why.
///
/// This is the disable-side sibling of the re-enable ordering fix, and it breaks the
/// invariant that fix recorded: a successful restore must be followed by a durable
/// record of the release, because the driver has no other way to say so.
#[test]
fn disable_keeps_only_the_ownership_it_did_not_release() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill_and_displacement(
        &world,
        "sec-audit",
        "memory-core",
        Some("memory"),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with a skill and a displacement");
    assert!(displaced_marker_exists(&world, "memory-core"));

    // Replace the materialized skill directory with a plain file: `remove_tree`
    // then fails structurally, with no permission tricks involved.
    let skill_dir = world.openclaw_home.join("skills/sec-audit");
    std::fs::remove_dir_all(&skill_dir).expect("clear the materialized skill dir");
    std::fs::write(&skill_dir, b"not a directory").expect("skill path is now a file");

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable reports incomplete cleanup rather than failing");
    assert!(!outcome.report.cleanup_complete);
    assert!(!outcome.claim_removed, "the receipt is kept for retry");

    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the restore really ran even though the skill cleanup failed: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "and the plugin really came back"
    );

    // The ownership that was handed back must be gone from the kept receipt.
    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("receipt kept for retry")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        Vec::<String>::new(),
        "a release already performed must not survive in the receipt kept for retry"
    );
    assert!(
        !kept
            .resources
            .iter()
            .any(|resource| resource.id == "openclaw_displaced_plugin_memory-core"),
        "and its resource must go with it, or the receipt would not validate: {:?}",
        kept.resources
    );

    // The operator's own choice, made after the partial disable.
    operator_disables_plugin(&world, "memory-core");
    std::fs::remove_file(&skill_dir).expect("clear the blocking file so retry can finish");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry completes the remaining cleanup");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not undo a choice the operator made after the ownership was \
         given up: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the plugin must still be off, exactly as the operator left it"
    );
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
}

/// The same guarantee with several displacements, where only some restores succeed —
/// the case a single-entry receipt cannot express, and the one where "keep the whole
/// receipt" is most obviously wrong.
#[test]
fn disable_keeps_only_the_displacements_it_failed_to_restore() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);

    // The first restore succeeds, the second fails.
    guard.set("FAKE_OC_ENABLE_FAIL_ID", "memory-lancedb");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable reports incomplete cleanup rather than failing");
    assert!(!outcome.report.cleanup_complete);
    assert!(!outcome.claim_removed);
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the first restore really ran: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
    assert!(
        displaced_marker_exists(&world, "memory-lancedb"),
        "and the second really did not"
    );

    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-lancedb".to_string()],
        "the kept receipt must name only the plugin that is still displaced"
    );

    // The operator closes the plugin that was already handed back, then the retry
    // runs with the failure cleared.
    guard.unset("FAKE_OC_ENABLE_FAIL_ID");
    operator_disables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry completes the remaining restore");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not undo the operator's own disable: {appended:?}"
    );
    assert!(
        argv_contains(&appended, "plugins enable memory-lancedb"),
        "and must still finish the restore it owes: {appended:?}"
    );
    assert!(displaced_marker_exists(&world, "memory-core"));
    assert!(!displaced_marker_exists(&world, "memory-lancedb"));
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
}

/// A restore the host vetoes is a *release*, and its own report says so in those
/// words — "treated as released", "run `openclaw plugins enable` yourself". So the
/// receipt kept for an unrelated failure has to stop naming that plugin, exactly
/// as it does for a restore that succeeded.
///
/// This is the mixed case a single-entry receipt cannot express, and the one where
/// keeping the whole receipt is most visibly wrong: `plugins.deny` vetoes the
/// first displacement, the second's restore fails, and the deny is precisely what
/// an operator lifts later. With the vetoed entry left behind, the retry then ran
/// the `plugins enable` the earlier run had already handed to the operator — over
/// whatever they did once the deny was gone.
#[test]
fn disable_does_not_keep_ownership_a_veto_already_released() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);

    set_config_answer(&world, "plugins.deny", "memory-core");
    guard.set("FAKE_OC_ENABLE_FAIL_ID", "memory-lancedb");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable reports incomplete cleanup rather than failing");
    assert!(!outcome.report.cleanup_complete);
    assert!(!outcome.claim_removed, "the receipt is kept for retry");

    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the deny vetoes that restore: {appended:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.deny")
                && m.contains("memory-core")
                && m.contains("treated as released")),
        "and the report hands the plugin back to the operator: {:?}",
        outcome.report.messages
    );

    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("receipt kept for retry")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-lancedb".to_string()],
        "the kept receipt must name only the plugin this adapter still owes a restore"
    );
    assert!(
        !kept
            .resources
            .iter()
            .any(|resource| resource.id == "openclaw_displaced_plugin_memory-core"),
        "and the released entry's resource must go with it, or the receipt would \
         not validate: {:?}",
        kept.resources
    );

    // The operator does what the report told them to: lift the deny. Then the
    // retry runs with the restore failure cleared.
    std::fs::remove_file(world.openclaw_home.join("config/plugins.deny"))
        .expect("the deny is lifted");
    guard.unset("FAKE_OC_ENABLE_FAIL_ID");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry completes the remaining restore");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not perform a release the earlier run already announced, \
         over whatever the operator did once the deny was gone: {appended:?}"
    );
    assert!(
        argv_contains(&appended, "plugins enable memory-lancedb"),
        "and must still finish the restore it owes: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "memory-core stays exactly as the first disable left it — off, and now the \
         operator's to re-enable"
    );
    assert!(!displaced_marker_exists(&world, "memory-lancedb"));
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
}

/// The same duty on the path that produces no report at all: a driver that errors
/// out *after* handing ownership back. The releases it performed exist only in the
/// claim the Manager lent it, so propagating the error bare used to drop them and
/// leave the durable receipt naming plugins that are enabled again — which the
/// retry then enabled a second time.
///
/// The injection is the real shape of that failure rather than a non-zero exit:
/// `FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID` makes the CLI unspawnable right after the
/// first successful `plugins enable`, which is what a package removed or an install
/// broken mid-disable looks like. A verb that exits non-zero cannot express it —
/// that is a report, and it takes the `cleanup_complete` path the test above
/// covers.
#[test]
fn disable_records_released_ownership_when_the_cli_stops_midway() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);

    guard.set("FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID", "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a CLI that cannot be spawned is an error, not an incomplete report");
    match &err {
        AdapterError::FrameworkCli { reason, .. } => assert!(
            reason.contains("failed to spawn"),
            "the fixture must fail at the spawn itself: {reason}"
        ),
        other => panic!("expected a spawn failure, got {other:?}"),
    }
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the first restore really ran before the CLI went away: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "and the plugin really came back"
    );

    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("receipt kept for retry")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-lancedb".to_string()],
        "the release performed before the error must be durable rather than lost \
         with the in-memory claim"
    );
    assert!(
        !kept
            .resources
            .iter()
            .any(|resource| resource.id == "openclaw_displaced_plugin_memory-core"),
        "and its resource must go with it, or the receipt would not validate: {:?}",
        kept.resources
    );

    // The CLI comes back, and the retry finishes the cleanup it owes.
    write_fake_openclaw(world.fake_bin.parent().expect("bin dir"));
    guard.unset("FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry completes the remaining restore");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not hand back a plugin the failed run already handed \
         back: {appended:?}"
    );
    assert!(
        argv_contains(&appended, "plugins enable memory-lancedb"),
        "and must still finish the restore it owes: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-lancedb"));
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
}

/// The one entry a partial disable must *keep*: a displacement whose hand-off
/// never ran.
///
/// That branch is not a release. This adapter never held the plugin, so there is
/// no transition to record as undone and no retry action to prevent — `applied` is
/// a fact about this receipt's own past, so the retry reaches the same verdict
/// whatever the host does in between. And the entry is load-bearing: it is the
/// receipt's only evidence that the plugin is enabled *and* unclaimed, which is
/// what `status` reads as `never_displaced`. Striking it would delete that signal
/// and buy nothing, which is why it is the exception to the vetoes above rather
/// than an oversight.
#[test]
fn disable_keeps_an_unapplied_displacement_it_never_owned() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    configure_plugin_with_skill_and_displacement(
        &world,
        "sec-audit",
        "memory-core",
        Some("memory"),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail the enable before the hand-off");
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "fixture must fail before the hand-off: the plugin is still enabled"
    );

    // Make the cleanup incomplete *after* the restore step is reached: a skill
    // path that is a plain file fails `remove_tree` structurally.
    let skill_dir = world.openclaw_home.join("skills/sec-audit");
    let _ = std::fs::remove_dir_all(&skill_dir);
    std::fs::write(&skill_dir, b"not a directory").expect("skill path is now a file");

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable reports incomplete cleanup rather than failing");
    assert!(!outcome.report.cleanup_complete);
    assert!(!outcome.claim_removed, "the receipt is kept for retry");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "there was never a hand-off to undo: {appended:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("memory-core") && m.contains("never disabled it")),
        "the report must still say why nothing was restored: {:?}",
        outcome.report.messages
    );

    // The declaration survives, still marked unapplied ...
    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("receipt kept for retry")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "an entry this adapter never owned is not a release, so it stays"
    );
    let DriverPayload::OpenClaw(payload) = &kept.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        !payload.displaced_plugins[0].applied,
        "and it must still read as unperformed: {:?}",
        payload.displaced_plugins
    );

    // ... which is what keeps `status` able to report it.
    let condition = displacement_condition(&manager);
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "an unperformed hand-off is not a released displacement: {:?}",
        condition.reason
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("never disabled it"),
        "the verdict must still name the plugin: {reason}"
    );

    // The retry finishes the cleanup, and still restores nothing.
    std::fs::remove_file(&skill_dir).expect("clear the blocking file so retry can finish");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retry completes the remaining cleanup");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not undo a hand-off that never happened: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "and must leave the plugin enabled, exactly as it has been all along"
    );
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
}

/// The same guarantee on the re-enable path, where the receipt at risk is the
/// *prior* one rather than the current one.
///
/// A state-directory migration makes `cleanup_replaced_claim` run a full `disable`
/// against the prior receipt's own home. That can also partially fail, and the
/// Manager keeps the prior receipt durable when it does — so the ownership the
/// driver already handed back has to have been struck from *that* receipt too, and
/// the Manager has to persist it before reporting the failure. Without this the
/// migration path had exactly the defect the disable path had, one level up.
#[test]
fn migration_cleanup_keeps_only_the_ownership_it_did_not_release() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);
    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            displaced_marker_exists(&world, id),
            "fixture must start from both plugins displaced: {id}"
        );
    }

    let new_home = world._root.path().join("configured-openclaw-state");
    for dir in ["registry", "config"] {
        std::fs::create_dir_all(new_home.join(dir)).expect("new home dir");
    }
    for id in ["memory-core", "memory-lancedb"] {
        std::fs::write(new_home.join("registry").join(id), b"").expect("bundled plugin");
    }
    guard.set("OPENCLAW_STATE_DIR", &new_home);
    // The prior home's first restore succeeds, its second fails.
    guard.set("FAKE_OC_ENABLE_FAIL_ID", "memory-lancedb");
    let logged_before = argv_lines(&world.argv_log()).len();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an incomplete migration cleanup must fail the re-enable");
    assert!(
        matches!(err, AdapterError::ReenableCleanupIncomplete { .. }),
        "{err:?}"
    );
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the first restore really ran in the prior home: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
    assert!(
        displaced_marker_exists(&world, "memory-lancedb"),
        "and the second really did not"
    );

    // The prior receipt is what the Manager kept, and it must have been updated.
    assert!(
        world.has_claim(),
        "an incomplete cleanup keeps the prior receipt durable for retry"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-lancedb".to_string()],
        "the kept prior receipt must name only the plugin still displaced"
    );

    guard.unset("FAKE_OC_ENABLE_FAIL_ID");
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry tolerates the plugin it already unregistered");
    assert!(!displaced_marker_exists(&world, "memory-lancedb"));
    assert!(new_home.join("registry").join(COMPONENT).exists());
}

/// The same guarantee on the re-enable path *and* on the route that produces no
/// report at all: a migration's `cleanup_replaced_claim` runs a full `disable`
/// against the prior home, and a CLI that stops being spawnable halfway through it
/// surfaces as an error rather than as `cleanup_complete = false`. The prior
/// receipt is still the durable one at that point, so the release the driver
/// already performed has to reach it before the error propagates — and the prior
/// receipt's own status is not the Manager's to change here, so it is persisted
/// exactly as the driver left it.
#[test]
fn migration_cleanup_records_released_ownership_when_the_cli_stops_midway() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);

    let new_home = world._root.path().join("configured-openclaw-state");
    for dir in ["registry", "config"] {
        std::fs::create_dir_all(new_home.join(dir)).expect("new home dir");
    }
    for id in ["memory-core", "memory-lancedb"] {
        std::fs::write(new_home.join("registry").join(id), b"").expect("bundled plugin");
    }
    guard.set("OPENCLAW_STATE_DIR", &new_home);
    // The prior home's first restore succeeds and takes the CLI with it, so the
    // second one cannot be spawned at all.
    guard.set("FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID", "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a cleanup whose CLI vanished must fail the re-enable");
    match &err {
        AdapterError::FrameworkCli { reason, .. } => assert!(
            reason.contains("failed to spawn"),
            "the fixture must fail at the spawn itself: {reason}"
        ),
        other => panic!("expected a spawn failure, got {other:?}"),
    }
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the first restore really ran in the prior home: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));

    assert!(
        world.has_claim(),
        "a failed cleanup keeps the prior receipt durable for retry"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-lancedb".to_string()],
        "the release performed before the error must be durable in the prior receipt"
    );

    // The CLI comes back and the retry completes the migration.
    write_fake_openclaw(world.fake_bin.parent().expect("bin dir"));
    guard.unset("FAKE_OC_UNLINK_BIN_AFTER_ENABLE_ID");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry tolerates the plugin it already unregistered");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the retry must not hand back a plugin the failed run already handed \
         back: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-lancedb"));
    assert!(new_home.join("registry").join(COMPONENT).exists());
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

/// A receipt persisted in front of `apply_enable` describes a hand-off that has
/// not happened yet, and must not be readable as one that has.
///
/// The reported sequence: runtime verification fails, so `plugins disable` never
/// runs — but the Manager persisted the whole receipt before `apply_enable`
/// started, displacement entry included. The operator then disables that plugin
/// themselves and runs `adapter disable`, which reads the stale ownership and
/// issues `plugins enable`, undoing a choice made *after* the failed enable and
/// leaving nothing behind to show why. Same takeover window as the enable-side
/// re-confirmation, entered through the persist/apply ordering instead of through
/// probe attribution.
#[test]
fn failed_enable_before_the_handoff_leaves_no_displacement_ownership() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail enable");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "the hand-off never ran: {lines:?}"
    );

    // Read the receipt the failed enable left behind, but assert on it only after
    // the behavioural payoff below: *how* the driver avoids the false ownership is
    // its business, and a different mechanism that gets the behaviour right should
    // not fail this test.
    let leftover = {
        let state = world.load_state();
        let claim = state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .expect("receipt kept for cleanup retry");
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
        let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
            panic!("expected OpenClaw receipt payload");
        };
        payload.displaced_plugins.clone()
    };

    // The operator's own choice, made after the failed enable.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    operator_disables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable cleans up the failed enable");

    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "disable must never undo a choice the operator made after the failed enable: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the plugin must still be off, exactly as the operator left it"
    );
    assert!(
        disabled
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("never disabled it")),
        "the report must say why nothing was restored, or the operator is left \
         guessing whether the plugin is still off by accident: {:?}",
        disabled.report.messages
    );
    assert!(disabled.claim_removed);
    assert!(!world.has_claim());

    // The mechanism: the declared displacement stays in the receipt so a retry can
    // still perform it, but marked as not yet done — which is what makes it safe to
    // read as something other than ownership.
    assert_eq!(
        leftover.len(),
        1,
        "the declaration itself is worth keeping: {leftover:?}"
    );
    assert!(
        !leftover[0].applied,
        "a hand-off that never ran must not be recorded as one: {leftover:?}"
    );
}

/// The mirror case, so the mark cannot be "fixed" by never setting it: a hand-off
/// that *did* run is recorded as applied, and disable really does hand the plugin
/// back.
#[test]
fn successful_handoff_is_recorded_as_applied_and_restored() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        payload.displaced_plugins[0].applied,
        "a hand-off that ran must be recorded as one, or disable would strand the \
         host with the plugin off and nothing owning it: {:?}",
        payload.displaced_plugins
    );
    drop(state);

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "an applied displacement is real ownership and must be handed back: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
}

/// Stage a receipt whose displacement was declared but never performed: the enable
/// fails at runtime verification, which is after install and config but before
/// `apply_displacements`. Returns the argv-log length at that point.
fn stage_failed_enable_with_unapplied_displacement(
    guard: &OpenClawEnvGuard,
) -> (World, AdapterManager) {
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(guard, None);
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail enable");
    assert!(
        !argv_contains(
            &argv_lines(&world.argv_log()),
            "plugins disable memory-core"
        ),
        "fixture must fail before the hand-off, not during it"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt kept for cleanup retry");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "fixture must leave the declaration behind"
    );
    assert!(
        !payload.displaced_plugins[0].applied,
        "fixture must leave it unapplied: {:?}",
        payload.displaced_plugins
    );
    (world, manager)
}

/// An entry whose hand-off never ran is a declaration of intent, not ownership —
/// and *inheriting* it is worse than inheriting nothing, because the inheritance is
/// precisely what suppresses the re-confirmation.
///
/// The reported sequence end to end: the enable fails at runtime verification, the
/// operator then disables the plugin themselves, and a second enable finds it off so
/// `prepare_enable` correctly declines to claim it. But `preserve_reenable_facts`
/// used to put the prior's unapplied entry back into the replacement receipt anyway,
/// where it is in neither `freshly_claimed` nor the probe's output — so
/// `apply_displacements` skips re-confirming it, marks it applied, and runs a
/// `plugins disable` that exits 0 having changed nothing. From then on the receipt
/// claims a transition nobody made, and the final `adapter disable` re-enables a
/// plugin the operator closed between the two enables.
#[test]
fn reenable_does_not_inherit_a_displacement_whose_handoff_never_ran() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_failed_enable_with_unapplied_displacement(&guard);

    // The operator's own choice, made between the two enables.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    operator_disables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the re-enable itself succeeds");

    let after_enable = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&after_enable, "plugins disable memory-core"),
        "the operator already closed it, so there is no transition to claim and no \
         disable to run: {after_enable:?}"
    );
    assert!(
        !argv_contains(&after_enable, "plugins enable memory-core"),
        "and replacing the receipt must not restore an entry that was never applied, \
         either: {after_enable:?}"
    );
    let claim = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("claim")
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    // Asserted as the invariant that actually protects the operator — nothing in
    // the replacement receipt may be marked as a hand-off this adapter performed —
    // rather than as one particular way of achieving it.
    assert!(
        payload.displaced_plugins.iter().all(|entry| !entry.applied),
        "an unapplied entry is not ownership to inherit, so nothing may promote it \
         to applied: {:?}",
        payload.displaced_plugins
    );

    // The payoff the reviewer asked to see asserted last.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "disable must never re-enable a plugin the operator closed between the two \
         enables: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and it must still be off, exactly as the operator left it"
    );
    assert!(!world.has_claim());
}

/// The dry-run must not promise a carry-over the real disable would decline.
///
/// `plan_enable` built its `carried` set from *every* id in the prior receipt, and
/// checked it before probing, so an unapplied entry was previewed as "the receipt
/// being replaced already claims this displacement and carries it over, so it stays
/// claimed and disable will hand it back" — while the real `disable`, reading the
/// same receipt through `restore_decision`, answers `SkipNotApplied` and hands
/// nothing back. A preview that contradicts the operation it describes is worse than
/// no preview: the operator plans around it, and because every veto still counts as
/// completed cleanup the receipt is removed afterwards and nothing records the
/// divergence.
#[test]
fn reenable_dry_run_does_not_promise_a_carryover_for_an_unapplied_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_failed_enable_with_unapplied_displacement(&guard);
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    operator_disables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan");
    let plan = match outcome {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };

    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.contains("carries it over")
                || action.contains("disable will hand it back")),
        "an unapplied entry is not carried over, so the preview must not say it is: {:?}",
        plan.actions
    );
    assert!(
        plan.actions.iter().any(|action| action
            .contains("leave openclaw plugin 'memory-core' alone")
            && action.contains("not claimed")),
        "the preview must describe what the real enable will do, which is to respect \
         the operator's own disable: {:?}",
        plan.actions
    );

    // A plan may probe, but must not mutate — and must not consume the receipt.
    let planned = argv_appended(&world, logged_before);
    assert_dry_run_only_probed(&planned);
    assert!(world.has_claim(), "dry-run must not touch the receipt");
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and must leave the operator's own disable alone"
    );
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

/// A plugin adapter block declaring one displaced framework plugin.
/// A plugin adapter contract declaring *both* a materialized skill and a displaced
/// framework plugin, so one re-enable exercises the Manager's stale-file prune and
/// the driver's host-side cleanup together — the combination whose ordering decides
/// whether a file failure can leave the receipt disagreeing with the host.
fn configure_plugin_with_skill_and_displacement(
    world: &World,
    skill_name: &str,
    id: &str,
    slot: Option<&str>,
) {
    let slot_line = slot
        .map(|s| format!("\nslot = \"{s}\""))
        .unwrap_or_default();
    let block = format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.openclaw]
skills = ["{skill_name}"]

[[adapters.openclaw.displaces]]
id = "{id}"{slot_line}
"#
    );
    write_openclaw_manifest(&world.layout, &block);
    let skill_source = world.resource_root.join("skills").join(skill_name);
    std::fs::create_dir_all(&skill_source).expect("skill source");
    std::fs::write(skill_source.join("marker.txt"), b"skill").expect("skill marker");
    seed_bundled_plugin(world, id);
}

fn plugin_adapter_block_with_displacement(id: &str, slot: Option<&str>) -> String {
    let slot_line = slot
        .map(|s| format!("\nslot = \"{s}\""))
        .unwrap_or_default();
    format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.displaces]]
id = "{id}"{slot_line}
"#
    )
}

/// A plugin adapter block declaring several displaced framework plugins, each
/// with an optional exclusive slot.
fn plugin_adapter_block_with_displacements(entries: &[(&str, Option<&str>)]) -> String {
    let declared = entries
        .iter()
        .map(|(id, slot)| {
            let slot_line = slot
                .map(|s| format!("\nslot = \"{s}\""))
                .unwrap_or_default();
            format!("[[adapters.openclaw.displaces]]\nid = \"{id}\"{slot_line}\n")
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"[[adapters]]
framework = "openclaw"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

{declared}"#
    )
}

/// Swap the adapter's own plugin resource with a displaced one in the persisted
/// receipt. Both orders are legitimate as far as every generic check is
/// concerned — `resources` is a set keyed by id, not an ordered record — so this
/// is exactly the hand edit that used to send `disable` after the wrong plugin.
fn reorder_resources_displaced_first(world: &World) {
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    let claim = state
        .adapter_claims
        .iter_mut()
        .find(|claim| claim.component == COMPONENT && claim.framework == FRAMEWORK)
        .expect("openclaw claim");
    let own = claim
        .resources
        .iter()
        .position(|r| r.id == "openclaw_plugin")
        .expect("own plugin resource");
    let displaced = claim
        .resources
        .iter()
        .position(|r| r.purpose == "openclaw_displaced_plugin")
        .expect("displaced resource");
    claim.resources.swap(own, displaced);
    assert_eq!(
        claim.resources[own].purpose, "openclaw_displaced_plugin",
        "fixture must actually reorder"
    );
    state.save(&state_path).expect("save reordered receipt");
}

/// Register a bundled plugin in the fake CLI's registry, so `plugins enable`
/// can restore it the way the real host restores a bundled plugin.
fn seed_bundled_plugin(world: &World, id: &str) {
    let registry = world.openclaw_home.join("registry");
    std::fs::create_dir_all(&registry).expect("registry dir");
    std::fs::write(registry.join(id), b"").expect("bundled plugin marker");
}

/// Seed a bundled plugin that declares a plugin `kind`.
///
/// The kind is what makes the host apply exclusive slot selection when another
/// plugin of the same kind is installed or enabled, so a fixture that needs that
/// behavior has to declare it on both sides.
fn seed_bundled_plugin_with_kind(world: &World, id: &str, kind: &str) {
    let registry = world.openclaw_home.join("registry");
    std::fs::create_dir_all(&registry).expect("registry dir");
    std::fs::write(registry.join(id), kind).expect("bundled plugin marker");
}

/// Give the staged bundle a plugin `kind`, which is the other half of what makes
/// OpenClaw's install and enable paths apply exclusive slot selection.
///
/// Must run before [`World::manager`], which records the bundle's file digests.
fn declare_bundle_plugin_kind(world: &World, kind: &str) {
    std::fs::write(
        world.resource_root.join("openclaw.plugin.json"),
        format!(r#"{{"id":"{COMPONENT}","name":"Tokenless","kind":"{kind}"}}"#),
    )
    .expect("plugin manifest with a kind");
}

/// The persisted answer the fake CLI would give for `config get <key>`, or `None`
/// when the host has no such key — which the driver reads as "not set".
fn config_answer(world: &World, key: &str) -> Option<String> {
    std::fs::read_to_string(world.openclaw_home.join("config").join(key)).ok()
}

/// Assert that a dry-run issued nothing but read-only probes.
///
/// An allowlist, not a denylist of the mutating verbs this driver happens to have
/// today. A denylist cannot fail on a verb nobody thought to forbid, which is
/// precisely the verb a later change would add — and a dry-run that mutates is the
/// one failure mode these tests exist to rule out. Everything a plan may run is a
/// capability probe (`--help`), a version read, an inventory read, or a
/// `config get`; anything else fails here naming the offending argv.
fn assert_dry_run_only_probed(planned: &[String]) {
    for line in planned {
        let read_only = line == "--version"
            || line == "plugins list"
            || line.ends_with("--help")
            || line.starts_with("config get ");
        assert!(
            read_only,
            "a dry-run must run nothing but read-only probes, and this is not one: \
             {line:?}\nfull argv: {planned:?}"
        );
    }
}

/// The `DisplacedPluginsReleased` condition of a one-component status.
fn find_displacement_condition(status: &StatusReport) -> AdapterCondition {
    status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .cloned()
        .expect("a claimed displacement is part of what makes the adapter work")
}

/// Shorthand for the common "enable with a displacement, then read status" pair.
fn displacement_condition(manager: &AdapterManager) -> AdapterCondition {
    let status = manager.status(Some(COMPONENT)).expect("status");
    find_displacement_condition(&status)
}

/// Model an operator running `openclaw plugins disable <id>` themselves: the same
/// two writes the fake CLI makes, with no ANOLISA involvement and no receipt.
fn operator_disables_plugin(world: &World, id: &str) {
    let disabled = world.openclaw_home.join("disabled");
    std::fs::create_dir_all(&disabled).expect("disabled dir");
    std::fs::write(disabled.join(id), b"").expect("operator's own disable");
    set_config_answer(world, &format!("plugins.entries.{id}.enabled"), "false");
}

/// Model an operator running `openclaw plugins enable <id>` themselves: the same
/// two writes the fake CLI makes. This voids any ownership a receipt had over the
/// plugin's disabled state, which is the point of the tests that use it.
fn operator_enables_plugin(world: &World, id: &str) {
    let marker = world.openclaw_home.join("disabled").join(id);
    if marker.exists() {
        std::fs::remove_file(&marker).expect("clear the operator's disabled marker");
    }
    set_config_answer(world, &format!("plugins.entries.{id}.enabled"), "true");
}

/// Rename a receipt's displaced-plugin resource and its reference together — which
/// is all validation asks for. `claim_displaced_plugins` checks that the reference
/// resolves to a framework-plugin resource of this framework whose `plugin_id`
/// matches, not that its *name* is the canonical `openclaw_displaced_plugin_<id>`.
fn rename_displacement_resource(world: &World, new_id: &str) {
    let mut state = world.load_state();
    let mut claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let mut renamed = false;
    for resource in &mut claim.resources {
        if resource.purpose == "openclaw_displaced_plugin" {
            resource.id = new_id.to_string();
            renamed = true;
        }
    }
    assert!(renamed, "fixture must have a displaced-plugin resource");
    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        payload.displaced_plugins[0].applied,
        "fixture must start from a performed hand-off"
    );
    payload.displaced_plugins[0].resource = new_id.to_string();
    state.upsert_adapter_claim(claim);
    state
        .save(&world.layout.state_dir.join("installed.toml"))
        .expect("persist the renamed receipt");
}

/// Every displaced-plugin entry the persisted receipt still claims, resolved
/// through its resources the way the driver resolves them, as
/// `(plugin id, slot, guard slot, applied)`.
///
/// The two slot columns are what the receipt distinguishes: the slot an entry owns,
/// which is exclusive, and the one its restore consults without owning it, which is
/// not — see [`DisplacedPluginRef::guard_slot`].
fn persisted_displacements(world: &World) -> Vec<(String, Option<String>, Option<String>, bool)> {
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    payload
        .displaced_plugins
        .iter()
        .map(|entry| {
            let plugin_id = match claim.resource(&entry.resource) {
                Some(resource) => match &resource.kind {
                    ClaimResourceKind::FrameworkPlugin { plugin_id, .. } => plugin_id.clone(),
                    other => panic!("displacement resource is not a framework plugin: {other:?}"),
                },
                None => panic!("dangling displacement reference {:?}", entry.resource),
            };
            (
                plugin_id,
                entry.slot.clone(),
                entry.guard_slot.clone(),
                entry.applied,
            )
        })
        .collect()
}

/// The plugin ids the persisted receipt still claims as displaced, resolved
/// through its resources the way the driver resolves them.
fn persisted_displacement_ids(world: &World) -> Vec<String> {
    persisted_displacements(world)
        .into_iter()
        .map(|(plugin_id, _, _, _)| plugin_id)
        .collect()
}

/// The single displaced-plugin entry of the persisted receipt.
fn persisted_displacement(world: &World) -> DisplacedPluginRef {
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "one plugin must be claimed exactly once: {:?}",
        payload.displaced_plugins
    );
    payload.displaced_plugins[0].clone()
}

/// Persist the answer `config get <key>` will echo.
fn set_config_answer(world: &World, key: &str, value: &str) {
    let dir = world.openclaw_home.join("config");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join(key), value).expect("config answer");
}

fn displaced_marker_exists(world: &World, id: &str) -> bool {
    world.openclaw_home.join("disabled").join(id).exists()
}

fn argv_contains(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line == needle)
}

/// The argv log is cumulative for the whole test, so an assertion about one
/// step has to look at what that step appended, not at the fixture's enable too.
fn argv_appended(world: &World, since: usize) -> Vec<String> {
    let all = argv_lines(&world.argv_log());
    all[since.min(all.len())..].to_vec()
}

#[test]
fn enable_disables_declared_displaced_plugin_and_disable_restores_it() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with a declared displacement");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "enable must release the displaced plugin's tool names: {lines:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the displaced plugin must actually be disabled"
    );

    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("claim");
    let resource = claim
        .resource("openclaw_displaced_plugin_memory-core")
        .expect("displaced plugin resource");
    assert_eq!(resource.purpose, "openclaw_displaced_plugin");
    assert!(matches!(
        &resource.kind,
        ClaimResourceKind::FrameworkPlugin { plugin_id, .. } if plugin_id == "memory-core"
    ));
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("openclaw payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert_eq!(
        payload.displaced_plugins[0].resource,
        "openclaw_displaced_plugin_memory-core"
    );
    assert_eq!(payload.displaced_plugins[0].slot.as_deref(), Some("memory"));

    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable hands the plugin back");
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "disable must restore what enable displaced: {lines:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the restored plugin must no longer be disabled"
    );
    assert!(!world.has_claim());
}

#[test]
fn enable_does_not_claim_a_displaced_plugin_the_operator_already_disabled() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "false");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable over an operator-disabled plugin");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "an operator's own disable is not this adapter's transition: {lines:?}"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("claim");
    assert!(
        claim
            .resource("openclaw_displaced_plugin_memory-core")
            .is_none()
    );

    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "disable must never re-enable a plugin the operator turned off: {lines:?}"
    );
}

#[test]
fn disable_leaves_the_slot_with_a_third_plugin_selected_after_enable() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    assert!(displaced_marker_exists(&world, "memory-core"));

    // The operator moves the exclusive memory slot to another backend.
    set_config_answer(&world, "plugins.slots.memory", "memory-lancedb");
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "restoring would drag the slot back from memory-lancedb: {lines:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the operator's later backend must keep serving the slot"
    );
}

#[test]
fn disable_restores_when_the_slot_still_belongs_to_this_adapter() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    set_config_answer(&world, "plugins.slots.memory", COMPONENT);
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "a slot this adapter still owns is handed back: {lines:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
}

#[test]
fn displacement_dry_run_plans_the_handoff_without_mutating() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("plan");
    let plan = match outcome {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    assert!(
        plan.actions.iter().any(
            |action| action.contains("disable openclaw plugin 'memory-core'")
                && action.contains("plugins.slots.memory")
        ),
        "plan must show the hand-off and the slot guard: {:?}",
        plan.actions
    );
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "dry-run must not mutate: {lines:?}"
    );
    assert!(!world.has_claim());
}

/// Stage an enabled OpenClaw adapter whose contract displaces `memory-core`
/// on the exclusive `memory` slot. Every displacement test below starts from
/// exactly this state.
fn stage_enabled_with_displacement(guard: &OpenClawEnvGuard) -> (World, AdapterManager) {
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "fixture must start from a claimed displacement"
    );
    (world, manager)
}

/// OpenClaw spells a deliberately closed memory slot `plugins.slots.memory =
/// "none"`. That is the operator's choice, not an empty slot: `plugins enable`
/// re-runs the framework's exclusive slot selection and would silently pick a
/// memory backend the operator just declined.
#[test]
fn disable_never_reopens_an_explicitly_closed_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    set_config_answer(&world, "plugins.slots.memory", "none");
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "restoring would re-run slot selection and undo the operator's off: {lines:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|message| message.contains("plugins.slots.memory")
                && message.contains("explicitly")),
        "disable must say the slot was explicitly closed, not just that it stepped aside: {:?}",
        outcome.report.messages
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the closed slot must stay closed"
    );
    // Stepping aside is a completed cleanup, not a failure: there is nothing
    // left to retry, and keeping the receipt would strand it.
    assert!(outcome.report.cleanup_complete);
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
}

/// The off-words the enablement probe honors must classify the same way here,
/// or one host's rendering of "off" would be respected on one key and
/// overridden on the other.
#[test]
fn disable_honors_every_explicit_off_spelling_of_a_slot() {
    for token in ["none", "off", "disabled", "false", "no", "0"] {
        let guard = OpenClawEnvGuard::acquire();
        let (world, manager) = stage_enabled_with_displacement(&guard);
        set_config_answer(&world, "plugins.slots.memory", token);

        manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("disable");

        let lines = argv_lines(&world.argv_log());
        assert!(
            !argv_contains(&lines, "plugins enable memory-core"),
            "slot '{token}' is an explicit off and must not be restored: {lines:?}"
        );
    }
}

/// By re-enable time `memory-core` is disabled *by this adapter*, so a fresh
/// `plugins.entries.<id>.enabled` probe reads `false` and would plan "leave it
/// alone, disable will not re-enable it" — the exact opposite of what
/// `preserve_reenable_facts` carries over and a later disable then does.
#[test]
fn reenable_dry_run_plans_the_displacement_the_receipt_carries_over() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    // The argv log is cumulative and already holds the fixture's real enable,
    // so only what the dry-run appends is under test here.
    let logged_before = argv_lines(&world.argv_log()).len();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan");
    let plan = match outcome {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };

    assert!(
        plan.actions.iter().any(|action| action
            .contains("keep openclaw plugin 'memory-core' disabled")
            && action.contains("disable will hand it back")
            && action.contains("plugins.slots.memory")),
        "a re-enable must plan from the receipt's displacement, not from a fresh probe: {:?}",
        plan.actions
    );
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.contains("leave openclaw plugin 'memory-core' alone")),
        "the preview must not promise the opposite of the real lifecycle: {:?}",
        plan.actions
    );
    // A plan may probe (`--version`, `config get`, `install --help`), but it
    // must not run a single mutating verb.
    let all = argv_lines(&world.argv_log());
    let planned = &all[logged_before.min(all.len())..];
    assert_dry_run_only_probed(planned);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "dry-run must leave the displacement in place"
    );
    assert!(world.has_claim(), "dry-run must not touch the receipt");
}

/// `apply_enable` releases the tool names exactly once. A later
/// `openclaw plugins enable memory-core` puts the first-wins collision straight
/// back while this adapter's own plugin still lists and loads fine, so `status`
/// has to be the thing that notices.
#[test]
fn status_degrades_when_a_displaced_plugin_is_reenabled_behind_our_back() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    // Baseline: the hand-off is recorded in config, which is all this driver can
    // see — so Unknown, not Healthy. See the pending-restart case below.
    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("a claimed displacement is part of what makes the adapter work");
    assert_eq!(condition.status, ConditionStatus::Unknown);

    // An operator command or a framework update turns the bundled plugin back on.
    // *That* config settles it in the one direction config can: whether or not the
    // gateway has caught up, the hand-off this adapter owns is not in force.
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");
    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "the collision is back even though this adapter's own plugin verifies clean"
    );
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::False);
    assert!(
        condition
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("memory-core")),
        "the verdict must name the plugin that came back: {:?}",
        condition.reason
    );
}

/// A probe the host cannot answer is not a verdict: status must say it could
/// not verify rather than report the displacement healthy or broken.
#[test]
fn status_reports_unknown_when_the_displacement_probe_cannot_run() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_displacement(&guard);

    guard.set("FAKE_OC_PROBE_FAIL", "config_get");
    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::Unknown);
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Unknown);
}

/// A plugin that cannot load is not holding the tool names, however its own
/// enablement flag reads.
///
/// `status` used to read only `plugins.entries.<id>.enabled` and treat anything
/// that was not a positive `false` as "the collision is back", so a framework
/// upgrade that removed the plugin from the host still reported a regression and
/// degraded the whole adapter. The restore branch already asked the inventory
/// first, so the two sides of one receipt disagreed.
///
/// Not a collision is still not the same as *cleared*, and the verdict below is
/// `Unknown` rather than `True`: `plugins list` is answered by a freshly spawned CLI
/// reading the host's current discovery set, so absence says the plugin is gone from
/// that, not that the gateway already running stopped serving its tools — a module
/// loaded before the removal stays loaded, which is why OpenClaw's own
/// `plugins uninstall` asks for a gateway restart. Same missing gateway evidence as
/// the policy branch, different trigger.
#[test]
fn status_does_not_report_a_collision_for_a_plugin_the_host_no_longer_has() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    // Control: with the plugin present in the inventory, this flag alone really
    // does mean the collision is back.
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");
    let condition = displacement_condition(&manager);
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "the control must show the flag on its own degrades the adapter"
    );

    // A framework update drops the bundled plugin. Its enablement flag still
    // reads `true`, but there is nothing left to hold the names.
    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("drop the plugin from the host's inventory");
    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = find_displacement_condition(&status);
    assert_eq!(
        condition.status,
        ConditionStatus::Unknown,
        "a plugin the host no longer has cannot be a collision, and a cold inventory \
         read cannot clear the running gateway either: {:?}",
        condition.reason
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "and it must not degrade the adapter over a hand-off nothing contests"
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Healthy,
        "nor clear it: whether the gateway already stopped serving those tools is \
         exactly what ANOLISA cannot see"
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("inventory"),
        "the verdict must say what rules the plugin out, so the operator does not go \
         looking for a plugin that is gone: {reason}"
    );
    assert!(
        reason.contains("tool call"),
        "and must point at the one check that settles it: {reason}"
    );
    assert!(
        !reason.contains("plugins inspect"),
        "a cold probe reads the same persisted state this verdict came from, so it \
         must not be offered as confirmation: {reason}"
    );
    // `plugins list` does appear once above, naming the read this verdict came from
    // rather than offering it as a check — the round-8 rule is about advice, and the
    // advice this reason gives is the tool call asserted just above. The two
    // recorded-hand-off tests pin the stricter wording, where no such mention is
    // load-bearing at all.
}

/// A removal must not upgrade a recorded hand-off either — the reviewer's repro for
/// the same missing evidence, from the state a successful enable actually leaves.
///
/// The hand-off is recorded, so the condition was already `Unknown`: config proves
/// the write, not that the gateway applied it. Dropping the plugin from the
/// inventory used to flip that to `Healthy` with a reason saying nothing needed
/// doing, which is the false all-clear arriving exactly when the operator may still
/// have to restart the gateway or check a real tool call.
#[test]
fn status_does_not_clear_a_recorded_handoff_the_inventory_no_longer_lists() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "fixture must start from a hand-off this adapter really performed"
    );
    let before = displacement_condition(&manager);
    assert_eq!(
        before.status,
        ConditionStatus::Unknown,
        "a recorded hand-off is unobservable, not cleared: {:?}",
        before.reason
    );

    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("a framework update drops the bundled plugin");
    let status = manager.status(Some(COMPONENT)).expect("status");
    let after = find_displacement_condition(&status);
    assert_eq!(
        after.status,
        ConditionStatus::Unknown,
        "removal from a cold inventory is not evidence about the running gateway: {:?}",
        after.reason
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Healthy,
        "and must not turn an unobservable hand-off into an all-clear"
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "nor report a collision over a plugin the host no longer has"
    );
    let reason = after.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("inventory"),
        "the verdict must still name the plugin and what rules it out: {reason}"
    );
}

/// The same disagreement at the policy level, for both ways OpenClaw can keep a
/// plugin from loading on *every* path it has: a denylist entry and the global
/// `plugins.enabled` switch. Neither says anything about
/// `plugins.entries.<id>.enabled`, which is why reading only that key reported a
/// collision the host was actively preventing.
///
/// A restrictive `plugins.allow` used to be the third row here and is not one any
/// more — see `status_reports_a_collision_an_allowlist_does_not_explain` below.
///
/// What a policy does *not* prove is that the running gateway stopped serving the
/// plugin's tools, so the verdict is `Unknown` rather than `True`: with
/// `gateway.reload.mode = off` OpenClaw's config reloader leaves the running process
/// untouched. Both halves are asserted below — not `Degraded` (the collision the
/// host is preventing) and not `Healthy` (the all-clear a config write cannot give).
#[test]
fn status_does_not_report_a_collision_for_a_plugin_policy_keeps_off() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");

    for (key, value, needle) in [
        ("plugins.deny", "memory-core", "plugins.deny"),
        ("plugins.enabled", "false", "plugins.enabled"),
    ] {
        // Each case starts from a clean policy so the three do not mask each other.
        for other in ["plugins.deny", "plugins.allow", "plugins.enabled"] {
            if other != key {
                set_config_answer(&world, other, "");
            }
        }
        set_config_answer(&world, key, value);

        let status = manager.status(Some(COMPONENT)).expect("status");
        let condition = find_displacement_condition(&status);
        assert_eq!(
            condition.status,
            ConditionStatus::Unknown,
            "{key} = {value} keeps the plugin from loading, so its `true` enablement \
             flag is not a collision — but a config write is not an observation of \
             the running gateway either: {:?}",
            condition.reason
        );
        assert_ne!(
            status.entries[0].report.summary,
            AdapterSummary::Degraded,
            "{key} = {value} must not degrade the adapter"
        );
        assert_ne!(
            status.entries[0].report.summary,
            AdapterSummary::Healthy,
            "{key} = {value} must not clear the adapter either: whether the gateway \
             already stopped serving those tools is exactly what ANOLISA cannot see"
        );
        assert!(
            condition
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("memory-core") && reason.contains(needle)),
            "the verdict must name the policy that keeps it off, since that is the \
             thing the operator would otherwise go and undo: {:?}",
            condition.reason
        );
    }
}

/// ... and the allowlist is deliberately not one of them.
///
/// `plugins.allow` omitting the plugin does not keep it from loading, so it cannot
/// explain a collision away. Two of OpenClaw's own load paths never consult it: the
/// activation decision enables whichever plugin owns `plugins.slots.memory` *before*
/// reaching the allowlist check (v2026.4.14 `src/plugins/config-state.ts:310-345`,
/// v2026.9.2 `src/plugins/config-activation-shared.ts:204-218`), and since the
/// v2026.7.2 beta line the memory dreaming sidecar is a separate path that checks
/// only the global switch, the denylist and an explicit `enabled = false`
/// (`resolveAuthorizedDreamingSidecar`, v2026.9.2
/// `src/plugins/loader-shared.ts:90-137`) and is then hard-coded to
/// `enabled: true` by the loader (`src/plugins/loader-runtime-candidate.ts:108-119`).
/// Filing the entry under "policy keeps it from loading" reported `Unknown` — no
/// collision — for a plugin the host may be serving `memory_get` / `memory_search`
/// from right now, which is the false all-clear this condition exists to prevent.
///
/// So the allowlist falls through to the enablement flag like any policy the driver
/// cannot positively read, and a `true` flag is the collision it looks like. The
/// allowlist still vetoes a *restore*, which is a different question with a
/// different answer; see `disable_reports_the_allowlist_that_survives_its_uninstall`.
#[test]
fn status_reports_a_collision_an_allowlist_does_not_explain() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    for key in ["plugins.deny", "plugins.enabled"] {
        set_config_answer(&world, key, "");
    }
    set_config_answer(&world, "plugins.allow", "some-other-plugin");
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = find_displacement_condition(&status);
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "an allowlist does not rule the plugin out, so its `true` enablement flag \
         decides — and that is a collision: {:?}",
        condition.reason
    );
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "and the adapter must be degraded over it"
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core"),
        "the verdict must still name the plugin: {reason}"
    );
    assert!(
        !reason.contains("plugins.allow"),
        "and must not offer the allowlist as the explanation, since the host has \
         two load paths that ignore it: {reason}"
    );
}

/// A policy added *after* the hand-off must not upgrade the verdict.
///
/// The reported repro: the hand-off is recorded (`plugins.entries.memory-core.enabled`
/// is false, so the condition was already `Unknown` — config proves the write, not
/// that the gateway applied it), and then `plugins.deny = ["memory-core"]` appears.
/// Filing a policy block under "released" turned that `Unknown` into `Healthy`, with
/// a reason saying the plugin could not shadow this adapter and nothing needed
/// doing — a false all-clear delivered exactly when the operator may still have to
/// restart the gateway or check a real tool call, because with
/// `gateway.reload.mode = off` OpenClaw's config reloader explicitly leaves the
/// running process alone.
#[test]
fn status_does_not_clear_a_recorded_handoff_because_a_policy_now_blocks_it() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "fixture must start from a hand-off this adapter really performed"
    );

    let before = displacement_condition(&manager);
    assert_eq!(
        before.status,
        ConditionStatus::Unknown,
        "a recorded hand-off is unobservable, not cleared: {:?}",
        before.reason
    );

    set_config_answer(&world, "plugins.deny", "memory-core");
    let status = manager.status(Some(COMPONENT)).expect("status");
    let after = find_displacement_condition(&status);
    assert_eq!(
        after.status,
        ConditionStatus::Unknown,
        "a persisted policy write says nothing about the process already running: {:?}",
        after.reason
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Healthy,
        "adding a deny entry must not turn an unobservable hand-off into an all-clear"
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "and must not report a collision the host is actively preventing either"
    );
    let reason = after.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("plugins.deny"),
        "the verdict must still name the plugin and the policy: {reason}"
    );
    assert!(
        reason.contains("tool call"),
        "and must point at the one check that settles it: {reason}"
    );
    for cold in ["plugins list", "plugins inspect"] {
        assert!(
            !reason.contains(cold),
            "a cold probe reads the same config this verdict came from, so it must \
             not be offered as confirmation ({cold}): {reason}"
        );
    }
}

/// ... and the control that keeps those three honest: with no policy blocking it
/// and the plugin still in the inventory, the same `true` flag *is* a collision.
#[test]
fn status_still_reports_a_collision_no_policy_explains() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    for key in ["plugins.deny", "plugins.allow", "plugins.enabled"] {
        set_config_answer(&world, key, "");
    }
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    assert_eq!(
        find_displacement_condition(&status).status,
        ConditionStatus::False,
        "nothing rules the plugin out, so its enablement flag decides"
    );
}

/// A receipt that claims no displacement keeps its existing condition set: the
/// new signal must not appear for adapters that never declared one.
#[test]
fn status_omits_the_displacement_condition_without_a_claimed_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(&world.layout, &plugin_adapter_block(None));
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert!(
        !status.entries[0]
            .report
            .conditions
            .iter()
            .any(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased),
        "no claimed displacement, no displacement condition: {:?}",
        status.entries[0].report.conditions
    );
}

/// Rewrite the installed manifest so a re-enable sees a different contract,
/// keeping the staged bundle and every host-side fact untouched.
fn redeclare_displacement(world: &World, block: &str) {
    write_openclaw_manifest(&world.layout, block);
}

/// Mutate the persisted receipt's displacement references, the way a truncated
/// write or a hand-edited `installed.toml` would.
fn forge_displaced_references(world: &World, forge: impl FnOnce(&mut Vec<DisplacedPluginRef>)) {
    let state_path = world.layout.state_dir.join("installed.toml");
    let mut state = world.load_state();
    let claim = state
        .adapter_claims
        .iter_mut()
        .find(|claim| claim.component == COMPONENT && claim.framework == FRAMEWORK)
        .expect("openclaw claim");
    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    forge(&mut payload.displaced_plugins);
    state.save(&state_path).expect("save forged receipt");
}

/// One way of corrupting a receipt's displacement references, by label.
type DisplacementForgery = (&'static str, fn(&mut Vec<DisplacedPluginRef>));

/// Point two legitimate displacements at one exclusive slot, the way a
/// hand-edited `installed.toml` could.
fn forge_two_displacement_slots(world: &World, slot: &str) {
    forge_displaced_references(world, |entries| {
        assert_eq!(entries.len(), 2, "fixture must claim two displacements");
        entries[1].slot = Some(slot.to_string());
    });
}

/// Every way a displacement reference can be corrupt. Each one must be rejected
/// before the driver acts on the receipt, not part-way through.
fn displacement_forgeries() -> Vec<DisplacementForgery> {
    vec![
        (
            "dangling reference",
            |entries: &mut Vec<DisplacedPluginRef>| {
                entries[0].resource = "openclaw_displaced_plugin_nope".to_string();
            },
        ),
        (
            "reference to the adapter's own plugin resource",
            |entries: &mut Vec<DisplacedPluginRef>| {
                // Resolves fine and *is* a FrameworkPlugin, so only the purpose
                // check tells this apart from a real displacement — and honoring
                // it would drive `plugins enable` against this adapter's own
                // plugin.
                entries[0].resource = "openclaw_plugin".to_string();
            },
        ),
        (
            "the same reference twice",
            |entries: &mut Vec<DisplacedPluginRef>| {
                entries.push(entries[0].clone());
            },
        ),
        (
            "an entry that both claims and guards one slot",
            |entries: &mut Vec<DisplacedPluginRef>| {
                // A guard is the key an entry consults *because* another entry owns
                // it. Both on one entry is two answers to "which slot does this
                // restore read", and only one of them can be the one a later
                // `disable` acts on.
                entries[0].guard_slot = entries[0].slot.clone();
            },
        ),
        (
            "an empty guard slot",
            |entries: &mut Vec<DisplacedPluginRef>| {
                // `plugins.slots.` is not a key: an entry that names one reads as
                // guarded and consults nothing, which is the unguarded restore the
                // field exists to prevent.
                entries[0].slot = None;
                entries[0].guard_slot = Some(String::new());
            },
        ),
    ]
}

/// A corrupt displacement receipt must never produce a `Healthy` report: the
/// driver cannot name what the adapter is supposed to have displaced, so every
/// condition it would report — including the one that says the tool names are
/// still this adapter's — is unverifiable guesswork.
#[test]
fn status_refuses_to_report_a_corrupt_displacement_receipt() {
    for (label, forge) in displacement_forgeries() {
        let guard = OpenClawEnvGuard::acquire();
        let (world, manager) = stage_enabled_with_displacement(&guard);
        forge_displaced_references(&world, forge);

        let err = manager
            .status(Some(COMPONENT))
            .expect_err("a receipt whose displacement references do not resolve is not reportable");
        assert!(
            matches!(&err, AdapterError::BundleInvalid { reason, .. }
                if reason.contains("displaced")),
            "{label}: expected a displaced-receipt rejection, got {err:?}"
        );
    }
}

/// The same corruption must stop `disable` *before* the uninstall, not three
/// steps later when the restore resolves its references. Discovering it after
/// `plugins uninstall` would leave the adapter's own plugin removed, the receipt
/// kept, and the bundled plugin still disabled — a partial uninstall driven by a
/// record the driver could not read.
#[test]
fn disable_mutates_nothing_for_a_corrupt_displacement_receipt() {
    for (label, forge) in displacement_forgeries() {
        let guard = OpenClawEnvGuard::acquire();
        let (world, manager) = stage_enabled_with_displacement(&guard);
        forge_displaced_references(&world, forge);

        let logged_before = argv_lines(&world.argv_log()).len();

        // The dry-run has to reject it too. A plan that succeeds for a receipt
        // the real disable refuses tells the operator the cleanup is available
        // when it is not — and `plan_disable_report` is a pure function of the
        // receipt, so nothing in it would ever notice on its own.
        let err = manager
            .disable(COMPONENT, Some(FRAMEWORK), true)
            .expect_err("disable --dry-run must reject the same receipts");
        assert!(
            matches!(&err, AdapterError::BundleInvalid { reason, .. }
                if reason.contains("displaced")),
            "{label}: dry-run: expected a displaced-receipt rejection, got {err:?}"
        );
        assert_eq!(
            argv_lines(&world.argv_log()).len(),
            logged_before,
            "{label}: a rejected plan must not call the host"
        );

        let err = manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect_err("disable must fail closed on an unresolvable displacement");
        assert!(
            matches!(&err, AdapterError::BundleInvalid { reason, .. }
                if reason.contains("displaced")),
            "{label}: expected a displaced-receipt rejection, got {err:?}"
        );

        let lines = argv_appended(&world, logged_before);
        assert!(
            lines.is_empty(),
            "{label}: disable must fail before its first host call: {lines:?}"
        );
        assert!(
            world.registry_marker_exists(),
            "{label}: the adapter's own plugin must still be registered"
        );
        assert!(
            displaced_marker_exists(&world, "memory-core"),
            "{label}: the displaced plugin must still be in the state enable left it"
        );
        assert!(
            world.has_claim(),
            "{label}: the receipt must be kept for retry"
        );
    }
}

/// A dry-run must be rejected the same way, and before it probes anything.
#[test]
fn enable_dry_run_rejects_a_corrupt_displacement_receipt_before_probing() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    forge_displaced_references(&world, |entries| {
        entries[0].resource = "openclaw_displaced_plugin_nope".to_string();
    });
    let logged_before = argv_lines(&world.argv_log()).len();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("a re-enable plan reads the receipt it would replace");
    assert!(
        matches!(&err, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("displaced")),
        "unexpected error: {err:?}"
    );
    // `plan_enable` resolves the host profile before it reaches the
    // displacement plan, so read-only probes may already have run. What must not
    // have happened is any mutation, or a plan handed back to the caller.
    let planned = argv_appended(&world, logged_before);
    for forbidden in [
        "plugins disable memory-core",
        "plugins enable memory-core",
        "plugins uninstall tokenless",
    ] {
        assert!(
            !argv_contains(&planned, forbidden),
            "a rejected plan must not mutate ('{forbidden}'): {planned:?}"
        );
    }
    assert!(
        world.has_claim(),
        "the prior receipt must survive a rejected plan"
    );
}

/// `plugin_id` is optional in the contract, so the declaration-time comparison
/// can only see `""`; the real id is whatever the driver resolves from
/// `openclaw.plugin.json` (or the component name). Without a second gate after
/// resolution, a contract could install and verify its own plugin, then disable
/// it, and still report success with an `Enabled` receipt.
#[test]
fn enable_rejects_self_displacement_resolved_from_the_bundle() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    // The staged bundle's `openclaw.plugin.json` names the component itself.
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement(COMPONENT, Some("memory")),
    );
    // Nothing seeded in the registry: any marker found below can only have come
    // from the rejected enable itself.
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    for dry_run in [true, false] {
        let err = manager
            .enable(COMPONENT, Some(FRAMEWORK), dry_run)
            .expect_err("an adapter may not displace itself");
        assert!(
            matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
                if reason.contains("must differ from the adapter's own plugin id")),
            "dry_run={dry_run}: unexpected error: {err:?}"
        );
    }
    assert!(
        argv_lines(&world.argv_log()).is_empty(),
        "the resolved-id gate runs before any host call: {:?}",
        argv_lines(&world.argv_log())
    );
    assert!(!world.has_claim(), "no receipt for a rejected contract");
    assert!(
        !world.registry_marker_exists(),
        "the adapter's own plugin must not have been installed"
    );
}

/// The declaration-time gate still catches the case it can see, so a contract
/// that names its own `plugin_id` is rejected without reading the bundle at all.
#[test]
fn enable_rejects_self_displacement_against_a_declared_plugin_id() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &format!(
            r#"[[adapters]]
framework = "openclaw"
plugin_id = "memory-core"
source = "adapters/{COMPONENT}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[[adapters.openclaw.displaces]]
id = "memory-core"
slot = "memory"
"#
        ),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an adapter may not displace itself");
    assert!(
        matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
            if reason.contains("must differ from the adapter's own plugin id")),
        "unexpected error: {err:?}"
    );
    assert!(argv_lines(&world.argv_log()).is_empty());
    assert!(!world.has_claim());
}

/// A component upgrade that drops the declaration must take effect. Inheriting
/// the prior receipt unconditionally would resurrect the stale fact — the fresh
/// probe claims nothing because this adapter already disabled the plugin,
/// same-home cleanup would have nothing to release, and `apply_enable` would
/// disable it again, so the new contract could never win.
#[test]
fn reenable_releases_a_displacement_the_new_contract_dropped() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    redeclare_displacement(&world, &plugin_adapter_block(None));
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a contract with no displacement");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "the dropped displacement must be handed back while the prior receipt is \
         still the durable record of why it was off: {lines:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the new contract does not displace it, so it must not stay disabled"
    );
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "the replacement receipt must not re-claim a dropped declaration: {:?}",
        payload.displaced_plugins
    );
    assert!(
        !claim
            .resources
            .iter()
            .any(|r| r.purpose == "openclaw_displaced_plugin"),
        "the resource list must not keep the dropped plugin either: {:?}",
        claim.resources
    );
}

/// A component upgrade that displaces a *different* plugin must release the old
/// one and claim the new one in the same re-enable.
#[test]
fn reenable_swaps_a_displacement_the_new_contract_replaced() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    seed_bundled_plugin(&world, "memory-lancedb");

    redeclare_displacement(
        &world,
        &plugin_adapter_block_with_displacement("memory-lancedb", Some("memory")),
    );
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a replacement displacement");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "the plugin the new contract no longer names must be handed back: {lines:?}"
    );
    assert!(
        argv_contains(&lines, "plugins disable memory-lancedb"),
        "the newly declared plugin must be displaced: {lines:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
    assert!(displaced_marker_exists(&world, "memory-lancedb"));

    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert_eq!(
        payload.displaced_plugins[0].resource,
        "openclaw_displaced_plugin_memory-lancedb"
    );
}

/// A component upgrade that moves the same plugin to a different exclusive slot
/// keeps the ownership (this adapter really did disable it) but must restore it
/// against the slot the *current* contract names — the slot is contract
/// metadata, not history.
#[test]
fn reenable_takes_the_slot_from_the_current_contract_not_the_prior_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    redeclare_displacement(
        &world,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory2")),
    );
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a moved slot");

    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "still declared, so it stays displaced"
    );
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert_eq!(
        payload.displaced_plugins[0].slot.as_deref(),
        Some("memory2"),
        "the replacement receipt must carry the current contract's slot"
    );

    // And disable consults the new slot, not the one the prior receipt named.
    set_config_answer(&world, "plugins.slots.memory2", "memory-lancedb");
    set_config_answer(&world, "plugins.slots.memory", COMPONENT);
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "the guard must read plugins.slots.memory2, the slot this contract declares: {lines:?}"
    );
}

/// The enable preview must name every veto the real restore applies — not just
/// the slot ones. `restore_decision` has four, and the inventory and policy
/// vetoes never look at the slot, so a preview built from `slot` alone describes
/// two of them and calls a slotless restore unconditional.
#[test]
fn enable_dry_run_preview_names_every_restore_veto() {
    for (label, slot) in [("slotful", Some("memory")), ("slotless", None)] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        write_openclaw_manifest(
            &world.layout,
            &plugin_adapter_block_with_displacement("memory-core", slot),
        );
        seed_bundled_plugin(&world, "memory-core");
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
        let manager = world.manager();

        let plan = match manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect("plan")
        {
            EnableOutcome::Planned { plan, .. } => plan,
            EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
        };
        let action = plan
            .actions
            .iter()
            .find(|a| a.contains("disable openclaw plugin 'memory-core'"))
            .unwrap_or_else(|| panic!("no displacement action in {:?}", plan.actions));

        for (needle, veto) in [
            ("inventory", "the plugin having left the host's inventory"),
            ("plugins.deny", "an explicit denylist"),
            ("plugins.allow", "a restrictive allowlist"),
        ] {
            assert!(
                action.contains(needle),
                "{label}: the preview must name the {veto} veto, which does not depend on \
                 the slot: {action}"
            );
        }
        assert!(
            !action.contains("unconditional"),
            "{label}: no restore is unconditional — four things can veto it: {action}"
        );
        if slot.is_some() {
            for needle in [
                "plugins.slots.memory",
                "another plugin",
                "explicitly closed",
            ] {
                assert!(
                    action.contains(needle),
                    "{label}: the slot vetoes must be named too: {action}"
                );
            }
        } else {
            assert!(
                !action.contains("plugins.slots."),
                "{label}: a slotless displacement has no slot to name: {action}"
            );
        }
    }
}

/// The reviewer's repro, end to end: enable a slotless displacement, then have a
/// policy keep the plugin off, then ask for a re-enable plan. The plan used to
/// promise an unconditional future restore; the real disable then declined to
/// issue one and removed the receipt anyway, so nothing recorded the divergence.
#[test]
fn reenable_preview_does_not_promise_a_restore_policy_will_block() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", None),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable a slotless displacement");

    // The operator adds a policy that keeps the plugin off.
    set_config_answer(&world, "plugins.deny", "memory-core");

    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan under the same contract")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    let carried = plan
        .actions
        .iter()
        .find(|a| a.contains("keep openclaw plugin 'memory-core' disabled"))
        .unwrap_or_else(|| panic!("no carry-over action in {:?}", plan.actions));
    assert!(
        !carried.contains("unconditional"),
        "the carried-over preview must not promise an unconditional restore: {carried}"
    );
    assert!(
        carried.contains("plugins.deny"),
        "and must name the policy veto that will actually fire: {carried}"
    );

    // And the real lifecycle agrees: no `plugins enable`, receipt still removed.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable");
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the policy blocks the restore, so none may be issued: {appended:?}"
    );
    assert!(outcome.report.cleanup_complete);
    assert!(!world.has_claim());
}

/// A plugin the host no longer has makes the contract unfulfillable, and the plan
/// must fail the same way the real enable does.
///
/// This test used to assert only the dry-run's *wording* — that the carry-over line
/// named the inventory veto — and never asked what the real enable would do. That
/// gap is exactly where the divergence hid: `plan_enable` tests `carried` before it
/// probes, so a displacement the receipt carries over skipped the existence check
/// altogether and the plan exited 0 promising "it stays claimed and disable will
/// hand it back", while `prepare_enable` probes unconditionally and fails the real
/// re-enable with `missing_displacement_target` before running a single planned
/// action. A preview that names a different condition than the real path applies is
/// worse than no preview: the operator plans around it.
#[test]
fn reenable_preview_fails_like_the_real_enable_for_a_vanished_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", None),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    // A framework upgrade drops the bundled plugin this contract displaces.
    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("the host drops the bundled plugin");
    let logged_before = argv_lines(&world.argv_log()).len();

    let dry_run = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("a carried-over displacement still has to exist on this host");
    assert!(
        matches!(&dry_run, AdapterError::InvalidAdapterInput { reason, .. }
            if reason.contains("memory-core") && reason.contains("inventory")),
        "the plan must fail on the inventory, not describe a carry-over: {dry_run:?}"
    );
    let real = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("the real re-enable must fail too");
    assert_eq!(
        dry_run.to_string(),
        real.to_string(),
        "the preview and the operation it describes must reach the same verdict"
    );

    // Neither attempt may mutate the host or consume the receipt: the first
    // enable's displacement is still applied ownership, and it is `disable` that
    // gets to conclude the host released it.
    let appended = argv_appended(&world, logged_before);
    for forbidden in [
        "plugins disable memory-core",
        "plugins enable memory-core",
        "plugins uninstall tokenless",
    ] {
        assert!(
            !argv_contains(&appended, forbidden),
            "a failed re-enable must not mutate the host ('{forbidden}'): {appended:?}"
        );
    }
    assert!(
        !appended
            .iter()
            .any(|line| line.starts_with("plugins install ") && !line.contains("--help")),
        "and must not install anything: {appended:?}"
    );
    assert!(
        world.has_claim(),
        "a rejected re-enable must leave the existing receipt alone"
    );

    // The payoff: that surviving receipt still works, and `disable` reaches the
    // inventory verdict itself rather than stranding the ownership.
    let logged_before = argv_lines(&world.argv_log()).len();
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "there is nothing left to hand back: {appended:?}"
    );
    assert!(
        disabled
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("no longer in this host's")),
        "and the operator must be told the displacement was released, not silently \
         dropped: {:?}",
        disabled.report.messages
    );
    assert!(disabled.report.cleanup_complete);
    assert!(!world.has_claim());
}

/// Two entries naming the same plugin are each well-formed on their own, so only
/// a cross-entry check can catch them. Left alone they would build two resources
/// with one id: the receipt passes the generic claim validation, gets persisted,
/// and gets its own plugin installed before `claim_displaced_plugins` rejects it
/// — a `cleanup_failed` receipt that neither status nor disable can consume.
#[test]
fn enable_rejects_a_duplicated_displacement_id_before_touching_the_host() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacements(&[
            ("memory-core", Some("memory")),
            ("memory-core", Some("memory")),
        ]),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    for dry_run in [true, false] {
        let err = manager
            .enable(COMPONENT, Some(FRAMEWORK), dry_run)
            .expect_err("a duplicated displacement id is not a usable contract");
        assert!(
            matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
                if reason.contains("memory-core") && reason.contains("more than once")),
            "dry_run={dry_run}: unexpected error: {err:?}"
        );
    }
    assert!(
        argv_lines(&world.argv_log()).is_empty(),
        "the contract must be rejected before any host call: {:?}",
        argv_lines(&world.argv_log())
    );
    assert!(!world.has_claim(), "no receipt for a rejected contract");
    assert!(
        !world.registry_marker_exists(),
        "the adapter's own plugin must not have been installed"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "nothing may be disabled by a contract that was never accepted"
    );
}

/// Two different plugins cannot share one exclusive slot, and the failure is
/// silent rather than loud: on disable the first restore makes that plugin the
/// slot's owner, the guard then reads it as a *third* owner for the second one
/// and steps aside, and the receipt is removed while the second plugin stays
/// disabled by this adapter with nothing left to record why.
#[test]
fn enable_rejects_two_displaced_plugins_sharing_one_exclusive_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacements(&[
            ("memory-core", Some("memory")),
            ("memory-lancedb", Some("memory")),
        ]),
    );
    seed_bundled_plugin(&world, "memory-core");
    seed_bundled_plugin(&world, "memory-lancedb");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an exclusive slot cannot be promised to two plugins");
    assert!(
        matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
            if reason.contains("plugins.slots.memory") || reason.contains("slot 'memory'")),
        "unexpected error: {err:?}"
    );
    assert!(argv_lines(&world.argv_log()).is_empty());
    assert!(!world.has_claim());
    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            !displaced_marker_exists(&world, id),
            "no plugin may be left disabled by a contract that was never accepted: {id}"
        );
    }
}

/// `resources` is a set keyed by id, so reordering it is a legitimate hand edit
/// that every generic check accepts. Resolving this adapter's own plugin through
/// `OpenClawClaim.plugin_resource` — rather than as "the first `FrameworkPlugin`
/// in the list" — is what keeps `disable` pointed at the right plugin: the
/// alternative uninstalls the *displaced* one and keeps the adapter's own.
#[test]
fn disable_uninstalls_its_own_plugin_even_with_receipt_resources_reordered() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    reorder_resources_displaced_first(&world);
    let logged_before = argv_lines(&world.argv_log()).len();

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable over a reordered but valid receipt");

    let lines = argv_appended(&world, logged_before);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("plugins uninstall tokenless")),
        "disable must uninstall this adapter's own plugin: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("plugins uninstall memory-core")),
        "disable must never uninstall the plugin it displaced: {lines:?}"
    );
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "the displaced plugin is handed back, not removed: {lines:?}"
    );
    assert!(outcome.claim_removed);
    assert!(outcome.report.cleanup_complete);
    assert!(
        !world.registry_marker_exists(),
        "the adapter's own registration must be gone"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the displaced plugin must be enabled again"
    );
}

/// Same, for a contract that replaces the displaced plugin: the plan must show
/// both halves — the old one handed back and the new one taken over.
#[test]
fn reenable_dry_run_lists_both_halves_of_a_replaced_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    seed_bundled_plugin(&world, "memory-lancedb");
    redeclare_displacement(
        &world,
        &plugin_adapter_block_with_displacement("memory-lancedb", Some("memory")),
    );

    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };

    assert!(
        plan.actions.iter().any(|action| action
            .contains("re-enable openclaw plugin 'memory-core'")
            && action.contains("no longer displaces")),
        "the dropped plugin's restore must be planned: {:?}",
        plan.actions
    );
    assert!(
        plan.actions
            .iter()
            .any(|action| action.contains("disable openclaw plugin 'memory-lancedb'")),
        "the newly declared plugin's hand-off must be planned too: {:?}",
        plan.actions
    );
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.contains("re-enable openclaw plugin 'memory-lancedb'")),
        "a plugin the new contract declares is not dropped: {:?}",
        plan.actions
    );
}

/// Two displaced plugins in one receipt, on two different exclusive slots — the
/// fixture the slot-uniqueness and per-plugin-remediation cases below need.
fn stage_enabled_with_two_displacements(guard: &OpenClawEnvGuard) -> (World, AdapterManager) {
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacements(&[
            ("memory-core", Some("memory")),
            ("memory-lancedb", Some("lance")),
        ]),
    );
    seed_bundled_plugin(&world, "memory-core");
    seed_bundled_plugin(&world, "memory-lancedb");
    world.apply_env(guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with two displacements");
    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            displaced_marker_exists(&world, id),
            "fixture must start from both plugins displaced: {id}"
        );
    }
    (world, manager)
}

/// Ownership is a fact about *one* OpenClaw state directory. A migration is a
/// different registry, where the operator may have disabled the same plugin
/// themselves — `prepare_enable` correctly declines to claim that, so inheriting
/// the prior receipt's claim would override a choice made in the new home, and a
/// later disable would re-enable a plugin the operator had closed.
#[test]
fn migration_does_not_inherit_displacement_ownership_the_new_home_never_had() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    let old_home = world.openclaw_home.clone();
    assert!(old_home.join("disabled/memory-core").exists());

    // A second OpenClaw instance, where the operator turned memory-core off
    // themselves before this adapter ever arrived.
    let new_home = world._root.path().join("configured-openclaw-state");
    for dir in ["registry", "disabled", "config"] {
        std::fs::create_dir_all(new_home.join(dir)).expect("new home dir");
    }
    std::fs::write(new_home.join("registry/memory-core"), b"").expect("bundled plugin");
    std::fs::write(new_home.join("disabled/memory-core"), b"").expect("operator's own disable");
    std::fs::write(
        new_home.join("config/plugins.entries.memory-core.enabled"),
        b"false",
    )
    .expect("persisted enablement");
    guard.set("OPENCLAW_STATE_DIR", &new_home);

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable into the new state directory");

    // The old home is released in full, because that is where this adapter was
    // what turned the plugin off.
    assert!(
        !old_home.join("disabled/memory-core").exists(),
        "the prior home's displacement must be handed back where it was taken"
    );
    // The new home's own state is left exactly as the operator set it.
    assert!(
        new_home.join("disabled/memory-core").exists(),
        "the operator's disable in the new home must survive the migration"
    );

    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "ownership in the new home comes from the new home's probe, which found an \
         operator's own disable: {:?}",
        payload.displaced_plugins
    );
    assert_eq!(
        recorded_openclaw_state_dir(&claim),
        new_home.as_path(),
        "the receipt must point at the new state directory"
    );

    // And the payoff: a later disable must not re-enable what the operator closed.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable the migrated receipt");
    let lines = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "disable must never re-enable a plugin the operator turned off in the new \
         home: {lines:?}"
    );
    assert!(new_home.join("disabled/memory-core").exists());
}

/// The same migration, but with the new home's enablement probe *unanswerable*
/// instead of positively `false`. That single change is what makes this the test for
/// the inheritance gate itself: the prior receipt really does own `memory-core`, and
/// an unverified claim is exactly what the round-12 split taught the driver to leave
/// alone. Inheriting it across the state directory would skip the re-confirm, run
/// `plugins disable` in the new home, and leave a receipt whose later disable
/// re-enables a plugin the operator *there* had closed — the failure
/// `preserve_openclaw_displaced_facts`'s gate exists to prevent, re-introduced
/// through the ownership round 12 protected.
#[test]
fn migration_does_not_inherit_an_unverified_displacement_claim() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    let old_home = world.openclaw_home.clone();
    assert!(
        old_home.join("disabled/memory-core").exists(),
        "the prior receipt must really own the displacement in the old home"
    );

    let new_home = world._root.path().join("configured-openclaw-state");
    for dir in ["registry", "disabled", "config"] {
        std::fs::create_dir_all(new_home.join(dir)).expect("new home dir");
    }
    std::fs::write(new_home.join("registry/memory-core"), b"").expect("bundled plugin");
    std::fs::write(new_home.join("disabled/memory-core"), b"").expect("operator's own disable");
    std::fs::write(
        new_home.join("config/plugins.entries.memory-core.enabled"),
        b"false",
    )
    .expect("persisted enablement");
    guard.set("OPENCLAW_STATE_DIR", &new_home);
    // prepare's one read of the enablement key fails, so the claim is unverified;
    // apply's re-confirm answers, and in this home it answers `false`.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ONCE",
        "plugins.entries.memory-core.enabled",
    );
    let logged_before = argv_lines(&world.argv_log()).len();

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable into the new state directory");

    let lines = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "the new home's operator closed it themselves, so an unverified claim must          not be inherited across the state directory: {lines:?}"
    );
    // Staging proof, so the assertion above cannot pass vacuously: exactly two
    // reads of the enablement key straddling install, so the one-shot failure
    // was consumed by prepare and not by the old home's cleanup, and apply
    // really did re-confirm.
    let key = "config get plugins.entries.memory-core.enabled";
    let reads: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.as_str() == key)
        .map(|(i, _)| i)
        .collect();
    let install = lines
        .iter()
        .position(|line| line.starts_with("plugins install ") && !line.contains("--help"))
        .expect("install ran in the new home");
    assert_eq!(
        reads.len(),
        2,
        "exactly prepare's read and apply's re-confirm, so the one-shot failure was          provably consumed by prepare and not by the old home's cleanup: {lines:?}"
    );
    assert!(
        reads[0] < install && reads[1] > install,
        "prepare read before install, the re-confirm after: {lines:?}"
    );
    assert!(
        new_home.join("disabled/memory-core").exists(),
        "the operator's disable in the new home must survive the migration"
    );
    assert!(
        !old_home.join("disabled/memory-core").exists(),
        "the prior home's own ownership is still handed back where it was taken"
    );

    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "ownership in the new home comes from the new home's probe alone: {:?}",
        payload.displaced_plugins
    );
    assert_eq!(recorded_openclaw_state_dir(&claim), new_home.as_path());

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable the migrated receipt");
    let lines = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "and disable must never re-enable what the operator closed in the new          home: {lines:?}"
    );
    assert!(new_home.join("disabled/memory-core").exists());
}

/// The migration dry-run has to show the same split: the prior home's restore,
/// and no carry-over of a claim that was never made in the new one.
#[test]
fn migration_dry_run_plans_the_prior_homes_restore_not_a_carryover() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    let old_home = world.openclaw_home.clone();
    let new_home = world._root.path().join("configured-openclaw-state");
    std::fs::create_dir_all(new_home.join("registry")).expect("new home registry");
    std::fs::write(new_home.join("registry/memory-core"), b"").expect("bundled plugin");
    guard.set("OPENCLAW_STATE_DIR", &new_home);
    let logged_before = argv_lines(&world.argv_log()).len();

    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("migration plan")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };

    assert!(
        plan.actions.iter().any(|action| action
            .contains("re-enable openclaw plugin 'memory-core'")
            && action.contains(&old_home.display().to_string())),
        "the plan must show the prior home's restore the real migration performs: {:?}",
        plan.actions
    );
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action
                .contains("the receipt being replaced already claims this displacement")),
        "a claim made in another state directory must not be shown as carried over: {:?}",
        plan.actions
    );
    let appended = argv_appended(&world, logged_before);
    for forbidden in ["plugins enable memory-core", "plugins disable memory-core"] {
        assert!(
            !argv_contains(&appended, forbidden),
            "dry-run must not perform the restore it announced: {appended:?}"
        );
    }
    assert!(old_home.join("disabled/memory-core").exists());
}

/// Two entries sharing one exclusive slot are not merely redundant. Disable
/// restores the first, the guard then reads that plugin as a *third* owner for
/// the second and steps aside, and the receipt is still removed as a completed
/// cleanup — leaving a plugin this adapter disabled with nothing recording why.
/// The Manager rejects that in a contract; the receipt has to be held to the
/// same rule, because the receipt is what disable consumes.
#[test]
fn a_receipt_sharing_one_slot_between_two_displacements_is_rejected() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);
    forge_two_displacement_slots(&world, "memory");
    let logged_before = argv_lines(&world.argv_log()).len();

    let err = manager
        .status(Some(COMPONENT))
        .expect_err("status must refuse a receipt that cannot be restored safely");
    assert!(
        matches!(&err, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("exclusive slot 'memory'")),
        "unexpected status error: {err:?}"
    );

    let err = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("disable --dry-run must reject the same receipt");
    assert!(
        matches!(&err, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("exclusive slot 'memory'")),
        "unexpected dry-run error: {err:?}"
    );
    let err = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("disable must reject the same receipt");
    assert!(
        matches!(&err, AdapterError::BundleInvalid { reason, .. }
            if reason.contains("exclusive slot 'memory'")),
        "unexpected disable error: {err:?}"
    );

    assert_eq!(
        argv_lines(&world.argv_log()).len(),
        logged_before,
        "a rejected receipt must not produce a single host call"
    );
    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            displaced_marker_exists(&world, id),
            "neither plugin may be left orphaned by a rejected receipt: {id}"
        );
    }
    assert!(world.has_claim(), "the receipt must be kept for repair");
}

/// `plugins.slots.` is not a key. An entry that names it would silently degrade
/// to an unguarded restore, so a contract spelling `slot = ""` is rejected rather
/// than read as "no slot" — omitting `slot` is how a contract asks for that.
#[test]
fn enable_rejects_a_displacement_declaring_an_empty_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacements(&[("memory-core", Some(""))]),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    for dry_run in [true, false] {
        let err = manager
            .enable(COMPONENT, Some(FRAMEWORK), dry_run)
            .expect_err("an empty slot suffix is a mistake, not a request");
        assert!(
            matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
                if reason.contains("empty slot")),
            "dry_run={dry_run}: unexpected error: {err:?}"
        );
    }
    assert!(argv_lines(&world.argv_log()).is_empty());
    assert!(!world.has_claim());
    assert!(!displaced_marker_exists(&world, "memory-core"));
}

/// The same empty suffix, hand-edited into a receipt, must be rejected there too:
/// `validate_config_key` only rejects an empty *whole* key, and `plugins.slots.`
/// is not empty.
#[test]
fn a_receipt_with_an_empty_slot_suffix_is_rejected() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    forge_displaced_references(&world, |entries| {
        entries[0].slot = Some(String::new());
    });
    let logged_before = argv_lines(&world.argv_log()).len();

    for err in [
        manager
            .status(Some(COMPONENT))
            .expect_err("status must refuse an unguarded slot reference"),
        manager
            .disable(COMPONENT, Some(FRAMEWORK), true)
            .expect_err("disable --dry-run must refuse it too"),
        manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect_err("disable must refuse it too"),
    ] {
        assert!(
            matches!(&err, AdapterError::BundleInvalid { reason, .. }
                if reason.contains("empty slot")),
            "unexpected error: {err:?}"
        );
    }
    assert_eq!(argv_lines(&world.argv_log()).len(), logged_before);
    assert!(displaced_marker_exists(&world, "memory-core"));
    assert!(world.has_claim());
}

/// A verdict the operator cannot act on by copying it is worse than one that only
/// names the problem: `openclaw plugins disable a, b` passes one malformed
/// argument, and `plugins.entries.a, b.enabled` is a key that has never existed.
#[test]
fn status_gives_one_remediation_command_per_displaced_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);
    for id in ["memory-core", "memory-lancedb"] {
        set_config_answer(&world, &format!("plugins.entries.{id}.enabled"), "true");
    }

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
    let reason = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition")
        .reason
        .clone()
        .expect("a False verdict must explain itself");

    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            reason.contains(&format!("openclaw plugins disable {id}`")),
            "each plugin needs its own runnable command: {reason}"
        );
    }
    assert!(
        !reason.contains("disable memory-core, memory-lancedb"),
        "a joined id list is not an argument the CLI accepts: {reason}"
    );
}

/// Same for the unverifiable branch: it must name the real per-plugin keys, not
/// interpolate a joined id list into one key that does not exist.
#[test]
fn status_names_one_real_config_key_per_unverified_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_two_displacements(&guard);
    guard.set("FAKE_OC_PROBE_FAIL", "config_get");

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::Unknown);
    let reason = condition.reason.clone().expect("reason");
    for id in ["memory-core", "memory-lancedb"] {
        assert!(
            reason.contains(&format!("plugins.entries.{id}.enabled")),
            "each plugin's real key must be named: {reason}"
        );
    }
    assert!(
        !reason.contains("plugins.entries.memory-core, memory-lancedb.enabled"),
        "that key has never existed: {reason}"
    );
}

/// `plugins.entries.<id>.enabled = false` is not the only way an operator turns
/// a plugin off. An explicit `plugins.deny` entry keeps it off too, and the host
/// will refuse the `plugins enable` a later restore would issue — so claiming the
/// transition promises a cleanup that can never succeed.
#[test]
fn enable_does_not_claim_a_displacement_an_explicit_deny_list_blocks() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(&world, "plugins.deny", "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable over a policy-disabled plugin");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "a policy already keeps it off, so the transition is not this adapter's: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "a policy-disabled plugin must not be claimed: {:?}",
        payload.displaced_plugins
    );

    // And the payoff: disable has nothing it cannot do, so it converges.
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "disable must not fight the operator's policy: {lines:?}"
    );
    assert!(outcome.report.cleanup_complete);
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
}

/// A restrictive `plugins.allow` that omits the plugin is *not* the same verdict,
/// though it used to be.
///
/// It was folded in with `plugins.deny` on the reasoning that the operator had
/// turned the plugin off by policy and the host would refuse the `plugins enable` a
/// later restore would issue. Neither half holds:
///
/// - It does not keep the plugin from loading. OpenClaw enables whichever plugin
///   owns `plugins.slots.memory` *before* it reaches the allowlist check (v2026.4.14
///   `src/plugins/config-state.ts:310-345`, v2026.9.2
///   `src/plugins/config-activation-shared.ts:204-218`), and since the v2026.7.2
///   beta line the memory dreaming sidecar is a second load path that checks the
///   global switch, the denylist and an explicit `enabled = false` and never the
///   allowlist (`resolveAuthorizedDreamingSidecar`, v2026.9.2
///   `src/plugins/loader-shared.ts:90-137`), after which the loader hard-codes it to
///   `enabled: true, activated: true` instead of asking
///   (`src/plugins/loader-runtime-candidate.ts:108-119,145-146`). With
///   `plugins.allow = ["memory-anolisa"]`, `plugins.slots.memory = "memory-anolisa"`,
///   dreaming on and `memory-core` not explicitly off, the bundled plugin loads and
///   registers `memory_get` / `memory_search`
///   (`extensions/memory-core/index.ts:273-279`) — the exact collision this hand-off
///   exists to release, left standing by the veto, with no displacement in the
///   receipt and so no `status` condition that could ever report it.
/// - It does not make the restore impossible either, which was the only reason the
///   veto was there. Whether `plugins enable` honors the allowlist changed between
///   versions (v2026.4.14 appends the id and reports success,
///   `src/plugins/enable.ts:12-24`; v2026.9.2 refuses and exits non-zero,
///   `src/plugins/enable.ts:38-40` with `src/cli/plugins-cli.runtime.ts:218-223`),
///   and it is a question about the host *at restore time* — which `restore_decision`
///   asks again then, against an allowlist this driver's own uninstall has already
///   rewritten. It stays a veto there; see
///   `disable_reports_the_allowlist_that_survives_its_own_uninstall`.
#[test]
fn enable_claims_a_displacement_a_restrictive_allow_list_does_not_exclude() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    // A non-vacant allowlist naming only this adapter's own plugin.
    set_config_answer(&world, "plugins.allow", COMPONENT);
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable under a restrictive allowlist");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "the allowlist does not keep the plugin from loading, so the hand-off must \
         still happen: {lines:?}"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and the ownership must be recorded, or `status` has nothing to check and a \
         later disable has nothing to hand back"
    );
}

/// The same configuration through the whole lifecycle, which is where the allowlist
/// question actually resolves.
///
/// `plugins.allow` still vetoes a *restore* on hosts that gate `plugins enable` on
/// it — but this driver's own `plugins uninstall` removes its plugin from that key
/// first, and OpenClaw drops the key entirely once nothing is left in it (v2026.4.14
/// `src/plugins/uninstall.ts:126-133`). For the reported configuration, where the
/// allowlist names only this adapter's plugin, that means the restriction is gone by
/// the time the restore is decided and the bundled backend really comes back.
#[test]
fn disable_hands_the_plugin_back_once_the_uninstall_drops_the_allowlist_entry() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(&world, "plugins.allow", &pretty_json_list(&[COMPONENT]));
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable under a restrictive allowlist");

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the host dropped this adapter's own allowlist entry when it uninstalled the \
         plugin, so nothing gates the restore any more: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "and the bundled backend must really be back on, not merely asked about"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.allow")),
        "no allowlist excuse, since the key restricts nothing by then: {:?}",
        outcome.report.messages
    );
    assert!(outcome.report.cleanup_complete);
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
}

/// The slot the hand-off *attribution* reasons about must be the one this `disable`
/// found, not the one its own `plugins uninstall` left behind.
///
/// OpenClaw's `removePluginFromConfig` resets a memory slot the uninstalled plugin
/// owned back to that slot's default — `memory-core`, from `DEFAULT_SLOT_BY_KEY`
/// (v2026.4.14 `src/plugins/uninstall.ts:147-155` with `src/plugins/slots.ts:17-20`)
/// — and this adapter's own plugin owns `plugins.slots.memory` for the whole life of
/// the receipt, so a post-uninstall read returns a value this cleanup just wrote.
/// The attribution needs the earlier one: it asks whether the slot names *this
/// adapter's* plugin, and after the reset it names the displaced one instead, which
/// reads as somebody else's selection. The consequence is not a wording problem —
/// the entry is skipped as never applied, the only receipt is dropped,
/// `cleanup_complete` is reported, and the bundled backend stays off with nothing
/// recording why.
///
/// `failed_verify_with_an_unreadable_slot_still_hands_the_plugin_back` reaches the
/// same state through an unreadable probe and covers the lifecycle, since the fake
/// now models the reset. This pins the ordering, which is the substance of the fix:
/// a lifecycle assertion can also be satisfied by a driver that happens to read the
/// slot at a lucky moment, and this one cannot. The slot *veto* keeps its live read
/// on purpose — it asks who owns the slot when the `plugins enable` would run, which
/// the uninstall really does change — so the capture is asserted only for the
/// unapplied entry that needs it.
#[test]
fn disable_attributes_an_unmarked_handoff_from_the_pre_uninstall_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Both sides declare `kind = "memory"`, which is what makes the host write the
    // slot this test is about at all.
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with a same-kind displacement");
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "fixture: the host's own slot selection wrote the slot this adapter's plugin \
         now owns"
    );
    assert!(
        persisted_displacement(&world).applied,
        "fixture: a clean enable marks the hand-off"
    );

    // Lose the mark, the way a process that died before persisting it would. The
    // host is untouched: the bundled plugin is off and the slot names this adapter's
    // own plugin, which is exactly the state the restore has to attribute.
    forge_displaced_references(&world, |entries| entries[0].applied = false);
    assert!(
        !persisted_displacement(&world).applied,
        "fixture: the mark is what was lost, not the hand-off"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);

    let first_read = appended
        .iter()
        .position(|line| line == "config get plugins.slots.memory")
        .expect("the attribution must read the slot it is about to reason from");
    let uninstall_at = appended
        .iter()
        .position(|line| line.starts_with("plugins uninstall"))
        .expect("disable uninstalls this adapter's own plugin");
    assert!(
        first_read < uninstall_at,
        "the attribution must read the slot before `plugins uninstall` resets it to \
         the slot default, or it reasons about a value its own cleanup wrote: \
         {appended:?}"
    );
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "and the hand-off it attributes must actually be handed back: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("memory-core"),
        "the host really did rewrite it — this is the reading a post-uninstall \
         attribution would have reasoned from"
    );
    assert!(
        outcome.report.cleanup_complete,
        "the cleanup must converge: {:?}",
        outcome.report.messages
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the host shows this adapter performing: {:?}",
        outcome.report.messages
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// A *vacant* allowlist is not a restriction. Reading "unset" as "nothing is
/// allowed" would switch the whole hand-off off on every host that merely never set
/// the key — which is the bug this feature exists to fix.
///
/// Since a restrictive allowlist stopped vetoing the hand-off, the enable-side half
/// below cannot fail on its own any more; the vacancy rule it pins is now read where
/// an allowlist still decides something, which is the restore. So both halves are
/// asserted: a key that names nothing gates the `plugins enable` no more than it
/// gated the `plugins disable`, and a host that merely never set it is not left with
/// a disabled bundled backend and an instruction to edit a key it does not have.
#[test]
fn enable_still_claims_a_displacement_when_the_allow_list_is_unset() {
    for answer in ["", "[]", "null", "none"] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        write_openclaw_manifest(
            &world.layout,
            &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
        );
        seed_bundled_plugin(&world, "memory-core");
        set_config_answer(&world, "plugins.allow", answer);
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
        let manager = world.manager();

        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("enable");

        let lines = argv_lines(&world.argv_log());
        assert!(
            argv_contains(&lines, "plugins disable memory-core"),
            "allow='{answer}' is vacant, not restrictive, so the hand-off must still \
             happen: {lines:?}"
        );

        let logged_before = lines.len();
        let outcome = manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("disable");
        let appended = argv_appended(&world, logged_before);
        assert!(
            argv_contains(&appended, "plugins enable memory-core"),
            "allow='{answer}' names nothing, so it gates the restore no more than it \
             gates the hand-off: {appended:?}"
        );
        assert!(
            !outcome
                .report
                .messages
                .iter()
                .any(|m| m.contains("plugins.allow")),
            "and disable must not send the operator to edit a key that was never \
             set: {:?}",
            outcome.report.messages
        );
        assert!(outcome.report.cleanup_complete);
        assert!(outcome.claim_removed);
    }
}

/// A contract naming a plugin this host does not have is a broken contract, and
/// has to fail before anything is installed. Discovering it later — when
/// `plugins disable` errors, after this adapter's own plugin is installed and
/// verified — leaves a receipt whose restore cannot converge either.
#[test]
fn enable_fails_before_installing_when_the_displaced_id_is_not_on_the_host() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Deliberately NOT seeded: this OpenClaw has no such plugin.
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    for dry_run in [true, false] {
        let err = manager
            .enable(COMPONENT, Some(FRAMEWORK), dry_run)
            .expect_err("a contract cannot displace a plugin the host does not have");
        assert!(
            matches!(&err, AdapterError::InvalidAdapterInput { reason, .. }
                if reason.contains("memory-core") && reason.contains("plugins list")),
            "dry_run={dry_run}: unexpected error: {err:?}"
        );
    }
    let lines = argv_lines(&world.argv_log());
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("plugins install ") && !l.contains("--help")),
        "the adapter's own plugin must not be installed for a broken contract: {lines:?}"
    );
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "and nothing may be disabled: {lines:?}"
    );
    assert!(!world.has_claim());
    assert!(!world.registry_marker_exists());
}

/// A host that cannot answer `plugins list` is not a host that says "absent":
/// without positive evidence the claim still happens, per the asymmetry the
/// driver documents.
#[test]
fn enable_still_claims_when_the_inventory_probe_cannot_run() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Not seeded, and the inventory probe fails outright — so "absent" is never
    // positively established.
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_PROBE_FAIL", "list");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("an unanswerable inventory is not evidence of absence");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "skipping the claim here would strand the host with nothing behind the slot: {lines:?}"
    );
}

/// Policy can be added *after* enable, which is exactly when a promised restore
/// becomes impossible. Treating that as a failure would strand the receipt
/// forever — with this adapter's own plugin already uninstalled — over a cleanup
/// that has nothing left to do.
#[test]
fn disable_treats_a_policy_blocked_restore_as_released() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(displaced_marker_exists(&world, "memory-core"));

    set_config_answer(&world, "plugins.deny", "memory-core");
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable must converge even though the restore is now impossible");

    assert!(
        outcome.report.cleanup_complete,
        "a policy-blocked restore is released, not failed: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim(), "no receipt may be stranded");
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.deny") && m.contains("memory-core")),
        "the operator must be told why, and how to undo it: {:?}",
        outcome.report.messages
    );
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "the host would refuse it anyway: {lines:?}"
    );
}

/// Same for a plugin the host no longer has at all: there is nothing to hand
/// back, and retrying will never create one.
#[test]
fn disable_treats_a_vanished_displaced_plugin_as_released() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("the host drops the bundled plugin");

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable must converge when the displaced plugin is gone");

    assert!(
        outcome.report.cleanup_complete,
        "an absent plugin is released, not failed: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("memory-core") && m.contains("inventory")),
        "the report must say what it found: {:?}",
        outcome.report.messages
    );
}

/// `plugins disable` only writes config; when the running gateway picks that up
/// depends on the host's plugin reload mode. So "disabled in config" proves the
/// hand-off was *recorded* and nothing about whether the running gateway is still
/// serving the bundled plugin — which is exactly the window a `Healthy` verdict
/// would misreport, because the operator's own tool could still be answered by
/// `memory-core` throughout it.
///
/// This driver has no channel to the live gateway. `plugins inspect --runtime`
/// is not one: it spawns a fresh CLI process that reads the same config and
/// inspects runtime in that process, so it echoes the config back. Reading
/// gateway liveness from it would produce a green test and a wrong answer on a
/// real host, so the verdict stays `Unknown`.
///
/// `Unknown` here means *unobservable*, not *pending a restart*. A gateway
/// restart changes what OpenClaw serves but not what ANOLISA can see, so the
/// reason must not present one as the thing that settles the verdict — that
/// would send the operator around a loop with no exit, and the assertions below
/// are what keep this doc honest about it. The only check that does settle it is
/// a real tool call, which travels through the running gateway.
#[test]
fn status_does_not_report_healthy_on_a_recorded_but_unverified_handoff() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    // Config really does say disabled: enable ran `plugins disable`.
    assert!(displaced_marker_exists(&world, "memory-core"));

    let status = manager.status(Some(COMPONENT)).expect("status");
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Healthy,
        "a hand-off whose effect on the running gateway is unobservable is not Healthy"
    );
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Unknown);
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::Unknown);
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core"),
        "the verdict must name the plugin it is about: {reason}"
    );
    assert!(
        reason.contains("recorded"),
        "and must say what *is* known, not only what is not: {reason}"
    );
    assert!(
        reason.contains("openclaw gateway restart"),
        "and must name the restart for hosts that need one: {reason}"
    );
    // But it must not assert the restart is *the* mechanism. Whether one is needed
    // depends on the host's plugin reload mode, which this driver cannot read, and
    // a host that hot-reloads `plugins.entries.*` would be told to take an
    // unnecessary gateway interruption on every single hand-off.
    assert!(
        reason.contains("reload mode"),
        "the restart must be conditional on the host's reload mode: {reason}"
    );
    for sole_mechanism in [
        "that is what applies it",
        "that is what makes the hand-off take effect",
    ] {
        assert!(
            !reason.contains(sole_mechanism),
            "a restart must not be presented as the only way the change takes effect              ('{sole_mechanism}'): {reason}"
        );
    }
    // The restart is not a way out of `unknown`: it changes what OpenClaw serves,
    // not what ANOLISA can observe, so a message implying "restart and re-check"
    // would send the operator around a loop with no exit. Say so, and point at
    // something that does settle it.
    assert!(
        reason.contains("does not change this verdict"),
        "the reason must not promise a convergence the code cannot produce: {reason}"
    );
    // The only check that settles this travels through the running gateway: a real
    // tool call. `plugins list` and `plugins inspect` read the persisted registry
    // and config — the same state this verdict came from — so before a restart they
    // show the bundled plugin disabled while the old gateway may still be serving
    // its tools. Offering either as confirmation would be worse than offering
    // nothing, because it moves the operator from "unknown" to "believes it is
    // fixed"; the warning about them lives in the user guide, not in this string.
    assert!(
        reason.contains("tool call"),
        "the reason must point at a check that goes through the running gateway: {reason}"
    );
    for cold in ["plugins list", "plugins inspect"] {
        assert!(
            !reason.contains(cold),
            "a cold probe must not be offered as confirmation ({cold}): {reason}"
        );
    }
    // `displaces` is a generic contract, so the advice has to be generic too. This
    // fixture's component is `tokenless`, not agent-memory: a reason naming
    // `memory_get` would send this adapter's operator after a tool it does not
    // register, and could not be used to confirm anything.
    assert!(
        !reason.contains("memory_get"),
        "a generic displacement verdict must not hardcode one component's tool: {reason}"
    );
    assert!(
        reason.contains("This component's own documentation"),
        "and must say where the concrete tool name lives: {reason}"
    );
}

/// A receipt persisted in front of `apply_enable` can outlive the process that
/// wrote it, and `status` must not read it as a completed hand-off.
///
/// The Manager saves the receipt — `status: Enabled`, displacement `applied: false`
/// — before `apply_enable` runs, and only rewrites it to `CleanupFailed` if
/// `apply_enable` *returns* an error. A hard exit in between (SIGKILL, OOM, power
/// loss) leaves the first state on disk: the adapter's own plugin installed and
/// verified loaded, and a displacement whose `plugins disable` never ran. Filing
/// that entry under "never applied, so nothing to verify" made the condition
/// `True`, `plugin_registered` was already `True`, and `summarize` reported
/// **Healthy** while the bundled plugin was still enabled and still winning the
/// framework's first-wins registry for every tool name this adapter registers —
/// the exact false all-clear `DisplacedPluginsReleased` exists to prevent, and the
/// driver's own reason text for that bucket already said "this adapter never
/// disabled it".
#[test]
fn status_does_not_report_healthy_on_a_handoff_that_never_ran() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_failed_enable_with_unapplied_displacement(&guard);
    guard.unset("FAKE_OC_RUNTIME_STATUS");

    // Model the hard exit: the Manager never reached the branch that marks the
    // receipt `CleanupFailed`, so what is on disk is `Enabled` with an unapplied
    // displacement. `summarize` only early-returns on `CleanupFailed`, so this is
    // the state that can reach a `Healthy` verdict.
    let mut state = world.load_state();
    let mut claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("the write-ahead receipt is on disk");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        !payload.displaced_plugins[0].applied,
        "fixture must leave the hand-off unperformed: {:?}",
        payload.displaced_plugins
    );
    claim.status = ClaimStatus::Enabled;
    state.upsert_adapter_claim(claim);
    state
        .save(&world.layout.state_dir.join("installed.toml"))
        .expect("persist the crash-state receipt");

    // The bundled plugin was never disabled, so it is still on and still holds the
    // names.
    assert!(!displaced_marker_exists(&world, "memory-core"));

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = find_displacement_condition(&status);
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "an unperformed hand-off is not a released displacement: {:?}",
        condition.reason
    );
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "and it must not summarize to Healthy while the bundled plugin serves every \
         tool name this adapter registers"
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("never disabled it"),
        "the verdict must name the plugin and say the hand-off never ran, so the \
         operator knows re-running the enable is the fix: {reason}"
    );

    // The round-18 rule still holds for an unapplied entry: a plugin that cannot
    // load is not holding anything, so it must not be reported as a collision.
    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("a framework upgrade drops the bundled plugin");
    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = find_displacement_condition(&status);
    assert_ne!(
        condition.status,
        ConditionStatus::False,
        "a plugin the host no longer has cannot hold the tool names, applied or \
         not: {:?}",
        condition.reason
    );
    assert_ne!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "and must not degrade the adapter over a hand-off nothing is contesting"
    );
}

/// The `Unknown` above must not be a shrug that hides a real problem: when config
/// positively says the bundled plugin is back on, the verdict is decisive.
#[test]
fn status_is_decisive_when_config_says_the_displaced_plugin_is_back_on() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::False);
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
}

/// An unreadable enablement key is a third, distinct answer: not "recorded", not
/// "back on", just "this host would not say". It keeps its own reason so the
/// operator is pointed at the key rather than at a restart that will not help.
#[test]
fn status_separates_an_unreadable_probe_from_a_recorded_handoff() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_displacement(&guard);
    guard.set("FAKE_OC_PROBE_FAIL", "config_get");

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::Unknown);
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("plugins.entries.memory-core.enabled"),
        "an unreadable probe must name the key it could not read: {reason}"
    );
    assert!(
        !reason.contains("gateway restart"),
        "and must not send the operator after a restart that changes nothing: {reason}"
    );
}

/// How OpenClaw actually renders an array-valued config key: pretty JSON, one
/// quoted element per line. Every policy test below feeds this shape rather than
/// a bare string, because a bare string is the one rendering the naive readers
/// happened to handle — a whole-token text search misses `"memory-core",` on the
/// quotes and comma, and a last-line reduction reads the closing `]` as vacant.
fn pretty_json_list(items: &[&str]) -> String {
    let body = items
        .iter()
        .map(|item| format!("  \"{item}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("[\n{body}\n]")
}

/// The deny path, end to end, against the host's real output shape.
#[test]
fn enable_and_disable_honor_a_pretty_json_deny_list() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.deny",
        &pretty_json_list(&["memory-core", "some-other-plugin"]),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable over a policy-disabled plugin");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "a multi-line JSON denylist must be read as naming memory-core: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "nothing may be claimed that policy already keeps off: {:?}",
        payload.displaced_plugins
    );

    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "disable must not attempt a restore the host would refuse: {lines:?}"
    );
    assert!(outcome.report.cleanup_complete);
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
}

/// The allow path, end to end, against the same shape. This is the one the
/// last-line reduction got worst: the closing `]` reads as an empty answer, so a
/// genuinely restrictive allowlist looked like no restriction at all.
///
/// The parse still matters, but not where it used to. A restrictive allowlist no
/// longer vetoes the *hand-off* — it does not keep the plugin from loading, and
/// skipping the displacement left the collision this feature exists to release (see
/// `enable_claims_a_displacement_a_restrictive_allow_list_does_not_exclude`). It
/// vetoes the *restore*, on the hosts that gate `plugins enable` on it, and only when
/// it survives the uninstall that drops this adapter's own entry from it — which is
/// what an allowlist naming anything else does. So the shape is pinned from the side
/// that still reads it.
#[test]
fn disable_reports_the_allowlist_that_survives_its_own_uninstall() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.allow",
        &pretty_json_list(&[COMPONENT, "some-other-plugin"]),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable under a restrictive allowlist");
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "the hand-off happens regardless of the allowlist: {lines:?}"
    );

    let logged_before = lines.len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "a multi-line JSON allowlist that still excludes the plugin gates the \
         restore, and this driver does not edit the operator's policy to force it: \
         {appended:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.allow") && m.contains("memory-core")),
        "and the report must name the key and the plugin, with the command that \
         undoes it: {:?}",
        outcome.report.messages
    );
    assert!(
        outcome.report.cleanup_complete,
        "a veto is a release, not a failure, so the receipt must not be stranded: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "the plugin stays exactly as this cleanup left it, with the reason reported"
    );
}

/// Parsing must not over-correct: an allowlist that *does* name the plugin is not a
/// restriction against it.
///
/// The hand-off no longer turns on that — a restrictive allowlist does not keep the
/// plugin from loading, so it is not a reason to skip the displacement — but the
/// restore still does, on the hosts that gate `plugins enable` on the key. An
/// allowlist misread as omitting a plugin it names would leave that plugin disabled
/// for good and tell the operator to edit a key that already permits it.
#[test]
fn enable_still_claims_when_a_pretty_json_allow_list_names_the_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.allow",
        &pretty_json_list(&[COMPONENT, "memory-core"]),
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "an allowlist naming the plugin permits the hand-off: {lines:?}"
    );

    let logged_before = lines.len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "and it permits the restore, which is the reading the parse has to get \
         right: {appended:?}"
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the plugin must really be back on"
    );
}

/// Hosts prepend diagnostics and echo the key; the JSON has to be found past both,
/// on the deny and the allow side alike.
///
/// The two sides now prove it from different operations, because they read the key
/// for different questions: a denylist vetoes the hand-off at enable, while an
/// allowlist does not (it cannot keep the plugin from loading) and is only read again
/// at restore. Both still have to *parse* the shape — an allowlist misread as vacant
/// would restore a plugin the operator gated, which is the same damage in the other
/// direction.
#[test]
fn policy_lists_behind_a_key_echo_are_still_parsed() {
    for key in ["plugins.deny", "plugins.allow"] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        write_openclaw_manifest(
            &world.layout,
            &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
        );
        seed_bundled_plugin(&world, "memory-core");
        // deny: names the plugin. allow: names this adapter's own plus one more, so
        // it survives the uninstall that drops the adapter's own entry and is still
        // restrictive when the restore reads it.
        let listed = if key == "plugins.deny" {
            vec!["memory-core"]
        } else {
            vec![COMPONENT, "some-other-plugin"]
        };
        set_config_answer(
            &world,
            key,
            &format!(
                "reading plugin policy...\n{key} = {}",
                pretty_json_list(&listed)
            ),
        );
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
        let manager = world.manager();

        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("enable");
        let lines = argv_lines(&world.argv_log());
        if key == "plugins.deny" {
            assert!(
                !argv_contains(&lines, "plugins disable memory-core"),
                "{key} behind a preamble and a key echo must still be honored: {lines:?}"
            );
            continue;
        }
        assert!(
            argv_contains(&lines, "plugins disable memory-core"),
            "{key} is not a reason to skip the hand-off: {lines:?}"
        );
        let logged_before = lines.len();
        let outcome = manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect("disable");
        let appended = argv_appended(&world, logged_before);
        assert!(
            !argv_contains(&appended, "plugins enable memory-core"),
            "{key} behind a preamble and a key echo must still be read as excluding \
             the plugin when the restore is decided: {appended:?}"
        );
        assert!(
            outcome
                .report
                .messages
                .iter()
                .any(|m| m.contains("plugins.allow")),
            "and the report must name the key it parsed: {:?}",
            outcome.report.messages
        );
    }
}

/// A JSON `null` or `[]` is a host saying "nothing here", not a restriction —
/// the same rule the bare-string cases pin, on the JSON rendering.
#[test]
fn enable_still_claims_when_a_json_policy_list_is_empty_or_null() {
    for answer in ["[]", "[\n]", "null"] {
        for key in ["plugins.deny", "plugins.allow"] {
            let guard = OpenClawEnvGuard::acquire();
            let world = stage();
            write_openclaw_manifest(
                &world.layout,
                &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
            );
            seed_bundled_plugin(&world, "memory-core");
            set_config_answer(&world, key, answer);
            world.apply_env(&guard, None);
            guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
            let manager = world.manager();

            manager
                .enable(COMPONENT, Some(FRAMEWORK), false)
                .expect("enable");
            let lines = argv_lines(&world.argv_log());
            assert!(
                argv_contains(&lines, "plugins disable memory-core"),
                "{key} = {answer} names nothing, so it restricts nothing: {lines:?}"
            );
        }
    }
}

/// The reported failure, through the whole enable→disable chain rather than
/// against the parser alone — because the damage is not a wrong `Vec<String>`, it
/// is what `policy_blocking_plugin` decides from it.
///
/// A diagnostics preamble plus a key echo whose value is null gives the JSON
/// reader no `[` or `{` to anchor on, so the fallback runs. Reading every line as
/// a value list turns the preamble's own words into allowlist entries; that reads
/// as a restrictive allowlist omitting `memory-core`, so `displacement_probe`
/// answers `AlreadyOff`, enable skips the hand-off entirely — both plugins keep
/// fighting over the tool names and the receipt records no ownership for `status`
/// to check — and disable prints instructions to edit a key whose value is null
/// while leaving the plugin disabled for good.
#[test]
fn enable_claims_the_displacement_a_preamble_plus_null_allowlist_does_not_forbid() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.allow",
        "reading policy...\nplugins.allow = null",
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "a null allowlist behind a preamble restricts nothing, so the hand-off must \
         still happen: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "ownership must be recorded so status has something to check: {:?}",
        payload.displaced_plugins
    );

    // And disable hands it back instead of inventing a policy excuse.
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins enable memory-core"),
        "the plugin must be restored, not left disabled behind a phantom allowlist: {lines:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.allow")),
        "disable must not tell the operator to edit a key whose value is null: {:?}",
        outcome.report.messages
    );
    assert!(outcome.report.cleanup_complete);
    assert!(outcome.claim_removed);
    assert!(!displaced_marker_exists(&world, "memory-core"));
}

/// The same preamble shape on the deny side must not manufacture a denial.
#[test]
fn enable_claims_the_displacement_a_preamble_plus_null_denylist_does_not_forbid() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.deny",
        "reading policy...\nplugins.deny = null",
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    assert!(
        argv_contains(
            &argv_lines(&world.argv_log()),
            "plugins disable memory-core"
        ),
        "a null denylist behind a preamble forbids nothing"
    );
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(!world.has_claim());
}

/// And a genuine denial behind that same preamble must still be honored — the fix
/// is about which line carries the value, not about ignoring the key.
#[test]
fn enable_honors_a_real_deny_entry_behind_a_diagnostic_preamble() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(
        &world,
        "plugins.deny",
        "reading policy...\nplugins.deny = memory-core",
    );
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    assert!(
        !argv_contains(
            &argv_lines(&world.argv_log()),
            "plugins disable memory-core"
        ),
        "a real denial must still be read past the preamble"
    );
}

/// Run `disable --dry-run` and then the real disable against the same world and
/// assert the preview's prediction about handing `memory-core` back matches what
/// the operation actually did.
///
/// This is the pairing the previews kept failing: the planner used to compose its
/// own wording, so it described two of the four vetoes the real restore applies
/// and called a slotless restore unconditional when the inventory and policy
/// vetoes do not look at the slot at all. Asserting the pairing rather than the
/// wording is what stops that drifting again — and because every veto leaves
/// `cleanup_complete` alone and the receipt is removed either way, a divergence
/// here leaves the operator nothing to notice it with afterwards.
fn assert_disable_preview_matches_real(
    world: &World,
    manager: &AdapterManager,
    label: &str,
    expect_restore: bool,
) {
    let preview = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("disable plan");
    assert!(preview.dry_run);
    let predicted = preview
        .report
        .messages
        .iter()
        .any(|m| m.contains("would re-enable openclaw plugin 'memory-core'"));
    assert_eq!(
        predicted, expect_restore,
        "{label}: the preview predicted the wrong outcome: {:?}",
        preview.report.messages
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    let real = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(world, logged_before);
    let did_restore = argv_contains(&appended, "plugins enable memory-core");
    assert_eq!(
        predicted, did_restore,
        "{label}: preview promised a restore={predicted} but the real disable did \
         restore={did_restore}: {appended:?}"
    );
    assert!(
        real.report.cleanup_complete,
        "{label}: stepping aside is a completed cleanup, not a failure: {:?}",
        real.report.messages
    );
}

/// A plain disable, nothing in the way: the preview promises the restore and the
/// restore happens.
#[test]
fn disable_preview_agrees_with_the_real_restore() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert_disable_preview_matches_real(&world, &manager, "plain", true);
    assert!(!displaced_marker_exists(&world, "memory-core"));
}

/// A slotless declaration is not "unconditional" — the inventory and policy
/// vetoes never consult the slot. With nothing in the way it restores, and the
/// preview says so.
#[test]
fn disable_preview_agrees_for_a_slotless_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", None),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable");
    assert_disable_preview_matches_real(&world, &manager, "slotless", true);
}

/// The reviewer's first repro: enable, then put the plugin in `plugins.deny`. The
/// preview used to promise a re-enable; the real disable then declined to issue
/// one and removed the receipt anyway.
#[test]
fn disable_preview_agrees_when_policy_blocks_the_restore() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    set_config_answer(&world, "plugins.deny", "memory-core");
    assert_disable_preview_matches_real(&world, &manager, "policy-blocked", false);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the host would refuse the restore, so the plugin stays as it was"
    );
    assert!(!world.has_claim(), "the receipt is still removed");
}

/// The reviewer's second repro: the host no longer has the plugin at all.
#[test]
fn disable_preview_agrees_when_the_displaced_plugin_has_vanished() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    std::fs::remove_file(world.openclaw_home.join("registry/memory-core"))
        .expect("the host drops the bundled plugin");
    assert_disable_preview_matches_real(&world, &manager, "vanished", false);
    assert!(!world.has_claim());
}

/// The two slot vetoes, paired the same way.
#[test]
fn disable_preview_agrees_when_the_slot_blocks_the_restore() {
    for (label, answer) in [
        ("third-owner", "memory-lancedb"),
        ("explicitly-closed", "none"),
    ] {
        let guard = OpenClawEnvGuard::acquire();
        let (world, manager) = stage_enabled_with_displacement(&guard);
        set_config_answer(&world, "plugins.slots.memory", answer);
        assert_disable_preview_matches_real(&world, &manager, label, false);
        assert!(
            displaced_marker_exists(&world, "memory-core"),
            "{label}: the operator's choice must survive"
        );
    }
}

/// The allowlist a disable preview reads must be the one the operation's own
/// uninstall leaves behind, not the one the host holds while the preview runs.
///
/// `removePluginFromConfig` drops the uninstalled plugin's id from `plugins.allow`
/// and removes the key entirely once nothing is left in it (v2026.4.14
/// `src/plugins/uninstall.ts:126-133`), and a real `disable` runs that uninstall
/// before it decides anything about restoring. A preview runs before the operation
/// it describes, so the key it read still named this adapter's own plugin — and for
/// the single-entry allowlist this driver's own enable leaves behind, that is the
/// only entry the key has. The plan therefore reported a restriction the operation
/// was about to delete, promised the bundled plugin would stay disabled, and the
/// disable re-enabled it anyway: a preview saying the opposite of the operation, on
/// the configuration this component ships with. Because every veto leaves
/// `cleanup_complete` alone and the receipt is removed either way, nothing
/// afterwards would have told the operator the plan they were given described a
/// different run.
///
/// The multi-entry control is the other half of the pairing and the reason this is
/// a projection rather than a blanket "ignore the allowlist in previews": an
/// allowlist naming anything besides this adapter's own plugin survives the
/// uninstall, still excludes the displaced one, and must be previewed as blocking
/// the restore — which is what `disable_reports_the_allowlist_that_survives_its_own_uninstall`
/// pins from the operation side.
#[test]
fn disable_preview_accounts_for_the_allowlist_its_own_uninstall_drops() {
    for (label, allow, expect_restore) in [
        ("dropped-by-uninstall", vec![COMPONENT], true),
        (
            "survives-uninstall",
            vec![COMPONENT, "some-other-plugin"],
            false,
        ),
    ] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        write_openclaw_manifest(
            &world.layout,
            &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
        );
        seed_bundled_plugin(&world, "memory-core");
        set_config_answer(&world, "plugins.allow", &pretty_json_list(&allow));
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
        let manager = world.manager();
        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("enable under a restrictive allowlist");

        assert_disable_preview_matches_real(&world, &manager, label, expect_restore);
        assert_eq!(
            config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
            Some(if expect_restore { "true" } else { "false" }),
            "{label}: the host must end up in the state the preview announced"
        );
    }
}

/// The re-enable side of the same defect: a displacement the new contract dropped
/// is restored by `cleanup_replaced_claim`, and the plan has to say so — with the
/// same verdict the real path reaches.
#[test]
fn reenable_preview_agrees_with_the_real_dropped_restore() {
    for (label, slot) in [("slotful", Some("memory")), ("slotless", None)] {
        let guard = OpenClawEnvGuard::acquire();
        let world = stage();
        write_openclaw_manifest(
            &world.layout,
            &plugin_adapter_block_with_displacement("memory-core", slot),
        );
        seed_bundled_plugin(&world, "memory-core");
        world.apply_env(&guard, None);
        guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
        let manager = world.manager();
        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("enable");
        redeclare_displacement(&world, &plugin_adapter_block(None));

        let plan = match manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect("re-enable plan")
        {
            EnableOutcome::Planned { plan, .. } => plan,
            EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
        };
        let predicted = plan.actions.iter().any(|a| {
            a.contains("would re-enable openclaw plugin 'memory-core'")
                && a.contains("no longer displaces")
        });
        assert!(
            predicted,
            "{label}: the plan must show the restore the real re-enable performs first: {:?}",
            plan.actions
        );

        let logged_before = argv_lines(&world.argv_log()).len();
        manager
            .enable(COMPONENT, Some(FRAMEWORK), false)
            .expect("re-enable");
        let appended = argv_appended(&world, logged_before);
        assert!(
            argv_contains(&appended, "plugins enable memory-core"),
            "{label}: the plan promised a restore, so the re-enable must perform it: {appended:?}"
        );
        assert!(
            !displaced_marker_exists(&world, "memory-core"),
            "{label}: the dropped plugin must actually be back"
        );
    }
}

/// A dry-run must not perform the restore it announces, nor touch the receipt.
#[test]
fn disable_dry_run_predicts_without_mutating() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    let logged_before = argv_lines(&world.argv_log()).len();

    let preview = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect("disable plan");
    assert!(preview.dry_run);
    assert!(!preview.claim_removed);
    assert!(
        preview
            .report
            .messages
            .iter()
            .any(|m| m.contains("would re-enable openclaw plugin 'memory-core'")),
        "the preview must include the restore a real disable performs: {:?}",
        preview.report.messages
    );
    let appended = argv_appended(&world, logged_before);
    for forbidden in ["plugins enable memory-core", "plugins uninstall tokenless"] {
        assert!(
            !argv_contains(&appended, forbidden),
            "dry-run must not perform what it announced: {appended:?}"
        );
    }
    assert!(displaced_marker_exists(&world, "memory-core"));
    assert!(world.has_claim());
}

/// `displaces` is not an agent-memory contract: any adapter may declare one for
/// its own colliding tools, and `DisplacedPluginSpec` carries no tool names. So a
/// status verdict that named one component's tool would be unusable advice for
/// every other adapter — and this suite's own component is `tokenless`, which is
/// what pinned that mistake in place.
#[test]
fn status_advice_is_generic_for_a_non_memory_adapter() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert_eq!(COMPONENT, "tokenless", "this fixture is not agent-memory");
    assert!(displaced_marker_exists(&world, "memory-core"));

    let status = manager.status(Some(COMPONENT)).expect("status");
    let reason = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition")
        .reason
        .clone()
        .expect("a recorded hand-off must explain itself");

    for agent_memory_specific in ["memory_get", "memory_search", "agent-memory"] {
        assert!(
            !reason.contains(agent_memory_specific),
            "a tokenless adapter must not be pointed at an agent-memory tool \
             ('{agent_memory_specific}'): {reason}"
        );
    }
    assert!(
        reason.contains("this adapter's own tools"),
        "the advice must be phrased against whatever the adapter registers: {reason}"
    );
    // The displaced plugin's own id is fair game — the receipt really does name it.
    assert!(
        reason.contains("memory-core"),
        "and must still name the plugin it is about: {reason}"
    );
}

/// `plugins.enabled = false` is the host's global plugin switch: OpenClaw refuses
/// every `plugins enable` under it. It says nothing about any one plugin's own
/// entry, so reading only `plugins.entries.<id>.enabled` / `plugins.deny` /
/// `plugins.allow` misses it — and an operator who flips it after a successful
/// enable would leave the driver retrying a restore that can never converge, with
/// the receipt kept as a cleanup failure forever instead of recognized as
/// released by policy.
#[test]
fn enable_does_not_claim_a_displacement_while_plugins_are_globally_disabled() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    set_config_answer(&world, "plugins.enabled", "false");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with the global switch off");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "with every plugin off there is no collision to resolve and no restore to promise: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "nothing may be claimed the host would refuse to hand back: {:?}",
        payload.displaced_plugins
    );
}

/// The reviewer's scenario: enable succeeds, *then* the global switch is turned
/// off. The restore is impossible, so it is released — not failed, which would
/// strand the receipt with this adapter's own plugin already uninstalled.
#[test]
fn disable_treats_a_globally_disabled_host_as_released() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(displaced_marker_exists(&world, "memory-core"));

    set_config_answer(&world, "plugins.enabled", "false");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable must converge when the host refuses every enable");

    assert!(
        outcome.report.cleanup_complete,
        "a globally disabled host is a release, not a retryable failure: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim(), "no receipt may be stranded");
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("plugins.enabled") && m.contains("memory-core")),
        "the operator must be told which switch and how to undo it: {:?}",
        outcome.report.messages
    );
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the host would refuse it anyway: {appended:?}"
    );
}

/// And the dry-run has to say the same, through the shared decision.
#[test]
fn disable_preview_agrees_when_plugins_are_globally_disabled() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    set_config_answer(&world, "plugins.enabled", "false");
    assert_disable_preview_matches_real(&world, &manager, "plugins-globally-disabled", false);
}

/// A receipt may displace several plugins and each can be in a different state.
/// Reporting only the highest-priority bucket silently drops the others, so the
/// operator fixes the one plugin named, re-runs status, and only then finds the
/// next.
#[test]
fn status_reports_every_displacement_anomaly_in_a_mixed_state() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_two_displacements(&guard);

    // memory-core was re-enabled behind our back; memory-lancedb cannot be read.
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_KEY",
        "plugins.entries.memory-lancedb.enabled",
    );

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "a plugin positively back on outranks one that could not be checked"
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core"),
        "the re-enabled plugin must be named: {reason}"
    );
    assert!(
        reason.contains("plugins.entries.memory-lancedb.enabled"),
        "and so must the unreadable one, with the key that failed: {reason}"
    );
    assert_eq!(
        status.entries[0].report.summary,
        AdapterSummary::Degraded,
        "the worst verdict still decides the summary"
    );
}

/// Same, for the pair that does not include a decisive `False`: both buckets are
/// `Unknown`, and dropping one would still hide a plugin from the operator.
#[test]
fn status_reports_recorded_and_unreadable_displacements_together() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_two_displacements(&guard);
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_KEY",
        "plugins.entries.memory-lancedb.enabled",
    );

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(condition.status, ConditionStatus::Unknown);
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core") && reason.contains("recorded"),
        "the recorded hand-off must be described: {reason}"
    );
    assert!(
        reason.contains("plugins.entries.memory-lancedb.enabled"),
        "and the unreadable one must not be dropped: {reason}"
    );
}

/// With one displaced plugin the condition can point at its receipt resource, so
/// a machine consumer does not have to parse the prose to find which plugin the
/// verdict is about.
#[test]
fn status_condition_names_the_receipt_resource_for_a_single_displacement() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_displacement(&guard);

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(
        condition.resource.as_ref().map(|r| r.id.as_str()),
        Some("openclaw_displaced_plugin_memory-core"),
        "a single displaced plugin is locatable from the condition itself"
    );
}

/// A receipt names the OpenClaw instance it took ownership in. `OPENCLAW_HOME`
/// and `OPENCLAW_STATE_DIR` can both have moved since, and the Manager still
/// validates such a receipt (the old home stays an allowed root), so probing the
/// caller's environment instead of the receipt's would report on a host this
/// receipt never touched. With `OPENCLAW_HOME=A` retained and
/// `OPENCLAW_STATE_DIR=B` overriding it, a B that happens to have this adapter
/// registered and the bundled plugin disabled reads clean — and the collision
/// already back in A goes unreported.
#[test]
fn status_probes_the_receipt_instance_not_the_callers_environment() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    let receipt_home = world.openclaw_home.clone();

    // A second instance that reads clean.
    let other = world._root.path().join("other-openclaw-state");
    for dir in ["registry", "config"] {
        std::fs::create_dir_all(other.join(dir)).expect("other instance dir");
    }
    std::fs::write(other.join("registry").join(COMPONENT), b"").expect("registered there too");
    std::fs::write(
        other.join("config/plugins.entries.memory-core.enabled"),
        b"false",
    )
    .expect("clean there");

    // STATE_DIR now points elsewhere, but OPENCLAW_HOME still names the receipt's
    // directory, so it remains an allowed root and the receipt validates.
    guard.set("OPENCLAW_STATE_DIR", &other);

    // The collision is back in the instance the receipt actually owns.
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");

    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = status.entries[0]
        .report
        .conditions
        .iter()
        .find(|c| c.kind == AdapterConditionKind::DisplacedPluginsReleased)
        .expect("displacement condition");
    assert_eq!(
        condition.status,
        ConditionStatus::False,
        "status must report on {}, the instance the receipt names, not on {}: {:?}",
        receipt_home.display(),
        other.display(),
        condition.reason
    );
    assert_eq!(status.entries[0].report.summary, AdapterSummary::Degraded);
}

/// The other side of the same boundary, pinned so a change to it is deliberate:
/// a receipt whose state directory is neither the current nor the legacy one is
/// rejected by the Manager's claim validation, before any driver code runs. So
/// `cleanup_replaced_claim`'s cross-home branch is reachable only for a
/// legacy-resolver → current-resolver migration, not for an arbitrary A→B move —
/// and A→B fails closed rather than operating on the wrong instance.
#[test]
fn a_receipt_in_an_unrelated_state_directory_is_rejected_not_migrated() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(displaced_marker_exists(&world, "memory-core"));

    // Both variables move, so the recorded directory is neither current nor legacy.
    let elsewhere = world._root.path().join("unrelated-openclaw-state");
    std::fs::create_dir_all(elsewhere.join("registry")).expect("elsewhere");
    guard.set("OPENCLAW_HOME", &elsewhere);
    guard.set("OPENCLAW_STATE_DIR", &elsewhere);

    let logged_before = argv_lines(&world.argv_log()).len();
    for err in [
        manager
            .status(Some(COMPONENT))
            .expect_err("status must not report on an untrusted instance"),
        manager
            .disable(COMPONENT, Some(FRAMEWORK), true)
            .expect_err("a dry-run must not plan against an untrusted instance"),
        manager
            .disable(COMPONENT, Some(FRAMEWORK), false)
            .expect_err("disable must not clean an untrusted instance"),
        manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect_err("re-enable must not migrate an untrusted receipt"),
    ] {
        assert!(
            matches!(&err, AdapterError::ClaimValidation(_)),
            "the trust boundary rejects it before any driver code runs: {err:?}"
        );
    }
    assert_eq!(
        argv_lines(&world.argv_log()).len(),
        logged_before,
        "a rejected receipt must not produce a single host call"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the recorded instance must be left exactly as it was"
    );
    assert!(world.has_claim(), "the receipt is kept, not discarded");
}

/// `prepare_enable` decides what the receipt may claim, but install, config
/// writes and runtime verification all run before `apply_displacements` mutates —
/// and the Manager's lock does not serialize a directly invoked `openclaw`. If
/// somebody turns the plugin off inside that window, `plugins disable` exits 0
/// having changed nothing, so the transition is not this adapter's; claiming it
/// anyway would make a later `adapter disable` re-open a plugin the operator had
/// just closed.
///
/// This fixture's bundle declares no plugin `kind`, so the host applies no exclusive
/// slot selection and nothing ANOLISA ran could have written that flag: the
/// takeover stays attributable, which is the half
/// `enable_keeps_the_displacement_its_own_slot_selection_turned_off` covers from the
/// other side.
#[test]
fn enable_releases_a_displacement_claim_taken_over_midway() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_OPERATOR_DISABLES_ON_INSTALL", "memory-core");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the enable itself still succeeds: our own plugin is installed and verified");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "somebody else already turned it off, so this enable must not claim the transition: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "the released claim must be gone from the persisted receipt, not merely skipped: {:?}",
        payload.displaced_plugins
    );
    assert!(
        !claim
            .resources
            .iter()
            .any(|r| r.purpose == "openclaw_displaced_plugin"),
        "and its resource must be dropped with it, or the receipt would not validate: {:?}",
        claim.resources
    );

    // The payoff: disable has nothing to undo, so it does not undo the operator.
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins enable memory-core"),
        "disable must never re-enable a plugin the operator turned off: {lines:?}"
    );
    assert!(!world.has_claim());
}

/// The other half of that attribution: a `false` flag this enable's *own* commands
/// wrote must stay this enable's claim.
///
/// OpenClaw applies exclusive slot selection on both `plugins install` and
/// `plugins enable` (v2026.4.14: `src/cli/plugins-install-persist.ts` and
/// `src/cli/plugins-cli.ts`, through `applySlotSelectionForPlugin` →
/// `applyExclusiveSlotSelection`): the slot for the selected plugin's kind comes to
/// name it, and every other plugin of that kind is written `enabled = false`.
/// `memory-anolisa` and `memory-core` both declare `kind = "memory"`, so installing
/// and enabling this adapter's own plugin turns the bundled one off with no operator
/// and no framework update anywhere — and by the time `apply_displacements`
/// re-confirmed its claim, the plugin it was about to disable was already off.
///
/// Reading that as a takeover released the claim, and the consequence was not
/// cosmetic: the receipt named no displacement, `disable` reported a complete
/// cleanup and removed it, and the host was left with the bundled memory backend off
/// and nothing that would ever restore one. The attribution that separates the two
/// cases is the other half of the same host write — the slot naming this adapter's
/// own plugin — which an operator's `plugins disable` does not touch.
#[test]
fn enable_keeps_the_displacement_its_own_slot_selection_turned_off() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Both sides declare `kind = "memory"`: that is what makes the host's own slot
    // selection turn the bundled plugin off during this enable.
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("enable with a same-kind displacement");

    // The host did it, and left the trace that says whose doing it was.
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and must leave the slot naming this adapter's own plugin, which is the \
         attribution the driver reads"
    );

    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "the hand-off is still performed; `plugins disable` is idempotent, so \
         running it against an already-off plugin costs nothing: {lines:?}"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "a transition this enable's own commands performed is still this enable's \
         to record"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        payload.displaced_plugins[0].applied,
        "and recorded as performed, so a later disable hands it back: {:?}",
        payload.displaced_plugins
    );

    // The payoff: disable restores the bundled backend. Before the fix there was
    // nothing left in the receipt to restore it from.
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(outcome.claim_removed);
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the host must not be left with no memory backend at all: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "and the plugin must really be back on"
    );
}

/// A failed verification must not lose a hand-off the host already performed *for*
/// this adapter.
///
/// The reported repro: both plugins declare `kind = "memory"`, so `plugins install`
/// and `plugins enable` turn `memory-core` off by themselves, and `verify_runtime`
/// then fails. Marking the hand-off only inside `apply_displacements` — which runs
/// *after* verification — left a receipt reading `applied = false` over a host where
/// the bundled backend was off because of this enable, so `restore_decision` answered
/// `SkipNotApplied`: disable told the operator "this adapter never disabled it",
/// reported a complete cleanup, removed the receipt, and left the host with no memory
/// backend and nothing recording why. No operator and no concurrency involved — an
/// ordinary failed-enable recovery path.
#[test]
fn failed_verify_keeps_the_handoff_slot_selection_already_performed() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");

    // The host performed the hand-off, and the receipt records it as performed —
    // durably, before the verification that failed.
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("a failed enable keeps a cleanup_failed receipt")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the displacement must survive the failed verification"
    );
    let DriverPayload::OpenClaw(payload) = &kept.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        payload.displaced_plugins[0].applied,
        "a hand-off the host performed for this enable must be recorded as \
         performed, or disable will disown it: {:?}",
        payload.displaced_plugins
    );

    // Recovery: whatever made verification fail is fixed, and the operator disables.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(outcome.claim_removed);
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the bundled backend must be handed back, not left off with the receipt \
         gone: {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and the report must not disown a hand-off this enable's own commands \
         performed: {:?}",
        outcome.report.messages
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the plugin must really be back on"
    );
}

/// An unreadable slot probe is not evidence about anybody.
///
/// The reported repro: both plugins declare `kind = "memory"` and every
/// `config get plugins.slots.memory` fails for the duration of the enable. The host's
/// own slot selection still turned `memory-core` off and still wrote the slot, so the
/// persisted state says the hand-off happened — but the one read that would attribute
/// it came back empty. Folding "could not read" into "not ours" made that a
/// destructive decision: the enable succeeded with an empty `displaced_plugins`, and
/// disable then removed the receipt while `memory-core` stayed off. This is the same
/// rule the re-confirmation already states for the enablement probe, and that `status`
/// states by keeping `unreadable` a bucket of its own.
#[test]
fn enable_keeps_the_claim_a_transient_slot_read_cannot_attribute() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the enable itself still succeeds");

    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "a probe the host cannot answer says nothing about who turned it off, so the \
         claim must survive it"
    );
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "and the hand-off is still performed: {lines:?}"
    );

    // The probe recovers, and disable hands the plugin back.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "the slot was written all along; only the read had failed"
    );
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(outcome.claim_removed);
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the host must not be left with no memory backend: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "and the plugin must really be back on"
    );
}

/// A declaration that names no slot is a declaration that the plugin does not compete
/// for one — and OpenClaw's slot selection only ever disables plugins of the selected
/// kind, so nothing this enable ran can be what turned it off.
///
/// The reported repro, and the protection that folding "no slot to ask about" into
/// "the host could not answer" had silently dropped: an operator turns the plugin off
/// while this adapter installs, the flag is the only evidence there is, and claiming it
/// anyway makes a later `adapter disable` re-open a plugin the operator closed — with
/// no slot guard on that restore either, since the declaration named no slot to
/// consult. The two negative answers are not the same claim: an unreadable probe is no
/// evidence and keeps the claim, while an omitted slot is an answer taken from the
/// contract that produced the receipt (`DisplacedPluginSpec::slot` — "declare it when
/// the displaced plugin re-takes an exclusive slot, omit it when it does not").
#[test]
fn enable_releases_a_slotless_displacement_an_operator_turned_off() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("other-plugin", None),
    );
    // Neither side declares a plugin kind, so the host applies no slot selection at
    // all and writes no slot that could corroborate anything.
    seed_bundled_plugin(&world, "other-plugin");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_OPERATOR_DISABLES_ON_INSTALL", "other-plugin");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the enable itself still succeeds: our own plugin installs and verifies");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable other-plugin"),
        "somebody else turned it off, and nothing this enable ran could have: {lines:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "the released claim must be gone from the persisted receipt, not merely \
         skipped: {:?}",
        payload.displaced_plugins
    );

    // The payoff: disable has nothing to undo, so it does not undo the operator.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable other-plugin"),
        "disable must never re-enable a plugin the operator turned off: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.other-plugin.enabled").as_deref(),
        Some("false"),
        "and it must still be off, exactly as the operator left it"
    );
    assert!(!world.has_claim());
}

/// A lost hand-off *record* must not become a lost hand-off.
///
/// The reported repro, and the gap the previous fix left: recording the hand-off needs
/// a readable slot, so with every `config get plugins.slots.memory` failing the entry
/// stays `applied = false` — and when verification then fails, `apply_displacements`
/// never runs either. The receipt kept for the retry described a plugin this adapter
/// *had* turned off as one it never touched: disable said "this adapter never disabled
/// it", reported a complete cleanup, removed the receipt, and left the bundled backend
/// off with nothing recording why. Keeping the claim is not the same as recording the
/// hand-off, and only the second makes disable hand the plugin back.
///
/// So the restore branch asks the host the same question the write-ahead mark asks,
/// through one shared predicate, and `status` files the entry under its own bucket
/// rather than repeating a provenance the same run is about to contradict.
#[test]
fn failed_verify_with_an_unreadable_slot_still_hands_the_plugin_back() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");

    // The host really performed the hand-off, and the receipt really could not
    // record it — that combination is the state under test.
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and the slot was written all along; only the read had failed"
    );
    let kept = {
        let state = world.load_state();
        state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("a failed enable keeps a cleanup_failed receipt")
    };
    assert_eq!(kept.status, ClaimStatus::CleanupFailed);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the claim itself survives"
    );
    let DriverPayload::OpenClaw(payload) = &kept.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
    assert!(
        !payload.displaced_plugins[0].applied,
        "the unreadable slot left the mark unwritten, which is what the restore \
         branch has to cope with: {:?}",
        payload.displaced_plugins
    );

    // The probe recovers. `status` must not disown a hand-off the host shows this
    // adapter performing.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    let condition = displacement_condition(&manager);
    assert_eq!(
        condition.status,
        ConditionStatus::Unknown,
        "config shows the hand-off in force, the gateway is unobservable: {:?}",
        condition.reason
    );
    let reason = condition.reason.clone().expect("reason");
    assert!(
        reason.contains("memory-core"),
        "the verdict must name the plugin: {reason}"
    );
    assert!(
        !reason.contains("not by this adapter"),
        "status must not file this under a provenance the host contradicts, and that \
         the disable below is about to reverse: {reason}"
    );
    assert!(
        reason.contains("slot") && reason.contains("hand it back"),
        "and must say whose doing it was, since that is what makes the restore \
         below correct rather than an undo of somebody else's choice: {reason}"
    );

    // And disable hands the plugin back instead of dropping the receipt over it.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(outcome.claim_removed);
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the bundled backend must be handed back even though the receipt never \
         marked the hand-off: {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and the report must not claim a provenance the same run contradicts: {:?}",
        outcome.report.messages
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the plugin must really be back on"
    );
}

/// A hand-off attribution a `disable` recovers has to outlive the `disable` that
/// recovered it.
///
/// The reported repro, three steps with no operator change anywhere in them: both
/// plugins declare `kind = "memory"`, and an enable whose slot read fails while
/// verification fails too leaves `memory-core` off behind a receipt reading
/// `applied = false`. The first disable attributes the hand-off correctly from its
/// pre-uninstall capture, uninstalls this adapter's own plugin — which resets
/// `plugins.slots.memory` to `memory-core` — and then fails the restore, keeping the
/// receipt and advising a retry. That advice was empty: the retry's capture reads the
/// slot the uninstall just wrote, `restore_decision` answers `SkipNotApplied`, the
/// cleanup reports complete, the receipt is removed, and the bundled backend stays
/// off with nothing left that records why.
///
/// The capture cannot be what survives, and that is not a defect in it — the host
/// stops holding the evidence the moment this driver uninstalls its own plugin, so
/// the readings are local to one call by construction. What has to survive is the
/// ownership they establish, and the only place it can is the receipt.
///
/// What this test cannot show is *when* the receipt gets it: the failed restore
/// returns control to the Manager, whose save would write the mark even if the
/// driver never did. `disable_persists_a_recovered_handoff_before_the_uninstall_destroys_it`
/// closes that gap by killing the process in between.
#[test]
fn failed_restore_keeps_the_recovered_handoff_retryable() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    // 1. An enable that cannot read the slot, and then fails verification: the host
    //    performed the hand-off and the receipt could not record it.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "and the unreadable slot left the mark unwritten, which is what the first \
         disable has to recover"
    );

    // 2. The slot read recovers; the restore is what fails instead.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    guard.set("FAKE_OC_ENABLE_FAIL_ID", "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();
    let first = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable reports incomplete cleanup rather than failing");
    assert!(!first.report.cleanup_complete);
    assert!(!first.claim_removed, "the receipt is kept for the retry");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "this attempt really does attribute the hand-off and owe the restore: \
         {appended:?}"
    );
    assert!(
        first
            .report
            .messages
            .iter()
            .any(|m| m.contains("memory-core") && m.contains("receipt is kept")),
        "and really does advise the retry the next step has to honour: {:?}",
        first.report.messages
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("memory-core"),
        "the uninstall reset the slot, so the host no longer holds the evidence"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the claim survives the failed restore"
    );
    // Read what this attempt left behind, but assert on the *mechanism* only after
    // the payoff below: how the driver keeps a recovered attribution is its
    // business, and a different mechanism that makes the retry restore should not
    // fail this test.
    let leftover = persisted_displacement(&world);

    // 3. The retry, with nothing changed in between but the failure being cleared.
    guard.unset("FAKE_OC_ENABLE_FAIL_ID");
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry completes the restore it owes");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "an unchanged host must not make the second attempt give up on a restore the \
         first one attributed: {appended:?}"
    );
    assert!(
        !retry
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the previous attempt recorded as its own: {:?}",
        retry.report.messages
    );
    assert!(retry.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );

    // The mechanism: the first attempt wrote the ownership it recovered into the
    // receipt, which is the only place it could have survived — the host had already
    // been left holding the slot value the uninstall put back.
    assert!(
        leftover.applied,
        "a hand-off attribution recovered from a pre-uninstall capture must be \
         durable rather than local to the call that recovered it: {leftover:?}"
    );
}

/// A hand-off a failed enable could not mark must survive the re-enable that
/// succeeds — not merely be recoverable from a host the next `disable` overwrites.
///
/// `failed_verify_with_an_unreadable_slot_still_hands_the_plugin_back` covers the
/// retry that goes straight to `disable`. This is the other retry, and it is the one
/// an operator actually runs: the enable failed, so they fix whatever broke and
/// enable again. The host state is unchanged between the two steps — no operator
/// edit, no framework update, nothing but the two faults being cleared — and the
/// re-enable used to lose the ownership in three moves that each looked correct:
///
/// 1. `prepare_enable` probes `memory-core`, reads a positive `false`, and answers
///    `FlagDisabled`. That answer says nothing about *who* turned the plugin off,
///    so the probe declined to claim it.
/// 2. `preserve_openclaw_displaced_facts` declines to inherit an unapplied entry,
///    so the replacement receipt names no displacement at all.
/// 3. `dropped_displaced_plugins` therefore hands `memory-core` back during the
///    same-home cleanup — and this enable's own install and enable immediately
///    switch it off again through the very slot selection the restore branch would
///    have attributed, over a receipt with nothing in it.
///
/// The re-enable reported success, the `disable` that followed uninstalled this
/// adapter's plugin, reported a receipt naming no plugin to hand back, removed the
/// receipt, and left the bundled backend off. No step in that sequence had anything
/// to say about the ownership, because by the time anything looked, the receipt was
/// the only record and it was empty.
///
/// The fix asks the predicate the other three recovery marks ask, in the one scope
/// that holds both halves of the question: a prior receipt for this same instance
/// names the plugin without having marked the hand-off, and the host shows this
/// adapter's own slot selection having performed it. That is positive evidence, so
/// the replacement receipt carries the entry as already performed — and it is why
/// the re-enable below must *not* hand the plugin back first: the cleanup has
/// nothing dropped to restore any more.
#[test]
fn reenable_recovers_a_handoff_the_failed_enable_never_marked() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Both plugins declare `kind = "memory"`, which is what makes the host write
    // the slot the attribution reads at all.
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    // 1. An enable that cannot read the slot, and then fails verification: the
    //    host performed the hand-off and the receipt could not record it.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and the slot this adapter's own plugin owns, which is the corroborating \
         half of the attribution"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "the unreadable slot left the mark unwritten, which is what the re-enable \
         has to recover"
    );

    // 2. Both faults cleared and the enable retried. Nothing else changes.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry succeeds: our own plugin installs and verifies");
    let appended = argv_appended(&world, logged_before);
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the replacement receipt must own the hand-off the prior one could not \
         mark; an empty one is what stranded the host"
    );
    assert!(
        persisted_displacement(&world).applied,
        "and must carry it as already performed, since that is what a later \
         disable acts on"
    );
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the same-home cleanup must have nothing dropped to restore: handing the \
         plugin back here only has this enable's own install switch it off again, \
         which is the sequence that lost the ownership: {appended:?}"
    );

    // 3. The payoff: the disable that follows really does hand the plugin back.
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "a successful re-enable must not cost the operator the bundled backend: \
         {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the receipt records as its own: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// Stage a world whose first enable performed the hand-off but could not mark it,
/// with both faults cleared again — the state a real retry is read from.
///
/// The fixture half of
/// [`reenable_recovers_a_handoff_the_failed_enable_never_marked`], which walks it
/// step by step because the staging *is* its subject. The two tests below start from
/// the same world and need it as a precondition, so they share this rather than each
/// carrying a third copy of it. Returns once the receipt names `memory-core` with
/// `applied = false` over a host whose memory slot names this adapter's own plugin —
/// positive evidence for the recovery, and the only record that the hand-off
/// happened at all.
fn stage_with_unmarked_handoff(guard: &OpenClawEnvGuard) -> (World, AdapterManager) {
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Both plugins declare `kind = "memory"`, which is what makes the host write the
    // slot the attribution reads at all.
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    // An enable that cannot read the slot, and then fails verification: the host
    // performed the hand-off and the receipt could not record it.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    guard.unset("FAKE_OC_RUNTIME_STATUS");

    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "fixture must reproduce the slot-selection side effect"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and the slot this adapter's own plugin owns, which is the corroborating \
         half of the attribution"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "fixture must leave a receipt naming the displacement"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "and must leave the hand-off unmarked, which is what a retry has to recover"
    );
    (world, manager)
}

/// One transient failure to read the slot must not cost the operator the recovery —
/// and it must not be answered by guessing, either.
///
/// [`reenable_recovers_a_handoff_the_failed_enable_never_marked`] proves the recovery
/// works when the host answers. This is the retry where it does not: the same failed
/// enable leaves the same unapplied entry over a plugin this adapter really turned
/// off, both faults are cleared, and then a *single* `config get plugins.slots.memory`
/// fails during the re-enable's own attribution read.
///
/// The attribution used to collapse "could not read" into "not this adapter", which
/// declined the recovery and then lost it exactly as before: the replacement receipt
/// named no displacement, the same-home cleanup handed `memory-core` back, this
/// enable's own install and enable switched it off again through the very selection
/// the read had failed to attribute, and the disable that followed reported a receipt
/// with nothing to restore and removed it — the bundled backend off, no operator
/// anywhere in the sequence, and nothing left recording why. Inheriting *every*
/// unapplied entry instead is the mirror-image mistake, and the one the evidence gate
/// exists to prevent: it would claim an operator's own disable and a later `disable`
/// would re-open a plugin they closed themselves.
///
/// So the re-enable fails while the host cannot answer, before its first mutation and
/// with the prior receipt untouched, and the retry — the one an operator runs after
/// whatever broke the read is over — recovers the ownership and hands the plugin back.
#[test]
fn review_reenable_preserves_unrecorded_handoff_on_one_failed_slot_probe() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);

    // 1. One transient failure of the attribution read, and nothing else.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_ONCE", "plugins.slots.memory");
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an unreadable attribution must fail the re-enable, not guess at it");
    let appended = argv_appended(&world, logged_before);
    // The failure must land before the first mutation, which is the whole reason it
    // is an error rather than a verdict: everything after this point acts on the
    // receipt the read failure just declined to carry forward.
    assert_dry_run_only_probed(&appended);
    assert!(
        err.to_string().contains("memory-core"),
        "the error must name the plugin whose ownership it could not settle: {err}"
    );

    // 2. The prior receipt is the thing being protected, so it has to survive
    //    byte-for-byte: still naming the displacement, still unmarked, and still the
    //    only record that this adapter is what turned the plugin off.
    assert!(
        world.has_claim(),
        "the failed re-enable must keep the receipt"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and must not have dropped the displacement it is the only record of"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "still unmarked, so the retry is asked the same question and can still \
         answer it"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "and the host must be exactly as the failed enable left it"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "including the slot reading the retry recovers the attribution from"
    );

    // 3. The retry, with the host answering: the ownership is recovered rather than
    //    lost, so the same-home cleanup has nothing dropped to restore.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry succeeds once the host answers the attribution");
    let appended = argv_appended(&world, logged_before);
    assert!(
        persisted_displacement(&world).applied,
        "the recovered hand-off must be carried as already performed, which is what \
         a later disable acts on"
    );
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "handing the plugin back here only has this enable's own install switch it \
         off again, which is the sequence that lost the ownership: {appended:?}"
    );

    // 4. The payoff the transient failure was protecting.
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "a transient read failure on a re-enable must not cost the operator the \
         bundled backend: {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the recovered receipt records as its own: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// The re-enable preview must describe the ownership the real re-enable recovers.
///
/// `prepare_enable` recovers an unmarked hand-off from host evidence, and the preview
/// knew nothing about it: `plan_enable`'s `FlagDisabled` arm read the positive `false`
/// and promised "already disabled before this adapter, so it is not claimed and
/// disable will not re-enable it" — over a real enable whose replacement receipt
/// carried the entry as applied and whose disable then really did re-enable the
/// plugin. The two commands disagreed about the one fact an operator runs a dry-run
/// to learn, and the disagreement left no trace: the disable succeeded and removed
/// the receipt, so nothing afterwards showed that the plan had described a different
/// run. Whoever assessed the cost of a later uninstall from that preview got the
/// opposite of the answer.
///
/// Both halves are read from the same world in the order an operator runs them, with
/// no host change in between, so the pairing cannot drift.
#[test]
fn review_reenable_preview_matches_recovered_ownership() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);

    // 1. The preview.
    let logged_before = argv_lines(&world.argv_log()).len();
    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    assert!(
        plan.actions.iter().any(|action| action
            .contains("keep openclaw plugin 'memory-core' disabled")
            && action.contains("disable will hand it back")
            && action.contains("plugins.slots.memory")),
        "the preview must report the ownership the real re-enable recovers, from the \
         same evidence: {:?}",
        plan.actions
    );
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.contains("leave openclaw plugin 'memory-core' alone")),
        "and must not promise the opposite of what the disable that follows does: {:?}",
        plan.actions
    );
    // The recovery is a read: it may cost the plan two more `config get` calls, and
    // nothing else.
    let all = argv_lines(&world.argv_log());
    assert_dry_run_only_probed(&all[logged_before.min(all.len())..]);

    // 2. The real re-enable, with nothing changed in between.
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the real re-enable succeeds");
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the receipt carries the displacement the preview said it would carry"
    );
    assert!(
        persisted_displacement(&world).applied,
        "as already performed, which is what the preview's `disable will hand it \
         back` rests on"
    );

    // 3. And the disable the preview spoke for.
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the preview promised the bundled backend would be handed back: {appended:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("re-enabled openclaw plugin 'memory-core'")),
        "and said so in the words the report really uses: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "so the plan and the operation agree on the host state as well"
    );
}

/// One transient failure of either half of a `disable`'s attribution read must stop
/// the disable before its first mutation — and the retry must then recover exactly
/// what the stopped one kept.
///
/// Shared by the two tests below because they are the same question asked of the two
/// halves the attribution is built from: the slot the pre-uninstall capture reads, and
/// the enablement flag the attribution reads live. A driver that guards one and folds
/// the other into "not this adapter" has guarded neither, and only running both says
/// so.
///
/// What makes `Unknown` an error here rather than a verdict is what the rest of the
/// disable does. It uninstalls this adapter's own plugin, which resets
/// `plugins.slots.memory` to the host default and so destroys the reading the
/// attribution was recovered from; the restore then skips an entry nothing marked; the
/// cleanup reports itself complete; and the Manager removes the one receipt that ever
/// named the hand-off. Folding "could not read" into "not ours" therefore used to end
/// with the bundled backend off, no record of who turned it off, nothing left for a
/// retry to recover from, and a report reading "this adapter never disabled it and
/// there is nothing to hand back" — no crash and no operator anywhere in the sequence.
/// The capture exists to keep the evidence alive across the disable; failing while it
/// cannot be read is what keeps it alive across the disable *attempt*.
fn assert_disable_stops_on_unreadable_attribution(
    world: &World,
    manager: &AdapterManager,
    guard: &OpenClawEnvGuard,
    fail_key: &str,
) {
    // 1. One transient failure of that one key, and nothing else wrong with the host.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_ONCE", fail_key);
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an unreadable attribution must stop the disable, not be spent as 'not ours'");
    let appended = argv_appended(world, logged_before);
    assert!(
        err.to_string().contains("memory-core"),
        "the error must name the plugin whose ownership it could not settle: {err}"
    );
    // Nothing but reads ran, which is the whole reason this is an error rather than a
    // verdict: every step after this point is one the retry could not undo.
    assert_dry_run_only_probed(&appended);

    // 2. Both halves of the evidence survive, because nothing did.
    assert!(
        world.has_claim(),
        "the stopped disable must keep the receipt it could not act on"
    );
    assert_eq!(
        persisted_displacement_ids(world),
        vec!["memory-core".to_string()],
        "still naming the displacement it is the only record of"
    );
    assert!(
        !persisted_displacement(world).applied,
        "and still unmarked, so the retry is asked the same question and can still \
         answer it"
    );
    assert!(
        world.registry_marker_exists(),
        "this adapter's own plugin must still be installed — the uninstall is what \
         resets the slot the attribution reads: {err}"
    );
    assert_eq!(
        config_answer(world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "the slot must still name this adapter's plugin, so the retry has something to \
         recover the attribution from"
    );
    assert_eq!(
        config_answer(world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "and the bundled backend must be exactly as the failed enable left it"
    );

    // 3. The retry, with the host answering: the recovered ownership is written down
    //    before the uninstall and handed back after it.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_ONCE");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry succeeds once the host answers the attribution");
    let appended = argv_appended(world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "a transient read failure on a disable must not cost the operator the bundled \
         backend: {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the recovered attribution records as its own: \
         {:?}",
        outcome.report.messages
    );
    assert!(
        outcome.report.cleanup_complete,
        "the retry must converge: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert!(!displaced_marker_exists(world, "memory-core"));
    assert_eq!(
        config_answer(world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// The slot half: the pre-uninstall capture's own `config get plugins.slots.memory`
/// fails once, so the attribution has no reading to reason from.
///
/// This is the reviewer's repro. The fixture is the failed enable
/// `stage_with_unmarked_handoff` stages — the host really did turn the bundled plugin
/// off and really did write the slot naming this adapter's own plugin, and the receipt
/// could not record either — and the only fault injected afterwards is the one read.
#[test]
fn review_disable_keeps_unmarked_handoff_on_unreadable_capture() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    assert_disable_stops_on_unreadable_attribution(
        &world,
        &manager,
        &guard,
        "plugins.slots.memory",
    );
}

/// The enablement half: the capture reads the slot perfectly well, and the live
/// `config get plugins.entries.memory-core.enabled` that corroborates it fails once.
///
/// Neither half carries the attribution on its own — a slot naming this adapter's
/// plugin says its selection ran, not that *this* plugin was one of the ones it
/// switched off — so an unreadable flag is as unreadable a verdict as an unreadable
/// slot, and must stop the disable the same way.
#[test]
fn review_disable_keeps_unmarked_handoff_on_unreadable_enablement_flag() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    assert_disable_stops_on_unreadable_attribution(
        &world,
        &manager,
        &guard,
        "plugins.entries.memory-core.enabled",
    );
}

/// The preview has to fail where the operation it describes fails.
///
/// A real disable now stops on an attribution the host cannot answer, so a plan that
/// kept predicting a restore decision through the same unreadable evidence would
/// describe a disable that does not exist: "would leave openclaw plugin 'memory-core'
/// alone ... this adapter never disabled it" over a command that refuses to run at
/// all. That is the contradiction `restore_decision` was made the single source of
/// truth to prevent, arriving through a failure rather than a veto — and it is the
/// disable side of the agreement
/// `review_reenable_preview_matches_recovered_ownership` pins for the re-enable.
///
/// The second half is what keeps the gate honest in the other direction: once the host
/// answers, the plan promises the hand-back and the disable performs it, so the
/// preview fails *only* while the operation would.
#[test]
fn review_disable_preview_fails_like_the_real_disable_on_unreadable_capture() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);

    guard.set("FAKE_OC_CONFIG_GET_FAIL_ONCE", "plugins.slots.memory");
    let logged_before = argv_lines(&world.argv_log()).len();
    let plan_err = manager
        .disable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("a plan must fail where the operation it describes fails");
    let appended = argv_appended(&world, logged_before);
    assert_dry_run_only_probed(&appended);
    assert!(
        plan_err.to_string().contains("memory-core"),
        "and must name the plugin it could not settle, the way the operation does: \
         {plan_err}"
    );
    assert!(
        world.has_claim() && !persisted_displacement(&world).applied,
        "a preview that could not be completed leaves the receipt exactly as it found it"
    );

    guard.unset("FAKE_OC_CONFIG_GET_FAIL_ONCE");
    assert_disable_preview_matches_real(&world, &manager, "unreadable-then-answered", true);
}

/// The actions an `enable --dry-run` planned, panicking if it applied instead.
///
/// Every preview assertion in this file needs the same two things — the plan's own
/// wording, and the guarantee that reading it changed nothing — and the second is
/// what makes the first worth asserting at all.
fn planned_actions(outcome: EnableOutcome) -> Vec<String> {
    match outcome {
        EnableOutcome::Planned { plan, .. } => plan.actions,
        EnableOutcome::Enabled(_) => panic!("a dry-run must return a plan"),
    }
}

/// A contract that drops a displacement declaration must not be what turns an
/// unreadable attribution into a lost ownership.
///
/// The reviewer's repro, and the path both of the last two rounds walked past.
/// `prepare_enable` and `plan_enable` ask [`recovered_prior_handoff`] about every
/// unapplied entry the prior receipt names, but they walk the *current* contract to
/// find them — so a displacement the contract being enabled no longer declares is
/// invisible to both, and the one place left that can act on it is
/// `cleanup_replaced_claim`. Its restore asks the boolean collapse, which folds
/// "the host could not say" into `SkipNotApplied`, and that fold is right for a
/// `disable`: the receipt survives the skip and the next attempt is asked again. A
/// same-home re-enable is the one caller it is wrong for, because its `Ok` is what
/// lets the Manager replace the prior receipt with one naming no such plugin — so
/// the skip is the last word on the ownership. One transient `config get` failure,
/// no crash and no operator anywhere in the sequence, and the host is left with the
/// bundled backend off, `cleanup_complete = true`, and nothing recording which
/// adapter turned it off.
///
/// So the re-enable fails while the host cannot answer, before its first mutation
/// and with the prior receipt exactly as it was, and the retry — with the host
/// answering — really performs the restore the dropped declaration left to it
/// instead of reporting it away.
#[test]
fn review_dropped_displacement_keeps_an_unreadable_handoff_out_of_the_replacement_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);

    // The contract being enabled no longer displaces anything. This is what hides
    // the entry from both of the recovery's callers and leaves the same-home
    // cleanup as the only place that can still act on it.
    redeclare_displacement(&world, &plugin_adapter_block(None));

    // 1. One transient failure of the attribution read, and nothing else.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_ONCE", "plugins.slots.memory");
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err(
            "an unreadable attribution must fail the re-enable, not be spent as \
             'this adapter never disabled it'",
        );
    let appended = argv_appended(&world, logged_before);
    assert!(
        err.to_string().contains("memory-core"),
        "the error must name the plugin whose ownership it could not settle: {err}"
    );
    // The failure must land before the first mutation: everything after this point
    // is a host change the receipt swap would then have to describe.
    assert_dry_run_only_probed(&appended);

    // 2. The prior receipt is the thing being protected, and unlike the disable
    //    side it has no later attempt to protect it — this is the last one.
    assert!(
        world.has_claim(),
        "the failed re-enable must keep the receipt rather than swap in one that \
         names nothing"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and must not have dropped the displacement it is the only record of"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "still unmarked, so the retry is asked the same question and can still \
         answer it"
    );
    assert!(
        world.registry_marker_exists(),
        "this adapter's own plugin must still be installed: {err}"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "the slot must still name it, so the retry has something to recover the \
         attribution from"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "and the bundled backend must be exactly as the failed enable left it"
    );

    // 3. The retry, with the host answering: the dropped displacement is really
    //    handed back, which is the observable difference between asking the
    //    tri-state and guessing at it.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_ONCE");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry succeeds once the host answers the attribution");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "a transient read failure must not cost the operator the restore the \
         dropped declaration left to this cleanup: {appended:?}"
    );
    // The restore is not the end of the story, and asserting an empty receipt
    // here used to be: this enable's own install re-selects the same slot and turns
    // `memory-core` straight off again, so what the swap has to leave behind is the
    // *responsibility* rather than nothing.
    // `independent_dropped_handoff_survives_remaining_install` walks that half with
    // no fault injected at all; what this test owes is that one transient read
    // failure costs neither the restore nor the record that outlives it.
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the replacement receipt keeps the displacement its own registration \
         performed again, so the restore the retry just ran is not the last word \
         on the ownership"
    );
    assert!(
        persisted_displacement(&world).applied,
        "and marks the hand-off performed, which is what a later disable acts on"
    );
}

/// The dropped-displacement preview has to fail where the re-enable it describes
/// fails, and for the same read.
///
/// `plan_dropped_displacement_restores` predicts the restore through
/// `restore_decision`, whose `SkipNotApplied` arm is the boolean collapse — so a
/// plan read while the host cannot answer used to promise "would leave openclaw
/// plugin 'memory-core' alone ... this adapter never disabled it" over a re-enable
/// that now refuses to run at all. That is the contradiction
/// `review_disable_preview_fails_like_the_real_disable_on_unreadable_capture` pins
/// for the disable, arriving on the re-enable side, and it is worse here than a
/// wrong wording: the plan is the only place an operator can see that the contract
/// change is about to let go of an ownership, and it is the place that said the
/// ownership did not exist.
///
/// The second half keeps the gate honest in the other direction: once the host
/// answers, the plan promises the hand-back and the re-enable performs it, so the
/// preview fails *only* while the operation would.
#[test]
fn review_dropped_displacement_preview_fails_like_the_real_reenable() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    redeclare_displacement(&world, &plugin_adapter_block(None));

    guard.set("FAKE_OC_CONFIG_GET_FAIL_ONCE", "plugins.slots.memory");
    let logged_before = argv_lines(&world.argv_log()).len();
    let plan_err = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("a plan must fail where the operation it describes fails");
    let appended = argv_appended(&world, logged_before);
    assert_dry_run_only_probed(&appended);
    assert!(
        plan_err.to_string().contains("memory-core"),
        "and must name the plugin it could not settle, the way the operation does: \
         {plan_err}"
    );
    assert!(
        world.has_claim() && !persisted_displacement(&world).applied,
        "a preview that could not be completed leaves the receipt exactly as it \
         found it"
    );

    guard.unset("FAKE_OC_CONFIG_GET_FAIL_ONCE");
    let actions = planned_actions(
        manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect("the plan completes once the host answers"),
    );
    assert!(
        actions.iter().any(|action| action
            .contains("would re-enable openclaw plugin 'memory-core'")
            && action.contains("no longer displaces")),
        "and then promises the hand-back the dropped declaration leaves to the \
         cleanup: {actions:?}"
    );
    assert!(
        !actions.iter().any(
            |action| action.contains("leave openclaw plugin 'memory-core' alone")
                && action.contains("never disabled it")
        ),
        "and must not describe the skip the stopped attempt would have guessed at: \
         {actions:?}"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the re-enable performs what the plan promised");
    assert!(
        argv_contains(
            &argv_appended(&world, logged_before),
            "plugins enable memory-core"
        ),
        "so the preview is exactly as wide as the operation and no wider"
    );
}

/// A second transient read failure must not spend an attribution the first read
/// already settled.
///
/// The half of the dropped-displacement gate that
/// `review_dropped_displacement_keeps_an_unreadable_handoff_out_of_the_replacement_receipt`
/// does not reach. That test fails the *first* `config get plugins.slots.memory`
/// the re-enable issues, which is the one `attribute_dropped_prior_handoffs` asks,
/// and the gate stops the operation before it mutates anything. This one lets that
/// read succeed and fails the *second* — the one `restore_decision` used to ask
/// again when it decided whether an unapplied entry is a hand-off this adapter
/// performed. Asking the host twice is two windows, not one: a failure in the
/// second collapsed into `RestoreDecision::SkipNotApplied`, the restore was
/// reported as never owed, `cleanup_complete` stayed true, and the receipt swap
/// deleted the only record of the ownership. The enable "succeeded" over
/// `displaced_plugins = []` with no `plugins enable memory-core` anywhere in its
/// argv, and nothing about the host explained why — one failed read, no operator,
/// no crash.
///
/// The verdict the gate settled is now handed to the restore instead of being
/// re-asked, so the second window has no attribution left to get wrong. The only
/// read still live in it is the slot *veto*, whose unanswerable answer keeps the
/// restore rather than declining it.
#[test]
fn independent_dropped_handoff_second_read_failure() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    redeclare_displacement(&world, &plugin_adapter_block(None));

    // The first `config get plugins.slots.memory` succeeds and settles the
    // attribution as this adapter's own; the second fails.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_ON_NTH", "plugins.slots.memory:2");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager.enable(COMPONENT, Some(FRAMEWORK), false).expect(
        "a read failure inside the restore's own attribution window must not fail \
         an enable whose attribution is already settled",
    );
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the settled attribution has to reach the restore, not be re-asked where a \
         second transient failure can spend it as 'never disabled it': {appended:?}"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and the recovery responsibility has to survive the receipt swap, because \
         this enable's own install re-selects the slot the restore just released"
    );
    assert!(
        persisted_displacement(&world).applied,
        "marked performed, on the evidence the post-install attribution read"
    );

    // The final state — the one assertion a command in the argv cannot stand in
    // for, because argv proves a command ran and not that the plugin ended up on.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_ON_NTH");
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// A contract change, the slot selection that follows it and the recovery record
/// have to describe one final state.
///
/// The ordering half, with no fault injected anywhere: every read answers and every
/// command succeeds. The same-home cleanup restores `memory-core` before
/// `apply_enable` runs, and the install and enable that follow re-select the memory
/// slot for this adapter's own plugin — OpenClaw's exclusive selection turning
/// every other plugin of that kind off again. The restore is undone by the very
/// operation it made room for, and the replacement receipt named nothing, so the
/// disable that followed reported a complete cleanup and removed itself over a
/// bundled backend still switched off. A test that asserted only "`plugins enable
/// memory-core` ran" and "the new receipt is empty" passed over that host, because
/// both halves are true of it.
///
/// Moving the restore after the install is not the fix and cannot be: `plugins
/// enable memory-core` re-runs the same selection from the other side, so restoring
/// last would switch *this adapter's own* plugin off and report success over a
/// plugin it had just unseated. What changes is the record. The enable asks the
/// attribution again once its own registration has run and keeps whichever dropped
/// displacements the host shows it having performed again, so the receipt, the host
/// and the disable that follows all describe the same state.
#[test]
fn independent_dropped_handoff_survives_remaining_install() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    redeclare_displacement(&world, &plugin_adapter_block(None));

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a contract that no longer displaces anything");
    let appended = argv_appended(&world, logged_before);

    // The restore still runs first and the install that undoes it still runs. That
    // ordering is not the bug and is not what changed.
    let restore_at = appended
        .iter()
        .position(|line| line == "plugins enable memory-core")
        .expect("the dropped displacement must be handed back before the swap");
    let install_at = appended
        .iter()
        .position(|line| line.starts_with("plugins install ") && !line.contains("--help"))
        .expect("the re-enable installs this adapter's own plugin");
    assert!(
        restore_at < install_at,
        "the restore has to precede the registration that re-selects the slot: \
         {appended:?}"
    );

    // ... so the host after the enable holds a plugin this adapter turned off, and
    // the receipt is what has to say so.
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "the install's own slot selection turns the restored plugin back off"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and the replacement receipt keeps the responsibility for it instead of \
         letting the swap delete the only record"
    );
    assert!(
        persisted_displacement(&world).applied,
        "marked performed, because this enable's own selection is what performed it"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the restore the retained entry makes possible: {appended:?}"
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the final state is a bundled backend that is really back on, not a report \
         claiming the cleanup was complete over one that is off"
    );
}

/// An install that fails *after* its own slot selection ran still owes the recovery.
///
/// The failure-window half of the duty
/// [`independent_dropped_handoff_survives_remaining_install`] proves for a successful
/// enable, and the half a confirmation recorded only on success cannot reach.
/// `plugins install` is not an atomic step: it registers the bundle and applies
/// OpenClaw's exclusive slot selection before the exit status is decided, so a
/// non-zero exit is a report about a command that has already turned the dropped
/// plugin back off. The same-home cleanup had handed `memory-core` back, the receipt
/// the Manager swapped in before `apply_enable` named nothing to restore, and the
/// candidates lived only in `PreparedEnable` — which dies with the process. The
/// `disable` an operator ran next reported `cleanup_complete`, removed the receipt,
/// and left the bundled backend off with nothing recording why.
///
/// Staging the candidates as unapplied entries *before* the install is what closes
/// it: the record exists ahead of the command that can invalidate it, and the
/// disable's own attribution recovers the ownership from the capture it takes before
/// its uninstall destroys it.
#[test]
fn review_dropped_handoff_survives_install_error_after_registration() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    redeclare_displacement(&world, &plugin_adapter_block(None));

    // Register, select the slot, *then* fail: the mutation has already happened by
    // the time the driver reads the exit status.
    guard.set("FAKE_OPENCLAW_FAIL", "install_after_register");
    let logged_before = argv_lines(&world.argv_log()).len();
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an install that exits non-zero must fail the enable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        format!("{err}").contains("boom-after-register"),
        "and must fail on that install: {err}"
    );
    assert!(
        appended
            .iter()
            .any(|line| line.starts_with("plugins install ") && !line.contains("--help")),
        "the install really ran: {appended:?}"
    );

    // The side effect the failure is a report about: the cleanup's restore was
    // undone by the very command that then failed, and the slot proves it was this
    // adapter's own selection that did it.
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "the install's slot selection turns the restored plugin back off"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and names this adapter's own plugin, which is the attribution a later \
         disable recovers the ownership from"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "the recovery responsibility has to survive an install that failed after it \
         mutated, which is what staging it before the command is for"
    );
    assert!(
        !persisted_displacement(&world).applied,
        "staged rather than claimed: the enable never reached the pass that could \
         confirm it, and an unconfirmed entry must not read as ownership"
    );

    // Whatever broke the install is over. The record left behind has to be enough.
    guard.unset("FAKE_OPENCLAW_FAIL");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the restore the staged entry makes possible: {appended:?}"
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the final state is a bundled backend that is really back on, not a report \
         of a complete cleanup over one that is off"
    );
}

/// The contrast that keeps the staging honest: an install that fails *before* it
/// mutates must leave a candidate nobody can spend.
///
/// Staging is write-ahead, so a failed enable leaves the entry in the receipt either
/// way — the two windows are told apart by the attribution, not by the record. Here
/// the cleanup's restore held and nothing turned the plugin back off, so the slot
/// names the plugin itself and the flag reads on: the disable recovers no ownership,
/// restores nothing, and says so. Recording the candidate must not become claiming
/// it, or an operator who never lost the backend is handed a `plugins enable` for a
/// plugin they may since have closed themselves.
#[test]
fn review_dropped_handoff_is_not_claimed_when_the_install_never_mutated() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    redeclare_displacement(&world, &plugin_adapter_block(None));

    // Exit before the registration and therefore before the slot selection.
    guard.set("FAKE_OPENCLAW_FAIL", "install");
    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an install that exits non-zero must fail the enable");
    assert!(
        format!("{err}").contains("boom-install"),
        "and must fail on that install: {err}"
    );

    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the cleanup's restore held: nothing turned the plugin back off"
    );

    // Read the receipt the failed enable left behind, but assert on it only after
    // the behavioural payoff below, so that a different mechanism that gets the
    // behaviour right does not fail this test.
    let staged = persisted_displacements(&world);

    guard.unset("FAKE_OPENCLAW_FAIL");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "a staged candidate is not ownership: {appended:?}"
    );
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("never disabled it")),
        "and the report must say why nothing was restored: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the plugin is still on, exactly as the cleanup left it"
    );

    // The mechanism: write-ahead stages the candidate before the install either
    // way, so the two windows are told apart by the mark and not by the record.
    assert_eq!(
        staged,
        vec![(
            "memory-core".to_string(),
            Some("memory".to_string()),
            None,
            false
        )],
        "staged, and unapplied because no mutation performed the hand-off"
    );
}

/// A contract that moves one exclusive slot from one bundled plugin to another owes
/// *both* plugins a recovery, and the receipt has room for both.
///
/// No fault injected anywhere: every read answers and every command succeeds. The
/// same-home cleanup hands `memory-core` back, the install and enable that follow
/// re-select the memory slot for this adapter's own plugin and turn it off again
/// together with the newly declared `memory-lancedb`, and the replacement receipt
/// named only the new one. The dropped entry used to be skipped at that point
/// because the receipt refuses two entries sharing one exclusive slot — and skipping
/// it made an unsupported contract change *succeed*: the disable that followed
/// reported `cleanup_complete` and removed itself over a host whose memory slot had
/// been reset to `memory-core` by the uninstall, so the veto stepped aside for
/// `memory-lancedb` too and nothing was left behind any backend.
///
/// What the entry loses is the *ownership* of the slot, not the responsibility and
/// not the veto. One exclusive slot, one entry owning it — the one the current
/// contract declares. But the plugin this entry restores still declares that slot's
/// kind, so `plugins enable` on it re-runs OpenClaw's selection over the same key
/// whichever entry owns it, and the receipt therefore keeps the key as a guard:
/// [`review_same_slot_replacement_restore_keeps_an_operator_closed_slot`] and
/// [`review_same_slot_replacement_restore_keeps_a_third_plugin_selection`] are the
/// two choices that guard protects. The plugin id and the ownership of the
/// transition are what a restore acts on, and both survive.
#[test]
fn review_same_slot_replacement_keeps_old_recovery_responsibility() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    // Same kind as the bundle and as the plugin being dropped, so the host really
    // does write the slot both of them compete for.
    seed_bundled_plugin_with_kind(&world, "memory-lancedb", "memory");
    redeclare_displacement(
        &world,
        &plugin_adapter_block_with_displacement("memory-lancedb", Some("memory")),
    );

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a contract that moved the slot to another plugin");

    let entries = persisted_displacements(&world);
    assert_eq!(
        entries
            .iter()
            .map(|(plugin_id, _, _, _)| plugin_id.as_str())
            .collect::<Vec<_>>(),
        vec!["memory-lancedb", "memory-core"],
        "the replacement receipt keeps both responsibilities instead of letting the \
         slot collision delete the older one: {entries:?}"
    );
    let lancedb = entries
        .iter()
        .find(|(id, _, _, _)| id == "memory-lancedb")
        .unwrap();
    assert_eq!(
        lancedb.1.as_deref(),
        Some("memory"),
        "the declared plugin keeps the slot the current contract names: {entries:?}"
    );
    assert_eq!(
        lancedb.2, None,
        "an entry that owns its slot consults that one and records no separate \
         guard: {entries:?}"
    );
    assert!(lancedb.3, "and the hand-off it performed: {entries:?}");
    let core = entries
        .iter()
        .find(|(id, _, _, _)| id == "memory-core")
        .unwrap();
    assert_eq!(
        core.1, None,
        "a dropped plugin the contract no longer declares has no contract slot, and \
         the receipt cannot give one exclusive slot to two entries: {entries:?}"
    );
    assert_eq!(
        core.2.as_deref(),
        Some("memory"),
        "but it still competes for that slot, so the receipt keeps the key its \
         restore has to consult: {entries:?}"
    );
    assert!(
        core.3,
        "the dropped plugin's hand-off is still owned, which is the whole point of \
         keeping the entry: {entries:?}"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the restore the retained entry makes possible: {appended:?}"
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "and the host ends with a backend behind the slot, not with every plugin of \
         that kind switched off"
    );
}

/// Stage the world the two operator-choice tests below start from: one receipt whose
/// declared displacement took the memory slot from an older one, so the older plugin's
/// entry survives in that receipt without owning the slot it still competes for.
///
/// The fixture half of
/// [`review_same_slot_replacement_keeps_old_recovery_responsibility`], which asserts the
/// receipt shape this leaves behind; the two tests below assert what a `disable` does
/// with that shape once an operator has chosen something else for the slot.
fn stage_same_slot_replacement(guard: &OpenClawEnvGuard) -> (World, AdapterManager) {
    let (world, manager) = stage_with_unmarked_handoff(guard);
    // Same kind as the bundle and as the plugin being dropped, so the host really does
    // write the slot all three of them compete for.
    seed_bundled_plugin_with_kind(&world, "memory-lancedb", "memory");
    redeclare_displacement(
        &world,
        &plugin_adapter_block_with_displacement("memory-lancedb", Some("memory")),
    );
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a contract that moved the slot to another plugin");

    assert_eq!(
        persisted_displacements(&world)
            .iter()
            .map(|(id, slot, guard_slot, applied)| (
                id.as_str(),
                slot.as_deref(),
                guard_slot.as_deref(),
                *applied
            ))
            .collect::<Vec<_>>(),
        vec![
            ("memory-lancedb", Some("memory"), None, true),
            ("memory-core", None, Some("memory"), true),
        ],
        "fixture must leave both responsibilities in one receipt, the older one guarding \
         the slot the newer one owns"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and must leave the slot on this adapter's own plugin, so whatever it names \
         afterwards is an operator's choice and not the enable's"
    );
    (world, manager)
}

/// An operator who closed the memory slot after the displacement still has it closed
/// once the cleanup is done.
///
/// The half of a same-slot replacement the receipt shape cannot show. Both entries name
/// `plugins.slots.memory`: the declared one owns it, the older one guards on it. A
/// `disable` that read only the ownership vetoed the declared entry, correctly, and then
/// restored the older one *unguarded* — `plugins enable memory-core` re-runs OpenClaw's
/// slot selection, which wrote the slot back to `memory-core` over the operator's
/// explicit `none`, and the report called that a complete cleanup. Dropping the slot
/// metadata had not dropped the side effect, and the guard the declared entry carries
/// does not cover a restore of a different plugin.
#[test]
fn review_same_slot_replacement_restore_keeps_an_operator_closed_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_same_slot_replacement(&guard);

    // The operator closes the slot: no memory backend at all, chosen on purpose.
    set_config_answer(&world, "plugins.slots.memory", "none");

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    for forbidden in [
        "plugins enable memory-core",
        "plugins enable memory-lancedb",
    ] {
        assert!(
            !argv_contains(&appended, forbidden),
            "neither entry may re-open a slot the operator closed: {appended:?}"
        );
    }
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("plugins.slots.memory is explicitly 'none'")),
        "and the report must name the choice the retained entry stepped aside for \
         rather than stay quiet about it: {:?}",
        outcome.report.messages
    );
    assert!(
        outcome.report.cleanup_complete,
        "a veto is a release, so the cleanup is still complete: {:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("none"),
        "the operator's choice is what the host still says"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "and the cleanup left the operator the plugin it declined to re-enable, \
         instead of re-opening it behind them"
    );
}

/// The same replacement with a third plugin behind the slot: that selection survives the
/// cleanup too.
///
/// The other half of the reproduction, and the reason the veto cannot be a special case
/// for an explicitly closed slot. `plugins enable memory-core` does not choose between
/// "closed" and "open" — it re-runs OpenClaw's selection, which writes the slot to the
/// plugin being enabled over whoever holds it, so a selection made *for* somebody else is
/// overwritten exactly as a selection made against everybody is.
#[test]
fn review_same_slot_replacement_restore_keeps_a_third_plugin_selection() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_same_slot_replacement(&guard);

    // The operator moved the slot to a plugin this receipt has never claimed.
    seed_bundled_plugin_with_kind(&world, "memory-third", "memory");
    set_config_answer(&world, "plugins.slots.memory", "memory-third");

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    for forbidden in [
        "plugins enable memory-core",
        "plugins enable memory-lancedb",
    ] {
        assert!(
            !argv_contains(&appended, forbidden),
            "neither entry may take the slot back from its current owner: {appended:?}"
        );
    }
    assert!(
        outcome
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("now belongs to 'memory-third'")),
        "and the report must name the owner the retained entry stepped aside for: {:?}",
        outcome.report.messages
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("memory-third"),
        "the operator's selection is what the host still says"
    );
}

/// A guard is not a one-replacement memory: the responsibility it belongs to outlives
/// the contract change that first took the slot away from it.
///
/// The staging pass reads the prior entry's slot to decide what the replacement can
/// hold, and a prior entry that already gave its slot up has none to read. Carrying only
/// the ownership through [`DroppedPriorDisplacement`] would therefore stage the second
/// replacement's entry slotless *and* guardless, silently restoring the unguarded
/// restore one contract change later than the one that removed it — no fault, no
/// operator, nothing in the report to say the veto went anywhere.
#[test]
fn a_second_replacement_keeps_the_guard_the_first_one_recorded() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_same_slot_replacement(&guard);

    // A third contract, declaring no displacement at all: both entries are dropped, and
    // the older of them has already given its slot up once.
    redeclare_displacement(&world, &plugin_adapter_block(None));
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable under a contract that declares no displacement");

    assert_eq!(
        persisted_displacements(&world)
            .iter()
            .map(|(id, slot, guard_slot, applied)| (
                id.as_str(),
                slot.as_deref(),
                guard_slot.as_deref(),
                *applied
            ))
            .collect::<Vec<_>>(),
        vec![
            ("memory-lancedb", Some("memory"), None, true),
            ("memory-core", None, Some("memory"), true),
        ],
        "the second replacement owes the same two responsibilities, and the older one \
         still names the key it competes for"
    );

    // And the key it kept is still a veto rather than decoration.
    set_config_answer(&world, "plugins.slots.memory", "none");
    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "a guard carried through a second replacement must still guard: {appended:?}"
    );
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("none"),
        "and the operator's choice is still what the host says"
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
}

/// An entry that gave its slot up is still recoverable from a `disable`'s own capture.
///
/// The recovery the receipt shape above makes possible on the disable side, and the one
/// shape an earlier version of this driver documented as terminal: an unapplied entry
/// with no slot of its own had no key for the pre-uninstall capture to be read against,
/// so the attribution answered "the contract declared no slot", the restore concluded
/// this adapter never disabled the plugin, and the report said so over a plugin this
/// adapter's own selection really had turned off. Recovering it needed a receipt that
/// could hold a key alongside another entry's ownership of that key, which is what a
/// guard slot is.
///
/// Forged rather than fault-injected: the receipt is the state under test, and an enable
/// that cannot read the slot fails before it can stage anything.
#[test]
fn disable_recovers_an_unmarked_handoff_whose_slot_another_entry_owns() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_same_slot_replacement(&guard);
    // The mark is what a real enable could not write: the host performed the hand-off,
    // the slot read that would have shown it did not answer.
    forge_displaced_references(&world, |entries| {
        let core = entries
            .iter_mut()
            .find(|entry| entry.slot.is_none() && entry.guard_slot.as_deref() == Some("memory"))
            .expect("fixture must leave the dropped entry guarding the memory slot");
        core.applied = false;
    });
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some(COMPONENT),
        "and the host must still show this adapter's own selection, which is the \
         corroborating half of the attribution"
    );
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "over a plugin that is off"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    let outcome = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the capture must have recovered the hand-off the mark missed: {appended:?}"
    );
    assert!(
        !outcome
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("never disabled it")),
        "and must not disown it in the same breath: {:?}",
        outcome.report.messages
    );
    assert!(
        outcome.report.cleanup_complete,
        "{:?}",
        outcome.report.messages
    );
    assert!(outcome.claim_removed);
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// Move the world's OpenClaw state directory to a fresh home, the way a resolver
/// change does, and declare the bundled plugin there so the current contract's
/// displacement declaration still resolves.
///
/// Returns the new home. The prior receipt keeps naming the old one, which is what
/// makes the re-enable a migration and its cleanup a full `disable` of the prior
/// receipt against the prior home.
fn migrate_state_dir(guard: &OpenClawEnvGuard, world: &World) -> PathBuf {
    let new_home = world.root().join("configured-openclaw-state");
    for dir in ["registry", "config"] {
        std::fs::create_dir_all(new_home.join(dir)).expect("new home dir");
    }
    std::fs::write(new_home.join("registry").join("memory-core"), "memory")
        .expect("bundled plugin in the new home");
    guard.set("OPENCLAW_STATE_DIR", &new_home);
    new_home
}

/// A migration preview must fail where the migration it describes fails, on either
/// half of the attribution.
///
/// Shared by the two tests below because they are one question asked of the two
/// readings the attribution is built from, and guarding one without the other
/// guards neither.
///
/// The cross-home branch of `cleanup_replaced_claim` is `return self.disable(prior,
/// ctx, progress)`, so it inherited the attribution gate the last round gave the
/// real disable — and `plan_reenable_cleanup`'s cross-home branch kept calling
/// `restore_preview_lines` directly, because the gate had been added to
/// `plan_disable_restores` rather than to something both previews share. The result
/// was a dry-run that listed the prior home's uninstall, described the unapplied
/// entry as a hand-off that never happened and needed no restore, and then an
/// enable that stopped in the prior-home cleanup with "cannot read the evidence".
/// An operator planning a migration was told the one thing the migration would not
/// do, and told it by the command whose job is to say what the migration will do.
fn assert_migration_preview_fails_like_the_real_cleanup(
    world: &World,
    manager: &AdapterManager,
    guard: &OpenClawEnvGuard,
    fail_key: &str,
) {
    // 1. Both halves of the operation and of the plan, read from one host state and
    //    in the order an operator runs them.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", fail_key);
    let logged_before = argv_lines(&world.argv_log()).len();
    let plan_err = manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect_err("a migration plan must fail where the migration it describes fails");
    assert_dry_run_only_probed(&argv_appended(world, logged_before));

    let logged_before = argv_lines(&world.argv_log()).len();
    let real_err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("an unreadable attribution must stop the migration cleanup");
    assert_dry_run_only_probed(&argv_appended(world, logged_before));

    for (which, err) in [("plan", &plan_err), ("operation", &real_err)] {
        let reason = err.to_string();
        assert!(
            reason.contains("cannot read the evidence"),
            "the {which} must fail on the attribution, not on something else: {reason}"
        );
        assert!(
            reason.contains("memory-core"),
            "and must name the plugin whose ownership it could not settle: {reason}"
        );
    }

    // 2. Nothing ran, in the prior home or in the new one, so both the evidence and
    //    the receipt the migration would have replaced are still there.
    assert!(
        world.has_claim(),
        "the stopped migration must keep the prior receipt"
    );
    assert_eq!(
        persisted_displacement_ids(world),
        vec!["memory-core".to_string()],
        "still naming the displacement it is the only record of"
    );
    assert!(
        !persisted_displacement(world).applied,
        "and still unmarked, so the retry is asked the same question"
    );
    assert!(
        world.registry_marker_exists(),
        "the prior installation must be untouched: {real_err}"
    );
    assert_eq!(
        config_answer(world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("false"),
        "and the bundled backend must be exactly as the failed enable left it"
    );

    // 3. With the host answering, the plan promises the prior-home restore and the
    //    migration performs it — so the gate is exactly as wide as the operation.
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    let actions = planned_actions(
        manager
            .enable(COMPONENT, Some(FRAMEWORK), true)
            .expect("the migration plan completes once the host answers"),
    );
    assert!(
        actions.iter().any(|action| action
            .contains("would re-enable openclaw plugin 'memory-core'")
            && action.contains("in the prior state directory")),
        "the plan must describe the restore the prior-home cleanup performs: \
         {actions:?}"
    );
    assert!(
        !actions.iter().any(
            |action| action.contains("leave openclaw plugin 'memory-core'")
                && action.contains("never disabled it")
        ),
        "and must not call the unmarked hand-off one that never happened, which is \
         what it said while the host could not answer: {actions:?}"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the migration succeeds once the host answers the attribution");
    assert!(
        argv_contains(
            &argv_appended(world, logged_before),
            "plugins enable memory-core"
        ),
        "and must really hand the bundled backend back in the prior home"
    );
}

/// The slot half: the prior home's `config get plugins.slots.memory` stops
/// answering, so the attribution has no reading to reason from.
///
/// This is the reviewer's repro — the unmarked-handoff fixture moved to a new state
/// directory, with the bundled plugin declared there so the current contract's
/// displacement still resolves and the migration is one the trust boundary admits.
#[test]
fn review_migration_preview_fails_like_the_real_cleanup_on_unreadable_slot() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    migrate_state_dir(&guard, &world);
    assert_migration_preview_fails_like_the_real_cleanup(
        &world,
        &manager,
        &guard,
        "plugins.slots.memory",
    );
}

/// The enablement half: the slot reads perfectly well and the live `config get
/// plugins.entries.memory-core.enabled` that corroborates it does not.
///
/// Neither half carries the attribution on its own, so a migration plan that guards
/// only the slot still describes a restore the operation refuses to perform.
#[test]
fn review_migration_preview_fails_like_the_real_cleanup_on_unreadable_enablement_flag() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_with_unmarked_handoff(&guard);
    migrate_state_dir(&guard, &world);
    assert_migration_preview_fails_like_the_real_cleanup(
        &world,
        &manager,
        &guard,
        "plugins.entries.memory-core.enabled",
    );
}

/// Name of the child half of the crash test below. The parent invokes it by
/// string through the test binary, so the two cannot drift silently — the parent
/// asserts the child died by SIGKILL, which only the real child can.
const DISABLE_CHILD_TEST: &str = "disable_child_killed_between_the_uninstall_and_the_save";

/// Staged-world prefix the parent hands that child, which reopens the world from
/// disk rather than inheriting it in memory.
const DISABLE_CHILD_ROOT_ENV: &str = "ANOLISA_TEST_OPENCLAW_WORLD_ROOT";

/// A hand-off attribution a `disable` recovers must be on disk before the
/// `plugins uninstall` that destroys the evidence for it — not merely before
/// `disable` returns.
///
/// `failed_restore_keeps_the_recovered_handoff_retryable` proves the recovery
/// outlives a failed restore, but it proves it the comfortable way: the restore
/// fails, control returns to the Manager, and the Manager's own save writes the
/// mark. The window that save cannot reach is the one between the destructive host
/// mutation and itself, and it is the window a crash actually lands in. This
/// reproduces it with a real kill instead of a modelled one: the disable runs in a
/// child process, and the fake CLI SIGKILLs that child from inside its own
/// `plugins uninstall` handler, immediately after the config rewrite that resets
/// `plugins.slots.memory` to `memory-core`. Nothing the child had not already
/// persisted survives, so what is on disk afterwards is exactly what a crash there
/// leaves behind.
///
/// Without a write-ahead save the receipt still reads `applied = false` while the
/// host says the slot belongs to the bundled plugin again: of the two facts the
/// attribution was recovered from, one is destroyed and the other was never
/// recorded. The retry then has nothing to recover from, reports that this adapter
/// never disabled `memory-core`, removes the receipt over a bundled backend still
/// switched off, and the stranded host the capture exists to prevent arrives one
/// attempt later with nothing done in between.
#[test]
fn disable_persists_a_recovered_handoff_before_the_uninstall_destroys_it() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    // Both sides declare `kind = "memory"`, which is what makes the host write the
    // slot the attribution is read from at all.
    declare_bundle_plugin_kind(&world, "memory");
    seed_bundled_plugin_with_kind(&world, "memory-core", "memory");
    world.apply_env(&guard, None);
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());

    // An enable that cannot read the slot, and then fails verification: the host
    // performed the hand-off and the receipt could not record it.
    guard.set("FAKE_OC_CONFIG_GET_FAIL_KEY", "plugins.slots.memory");
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let manager = world.manager();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a non-loaded runtime status must fail the enable");
    guard.unset("FAKE_OC_CONFIG_GET_FAIL_KEY");
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    assert!(
        !persisted_displacement(&world).applied,
        "fixture must leave the mark unwritten, so the disable below has to recover it"
    );

    // The crash. A child process runs the disable and is SIGKILLed from inside the
    // fake CLI's uninstall handler, so no handler of ours — the driver's or the
    // Manager's — runs again in it, and the flock it held dies with it.
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([DISABLE_CHILD_TEST, "--exact", "--ignored", "--nocapture"])
        .env(DISABLE_CHILD_ROOT_ENV, world.root())
        .env("OPENCLAW_BIN", &world.fake_bin)
        .env("OPENCLAW_HOME", &world.openclaw_home)
        .env("FAKE_OC_ARGV_LOG", world.argv_log())
        .status()
        .expect("run the child disable");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "the child must have been killed by the fake CLI mid-cleanup, not merely \
         failed in a way the Manager could have reported around: {status}"
    );

    // The host mutation the kill landed after. This is what makes the window real
    // rather than hypothetical: the reading the attribution came from is gone, and
    // this adapter's own plugin — the only thing that could have been asked about
    // it — is gone with it.
    assert_eq!(
        config_answer(&world, "plugins.slots.memory").as_deref(),
        Some("memory-core"),
        "fixture: the uninstall really ran and really reset the slot"
    );
    assert!(
        !world.registry_marker_exists(),
        "fixture: and it really removed this adapter's own plugin"
    );

    // The substance of the fix: the recovered ownership outlived the process that
    // recovered it, because the driver wrote it down before the uninstall rather
    // than leaving it to a save that the kill never reached.
    let leftover = persisted_displacement(&world);
    assert!(
        leftover.applied,
        "a hand-off attribution recovered from a pre-uninstall capture must be \
         durable before that uninstall, or a crash in between loses the only record \
         that this adapter turned the bundled plugin off: {leftover:?}"
    );
    assert_eq!(
        persisted_displacement_ids(&world),
        vec!["memory-core".to_string()],
        "and the receipt the crash left behind is still there to retry from"
    );

    // The retry that receipt now honestly advises, over a host that holds no
    // evidence of the hand-off at all.
    let logged_before = argv_lines(&world.argv_log()).len();
    let retry = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the retry completes the restore the killed attempt owed");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the retry must hand the bundled backend back rather than give up on an \
         attribution the previous attempt recorded: {appended:?}"
    );
    assert!(
        !retry
            .report
            .messages
            .iter()
            .any(|m| m.contains("never disabled it")),
        "and must not disown a hand-off the killed attempt had already recovered: {:?}",
        retry.report.messages
    );
    assert!(
        retry.claim_removed,
        "the cleanup converges, so the receipt goes: {:?}",
        retry.report.messages
    );
    assert!(!world.has_claim());
    assert_eq!(
        config_answer(&world, "plugins.entries.memory-core.enabled").as_deref(),
        Some("true"),
        "the bundled backend must really be back on"
    );
}

/// The child half of
/// [`disable_persists_a_recovered_handoff_before_the_uninstall_destroys_it`]: it
/// reopens the world the parent staged and runs the disable that the fake CLI
/// SIGKILLs it in the middle of.
///
/// `#[ignore]`d because it is only meaningful as that child. Reaching either
/// `panic!` below means the kill never fired, which the parent reports as a
/// missing SIGKILL rather than as a failure here.
#[test]
#[ignore = "child process of disable_persists_a_recovered_handoff_before_the_uninstall_destroys_it"]
fn disable_child_killed_between_the_uninstall_and_the_save() {
    let root = std::env::var_os(DISABLE_CHILD_ROOT_ENV).map_or_else(
        || panic!("must be run by the parent test, which passes {DISABLE_CHILD_ROOT_ENV}"),
        PathBuf::from,
    );
    // Reopen the staged world from the prefix alone: everything the parent built
    // under it is on disk, and the env contract is re-applied here because env is
    // per process and the fake CLI reads it in this one.
    let layout = FsLayout::system(Some(root.clone()));
    let user_home = root.join("home");
    let openclaw_home = root.join("openclaw-home");
    let fake_bin = write_fake_openclaw(&root);
    let resource_root = layout
        .datadir
        .join("adapters")
        .join(COMPONENT)
        .join(FRAMEWORK);
    record_owned_adapter_files(&layout, &resource_root);
    // SAFETY: invoked with `--exact --ignored`, so this process runs this one test
    // and no other thread reads the OpenClaw env contract.
    unsafe {
        std::env::set_var("OPENCLAW_BIN", &fake_bin);
        std::env::set_var("OPENCLAW_HOME", &openclaw_home);
        std::env::set_var("FAKE_OC_ARGV_LOG", root.join("argv.log"));
        // Aim the fake CLI's kill at this process, from inside its own
        // `plugins uninstall` handler.
        std::env::set_var(
            "FAKE_OC_KILL_PID_AFTER_UNINSTALL",
            std::process::id().to_string(),
        );
    }
    let manager = AdapterManager::new(layout, Some(user_home), "tester".to_string());
    match manager.disable(COMPONENT, Some(FRAMEWORK), false) {
        Ok(_) => panic!(
            "the disable returned; the fake CLI was supposed to SIGKILL this process \
             from inside `plugins uninstall`"
        ),
        Err(err) => panic!(
            "the disable failed before reaching `plugins uninstall`, so nothing about \
             the crash window was exercised: {err}"
        ),
    }
}

/// The same window, but on a host where nothing changes: the claim stands and the
/// hand-off still happens. Without this the re-confirmation could pass by simply
/// never claiming anything.
#[test]
fn enable_still_claims_a_displacement_when_nothing_changed_midway() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, _manager) = stage_enabled_with_displacement(&guard);
    let lines = argv_lines(&world.argv_log());
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "an uncontested displacement must still be claimed and applied: {lines:?}"
    );
    assert!(displaced_marker_exists(&world, "memory-core"));
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(payload.displaced_plugins.len(), 1);
}

/// A transient probe failure during a re-enable must not cost the ownership the
/// *prior* receipt established.
///
/// The sequence this pins: `prepare_enable`'s one read of
/// `plugins.entries.memory-core.enabled` fails, so the plugin's state is unknown.
/// It is still claimed — skipping the claim would strand the host — but the claim
/// is not attributable to this enable, so it must not be re-confirmed later.
/// `preserve_openclaw_displaced_facts` declines to re-add the prior fact because
/// the fresh claim already occupies the same resource, and by the time
/// `apply_displacements` runs the probe answers again: `false`, because a previous
/// enable of this same adapter is what turned it off. Reading that as "somebody
/// else just closed it" deletes the claim from the replacement receipt, the prior
/// receipt is replaced, and the ownership is gone for good — `adapter disable`
/// then never hands the plugin back and still removes the receipt.
#[test]
fn reenable_keeps_prior_ownership_through_a_transient_probe_failure() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the prior enable really did disable it"
    );

    // Exactly one failed read of the enablement key: prepare's.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ONCE",
        "plugins.entries.memory-core.enabled",
    );
    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable survives a transient probe failure");

    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "ownership established by the prior receipt must survive the re-enable: {:?}",
        payload.displaced_plugins
    );
    assert_eq!(
        payload.displaced_plugins[0].resource,
        "openclaw_displaced_plugin_memory-core"
    );

    // And the payoff the reviewer asked to see: disable really does hand it back.
    let logged_before = argv_lines(&world.argv_log()).len();
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the restored ownership must mean a real restore: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the plugin must actually be enabled again"
    );
    assert!(disabled.claim_removed);
    assert!(!world.has_claim());
}

/// The same transient probe failure, but with the re-enable failing *before* the
/// hand-off — which is the case the successful re-enable above cannot reach.
///
/// `prepare_enable` writes a fresh entry for the resource, and it is always
/// unapplied, because prepare runs before any mutation. `preserve_reenable_facts`
/// then declines to re-add the prior entry, since the fresh one already occupies
/// that resource — and the prior entry was the only record that the hand-off had
/// actually run. Unless the mark is merged across, a re-enable that fails during
/// install, config or runtime verification leaves a `cleanup_failed` receipt whose
/// displacement reads as never performed, `restore_decision` answers
/// `SkipNotApplied`, and a later disable refuses to hand back a plugin the *first*
/// enable really did disable — while removing the receipt, so nothing records it.
///
/// Note the reachability: this only bites when `apply_enable` fails before
/// `apply_displacements`. On the success path the mark is re-established there,
/// which is why the test above passes either way.
#[test]
fn failed_reenable_keeps_the_applied_ownership_it_replaced() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the prior enable really did disable it"
    );

    // prepare's one read of the enablement key fails, so the claim is unverified
    // and inherited rather than re-confirmed; and the re-enable then fails at
    // runtime verification, before `apply_displacements` can re-establish anything.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ONCE",
        "plugins.entries.memory-core.enabled",
    );
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let logged_before = argv_lines(&world.argv_log()).len();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail the re-enable");

    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "replacing the receipt must not restore a displacement it is carrying \
         over: {appended:?}"
    );

    // Read what the failed re-enable left behind. Only the *mechanism* assertion is
    // deferred until after the payoff below — how the driver keeps the ownership is
    // its business, and a different mechanism that gets the behaviour right should
    // not fail this test — while `CleanupFailed` is asserted here because it is part
    // of the staging: the receipt has to be the retryable one for the rest to mean
    // anything.
    let leftover = {
        let state = world.load_state();
        let claim = state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("a failed re-enable must leave a retryable receipt behind");
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
        let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
            panic!("expected OpenClaw receipt payload");
        };
        payload.displaced_plugins.clone()
    };

    // The payoff: disable still hands back what the *first* enable took.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    let logged_before = argv_lines(&world.argv_log()).len();
    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "ownership established by the first enable must still mean a real \
         restore: {appended:?}"
    );
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "and the plugin must actually be enabled again"
    );
    assert!(disabled.claim_removed);
    assert!(!world.has_claim());

    // The mechanism: the displacement survived, and still reads as performed.
    assert_eq!(
        leftover.len(),
        1,
        "the displacement must survive the failed re-enable: {leftover:?}"
    );
    assert!(
        leftover[0].applied,
        "the first enable really did perform this hand-off, and a re-enable that \
         never reached its own must not downgrade that record: {leftover:?}"
    );
}

/// A receipt's displaced-plugin resource id is whatever the receipt says it is, and
/// nothing downstream may re-derive it from the plugin id.
///
/// `claim_displaced_plugins` validates that the reference is unique, that the
/// resource it names exists, and that the resource is a framework plugin of this
/// framework whose `plugin_id` matches — but not that its *name* is the canonical
/// `openclaw_displaced_plugin_<id>`. So a consistently renamed receipt passes every
/// check, and the helpers that rebuilt the id from the plugin id then looked up a
/// resource the receipt does not contain. That is not a fail-closed rejection: by
/// the time `mark_displacement_applied` runs, `apply_enable` has already persisted
/// the new receipt, installed this adapter's own plugin and enabled it, so the
/// failure lands in the middle of a mutation. `status` failed more quietly still,
/// returning no `resource` reference for the one case the reference exists to serve.
#[test]
fn renamed_displacement_resource_survives_status_and_reenable() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    // Rename the resource and its reference together, which is all validation asks
    // for.
    const RENAMED: &str = "openclaw_displaced_memory_slot";
    rename_displacement_resource(&world, RENAMED);

    // Read status while the renamed receipt is still the one on disk — a re-enable
    // rewrites it with canonical ids — but assert on it below, after the more
    // serious symptom: a re-enable that fails part way through a mutation.
    let status = manager.status(Some(COMPONENT)).expect("status");
    let condition = find_displacement_condition(&status);

    // A plain re-enable inherits the entry and completes: marking it must find the
    // id the receipt uses, not the canonical one.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("a renamed resource id is not a reason to fail after mutating");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "the inherited ownership is still off by this adapter's own hand, so the \
         re-enable must not restore it: {appended:?}"
    );

    assert_eq!(
        condition.resource.as_ref().map(|r| r.id.as_str()),
        Some(RENAMED),
        "the single-displacement condition must point at the receipt's own resource, \
         not at one re-derived from the plugin id: {:?}",
        condition.resource
    );

    // And the ownership is still usable afterwards.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins enable memory-core"),
        "the carried ownership must still mean a real restore: {appended:?}"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
    assert!(!world.has_claim());
}

/// A dry-run must preview the host mutation it is about to make — including when
/// that mutation overrides something the operator did *after* the last enable.
///
/// `plan_enable` used to short-circuit on ownership: any displacement the prior
/// receipt owned was previewed as "carries it over, so it stays claimed and disable
/// will hand it back", without reading the plugin's enablement. But `prepare_enable`
/// always probes, so once the operator turns the plugin back on the real enable
/// claims it fresh and runs `plugins disable` — a host mutation the plan had just
/// denied. Both halves are asserted against the same world so the pairing cannot
/// drift: while the plugin is still off the plan promises a carry-over, and once the
/// operator re-enables it the plan says the names will be taken back and the real
/// enable does exactly that.
#[test]
fn reenable_dry_run_previews_a_handoff_the_operator_undid() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);

    // Control: nothing has changed since the receipt was written, so the
    // displacement really is carried over.
    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    assert!(
        plan.actions.iter().any(|action| action
            .contains("keep openclaw plugin 'memory-core' disabled")
            && action.contains("carries it over")),
        "an uncontested displacement is carried over: {:?}",
        plan.actions
    );

    // The operator turns it back on, which voids that ownership.
    operator_enables_plugin(&world, "memory-core");
    assert!(
        !displaced_marker_exists(&world, "memory-core"),
        "the operator's re-enable must be visible before the plan is read"
    );
    let logged_before = argv_lines(&world.argv_log()).len();

    let plan = match manager
        .enable(COMPONENT, Some(FRAMEWORK), true)
        .expect("re-enable plan")
    {
        EnableOutcome::Planned { plan, .. } => plan,
        EnableOutcome::Enabled(_) => panic!("dry-run must return a plan"),
    };
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.contains("carries it over")),
        "the prior ownership is void, so the plan must not promise a carry-over: {:?}",
        plan.actions
    );
    assert!(
        plan.actions.iter().any(|action| action
            .contains("disable openclaw plugin 'memory-core' again")
            && action.contains("was re-enabled after")),
        "and must say the tool names will be taken back, since that overrides a \
         choice the operator made after the last enable: {:?}",
        plan.actions
    );
    // A plan may probe but must not mutate.
    let planned = argv_appended(&world, logged_before);
    assert_dry_run_only_probed(&planned);

    // The pairing: the real enable performs the mutation the plan described.
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins disable memory-core"),
        "the previewed hand-off must be the one actually performed: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and it must really have taken the tool names back"
    );
}

/// A renamed displacement resource is a receipt this driver accepts everywhere else,
/// so it must not become a permanent block on re-enabling.
///
/// `preserve_openclaw_displaced_facts` keyed "does the replacement already claim
/// this?" on the *resource id*. A consistently renamed receipt passes validation and
/// is consumed normally by `status` and `disable`, but the key then did not match the
/// canonical resource `prepare_enable` writes, so the prior entry was added beside
/// the fresh one and `claim_displaced_plugins` rejected the re-enable on a duplicate
/// claim. It failed before any mutation, but it failed on *every* attempt, and the
/// operator's only way out was to disable first — for a receipt we had just told them
/// was fine.
#[test]
fn renamed_displacement_receipt_survives_the_operator_reenabling_the_plugin() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    rename_displacement_resource(&world, "openclaw_displaced_memory_slot");

    // The operator turns the plugin back on, so this round's probe is a positive
    // `ClaimEnabled` and prepare writes a fresh entry under the canonical resource.
    operator_enables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("a renamed resource id must not block the re-enable");

    let appended = argv_appended(&world, logged_before);
    assert!(
        argv_contains(&appended, "plugins disable memory-core"),
        "the operator's re-enable is void, so the hand-off is performed again: {appended:?}"
    );
    assert!(displaced_marker_exists(&world, "memory-core"));

    // One plugin, claimed exactly once, under this round's own canonical resource —
    // the renamed prior entry was recognised as the same plugin rather than added
    // beside it. And applied, because this round really did perform the hand-off.
    let entry = persisted_displacement(&world);
    assert_eq!(entry.resource, "openclaw_displaced_plugin_memory-core");
    assert!(
        entry.applied,
        "this round performed the hand-off, so the receipt must say so: {entry:?}"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(
        argv_contains(
            &argv_appended(&world, logged_before),
            "plugins enable memory-core"
        ),
        "and the fresh ownership must still mean a real restore"
    );
    assert!(!world.has_claim());
}

/// The same renamed receipt with a *transient* probe failure instead of a positive
/// read — the other route to the duplicate, and the one where the prior ownership is
/// genuinely inherited rather than void.
#[test]
fn renamed_displacement_receipt_survives_a_transient_probe_failure() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    rename_displacement_resource(&world, "openclaw_displaced_memory_slot");

    // prepare's one read of the enablement key fails, so the claim is unverified and
    // inherited: prepare writes a fresh entry under the canonical resource, and the
    // prior's renamed entry must be recognized as the same plugin rather than added
    // beside it.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ONCE",
        "plugins.entries.memory-core.enabled",
    );
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("a renamed resource id must not block an inherited re-enable");

    let entry = persisted_displacement(&world);
    assert!(
        entry.applied,
        "the prior receipt performed this hand-off, so the inherited entry must say \
         so: {entry:?}"
    );

    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    assert!(
        argv_contains(
            &argv_appended(&world, logged_before),
            "plugins enable memory-core"
        ),
        "and the inherited ownership must still mean a real restore"
    );
    assert!(!displaced_marker_exists(&world, "memory-core"));
    assert!(!world.has_claim());
}

/// The mirror of the test above, and the reason the mark cannot be merged on a
/// resource match alone.
///
/// Here the prior receipt's ownership is *void*: the operator re-enabled the plugin
/// themselves after the first enable, so this round's probe reads a positive
/// `ClaimEnabled` and the claim it makes is fresh and unperformed. Merging the
/// prior's `applied = true` across anyway — which is what keying on the resource
/// match does, since it cannot see the probe's attribution — makes the
/// `cleanup_failed` receipt claim a hand-off this round never reached. The operator
/// then disables the plugin again, by hand, and `adapter disable` re-enables it:
/// undoing a choice made after the failed enable, with the receipt removed
/// afterwards so nothing records why.
#[test]
fn failed_reenable_does_not_inherit_an_ownership_the_operator_voided() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "the prior enable really did disable it"
    );

    // The operator turns it back on, which voids that ownership: whatever happens
    // to the plugin from here is not the first enable's doing.
    operator_enables_plugin(&world, "memory-core");
    assert!(!displaced_marker_exists(&world, "memory-core"));

    // The re-enable now probes a positively-enabled plugin, and then fails before
    // the hand-off.
    guard.set("FAKE_OC_RUNTIME_STATUS", "error");
    let logged_before = argv_lines(&world.argv_log()).len();
    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("non-loaded runtime status must fail the re-enable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins disable memory-core"),
        "the hand-off never ran: {appended:?}"
    );

    let leftover = {
        let state = world.load_state();
        let claim = state
            .find_adapter_claim(COMPONENT, FRAMEWORK)
            .cloned()
            .expect("a failed re-enable must leave a retryable receipt behind");
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
        let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
            panic!("expected OpenClaw receipt payload");
        };
        payload.displaced_plugins.clone()
    };

    // The operator's own choice, made *after* the failed enable.
    guard.unset("FAKE_OC_RUNTIME_STATUS");
    operator_disables_plugin(&world, "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    let disabled = manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");

    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "disable must never undo a choice the operator made after the failed \
         enable: {appended:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the plugin must still be off, exactly as the operator left it"
    );
    assert!(
        disabled
            .report
            .messages
            .iter()
            .any(|message| message.contains("memory-core")
                && message.contains("never disabled it")),
        "the report must say the hand-off was never performed, not claim a restore \
         it declined to make: {:?}",
        disabled.report.messages
    );
    assert!(disabled.claim_removed);
    assert!(!world.has_claim());

    // The mechanism, asserted last: the voided ownership was not carried across.
    assert_eq!(leftover.len(), 1, "{leftover:?}");
    assert!(
        !leftover[0].applied,
        "a prior hand-off this round's own positive probe invalidated must not be \
         inherited as performed: {leftover:?}"
    );
}

/// The mirror case: a *persistent* probe failure is not a transient one, and the
/// claim still has to survive — the receipt is the only record that this adapter
/// is what turned the plugin off.
#[test]
fn reenable_keeps_prior_ownership_when_the_probe_cannot_run_at_all() {
    let guard = OpenClawEnvGuard::acquire();
    let (_world, manager) = stage_enabled_with_displacement(&guard);
    guard.set("FAKE_OC_PROBE_FAIL", "config_get");

    let outcome = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable with an unanswerable enablement probe");
    let claim = match outcome {
        EnableOutcome::Enabled(claim) => *claim,
        EnableOutcome::Planned { .. } => panic!("expected enabled"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "an unanswerable probe is not evidence that somebody else owns the transition: {:?}",
        payload.displaced_plugins
    );
}

/// The unverified half of the split is not a blanket exemption. An unattributable
/// claim is only un-attributable when there is an older ownership it could belong
/// to; a **first** enable has none, so the plugin going off mid-enable can only be
/// somebody else's doing and the receipt must not claim it.
///
/// This is the takeover window the apply-time re-confirm exists to close, reached
/// through a transient probe failure instead of a positive read: prepare cannot
/// answer, the operator closes the plugin during install, apply skips the
/// re-confirm, `plugins disable` then exits 0 having changed nothing, and a later
/// `adapter disable` re-opens a plugin the operator closed — with nothing left on
/// disk to show why.
#[test]
fn enable_releases_an_unverified_displacement_claim_when_no_prior_owns_it() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    // A first enable, so there is no prior receipt to inherit ownership from.
    assert!(
        !world.has_claim(),
        "this case is about a first enable; a prior receipt would change the answer"
    );
    // prepare's read of the enablement key fails once, and the operator turns the
    // plugin off during install, so apply's read answers `false`.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ONCE",
        "plugins.entries.memory-core.enabled",
    );
    guard.set("FAKE_OC_OPERATOR_DISABLES_ON_INSTALL", "memory-core");
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("the enable itself still succeeds: our own plugin is installed and verified");

    let lines = argv_lines(&world.argv_log());
    assert!(
        !argv_contains(&lines, "plugins disable memory-core"),
        "an unattributable claim with no prior ownership must not be acted on: {lines:?}"
    );
    // Staging proof, so the assertion above cannot pass vacuously: exactly two
    // reads of the enablement key — prepare's, which the one-shot failure consumed,
    // and apply's re-confirm after install. A run that never re-confirmed would
    // read it once and skip the window entirely.
    let key = "config get plugins.entries.memory-core.enabled";
    let reads: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.as_str() == key)
        .map(|(i, _)| i)
        .collect();
    let install = lines
        .iter()
        .position(|line| line.starts_with("plugins install ") && !line.contains("--help"))
        .expect("install ran");
    assert_eq!(
        reads.len(),
        2,
        "exactly prepare's read and apply's re-confirm, so the one-shot failure was          provably consumed by prepare: {lines:?}"
    );
    assert!(
        reads[0] < install && reads[1] > install,
        "the failed read must be prepare's, with apply's re-confirm after install: {lines:?}"
    );

    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "the released claim must be gone from the persisted receipt, not merely skipped: {:?}",
        payload.displaced_plugins
    );
    assert!(
        !claim
            .resources
            .iter()
            .any(|r| r.purpose == "openclaw_displaced_plugin"),
        "and its resource must be dropped with it, or the receipt would not validate: {:?}",
        claim.resources
    );

    // The payoff: disable has nothing to undo, so it does not undo the operator.
    let logged_before = lines.len();
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("disable");
    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins enable memory-core"),
        "disable must never re-enable a plugin the operator turned off: {appended:?}"
    );
    assert!(!world.has_claim());
}

/// The other half of the apply-time release condition, and the half neither a
/// one-shot nor a persistent `config get` failure can reach: prepare reads the
/// plugin **on** — a positive claim, so re-confirmable — and the re-confirm's own
/// read then fails. "Could not read" is not evidence that somebody turned it off,
/// because prepare saw it enabled, so the claim stands and the hand-off still
/// happens. Releasing here would strand the host with nothing behind the slot.
#[test]
fn enable_keeps_a_positive_claim_the_apply_probe_cannot_read() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, None);
    // The enablement key is read exactly twice — prepare, then apply's
    // re-confirm — so failing the second puts the two probes in different states.
    guard.set(
        "FAKE_OC_CONFIG_GET_FAIL_ON_NTH",
        "plugins.entries.memory-core.enabled:2",
    );
    guard.set("FAKE_OC_ARGV_LOG", world.argv_log());
    let manager = world.manager();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("an unreadable re-confirm must not fail the enable");

    let lines = argv_lines(&world.argv_log());
    let key = "config get plugins.entries.memory-core.enabled";
    assert_eq!(
        lines.iter().filter(|l| l.as_str() == key).count(),
        2,
        "prepare's read answered and apply's did not: {lines:?}"
    );
    assert!(
        argv_contains(&lines, "plugins disable memory-core"),
        "the claim prepare positively established must still be acted on: {lines:?}"
    );
    assert!(
        displaced_marker_exists(&world, "memory-core"),
        "and the hand-off must really have happened"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert_eq!(
        payload.displaced_plugins.len(),
        1,
        "an unreadable re-confirm is not evidence of a takeover: {:?}",
        payload.displaced_plugins
    );
}

/// Re-confirming must still release a claim this enable *positively* established
/// when the host says the plugin is off by the time apply runs — and it must do so
/// on a **re-enable**, where a prior receipt exists. That prior ownership is what
/// the round-12 split protects, and it is void here: somebody turned the plugin
/// back on after the prior enable, so this re-enable's own positive read is the
/// only claim in play. Without a prior receipt this case collapses into
/// [`enable_releases_a_displacement_claim_taken_over_midway`]; with one, it is what
/// stops the split from degenerating into "never re-confirm anything".
#[test]
fn reenable_still_releases_a_positive_claim_taken_over_midway() {
    let guard = OpenClawEnvGuard::acquire();
    let (world, manager) = stage_enabled_with_displacement(&guard);
    assert!(
        world.has_claim(),
        "fixture must hand us a prior receipt owning the displacement"
    );
    // The prior receipt still owns the displacement, but the host no longer
    // reflects it: somebody re-enabled the plugin, which voids that ownership.
    // `read_plugin_enablement` reads this key, so it is what the probe sees.
    set_config_answer(&world, "plugins.entries.memory-core.enabled", "true");
    // ... and then closes it again inside this re-enable's window: prepare sees it
    // enabled (a positive claim), apply sees it off.
    guard.set("FAKE_OC_OPERATOR_DISABLES_ON_INSTALL", "memory-core");
    let logged_before = argv_lines(&world.argv_log()).len();

    manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect("re-enable");

    let appended = argv_appended(&world, logged_before);
    assert!(
        !argv_contains(&appended, "plugins disable memory-core"),
        "a prior receipt does not license claiming a transition this enable did not make: {appended:?}"
    );
    let claim = world
        .load_state()
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .cloned()
        .expect("claim");
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        panic!("expected OpenClaw receipt payload");
    };
    assert!(
        payload.displaced_plugins.is_empty(),
        "a positively-established claim that the host contradicts must still be released,          prior receipt or not: {:?}",
        payload.displaced_plugins
    );
}

#[test]
fn failed_displacement_fails_enable_and_keeps_a_retryable_receipt() {
    let guard = OpenClawEnvGuard::acquire();
    let world = stage();
    write_openclaw_manifest(
        &world.layout,
        &plugin_adapter_block_with_displacement("memory-core", Some("memory")),
    );
    seed_bundled_plugin(&world, "memory-core");
    world.apply_env(&guard, Some("disable"));
    let manager = world.manager();

    let err = manager
        .enable(COMPONENT, Some(FRAMEWORK), false)
        .expect_err("a plugin that keeps the tool names must not pass as enabled");
    assert!(
        matches!(err, AdapterError::FrameworkCli { .. }),
        "unexpected error: {err:?}"
    );
    let state = world.load_state();
    let claim = state
        .find_adapter_claim(COMPONENT, FRAMEWORK)
        .expect("receipt kept for cleanup");
    assert_eq!(claim.status, ClaimStatus::CleanupFailed);
    assert!(
        claim
            .resource("openclaw_displaced_plugin_memory-core")
            .is_some()
    );

    guard.unset("FAKE_OPENCLAW_FAIL");
    manager
        .disable(COMPONENT, Some(FRAMEWORK), false)
        .expect("retryable cleanup");
    assert!(!world.has_claim());
}

/// Test doc comments are attached to whatever `#[test]` follows them, and nothing
/// checks that. Inserting a test between a doc block and its `fn` silently moves the
/// documentation onto the new test — legal Rust, green CI, and the two tests end up
/// describing each other's failure mode. That has now happened four times in this
/// file's subject area (a skill-name parser documented as a receipt struct, a
/// disable-plan builder as a validator, two neighbouring helpers in `openclaw.rs`,
/// and the migration-retry explanation landing on the stale-file prune test), so the
/// attachment is asserted directly against the source.
#[test]
fn test_docs_stay_attached_to_their_own_test() {
    let lines: Vec<&str> = include_str!("adapter_manager.rs").lines().collect();

    /// The contiguous `///` block immediately above `#[test] fn <name>`.
    fn doc_above(lines: &[&str], name: &str) -> Vec<String> {
        let needle = format!("fn {name}()");
        let at = lines
            .iter()
            .position(|line| line.contains(&needle))
            .unwrap_or_else(|| panic!("`{needle}` not found in adapter_manager.rs"));
        let mut i = at;
        // step over the attribute(s)
        while i > 0 && (lines[i - 1].trim().starts_with("#[") || lines[i - 1].trim().is_empty()) {
            i -= 1;
        }
        let mut doc = Vec::new();
        while i > 0 && lines[i - 1].trim_start().starts_with("///") {
            doc.push(lines[i - 1].trim().to_string());
            i -= 1;
        }
        doc.reverse();
        doc
    }

    for (name, must_own, must_not_own) in [
        (
            "reenable_materialized_cleanup_failure_mutates_nothing_on_the_host",
            "A stale-file prune that cannot complete",
            "already missing plugin",
        ),
        (
            "migration_cleanup_retry_tolerates_already_missing_plugin",
            "A migration cleanup that got as far as unregistering",
            // Not merely "stale-file prune": this doc legitimately *cross-references*
            // the prune test to explain why its own injection point moved. The
            // discriminator has to be the other doc's opening claim, not a word it
            // shares.
            "A stale-file prune that cannot complete",
        ),
        (
            "failed_enable_before_the_handoff_leaves_no_displacement_ownership",
            "A receipt persisted in front of",
            "migration cleanup",
        ),
        (
            "reenable_does_not_inherit_a_displacement_whose_handoff_never_ran",
            "An entry whose hand-off never ran",
            "A migration cleanup that got as far as unregistering",
        ),
    ] {
        let doc = doc_above(&lines, name).join("\n");
        assert!(
            !doc.is_empty(),
            "`{name}` has lost its doc comment entirely — whatever sits above it now \
             describes a different test"
        );
        assert!(
            doc.contains(must_own),
            "`{name}` must keep its own documentation ({must_own:?}):\n{doc}"
        );
        assert!(
            !doc.contains(must_not_own),
            "`{name}` has absorbed a neighbouring test's documentation \
             ({must_not_own:?}):\n{doc}"
        );
    }
}
