use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AdapterError, AgentRunHandle, AgentRunPoll, CoshCoreAdapter};
use cosh_shell::types::{
    AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
};

fn request(workspace: &Path) -> AgentRequest {
    let workspace = workspace.to_string_lossy().into_owned();
    AgentRequest {
        id: "activation-race".into(),
        session_id: "session".into(),
        command_block: CommandBlock {
            id: "command".into(),
            session_id: "session".into(),
            command: "test".into(),
            origin: Default::default(),
            cwd: workspace.clone(),
            end_cwd: workspace,
            started_at_ms: 0,
            ended_at_ms: 1,
            duration_ms: 1,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("test activation exclusion".into()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn finished_error(handle: &AgentRunHandle) -> AdapterError {
    let mut error = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match handle.poll_event_timeout(deadline.saturating_duration_since(Instant::now())) {
            Err(failure) => error = Some(failure),
            Ok(AgentRunPoll::Event(_)) => {}
            Ok(AgentRunPoll::Finished) => return error.expect("run must fail"),
            Ok(AgentRunPoll::Timeout) => panic!("run did not finish"),
        }
    }
}

fn short_activation_excludes_start(stale_cancel: bool) {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("core");
    fs::write(&program, r#"#!/usr/bin/env python3
import json, pathlib, sys, time
root = pathlib.Path(__file__).parent
if '--registry' in sys.argv:
    request = json.loads(sys.stdin.readline())
    (root / 'activation-started').write_text('ready')
    deadline = time.monotonic() + 10
    while not (root / 'release-activation').exists():
        if time.monotonic() >= deadline:
            sys.exit(72)
        time.sleep(0.01)
    print(json.dumps({'type':'registry_response', 'request_id':request['request_id'], 'success':True, 'data':{}}), flush=True)
else:
    (root / 'runtime-started').write_text('started')
    sys.exit(37)
"#).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = CoshCoreAdapter::new(program.to_string_lossy().into_owned(), true);
    if stale_cancel {
        let handle = adapter.start_cancellable(request(root.path()), CoshApprovalMode::Auto);
        let _ = finished_error(&handle);
        handle.cancel();
        fs::remove_file(root.path().join("runtime-started")).unwrap();
    }
    let activation_adapter = adapter.clone();
    let activate = thread::spawn(move || {
        activation_adapter.registry_query(
            "auth",
            "activate",
            serde_json::json!({"provider_id":"new"}),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("activation-started").exists() {
        assert!(
            Instant::now() < deadline,
            "activation never reached its process barrier"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let handle = adapter.start_cancellable(request(root.path()), CoshApprovalMode::Auto);
    let error = finished_error(&handle);
    fs::write(root.path().join("release-activation"), "release").unwrap();
    let activation_result = activate.join().unwrap();
    assert!(activation_result.is_ok(), "{activation_result:?}");
    assert!(error.message.contains("already processing"), "{error:?}");
    assert!(
        !root.path().join("runtime-started").exists(),
        "first runtime started before short activation finished"
    );
}

#[test]
fn short_activation_reserves_first_runtime_start() {
    short_activation_excludes_start(false);
}

#[test]
fn stale_cancel_cannot_bypass_short_activation_reservation() {
    short_activation_excludes_start(true);
}
