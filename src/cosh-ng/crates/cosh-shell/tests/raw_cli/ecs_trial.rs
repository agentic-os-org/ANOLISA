use super::*;

const CORE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
def send(value):
    print(json.dumps(value), flush=True)
def record(value):
    with open(os.environ['TRIAL_LOG'], 'a') as log:
        log.write(json.dumps(value) + '\n')
def state(revision, phase, local=False):
    send({'type':'ecs_trial_state','run_id':'core-opaque','flow_id':'flow-1','revision':revision,'phase':phase,'dispatch_state':os.environ.get('TRIAL_DISPATCH','not_dispatched'),'local_terminated':local,'resume_allowed':local and os.environ.get('TRIAL_DISPATCH','not_dispatched')=='not_dispatched'})
if '--registry' in sys.argv:
    request=json.loads(sys.stdin.readline()); record(request)
    action=request['action']
    data={'templates':[],'saved_providers':[],'effective_auth_required':False,'trial_preferences':{'auto_use':False}}
    if os.environ.get('TRIAL_LEGACY')!='1':
        data['ecs_trial']={'available':True,'active':False,'model':'qwen3.7-plus','provider_id':None}
    if action=='set_trial_preference':
        data={'auto_use':request['params']['auto_use']}
    elif action=='activate_trial':
        time.sleep(float(os.environ.get('TRIAL_ACTIVATION_DELAY','0')))
        data={'active_provider':'ecs-free-trial','model':'qwen3.7-plus','trial_preferences':{'auto_use':True}}
    saves=sum(json.loads(line).get('action')=='set_trial_preference' for line in open(os.environ['TRIAL_LOG']))
    fail=action=='set_trial_preference' and (os.environ.get('TRIAL_SAVE_FAIL')=='1' or (os.environ.get('TRIAL_SAVE_FAIL_ONCE')=='1' and saves==1))
    send({'type':'registry_response','request_id':request['request_id'],'success':not fail,'data':data,'error':'fixture save failed' if fail else None})
    sys.exit(0)
init=json.loads(sys.stdin.readline()); record(init)
send({'type':'control_response','response':{'subtype':'success','request_id':init['request_id'],'response':{'subtype':'initialize','protocol_version':1,'capabilities':{'can_handle_ecs_trial_v1':os.environ.get('TRIAL_LEGACY')!='1'}}}})
for line in sys.stdin:
    request=json.loads(line); record(request)
    if request.get('type')=='user':
        if os.environ.get('TRIAL_LEGACY')=='1':
            send({'type':'assistant','message':{'content':[{'type':'text','text':'legacy answer completed'}]}})
            send({'type':'result','subtype':'success','session_id':'trial-session','is_error':False,'result':'done'})
        else:
            state(1,'waiting_activation')
            if os.environ.get('TRIAL_ERROR_CODE'):
                code=os.environ['TRIAL_ERROR_CODE']
                send({'type':'ecs_trial_state','run_id':'core-opaque','flow_id':'flow-1','revision':1,'phase':'failed','code':code,'dispatch_state':'unknown','local_terminated':True,'resume_allowed':False})
                if os.environ.get('TRIAL_EXIT_AFTER_FAILURE')=='1': sys.exit(0)
                send({'type':'result','subtype':'error','session_id':'trial-session','is_error':True,'error_code':code,'errors':['provider_access:'+code],'result':'provider_access:'+code})
            if os.environ.get('TRIAL_AUTO_READY')=='1':
                state(1,'ready')
                send({'type':'assistant','message':{'content':[{'type':'text','text':'trial answer automatically resumed'}]}})
                send({'type':'result','subtype':'success','session_id':'trial-session','is_error':False,'result':'done'})
    elif request.get('type')=='ecs_trial_control':
        revision=request['revision']; action=request['action']
        if action=='pause':
            state(revision-1,'ready')
            state(revision,'paused',True)
            if os.environ.get('TRIAL_LATE_TEXT')=='1': send({'type':'assistant','message':{'content':[{'type':'text','text':'TRIAL_LATE_ANSWER_MUST_NOT_RENDER'}]}})
            if os.environ.get('TRIAL_EXIT_ON_PAUSE')=='1': sys.exit(0)
        elif action=='resume': state(revision,'waiting_activation')
        elif action=='cancel':
            state(revision,'terminated',True)
            if os.environ.get('TRIAL_HANG_AFTER_CANCEL')!='1': send({'type':'result','subtype':'error','session_id':'trial-session','is_error':True,'error_code':'trial_cancelled','errors':['provider_access:trial_cancelled'],'result':'provider_access:trial_cancelled'})
    elif request.get('type')=='registry_request':
        send({'type':'registry_response','request_id':request['request_id'],'success':True,'data':{'auto_use':False}})
"#;

fn run_trial(envs: &[(&str, &str)], steps: &[(&str, &[u8])]) -> (String, Vec<serde_json::Value>) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    fs::write(home.join(".hushlogin"), "").unwrap();
    let core = home.join("trial-core");
    let log = home.join("trial.log");
    write_executable(&core, CORE);
    let mut variables = vec![
        ("HOME", home.to_str().unwrap()),
        ("COSH_CORE_PATH", core.to_str().unwrap()),
        ("TRIAL_LOG", log.to_str().unwrap()),
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C.UTF-8"),
    ];
    variables.extend_from_slice(envs);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &variables,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        steps,
    );
    let requests = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    drop(directory);
    (compact_terminal_words(&output), requests)
}

#[test]
fn waiting_n_enter_opens_settings_without_confirming_and_session_disable_keeps_one_user() {
    let (output, requests) = run_trial(
        &[],
        &[
            ("cosh-osc$", "?? 帮我检查系统\n".as_bytes()),
            ("Esc cancel · n disable automatic access", b"n\n"),
            ("Disable for future sessions", b"\n"),
            ("Your question is retained; use /auth", b""),
        ],
    );
    let users: Vec<_> = requests
        .iter()
        .filter(|request| request["type"] == "user")
        .collect();
    assert_eq!(users.len(), 1, "{requests:?}\n{output}");
    assert_eq!(users[0]["ecs_trial_context"]["intent"], "user_initiated");
    assert_eq!(users[0]["ecs_trial_context"]["schema_version"], 1);
    assert!(requests
        .iter()
        .any(|request| request["type"] == "ecs_trial_control"
            && request["action"] == "pause"
            && request["run_id"] == "core-opaque"
            && request["revision"] == 2));
    assert!(!requests.iter().any(
        |request| request["action"] == "set_trial_preference" || request["action"] == "resume"
    ));
    assert!(!output.contains("Activated; continuing"), "{output}");
    assert!(!output.contains("No pending question"), "{output}");
}

#[test]
fn unknown_result_and_core_exit_preserve_permanent_close_with_reserved_fallback() {
    let (output, requests) = run_trial(
        &[("TRIAL_DISPATCH", "unknown"), ("TRIAL_EXIT_ON_PAUSE", "1")],
        &[
            ("cosh-osc$", "?? 帮我检查系统\n".as_bytes()),
            ("Esc cancel · n disable automatic access", b"n\n"),
            ("Esc return to Shell", b"2\n"),
            ("Automatic OS Console access disabled.", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["type"] == "user")
            .count(),
        1,
        "{requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["action"] == "set_trial_preference")
            .count(),
        1,
        "{requests:?}\n{output}"
    );
    assert!(!requests.iter().any(|request| request["action"] == "resume"));
    assert!(!output.contains("Activated; continuing"), "{output}");
}

#[test]
fn auth_explicit_trial_switch_activates_without_model_question() {
    let (output, requests) = run_trial(
        &[],
        &[
            ("cosh-osc$", b"/auth\n"),
            ("操作系统控制台（免配置接入）", b"\n"),
            (
                "Selected OS Console model service. You can ask a question now.",
                b"",
            ),
        ],
    );
    let activations: Vec<_> = requests
        .iter()
        .filter(|request| request["action"] == "activate_trial")
        .collect();
    assert_eq!(activations.len(), 1, "{requests:?}\n{output}");
    assert!(activations[0]["params"].is_null());
    assert!(requests.iter().any(
        |request| request["action"] == "state" && request["params"]["probe_ecs_trial"] == true
    ));
    assert!(!requests.iter().any(|request| request["type"] == "user"
        || request["action"] == "prepare"
        || request["action"] == "set_trial_preference"));
}

#[test]
fn failed_permanent_save_reports_session_only_without_success_claim() {
    let (output, requests) = run_trial(
        &[
            ("TRIAL_DISPATCH", "accepted"),
            ("TRIAL_EXIT_ON_PAUSE", "1"),
            ("TRIAL_SAVE_FAIL", "1"),
        ],
        &[
            ("cosh-osc$", b"?? trial-save-failure\n"),
            ("Esc cancel · n disable automatic access", b"n"),
            ("Esc return to Shell", b"2\n"),
            ("Unable to save; disabled for this session only.", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["action"] == "set_trial_preference")
            .count(),
        1,
        "{requests:?}"
    );
    assert!(
        !output.contains("Automatic OS Console access disabled."),
        "{output}"
    );
}

#[test]
fn failed_permanent_save_can_retry_disabled_preference_from_auth() {
    let (output, requests) = run_trial(
        &[
            ("TRIAL_DISPATCH", "accepted"),
            ("TRIAL_EXIT_ON_PAUSE", "1"),
            ("TRIAL_SAVE_FAIL_ONCE", "1"),
        ],
        &[
            ("cosh-osc$", b"?? trial-save-retry\n"),
            ("Esc cancel · n disable automatic access", b"n"),
            ("Esc return to Shell", b"2\n"),
            (
                "Unable to save; disabled for this session only.",
                b"/auth\n",
            ),
            ("重试保存免配置接入设置", b"\n"),
            ("Automatic OS Console access disabled.", b""),
        ],
    );
    let saves: Vec<_> = requests
        .iter()
        .filter(|request| request["action"] == "set_trial_preference")
        .collect();
    assert_eq!(saves.len(), 2, "{requests:?}\n{output}");
    assert!(saves
        .iter()
        .all(|request| request["params"]["auto_use"] == false));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["type"] == "user")
            .count(),
        1
    );
    assert!(!output.contains("Free trial enabled"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn activation_card_and_qr_stay_aligned_in_a_real_raw_pty() {
    use super::support::terminal_screen::TerminalSession;

    let mut session = TerminalSession::spawn_for_shell_with_startup_env(
        "bash",
        "enhanced",
        80,
        &[
            ("bin/trial-core", CORE),
            (
                ".copilot-shell/config.toml",
                "[shell]\nadapter_default = 'cosh-core'\n",
            ),
        ],
        &[
            ("COSH_CORE_PATH", "$HOME/bin/trial-core"),
            ("TRIAL_LOG", "$HOME/trial.log"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
    );
    session.send(b"?? explain load\n");
    session.wait_screen("activation link aligned", |screen| {
        screen
            .rows(0, 80)
            .any(|row| row.trim_end() == "https://console.aliyun.com/alinux/welcome?source=cosh")
    });
    session.resize_to(40, 80);
    session.wait_screen("complete QR aligned after resize", |screen| {
        (0..40)
            .filter(|row| {
                (2..43).all(|col| {
                    screen
                        .cell(*row, col)
                        .is_some_and(|cell| cell.bgcolor() == vt100::Color::Rgb(255, 255, 255))
                })
            })
            .count()
            == 21
    });
    session.send(b"\x03");
    session.wait_screen("cancel returns the prompt", |screen| {
        screen.contents().trim_end().ends_with("screen$")
    });
    session.finish();
}

#[test]
fn activation_continues_without_enter_and_without_a_second_user_submission() {
    let (output, requests) = run_trial(
        &[("TRIAL_AUTO_READY", "1")],
        &[
            ("cosh-osc$", b"?? trial-auto-resume\n"),
            (
                "trial answer automatically resumed",
                b"printf 'TRIAL_%s\\n' AFTER_AUTOMATIC_READY\n",
            ),
            ("TRIAL_AFTER_AUTOMATIC_READY", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["type"] == "user")
            .count(),
        1,
        "{requests:?}\n{output}"
    );
    assert!(!requests
        .iter()
        .any(|request| request["type"] == "ecs_trial_control"));
}

#[test]
fn escape_cancels_waiting_and_releases_native_shell_input() {
    let (output, requests) = run_trial(
        &[],
        &[
            ("cosh-osc$", b"?? trial-cancel\n"),
            ("Esc cancel · n disable automatic access", b"\x1b"),
            (
                "Your question is retained; use /auth",
                b"printf 'TRIAL_%s\\n' NATIVE_INPUT_RESTORED\n",
            ),
            ("TRIAL_NATIVE_INPUT_RESTORED", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(
                |request| request["type"] == "ecs_trial_control" && request["action"] == "cancel"
            )
            .count(),
        1,
        "{requests:?}\n{output}"
    );
    assert!(!requests
        .iter()
        .any(|request| request["action"] == "set_trial_preference"));
}

#[test]
fn old_core_capabilities_never_receive_trial_context() {
    let (output, requests) = run_trial(
        &[("TRIAL_LEGACY", "1")],
        &[
            ("cosh-osc$", b"?? legacy-core-question\n"),
            ("legacy answer completed", b""),
        ],
    );
    let users: Vec<_> = requests
        .iter()
        .filter(|request| request["type"] == "user")
        .collect();
    assert_eq!(users.len(), 1, "{requests:?}\n{output}");
    assert!(users[0].get("ecs_trial_context").is_none());
    assert!(!output.contains("AI Shell · Automatic access"), "{output}");
}

#[test]
fn late_answer_after_pause_is_not_rendered_after_cancel_returns_to_shell() {
    let (output, requests) = run_trial(
        &[("TRIAL_LATE_TEXT", "1")],
        &[
            ("cosh-osc$", b"?? late-output-question\n"),
            ("Esc cancel · n disable automatic access", b"n"),
            ("Disable for future sessions", b"\n"),
            (
                "Your question is retained; use /auth",
                b"printf 'AFTER_%s\\n' TRIAL_LATE_OUTPUT\n",
            ),
            ("AFTER_TRIAL_LATE_OUTPUT", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["type"] == "user")
            .count(),
        1
    );
    assert!(
        !output.contains("TRIAL_LATE_ANSWER_MUST_NOT_RENDER"),
        "{output}"
    );
}

#[test]
fn cancel_local_ack_without_result_is_reaped_before_permanent_preference_save() {
    let (output, requests) = run_trial(
        &[("TRIAL_HANG_AFTER_CANCEL", "1")],
        &[
            ("cosh-osc$", b"?? trial-hung-final-result\n"),
            ("Esc cancel · n disable automatic access", b"n"),
            ("Disable for future sessions", b"2\n"),
            ("Automatic OS Console access disabled.", b""),
        ],
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["type"] == "user")
            .count(),
        1,
        "{requests:?}\n{output}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["action"] == "set_trial_preference")
            .count(),
        1,
        "{requests:?}\n{output}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request["request"]["subtype"] == "interrupt"),
        "{requests:?}"
    );
}

#[test]
fn access_copy_delayed_switch_keeps_status_off_the_shell_prompt() {
    let (output, requests) = run_trial(
        &[("TRIAL_ACTIVATION_DELAY", "0.3")],
        &[
            ("cosh-osc$", b"/auth\n"),
            ("操作系统控制台（免配置接入）", b"\n"),
            (
                "Selected OS Console model service. You can ask a question now.",
                b"",
            ),
        ],
    );
    let start = output
        .find("Switching model service…")
        .expect("pending status");
    let end = output
        .find("Selected OS Console model service.")
        .expect("completed status");
    assert!(start < end, "{output}");
    assert!(
        !output[start..end].contains("cosh-osc$"),
        "prompt was restored before completion: {output}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["action"] == "activate_trial")
            .count(),
        1
    );
    assert!(!requests.iter().any(|r| r["type"] == "user"));
}

#[test]
fn kongche_terminal_failure_is_shown_once_with_the_specific_reason() {
    for (code, message) in [
        ("Sysom.CreditsExhausted", "Available quota is insufficient."),
        ("trial_wait_timeout", "Activation wait timed out."),
        (
            "EcsIdentityUnavailable",
            "Unable to access the ECS identity.",
        ),
    ] {
        for disconnect in ["0", "1"] {
            let (output, requests) = run_trial(
                &[
                    ("TRIAL_ERROR_CODE", code),
                    ("TRIAL_EXIT_AFTER_FAILURE", disconnect),
                ],
                &[
                    ("cosh-osc$", b"?? trial-failure\n"),
                    (message, b"printf 'KONGCHE_%s\n' FAILURE_DONE\n"),
                    ("KONGCHE_FAILURE_DONE", b""),
                ],
            );
            assert_eq!(output.matches(message).count(), 1, "{output}");
            assert!(!output.contains("Model service unavailable"), "{output}");
            assert!(!output.contains("Cancelled."), "{output}");
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request["type"] == "user")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn kongche_cancel_and_suppression_have_one_terminal_notice() {
    for choice in [0, 1, 2] {
        let mut steps: Vec<(&str, &[u8])> = vec![("cosh-osc$", b"?? trial-cancel\n")];
        if choice == 0 {
            steps.push(("Esc cancel · n disable automatic access", b"\x03"));
        } else {
            steps.push(("Esc cancel · n disable automatic access", b"n"));
            steps.push((
                "Disable for future sessions",
                if choice == 1 { b"\n" } else { b"2\n" },
            ));
        }
        let final_notice = match choice {
            0 => "Cancelled. Your question is retained",
            1 => "Automatic access is disabled for this session.",
            _ => "Automatic OS Console access disabled.",
        };
        steps.push((final_notice, b"printf 'KONGCHE_%s\n' CANCEL_DONE\n"));
        steps.push(("KONGCHE_CANCEL_DONE", b""));
        let (output, requests) = run_trial(&[], &steps);
        assert_eq!(
            output.matches("Cancelled.").count(),
            usize::from(choice == 0),
            "{output}"
        );
        assert_eq!(output.matches(final_notice).count(), 1, "{output}");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["action"] == "set_trial_preference")
                .count(),
            usize::from(choice == 2),
            "{requests:?}"
        );
    }
}
