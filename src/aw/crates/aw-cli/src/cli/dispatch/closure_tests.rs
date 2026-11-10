//! Failed calls close unfinished events while cleanup errors retain native vetoes.
use super::*;
use aw_service::{Binding, Identity, Request, Response};
use serde_json::json;
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

#[test]
fn dynamic_veto_closes_only_unvisited_steps_and_preserves_final_reason() {
    for deny in ["first", "last"] {
        check("openclaw", deny, None, None);
    }
}

#[test]
fn failed_final_calls_close_unfinished_events_and_preserve_denial() {
    for failure in ["request_not_admitted", "disconnect", "deadline_exceeded"] {
        check("openclaw", "first", Some(failure), None);
        check("openclaw", "last", Some(failure), None);
    }
}

#[test]
fn expired_invoke_budget_still_allows_bounded_event_cleanup() {
    check("openclaw", "last", Some("timeout"), None);
}

#[test]
fn cleanup_errors_preserve_specific_denials_and_report_uncertainty() {
    for error in [
        "unknown_event",
        "event_reply_unavailable",
        "audit_unavailable",
    ] {
        check("openclaw", "first", None, Some(error));
        check(
            "openclaw",
            "last",
            Some("request_not_admitted"),
            Some(error),
        );
    }
}

#[test]
fn qoder_sdk_denials_close_the_original_native_correlation() {
    for deny in ["first", "last"] {
        check("qoder-sdk", deny, None, None);
    }
}

#[test]
fn qoder_sdk_invocation_timeout_retains_denial_and_cleans_up() {
    check("qoder-sdk", "last", Some("timeout"), None);
}

#[test]
fn qoder_sdk_cleanup_errors_retain_the_final_specific_veto() {
    for error in [
        "disconnect",
        "timeout",
        "audit_unavailable",
        "event_close_timeout",
        "unknown_event",
    ] {
        check("qoder-sdk", "last", None, Some(error));
    }
}

#[test]
fn qoder_sdk_failed_invocation_and_cleanup_still_deny() {
    for error in ["disconnect", "audit_unavailable"] {
        check("qoder-sdk", "last", Some("timeout"), Some(error));
    }
}

fn check(adapter_id: &str, deny: &str, invoke_error: Option<&str>, close_error: Option<&str>) {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("aw.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let identity = Identity {
        generation: "fixture".into(),
        config_revision: "a".repeat(64),
    };
    let binding = HookBinding {
        adapter: adapter_id.into(),
        reloadable: true,
        resident_policy_reload: false,
        registration: None,
        socket,
        cwd: directory.path().into(),
        events: Default::default(),
        binding: Binding {
            identity: identity.clone(),
            instance_id: "instance".into(),
            target: "target".into(),
            audit_key: "audit".into(),
        },
    };
    let stopped = AtomicBool::new(false);
    let expires = Instant::now()
        + if invoke_error == Some("timeout") {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(4)
        };
    let (result, invoked, closed, active, correlation_live) = thread::scope(|threads| {
        let server = threads.spawn(|| {
                let deadline = Instant::now()+Duration::from_secs(5);
                let mut invoked = Vec::new();
                let mut closed = 0;
                let mut active = false;
                let mut correlation_live = false;
                while !stopped.load(Ordering::Acquire) {
                    assert!(Instant::now()<deadline,"callback fixture did not stop");
                    let (mut stream,_) = match listener.accept() {
                        Ok(value)=>value,
                        Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=> {
                            thread::sleep(Duration::from_millis(1));continue;
                        }
                        Err(error)=>panic!("accept: {error}"),
                    };
                    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut prefix = [0;4];stream.read_exact(&mut prefix).unwrap();
                    let mut bytes = vec![0;u32::from_be_bytes(prefix) as usize];
                    stream.read_exact(&mut bytes).unwrap();
                    let request:Request = serde_json::from_slice(&bytes).unwrap();
                    let mut error = None;
                    let result = match request.operation {
                        Operation::Status=>json!({}),
                        Operation::HookPlan {..}=>json!({"plan":{"budget_ms":3000,
                            "groups":[[{"id":"first","on_error":"block"}],
                                [{"id":"last","on_error":"block"}]],
                            "replacement":false,"observe":false}}),
                        Operation::OpenHookEvent {..}=> {
                            active=true;
                            correlation_live=adapter_id=="qoder-sdk";
                            json!({"event_id":"event", "instance_id":"instance",
                                "steps":["first","last"],"candidate_revision":"candidate"})
                        },
                        Operation::InvokeStep {step_id,..}=> {
                            invoked.push(step_id.clone());
                            if step_id==deny {
                                if invoke_error==Some("disconnect") { continue; }
                                if invoke_error==Some("timeout") {
                                    thread::sleep(Duration::from_millis(650));
                                    continue;
                                }
                                error=invoke_error.map(str::to_owned);
                            }
                            if step_id=="last" && error.is_none() { active=false; }
                            json!({"status":"ok","kind":"provider","effects":if step_id==deny {
                                json!([{"type":"block","reason_code":"fixture","text":"Specific retry arguments"}])
                            } else {json!([])}})
                        }
                        Operation::CloseHookEvent {instance_id,event,session_id,call_id}=> {
                            assert_eq!(adapter_id,"qoder-sdk");
                            assert_eq!(instance_id,"instance");
                            assert_eq!(event,"tool.before");
                            assert_eq!(session_id,"sdk-session");
                            assert_eq!(call_id,"sdk-tool-call");
                            if invoke_error==Some("timeout") { assert!(Instant::now()>expires); }
                            closed+=1;
                            assert!(correlation_live);
                            if close_error==Some("disconnect") { continue; }
                            if close_error==Some("timeout") {
                                thread::sleep(Duration::from_millis(2150));
                                continue;
                            }
                            if close_error.is_none() { active=false;correlation_live=false; }
                            error=close_error.map(str::to_owned);
                            json!({"closed":true})
                        }
                        Operation::CloseEvent {..}=> {
                            assert_eq!(adapter_id,"openclaw");
                            if invoke_error==Some("timeout") { assert!(Instant::now()>expires); }
                            closed+=1;
                            assert!(active, "successful final calls must not receive redundant close");
                            // unknown_event means another cleanup won; other failures
                            // cannot prove the event's release to this caller.
                            if close_error.is_none() || close_error==Some("unknown_event") { active=false; }
                            error=close_error.map(str::to_owned);
                            json!({"closed":true})
                        }
                        other=>panic!("unexpected callback operation: {other:?}"),
                    };
                    let response = Response {api_version:aw_service::VERSION.into(),identity:identity.clone(),
                        result:if error.is_none() {Some(result)} else {None},error,audit_key:None};
                    let bytes = serde_json::to_vec(&response).unwrap();
                    stream.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
                    stream.write_all(&bytes).unwrap();
                }
                (invoked,closed,active,correlation_live)
            });
        let result = invoke_bound(
            aw_adapter::select(adapter_id).unwrap(),
            binding,
            "tool.before",
            serde_json::to_vec(&if adapter_id == "qoder-sdk" {
                json!({"hook_event_name":"PreToolUse","cwd":directory.path(),
                    "session_id":"sdk-session","tool_use_id":"sdk-tool-call",
                    "tool_name":"Bash","tool_input":{"command":"fixture"}})
            } else {
                json!({"hook":"before_tool_call",
                    "event":{"toolName":"exec","params":{},"toolCallId":"call","runId":"run"},
                    "context":{"sessionId":"session","runId":"run"}})
            })
            .unwrap(),
            expires,
        );
        stopped.store(true, Ordering::Release);
        let (invoked, closed, active, correlation_live) = server.join().unwrap();
        (result, invoked, closed, active, correlation_live)
    });
    let result = result.unwrap();
    if adapter_id == "qoder-sdk" {
        assert!(matches!(result.exit, NativeExit::Code(2)));
        if invoke_error.is_some() {
            assert!(result.stdout.is_empty());
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&result.stdout).unwrap(),
                json!("Specific retry arguments")
            );
        }
    } else {
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            json!({"block":true,"blockReason":if invoke_error.is_some() {
                "Tool blocked by AW policy"
            } else { "Specific retry arguments" }})
        );
        assert!(matches!(result.exit, NativeExit::Code(0)));
    }
    if deny == "first" {
        assert_eq!(invoked, ["first"]);
        assert_eq!(closed, 1);
    } else {
        assert_eq!(invoked, ["first", "last"]);
        assert_eq!(
            closed,
            usize::from(invoke_error.is_some() || adapter_id == "qoder-sdk")
        );
    }
    let uncertain =
        close_error.is_some_and(|code| adapter_id == "qoder-sdk" || code != "unknown_event");
    assert_eq!(
        active,
        uncertain && (deny == "first" || invoke_error.is_some()),
        "unfinished event must be closed before callback returns"
    );
    assert_eq!(correlation_live, adapter_id == "qoder-sdk" && uncertain);
    assert_eq!(
        String::from_utf8_lossy(&result.stderr).contains("AW event cleanup failed"),
        uncertain
    );
}
