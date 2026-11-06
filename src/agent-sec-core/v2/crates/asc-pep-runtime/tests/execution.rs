//! Direct execution checks; real isolation is covered by the Linux helper suite.
#![cfg(target_os = "linux")]

use asc_pep_runtime::{SandboxBackend, prepare_launch, prepare_path_facts};
use asc_sandbox_types::{ExecutionSpec, LaunchSpec, OsPermissionSet};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;

fn fixture() -> (
    tempfile::TempDir,
    SandboxBackend,
    LaunchSpec,
    OsPermissionSet,
) {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("helper");
    std::fs::write(
        &helper,
        r#"#!/bin/bash
printf '%s\n' '{"stage":"installed"}' '{"stage":"started"}' '{"stage":"exited","exit_code":7,"signal":null}' >&"$4"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let launch =
        prepare_launch("/usr/bin/true", vec![], directory.path(), BTreeMap::new()).unwrap();
    let permissions = serde_json::from_value(serde_json::json!({
        "version": 1,
        "filesystem": {"default": "deny", "rules": []},
        "execution": {"deny_paths": []},
        "network": {"mode": "off"}
    }))
    .unwrap();
    (directory, SandboxBackend::new(helper), launch, permissions)
}

#[test]
fn direct_execution_returns_the_target_handshake() {
    let (_directory, backend, launch, permissions) = fixture();
    let result = backend.execute(launch, permissions).unwrap();
    assert!(result.installed);
    assert!(result.started);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.signal, None);
    assert_eq!(result.cancel_signal, None);
    assert_eq!(result.error, None);
}

#[test]
fn bound_execution_rejects_replaced_paths() {
    let (directory, backend, mut launch, permissions) = fixture();
    let target = directory.path().join("target");
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    launch.resolved_program = target.to_str().unwrap().into();
    let path_facts = prepare_path_facts(&launch, &permissions, &[]).unwrap();
    std::fs::rename(&target, directory.path().join("previous-target")).unwrap();
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    let error = backend
        .execute_bound(&ExecutionSpec {
            version: 1,
            launch,
            permissions,
            path_facts,
            mount_aliases: vec![],
        })
        .unwrap_err();
    assert!(error.to_string().contains("path identity changed"));
}

#[test]
fn sandbox_preserves_default_host_signals() {
    use rustix::process::{Signal, getpid, kill_process};
    use std::os::unix::process::ExitStatusExt as _;

    let signals = [
        ("INT", Signal::INT),
        ("TERM", Signal::TERM),
        ("HUP", Signal::HUP),
    ];
    if let Ok(selected) = std::env::var("ASC_PEP_TEST_HOST_SIGNAL") {
        let (directory, backend, launch, permissions) = fixture();
        let missing = std::env::var_os("ASC_PEP_TEST_MISSING_HELPER").is_some();
        let backend = if missing {
            SandboxBackend::new(directory.path().join("missing"))
        } else {
            backend
        };
        let result = backend.execute(launch, permissions);
        if missing {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().exit_code, Some(7));
        }
        let signal = signals
            .into_iter()
            .find(|(name, _)| *name == selected)
            .unwrap()
            .1;
        kill_process(getpid(), signal).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        panic!("sandbox call swallowed the host's default {selected} action");
    }
    for (name, signal) in signals {
        for missing in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "sandbox_preserves_default_host_signals",
                    "--nocapture",
                ])
                .env("ASC_PEP_TEST_HOST_SIGNAL", name)
                .env_remove("ASC_PEP_TEST_MISSING_HELPER");
            if missing {
                command.env("ASC_PEP_TEST_MISSING_HELPER", "1");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.signal(),
                Some(signal.as_raw()),
                "missing={missing}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
