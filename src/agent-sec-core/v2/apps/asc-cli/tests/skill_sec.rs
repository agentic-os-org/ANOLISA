//! Real Rust CLI processes exercise the daemon business contract and public audit projection.

mod common;

use asc_action_runtime::SecurityEventSink;
use asc_capability_skill_sec::{SkillSecConfig, SkillSecService, scanner::ScannerRegistry};
use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::RootManagedPrincipalPolicy;
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_service::ShutdownToken;
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use asc_security_events::SecurityEvent;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Events(Mutex<Vec<SecurityEvent>>);
impl SecurityEventSink for Events {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

async fn invoke(socket: &Path, arguments: &[&str]) -> (i32, Value, String) {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--socket".into(),
        socket.as_os_str().into(),
        "skill-ledger".into(),
    ];
    args.extend(arguments.iter().map(Into::into));
    let output = tokio::task::spawn_blocking(move || common::run(&args))
        .await
        .unwrap();
    let data = if output.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&output.stdout).unwrap()
    };
    (
        output.status.code().unwrap(),
        data,
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_runs_full_core_workflow_and_emits_safe_public_audit() {
    let directory = common::Directory::new();
    let state = directory.0.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let skill = directory.0.join("skill with spaces");
    fs::create_dir(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: test\ndescription: safe test\n---\nSafe skill",
    )
    .unwrap();
    fs::write(skill.join("run.sh"), "echo safe\n").unwrap();
    let path = skill.to_str().unwrap();
    let service = Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                state_dir: state.clone(),
                managed_skill_dirs: Vec::new(),
            },
            ScannerRegistry::default(),
        )
        .unwrap(),
    );
    let events = Arc::new(Events::default());
    let finalizer = asc_action_runtime::testing::audit_finalizer(events.clone());
    let dispatcher = Arc::new(DaemonDispatcher::new(
        PapService::new(
            Arc::new(ProcessLocalPapRepository::default()),
            Arc::new(PolicyTemplateCompiler),
        ),
        Arc::new(RootManagedPrincipalPolicy::default()),
        asc_daemon::skill_application(
            finalizer,
            asc_capability_skill_sec::executor::SkillSecExecutor::new(service),
        ),
    ));
    let socket = directory.0.join("daemon.sock");
    let shutdown = ShutdownToken::new();
    let signal = shutdown.clone();
    let mut config = BootstrapConfig::new(&socket);
    config.socket_mode = 0o666;
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    let task = tokio::spawn(async move {
        serve(config, dispatcher, Arc::new(JsonRejectionEncoder), signal)
            .await
            .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    assert_core_scan_contract(&socket, path, &state, &skill).await;
    assert_decision_and_export_contract(&socket, path, &directory.0).await;
    assert_root_identity_contract(&socket, path, &directory.0).await;
    let status = invoke(&socket, &["status", "--verbose"]).await.1;
    assert_eq!(
        status["skills"]["discovered"],
        1 + u64::from(rustix::process::geteuid().as_raw() == 0)
    );
    assert_public_audit(&events);
    shutdown.request();
    task.await.unwrap();
}

fn assert_public_audit(events: &Events) {
    let audit = events.0.lock().unwrap();
    let mut expected = vec![
        "status",
        "check",
        "audit",
        "analyze",
        "init",
        "scan",
        "check",
        "scan",
        "scan",
        "audit",
        "certify",
        "check",
        "decide",
        "show",
        "export",
        "decide",
        "list-scanners",
    ];
    if rustix::process::geteuid().as_raw() == 0 {
        expected.extend(["rotate-keys", "check", "scan"]);
    }
    expected.push("activate");
    if rustix::process::geteuid().as_raw() == 0 {
        expected.extend(["check", "rotate-keys", "export", "scan", "decide"]);
    }
    expected.push("status");
    // Each accepted RPC finalizes once; local CLI validation emits no daemon event.
    assert_eq!(
        audit
            .iter()
            .map(|event| event.details["request"]["command"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    for event in audit.iter() {
        assert_eq!(event.event_type, "skill_ledger");
        assert!([rustix::process::geteuid().as_raw(), 1001].contains(&event.uid));
        let details = serde_json::to_string(&event.details).unwrap();
        assert!(!details.contains("PRIVATE_FINDING_MARKER"));
        assert!(!details.contains("PRIVATE_REASON_MARKER"));
        assert!(!details.contains("echo safe"));
    }
}

async fn assert_core_scan_contract(socket: &Path, path: &str, state: &Path, skill: &Path) {
    let (code, status, _) = invoke(socket, &["status"]).await;
    assert_eq!(code, 0);
    assert_eq!(status["keys"]["initialized"], false);
    let checked = invoke(socket, &["check", path]).await;
    assert_eq!(checked.0, 0);
    assert_eq!(checked.1["status"], "none");
    let audited = invoke(socket, &["audit", path, "--verify-snapshots"]).await;
    assert_eq!(audited.0, 0);
    assert_eq!(audited.1["valid"], true);
    assert_eq!(invoke(socket, &["analyze", path]).await.0, 0);
    assert!(!state.join("signing-key.pk8").exists());
    assert!(!skill.join(".skill-meta").exists());
    assert_eq!(
        invoke(socket, &["analyze", path, "--format", "text"])
            .await
            .0,
        2
    );
    assert_eq!(invoke(socket, &["init", "--no-baseline"]).await.0, 0);
    assert!(!skill.join(".skill-meta").exists());
    let scanned = invoke(socket, &["scan", path]).await;
    assert_eq!(scanned.0, 0, "{}", scanned.2);
    assert_eq!(scanned.1["versionId"], "v000001");
    assert_eq!(invoke(socket, &["check", path]).await.1["status"], "pass");
    assert_eq!(invoke(socket, &["scan", path]).await.1["status"], "noop");
    assert_eq!(
        invoke(socket, &["scan", path, "--force"]).await.1["status"],
        "scanned"
    );
    assert_eq!(
        invoke(socket, &["audit", path, "--verify-snapshots"])
            .await
            .0,
        0
    );
}

async fn assert_decision_and_export_contract(socket: &Path, path: &str, directory: &Path) {
    let findings = directory.join("findings.json");
    fs::write(
        &findings,
        serde_json::to_vec(
            &json!([{"rule":"audit-private","level":"deny","message":"PRIVATE_FINDING_MARKER"}]),
        )
        .unwrap(),
    )
    .unwrap();
    let certified = invoke(
        socket,
        &[
            "certify",
            path,
            "--findings",
            findings.to_str().unwrap(),
            "--delete-findings",
        ],
    )
    .await;
    assert_eq!(certified.0, 0, "{}", certified.2);
    assert!(!findings.exists());
    let checked = invoke(socket, &["check", path]).await;
    assert_eq!(checked.0, 1);
    assert_eq!(checked.1["status"], "deny");
    assert_eq!(
        invoke(
            socket,
            &[
                "decide",
                path,
                "--action",
                "allow",
                "--reason",
                "PRIVATE_REASON_MARKER"
            ]
        )
        .await
        .0,
        0
    );
    let shown = invoke(socket, &["show", path]).await;
    assert_eq!(shown.0, 0);
    let output = directory.join("export");
    assert_eq!(
        invoke(
            socket,
            &[
                "export",
                path,
                "--version",
                "active",
                "--output",
                output.to_str().unwrap()
            ]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        fs::read(output.join("snapshot/run.sh")).unwrap(),
        b"echo safe\n"
    );
    assert_eq!(invoke(socket, &["decide", path, "--clear"]).await.0, 0);
    assert_eq!(
        invoke(socket, &["list-scanners"]).await.1["scanners"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

async fn assert_root_identity_contract(socket: &Path, path: &str, directory: &Path) {
    if rustix::process::geteuid().as_raw() == 0 {
        assert_eq!(invoke(socket, &["rotate-keys"]).await.0, 0);
        assert_eq!(
            invoke(socket, &["check", path]).await.1["status"],
            "tampered"
        );
        assert_eq!(invoke(socket, &["scan", path]).await.0, 0);
    }
    assert_eq!(invoke(socket, &["activate", path]).await.0, 0);
    if rustix::process::geteuid().as_raw() == 0 {
        let export_parent = directory.join("user-output");
        fs::create_dir(&export_parent).unwrap();
        rustix::fs::chown(
            &export_parent,
            Some(rustix::process::Uid::from_raw(1001)),
            None,
        )
        .unwrap();
        for arguments in [
            vec!["check", path],
            vec!["rotate-keys"],
            vec!["export", path, "--output", export_parent.to_str().unwrap()],
        ] {
            let expected = i32::from(arguments[0] == "rotate-keys");
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
                .arg("--socket")
                .arg(socket)
                .arg("--timeout-ms")
                .arg("5000")
                .arg("skill-ledger")
                .args(&arguments)
                .uid(1001)
                .gid(1001)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{:?}: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            );
            if arguments[0] == "rotate-keys" {
                assert!(String::from_utf8_lossy(&output.stderr).contains("root administrator"));
            }
        }
        assert_user_can_edit_after_rollback(socket, directory).await;
        assert_eq!(
            fs::metadata(export_parent.join("snapshot/SKILL.md"))
                .unwrap()
                .uid(),
            1001
        );
    }
}

async fn assert_user_can_edit_after_rollback(socket: &Path, directory: &Path) {
    let skill = directory.join("user-owned-skill");
    let nested = skill.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: user-owned\ndescription: safe fixture\n---\nSafe skill\n",
    )
    .unwrap();
    fs::write(nested.join("note.txt"), "original\n").unwrap();
    for path in [
        &skill,
        &nested,
        &skill.join("SKILL.md"),
        &nested.join("note.txt"),
    ] {
        rustix::fs::chown(path, Some(rustix::process::Uid::from_raw(1001)), None).unwrap();
    }
    assert_eq!(
        invoke(socket, &["scan", skill.to_str().unwrap()]).await.0,
        0
    );
    fs::write(nested.join("note.txt"), "changed\n").unwrap();
    let rollback = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .arg("--socket")
        .arg(socket)
        .args([
            "skill-ledger",
            "decide",
            skill.to_str().unwrap(),
            "--action",
            "rollback",
            "--version",
            "v000001",
        ])
        .uid(1001)
        .gid(1001)
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_eq!(fs::read(nested.join("note.txt")).unwrap(), b"original\n");
    assert_eq!(fs::metadata(nested.join("note.txt")).unwrap().uid(), 1001);
    assert_eq!(fs::metadata(&nested).unwrap().uid(), 1001);
    let edit = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'editable\\n' > \"$1\"", "fixture"])
        .arg(nested.join("note.txt"))
        .uid(1001)
        .gid(1001)
        .output()
        .unwrap();
    assert!(
        edit.status.success(),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );
    assert_eq!(fs::read(nested.join("note.txt")).unwrap(), b"editable\n");
}
