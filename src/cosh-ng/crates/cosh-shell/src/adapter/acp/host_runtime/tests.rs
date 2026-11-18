use super::super::super::{ApprovalResponse, HostExecutedInputWait, HostExecutedShellMetadata};
use super::*;
use std::time::{Duration, Instant};

fn request(mandatory: bool) -> AcpHostExecuteRequest {
    AcpHostExecuteRequest {
        session_id: "session-1".to_string(),
        tool_call_id: "call-1".to_string(),
        command: "echo hi".to_string(),
        cwd: "/tmp".to_string(),
        expected_cwd: None,
        pty_instance: "pty-1".to_string(),
        hook_requires_approval: mandatory,
        approval_id: None,
    }
}

fn result() -> HostExecutedShellResult {
    HostExecutedShellResult {
        llm_content: "hi\n".to_string(),
        return_display: None,
        metadata: HostExecutedShellMetadata {
            command: "echo hi".to_string(),
            status: "completed".to_string(),
            exit_code: 0,
            signal: None,
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            duration_ms: 7,
            output_ref: Some("terminal-1".to_string()),
            redaction_status: "none".to_string(),
            approval_id: Some("approval-1".to_string()),
            tool_use_id: Some("call-1".to_string()),
            input_wait: Some(HostExecutedInputWait {
                waited_secs: 3,
                interrupted: true,
            }),
        },
    }
}

#[test]
fn a_host_executed_result_is_carried_without_inventing_facts() {
    let outcome = decide(ApprovalDecision::HostExecutedShell {
        result: Box::new(result()),
    })
    .expect("host executed result");
    assert_eq!(outcome.command, "echo hi");
    assert_eq!(outcome.status, "completed");
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.signal, None);
    assert_eq!(outcome.end_cwd, "/tmp");
    assert_eq!(outcome.duration_ms, 7);
    assert_eq!(outcome.output_ref.as_deref(), Some("terminal-1"));
    assert_eq!(outcome.redaction_status, "none");
    assert_eq!(outcome.approval_id.as_deref(), Some("approval-1"));
    assert_eq!(outcome.tool_use_id.as_deref(), Some("call-1"));
    assert_eq!(outcome.llm_content, "hi\n");
    assert!(!outcome.llm_content_truncated);
    let wait = outcome.input_wait.expect("input wait facts");
    assert_eq!(wait.waited_secs, 3);
    assert!(wait.interrupted);
}

#[test]
fn a_denial_becomes_the_frozen_refusal_code() {
    let refusal = decide(ApprovalDecision::Deny {
        message: "the user refused".to_string(),
    })
    .expect_err("denial");
    assert_eq!(refusal.code, CODE_REFUSED);
    assert_eq!(refusal.message, "the user refused");
}

#[test]
fn a_native_allow_is_a_contract_violation_not_execution() {
    let refusal = decide(ApprovalDecision::Allow).expect_err("native allow");
    assert_eq!(refusal.code, CODE_CONSTRAINT);
}

#[test]
fn an_unrelated_decision_does_not_answer_a_foreground_execute() {
    let refusal = decide(ApprovalDecision::Answer {
        answer: "yes".to_string(),
    })
    .expect_err("unrelated decision");
    assert_eq!(refusal.code, CODE_CONSTRAINT);
}

#[test]
fn the_ask_carries_the_trusted_mandatory_flag_and_the_final_command() {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let responder = std::thread::spawn(move || {
        let _ = approval_tx.send(ApprovalChannelMessage::Response(ApprovalResponse {
            request_id: "call-1".to_string(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::HostExecutedShell {
                result: Box::new(result()),
            },
        }));
    });
    let outcome = authority(&request(true)).expect("host executed");
    responder.join().expect("responder");
    assert_eq!(outcome.status, "completed");
    let event = event_rx.recv().expect("ask event").expect("ask ok");
    match event {
        AgentEvent::ToolPermissionRequest {
            run_id,
            request_id,
            tool_name,
            tool_input,
            hook_requires_approval,
            ..
        } => {
            assert_eq!(run_id, "run-1");
            assert_eq!(request_id, "call-1");
            assert_eq!(tool_name, "shell");
            assert_eq!(tool_input["command"], "echo hi");
            assert!(hook_requires_approval);
        }
        other => panic!("expected a permission ask, got {other:?}"),
    }
}

#[test]
fn a_closed_approval_channel_reports_unknown_not_refused() {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    drop(approval_tx);
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let refusal = authority(&request(false)).expect_err("closed channel");
    assert_eq!(refusal.code, CODE_UNKNOWN);
    assert!(event_rx.recv().is_ok());
}

#[test]
fn a_response_for_another_action_does_not_answer_this_one() {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let responder = std::thread::spawn(move || {
        let unrelated = ApprovalChannelMessage::Response(ApprovalResponse {
            request_id: "call-other".to_string(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "not for this action".to_string(),
            },
        });
        approval_tx.send(unrelated).expect("unrelated sent");
        approval_tx
            .send(ApprovalChannelMessage::Receipt {
                request_id: "call-1".to_string(),
            })
            .expect("receipt sent");
        approval_tx
            .send(ApprovalChannelMessage::Response(ApprovalResponse {
                request_id: "call-1".to_string(),
                tool_use_id: None,
                tool_input: None,
                decision: ApprovalDecision::HostExecutedShell {
                    result: Box::new(result()),
                },
            }))
            .expect("matching sent");
    });
    let outcome = authority(&request(true)).expect("matching response wins");
    responder.join().expect("responder");
    assert_eq!(outcome.status, "completed");
    drop(event_rx);
}

#[test]
fn a_closed_event_channel_refuses_because_the_action_provably_did_not_run() {
    let (event_tx, event_rx) = mpsc::channel();
    drop(event_rx);
    let (_approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let refusal = authority(&request(true)).expect_err("closed event channel");
    assert_eq!(refusal.code, CODE_REFUSED);
    assert!(refusal.message.contains("did not run"));
}

fn ask_tool_input(expected_cwd: Option<&str>) -> serde_json::Value {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let responder = std::thread::spawn(move || {
        let _ = approval_tx.send(ApprovalChannelMessage::Response(ApprovalResponse {
            request_id: "call-1".to_string(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::HostExecutedShell {
                result: Box::new(result()),
            },
        }));
    });
    let mut ask = request(true);
    ask.expected_cwd = expected_cwd.map(str::to_string);
    authority(&ask).expect("host executed");
    responder.join().expect("responder");
    match event_rx.recv().expect("ask event").expect("ask ok") {
        AgentEvent::ToolPermissionRequest { tool_input, .. } => tool_input,
        other => panic!("expected a permission ask, got {other:?}"),
    }
}

#[test]
fn the_ask_forwards_the_directory_assertion_when_the_action_names_one() {
    let input = ask_tool_input(Some("/srv/project"));
    assert_eq!(input["expectedCwd"], "/srv/project");
    assert_eq!(input["command"], "echo hi");
}

#[test]
fn the_ask_omits_the_directory_assertion_when_none_was_named() {
    let input = ask_tool_input(None);
    assert!(input.get("expectedCwd").is_none());
}

#[test]
fn a_cancelled_run_never_delivers_the_ask() {
    let (event_tx, event_rx) = mpsc::channel();
    let (_approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(true)),
    );
    let refusal = authority(&request(true)).expect_err("cancelled before the ask");
    assert_eq!(refusal.code, CODE_REFUSED);
    assert!(refusal.message.contains("did not run"));
    assert!(event_rx.try_recv().is_err(), "no ask was delivered");
}

#[test]
fn cancellation_while_waiting_ends_with_an_unknown_terminal_fact() {
    let (event_tx, event_rx) = mpsc::channel();
    let (_approval_tx, approval_rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::clone(&cancelled),
    );
    let flag = Arc::clone(&cancelled);
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let refusal = authority(&request(true)).expect_err("cancelled while waiting");
    let elapsed = started.elapsed();
    canceller.join().expect("canceller");
    assert_eq!(refusal.code, CODE_UNKNOWN);
    assert!(
        elapsed < Duration::from_secs(2),
        "the wait must end promptly, took {elapsed:?}"
    );
    assert!(event_rx.recv().is_ok(), "the ask left before cancellation");
}

#[test]
fn an_uncancelled_wait_still_receives_a_late_human_decision() {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let responder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let _ = approval_tx.send(ApprovalChannelMessage::Response(ApprovalResponse {
            request_id: "call-1".to_string(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "the user refused".to_string(),
            },
        }));
    });
    let refusal = authority(&request(true)).expect_err("late human denial");
    responder.join().expect("responder");
    assert_eq!(refusal.code, CODE_REFUSED);
    assert_eq!(refusal.message, "the user refused");
    drop(event_rx);
}

#[test]
fn a_lost_card_without_cancel_or_decision_waits_indefinitely() {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let authority = super::authority(
        "run-1".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let outcome = authority(&request(true));
        let _ = done_tx.send(outcome.map(|_| ()).map_err(|refusal| refusal.code));
    });
    let ask = event_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the ask must be delivered");
    assert!(matches!(ask, Ok(AgentEvent::ToolPermissionRequest { .. })));
    // H3-4 gap: with the run neither cancelled nor answered, a lost card does
    // not self-terminate. After several poll slices the authority must still be
    // waiting; only an explicit decision or cancellation ends it.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        done_rx.try_recv().is_err(),
        "a lost card without cancel or decision must not self-terminate"
    );
    approval_tx
        .send(ApprovalChannelMessage::Response(ApprovalResponse {
            request_id: "call-1".to_string(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "released after observation".to_string(),
            },
        }))
        .expect("release decision");
    let code = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the authority returns once a decision arrives");
    assert_eq!(code, Err(CODE_REFUSED));
    waiter.join().expect("waiter");
    drop(event_rx);
}
