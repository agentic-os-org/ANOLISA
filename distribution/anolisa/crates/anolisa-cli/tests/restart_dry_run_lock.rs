//! CLI coverage: system `--dry-run restart` must preview on a non-writable
//! state root.
//!
//! Policy already lets a non-root system preview reach the handler. The
//! handler must not then create or exclusively lock the state-root file, or
//! a normal root-owned `/var/lib/anolisa` exits with permission denied
//! before listing units. Subprocesses stay on `--dry-run` with
//! `--install-mode system` and a temporary `--prefix`.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;

use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, OwnedArtifact, ProviderBinding,
};
use anolisa_core::state::{ObjectKind, ServiceRef};
use anolisa_core::{ServiceScope, state_store::StateStore};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::privilege;
use serde_json::Value;

mod common;

const TARGET: &str = "agentsight";
const UNIT: &str = "anolisa-restart-dry-run-probe.service";

struct RestartFixture {
    _tmp: tempfile::TempDir,
    prefix: PathBuf,
    state_dir: PathBuf,
}

impl RestartFixture {
    fn planted() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefix = tmp.path().join("system");
        seed_index(&prefix);
        let layout = plant_restartable(&prefix);
        Self {
            _tmp: tmp,
            prefix,
            state_dir: layout.state_dir,
        }
    }

    fn run_dry_run(&self) -> Output {
        let prefix = self.prefix.to_string_lossy().into_owned();
        common::run(&[
            "--json",
            "--no-color",
            "--dry-run",
            "--install-mode",
            "system",
            "--prefix",
            prefix.as_str(),
            "restart",
            TARGET,
        ])
    }
}

struct RestorePermissions {
    path: PathBuf,
    restore: u32,
}

impl RestorePermissions {
    fn dir_mode(path: &std::path::Path, restrict: u32, restore: u32) -> Self {
        let mut perms = std::fs::metadata(path)
            .expect("state dir metadata")
            .permissions();
        perms.set_mode(restrict);
        std::fs::set_permissions(path, perms).expect("restrict state dir");
        Self {
            path: path.to_path_buf(),
            restore,
        }
    }
}

impl Drop for RestorePermissions {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            let mut perms = metadata.permissions();
            perms.set_mode(self.restore);
            let _ = std::fs::set_permissions(&self.path, perms);
        }
    }
}

fn seed_index(prefix: &std::path::Path) {
    let repo_v1 = prefix.join("repo/v1");
    std::fs::create_dir_all(&repo_v1).expect("local repo");
    std::fs::write(
        repo_v1.join("components-v2.toml"),
        format!(
            "schema_version = 2\n\n[[components]]\nname = \"{TARGET}\"\ntargets = [{{ os = \"{os}\", arch = \"{arch}\" }}]\n",
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        ),
    )
    .expect("component index");
    let etc = prefix.join("etc/anolisa");
    std::fs::create_dir_all(&etc).expect("config dir");
    std::fs::write(
        etc.join("repo.toml"),
        format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file://{}\"\n",
            repo_v1.display()
        ),
    )
    .expect("repo config");
}

fn plant_restartable(prefix: &std::path::Path) -> FsLayout {
    let layout = FsLayout::system(Some(prefix.to_path_buf()));
    let state_path = layout.state_dir.join("installed.toml");
    let mut store = StateStore::empty_for_layout(&layout);
    store.upsert(Installation {
        kind: ObjectKind::Component,
        name: TARGET.to_string(),
        scope: InstallationScope::System,
        binding: ProviderBinding::Owned {
            artifact: OwnedArtifact {
                version: "1.0.0".to_string(),
                distribution_source: None,
                raw_package: None,
                manifest_digest: None,
                files: Vec::new(),
                services: vec![ServiceRef {
                    name: UNIT.to_string(),
                    manager: "systemd".to_string(),
                    restartable: true,
                    enabled: true,
                    scope: ServiceScope::System,
                }],
                external_modified_files: Vec::new(),
                provisioned_packages: Vec::new(),
            },
        },
        status: LifecycleStatus::Installed,
        installed_at: "2026-07-21T00:00:00Z".to_string(),
        last_operation_id: None,
        subscription_scope: Default::default(),
        enabled_features: Vec::new(),
        health: Vec::new(),
    });
    store.save(&state_path).expect("save state");
    layout
}

#[test]
fn restart_dry_run_json_previews_from_a_non_writable_system_state_root() {
    // Root ignores directory mode bits, so this cannot model a root-owned
    // system state root. Run the anolisa crate gates as anolisa-ci.
    if privilege::effective_uid() == 0 {
        return;
    }

    let fixture = RestartFixture::planted();
    let lock_file = fixture.state_dir.join("lock");
    assert!(
        !lock_file.exists(),
        "fixture must start without a lock file"
    );
    let _restore = RestorePermissions::dir_mode(&fixture.state_dir, 0o555, 0o755);

    let output = fixture.run_dry_run();
    assert_eq!(
        Some(0),
        output.status.code(),
        "non-writable system preview must succeed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be a JSON envelope");
    assert_eq!(value.get("ok"), Some(&Value::Bool(true)), "{value}");
    let data = value.get("data").expect("data");
    assert_eq!(data.get("dry_run"), Some(&Value::Bool(true)));
    assert_eq!(data.get("component").and_then(Value::as_str), Some(TARGET));
    let units = data
        .get("units")
        .and_then(Value::as_array)
        .expect("units array");
    assert_eq!(units.len(), 1, "{value}");
    assert_eq!(units[0].get("unit").and_then(Value::as_str), Some(UNIT));
    // The subprocess uses the real EnvService factory. Container hosts
    // return an unsupported manager, so the unit is `not_supported`
    // even when the non-writable preview itself is correct.
    let state = units[0].get("state").and_then(Value::as_str);
    assert!(
        matches!(state, Some("planned") | Some("not_supported")),
        "preview must keep the host-appropriate unit state, got {state:?}: {value}"
    );
    assert_eq!(units[0].get("changed"), Some(&Value::Bool(false)));
    assert!(
        !lock_file.exists(),
        "preview must not create {}",
        lock_file.display()
    );
}
