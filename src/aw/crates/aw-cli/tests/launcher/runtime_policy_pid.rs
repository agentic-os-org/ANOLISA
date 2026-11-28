//! PID selection across two reloadable processes sharing one target.

use super::{
    command, reloadable_document, report, spawn_reloadable, update, wait, Fixture, Service,
};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Output};

fn native_pid(ready: &Path) -> String {
    wait(ready);
    fs::read_to_string(ready).unwrap()
}

fn instance(service: &Service, pid: &str) -> Value {
    let pid = pid.parse::<u32>().unwrap();
    service.status()["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["process"]["pid"] == pid)
        .unwrap()
        .clone()
}

fn rejected(output: Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(stderr.contains(message), "{stderr}");
}

#[test]
fn update_by_pid_selects_one_live_process_and_preserves_idle_gate() {
    for adapter in ["qoder", "qwenpaw"] {
        let selected = Fixture::new();
        let other = Fixture::new();
        let baseline = reloadable_document(&selected, adapter);
        let service = Service::start(&selected, &baseline);
        let config = selected.root.join("aw.json");
        let selected_process = spawn_reloadable(&selected, &config, adapter);
        let other_process = spawn_reloadable(&other, &config, adapter);
        let pid = native_pid(&selected.root.join("native-ready"));
        let other_pid = native_pid(&other.root.join("native-ready"));
        assert_ne!(pid, other_pid);
        let before = instance(&service, &pid);
        let other_before = instance(&service, &other_pid);
        assert_eq!(before["target"], other_before["target"]);
        assert_eq!(before["policy_revision"], other_before["policy_revision"]);
        let initial = before["policy_revision"].as_str().unwrap();

        let mut candidate = baseline.clone();
        selected.provider(&mut candidate);
        if adapter == "qwenpaw" {
            candidate["spec"]["providers"]["policy"]["transport"]["argv"] = json!([
                selected.python,
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/action.py"),
                selected.root,
                "provider",
                "policy"
            ]);
        }
        command(&selected, 0, json!({"action": "turn", "hold": true}));
        wait(&selected.root.join("entered-0"));
        rejected(update(&selected, &candidate, &["--pid", &pid]), "busy");
        assert_eq!(instance(&service, &pid)["policy_revision"], initial);
        assert_eq!(instance(&service, &other_pid)["policy_revision"], initial);
        fs::write(selected.root.join("continue-0"), b"continue").unwrap();
        assert_eq!(report(&selected, 0)["executed"], true);

        // The busy attempt wrote the candidate file. An explicit socket also accepts a PID.
        let mut apply = selected.command();
        apply
            .args(["update", "--socket"])
            .arg(selected.root.join("state/aw.sock"))
            .args(["--pid", &pid, "--expected-revision", initial, "--policy"])
            .arg(selected.root.join("candidate.json"));
        let result = selected.successful(apply);
        let updated = &result["policy_revision"];
        assert_ne!(updated, initial);
        let current = instance(&service, &pid);
        assert_eq!(current["process"], before["process"]);
        assert_eq!(&current["policy_revision"], updated);
        assert_eq!(instance(&service, &other_pid)["policy_revision"], initial);

        rejected(
            update(
                &selected,
                &baseline,
                &["--pid", &pid, "--expected-revision", initial],
            ),
            "policy_revision_conflict",
        );
        rejected(
            update(&selected, &baseline, &["--pid", &u32::MAX.to_string()]),
            "cannot read live process identity",
        );
        rejected(
            update(
                &selected,
                &baseline,
                &["--pid", &std::process::id().to_string()],
            ),
            "no matching live root instance",
        );

        command(&selected, 1, json!({"action": "turn", "scenario": "block"}));
        command(&other, 0, json!({"action": "turn", "scenario": "block"}));
        let denied = report(&selected, 1);
        assert_eq!(denied["pid"], before["process"]["pid"]);
        assert_eq!(denied["executed"], false, "{adapter}: {denied}");
        assert_eq!(report(&other, 0)["executed"], true);

        command(&other, 1, json!({"action": "stop"}));
        let stopped = other_process.finish();
        assert!(stopped.status.success(), "{stopped:?}");
        rejected(
            update(&selected, &baseline, &["--pid", &other_pid]),
            "cannot read live process identity",
        );
        assert_eq!(&instance(&service, &pid)["policy_revision"], updated);
        command(&selected, 2, json!({"action": "stop"}));
        let stopped = selected_process.finish();
        assert!(stopped.status.success(), "{stopped:?}");
        service.released(&selected);
    }
}
