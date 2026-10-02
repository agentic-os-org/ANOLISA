use super::*;

const CORE: &str = r#"#!/bin/sh
if [ "$1" = --registry ]; then
    read -r request
    printf '%s\n' "$request" >> "$AUTH_REGISTRY_LOG"
    request_id=${request#*\"request_id\":\"}
    request_id=${request_id%%\"*}
    case "$request" in
        *'"action":"state"'*)
            data='{"templates":[{"id":"aliyun","label":"Aliyun Authentication","fields":[{"name":"access_key_id","label":"Access Key ID","secret":true,"required":true}]}],"saved_providers":[]}' ;;
        *'"action":"prepare"'*)
            data='{"mode":"ecs_ram_role","instance_id":"i-fixture","console_url":"https://example.invalid/authorize","values":{"auth_source":"ecs_ram_role"}}' ;;
        *'"action":"verify"'*)
            if [ "$AUTH_WAIT_ONCE" = 1 ] && [ ! -f "$AUTH_READY_MARK" ]; then
                : > "$AUTH_READY_MARK"
                data='{"status":"not_ready","reason":"role_missing"}'
            else
                data='{"status":"ready"}'
            fi ;;
        *'"action":"configure"'*) data='{"provider_id":"aliyun"}' ;;
        *) data='{"configured":true}' ;;
    esac
    printf '{"type":"registry_response","request_id":"%s","success":true,"data":%s}\n' "$request_id" "$data"
    exit 0
fi
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"ecs-test","model":"test-model","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"ecs-test","is_error":false,"result":"done"}'
"#;

fn run_auth(wait_once: bool, inputs: &[(&str, &[u8])]) -> (String, String) {
    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let log = home.path().join("requests");
    let mark = home.path().join("ready");
    write_executable(&core, CORE);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", home.path().to_str().unwrap()),
            ("COSH_CORE_PATH", core.to_str().unwrap()),
            ("AUTH_REGISTRY_LOG", log.to_str().unwrap()),
            ("AUTH_READY_MARK", mark.to_str().unwrap()),
            ("AUTH_WAIT_ONCE", if wait_once { "1" } else { "0" }),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        inputs,
    );
    (
        compact_terminal_words(&output),
        fs::read_to_string(log).unwrap(),
    )
}

#[test]
fn ecs_ready_configures_without_name_or_authorization_confirmation() {
    let (output, requests) = run_auth(
        false,
        &[
            ("cosh-osc$", b"/auth\n"),
            ("Select your AI provider:", b"\n"),
            ("Auth configured", b""),
        ],
    );
    assert!(output.contains("Auth configured"), "{output}");
    assert!(!output.contains("Enter Provider ID"), "{output}");
    assert!(
        !output.contains("https://example.invalid/authorize"),
        "{output}"
    );
    assert!(!output.contains("I have authorized"), "{output}");
    assert_eq!(
        requests.matches("\"action\":\"configure\"").count(),
        1,
        "{requests}"
    );
}

#[test]
fn ecs_waiting_becomes_ready_without_another_keypress() {
    let (output, requests) = run_auth(
        true,
        &[
            ("cosh-osc$", b"/auth\n"),
            ("Select your AI provider:", b"\n"),
            ("Auth configured", b""),
        ],
    );
    assert!(output.contains("Waiting for ECS RAM Role"), "{output}");
    assert!(
        output.contains("https://example.invalid/authorize"),
        "{output}"
    );
    assert!(output.contains("Auth configured"), "{output}");
    assert_eq!(
        requests.matches("\"action\":\"verify\"").count(),
        2,
        "{requests}"
    );
    assert_eq!(
        requests.matches("\"action\":\"configure\"").count(),
        1,
        "{requests}"
    );
}

#[test]
fn ecs_cancel_returns_shell_input_and_allows_a_new_auth_flow() {
    let (output, requests) = run_auth(
        true,
        &[
            ("cosh-osc$", b"/auth\n"),
            ("Select your AI provider:", b"\n"),
            ("Waiting for ECS RAM Role", b"\x03"),
            ("Auth cancelled", b"printf 'AUTH-CANCEL-OK\\n'\n"),
            ("AUTH-CANCEL-OK", b""),
            ("cosh-osc$", b"/auth\n"),
            ("Select your AI provider:", b"\n"),
            ("Auth configured", b""),
        ],
    );
    assert!(output.contains("Auth cancelled"), "{output}");
    assert!(output.contains("AUTH-CANCEL-OK"), "{output}");
    assert!(output.contains("Auth configured"), "{output}");
    assert_eq!(
        requests.matches("\"action\":\"configure\"").count(),
        1,
        "{requests}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn initial_menu_prepare_ctrl_c_reaps_probe_and_worker_in_live_shell() {
    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let log = home.path().join("requests");
    // Check recorded PIDs/TIDs before the harness closes the live shell's stdin.
    let script = CORE.replacen(
        "#!/bin/sh\n",
        r#"#!/bin/sh
if [ "$1" = --assert-menu-prepare-reaped ]; then
    read -r owner < "$AUTH_PROBE_DIR/owner"
    read -r probe < "$AUTH_PROBE_DIR/probe"
    result=ok
    kill -0 "$owner" 2>/dev/null || result=shell-exited
    [ -d "/proc/$owner/task" ] || result=shell-exited
    [ ! -e "/proc/$probe" ] || result=probe-not-reaped
    [ -s "$AUTH_PROBE_DIR/workers" ] || result=worker-never-started
    while read -r worker; do
        [ ! -e "$worker" ] || result=worker-not-reaped
    done < "$AUTH_PROBE_DIR/workers"
    for comm in /proc/"$owner"/task/*/comm; do
        [ -r "$comm" ] || continue
        read -r name < "$comm" || continue
        case "$name" in cosh-auth-ecs*) result=ecs-worker-still-running ;; esac
    done
    printf '%s\n' "$result" > "$AUTH_PROBE_DIR/check-result"
    printf '%s\n' 'AUTH-MENU-CANCEL-CHECKS-DONE'
    exit 0
fi
"#,
        1,
    );
    let script = script.replace(
        "*'\"action\":\"prepare\"'*)",
        r#"*'"action":"prepare"'*)
            mkfifo "$AUTH_PROBE_DIR/block" || exit 1
            printf '%s\n' "$PPID" > "$AUTH_PROBE_DIR/owner"
            printf '%s\n' "$$" > "$AUTH_PROBE_DIR/probe"
            : > "$AUTH_PROBE_DIR/workers"
            for comm in /proc/"$PPID"/task/*/comm; do
                [ -r "$comm" ] || continue
                read -r name < "$comm" || continue
                case "$name" in
                    cosh-auth-ecs*) printf '%s\n' "${comm%/comm}" >> "$AUTH_PROBE_DIR/workers" ;;
                esac
            done
            # Registry stdout is private; synchronize through the recorded owner's stdout.
            printf '%s\n' 'AUTH-MENU-PREPARE-STARTED' > "/proc/$PPID/fd/1"
            # No writer exists: only cancellation/timeout can end this request.
            read -r unused < "$AUTH_PROBE_DIR/block"
            exit 1
"#,
    );
    write_executable(&core, &script);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", home.path().to_str().unwrap()),
            ("COSH_CORE_PATH", core.to_str().unwrap()),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("AUTH_REGISTRY_LOG", log.to_str().unwrap()),
            ("AUTH_PROBE_DIR", home.path().to_str().unwrap()),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"/auth\n"),
            ("AUTH-MENU-PREPARE-STARTED", b"\x03"),
            (
                "Auth cancelled",
                b"\"$COSH_CORE_PATH\" --assert-menu-prepare-reaped\n",
            ),
            (
                "AUTH-MENU-CANCEL-CHECKS-DONE",
                b"printf 'AUTH-%s\\n' 'MENU-COMMAND-OK'\n",
            ),
            ("AUTH-MENU-COMMAND-OK", b""),
        ],
    );
    let output = compact_terminal_words(&output);
    assert!(output.contains("AUTH-MENU-PREPARE-STARTED"), "{output}");
    assert!(output.contains("Auth cancelled"), "{output}");
    assert!(output.contains("AUTH-MENU-COMMAND-OK"), "{output}");
    assert_eq!(
        fs::read_to_string(home.path().join("check-result"))
            .unwrap()
            .trim(),
        "ok",
        "probe PID and named worker must be gone while the same shell is alive: {output}"
    );
    // A timed-out fallback or late menu must not impersonate cancellation of prepare.
    assert!(!output.contains("Select your AI provider:"), "{output}");
    assert!(!output.contains("Auth configured"), "{output}");
    let requests = fs::read_to_string(log).unwrap();
    let auth_calls: Vec<serde_json::Value> = requests
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|call| call["domain"] == "auth")
        .collect();
    assert_eq!(
        auth_calls
            .iter()
            .map(|call| call["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["state", "prepare"],
        "cancel must not prepare again, verify, or configure: {requests}"
    );
}
