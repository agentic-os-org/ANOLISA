//! Retry suggestions and executed RTK ownership share the public lifecycle wire.

use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let build = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tokenless-validation");
        fs::create_dir_all(&build).unwrap();
        let root = tempfile::tempdir_in(fs::canonicalize(build).unwrap()).unwrap();
        let cli = root.path().join("tokenless");
        fs::write(&cli, format!(r#"#!/usr/bin/python3
import json, os, sys, time
r = json.load(sys.stdin)
assert sys.argv[1:] == ['compress']
mode = {mode:?}
if mode == 'failed': sys.exit(1)
if mode == 'slow': time.sleep(2)
if r['operation'] == 'pre_tool':
    assert r['input']['capabilities'] == {{'replace_arguments': False, 'block_and_suggest': True}}
    args = r['input']['arguments'].copy()
    args['command'] = ('env TOKENLESS_AGENT_ID=' + r['attribution']['agent_id']
        + ' TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=blocked-call TOKENLESS_DATA_DIR='
        + os.environ['TOKENLESS_DATA_DIR'] + ' /opt/tokenless/rtk git status')
    action, optimization = 'block_and_suggest', 'rtk'
    if mode == 'direct': action = 'replace_arguments'
    if mode == 'other_field': args['cwd'] = '/changed'
    if mode == 'empty': args['command'] = ''
    if mode == 'wrong_state': optimization = 'none'
    if mode == 'passthrough': args, action, optimization = r['input']['arguments'], 'passthrough', 'none'
    if mode == 'mutated_passthrough': action, optimization = 'passthrough', 'none'
    result = {{'arguments': args, 'action': action, 'output_optimization': optimization}}
else:
    assert r['input']['output_optimization'] == 'rtk', r
    assert r['input']['capabilities']['recovery'] == {{'kind':'none'}}
    result = {{'disposition':'passthrough','output':r['input']['content']}}
if mode == 'wrong_identity': r['attribution']['agent_id'] = 'other'
print(json.dumps({{'protocol_version':2,'operation':r['operation'], 'attribution':r['attribution'], 'result':result}}))
"#)).unwrap();
        fs::set_permissions(cli, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    fn request(&self, adapter: &str) -> Value {
        let tool = match adapter {
            "qoder" => "Bash",
            "hermes" => "terminal",
            "pi" => "bash",
            _ => "exec",
        };
        json!({"api_version":VERSION,"method":"invoke","request_id":"request","operation":"pre_tool","config":{"version":1},"config_revision":"b".repeat(64),"budget_ms":1000,"allowed_effects":["block"],"event":{"name":"tool.before","agent":{"adapter":adapter,"binding_id":"target","instance_id":"instance"},"session_id":"session","tool":{"name":tool,"native_name":tool,"call_id":"blocked-call","input":{"command":"git status","cwd":"/original"},"result":null},"native":{}}})
    }

    fn run(&self, value: Value) -> Value {
        let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
        let mut out = Vec::new();
        aw_provider_tokenless::run(
            &mut serde_json::to_vec(request.as_value()).unwrap().as_slice(),
            &mut out,
            &self.root.path().join("tokenless"),
            self.root.path(),
            Duration::from_millis(500),
        )
        .unwrap();
        serde_json::from_slice(&out).unwrap()
    }
}

#[test]
fn adapters_block_original_and_carry_retry_ownership_into_after() {
    for adapter in ["qoder", "openclaw", "qwenpaw", "hermes", "pi", "agentscope"] {
        let f = Fixture::new("suggest");
        let before = f.request(adapter);
        let reply = f.run(before.clone());
        let effect = &reply["effects"][0];
        assert_eq!(effect["type"], "block");
        assert_eq!(effect["reason_code"], "tokenless_retry");
        let text = effect["text"].as_str().unwrap();
        let retry: Value = serde_json::from_str(
            text.strip_prefix("[tokenless:rewrite] Retry this tool with arguments: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(retry["cwd"], "/original");
        assert!(!f.root.path().join("rtk").exists());
        let mut next = before;
        next["event"]["tool"]["call_id"] = json!("retry-call");
        next["event"]["tool"]["input"] = retry;
        assert_eq!(f.run(next.clone())["effects"], json!([]));
        // The suggested command is a fresh input to the real sec-core bridge,
        // regardless of Tokenless returning neutral for the attributed retry.
        let scanner = f.root.path().join("scanner");
        fs::write(
            &scanner,
            r#"#!/usr/bin/python3
import json, pathlib, sys
code = sys.argv[sys.argv.index('--code') + 1]
pathlib.Path(__file__).with_suffix('.seen').write_text(code)
print(json.dumps({'ok':True,'verdict':'deny'}))
"#,
        )
        .unwrap();
        fs::set_permissions(&scanner, fs::Permissions::from_mode(0o700)).unwrap();
        let mut scan = next.clone();
        scan["operation"] = json!("scan_code");
        let tool = scan["event"]["tool"]["name"].as_str().unwrap().to_owned();
        scan["config"] = json!({"version":1,"mode":"block","tools":{tool:{"language":"bash","input_pointer":"/command"}}});
        scan["allowed_effects"] = json!(["observe", "block"]);
        let scan = Protocol::new().unwrap().bind_invocation(scan).unwrap();
        let mut scanned = Vec::new();
        aw_provider_sec_core::run(
            &mut serde_json::to_vec(scan.as_value()).unwrap().as_slice(),
            &mut scanned,
            &scanner,
            &f.root.path().join("scanner.sock"),
            Duration::from_secs(1),
        )
        .unwrap();
        let denied: Value = serde_json::from_slice(&scanned).unwrap();
        assert!(denied["effects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effect| effect["type"] == "block"));
        assert_eq!(
            fs::read_to_string(scanner.with_extension("seen")).unwrap(),
            next["event"]["tool"]["input"]["command"].as_str().unwrap()
        );
        next["operation"] = json!("post_tool");
        next["allowed_effects"] = json!(["replace_result"]);
        next["event"]["name"] = json!("tool.after");
        next["event"]["tool"]["result"] = json!("rtk output");
        next["event"]["tool"]["result_projection"] =
            json!({"text":"rtk output","status":"success","content_origin":"command_output"});
        assert_eq!(f.run(next)["effects"], json!([]));
    }
}

#[test]
fn pretool_rejects_direct_mutations_and_keeps_execution_errors_visible() {
    for (mode, code) in [
        ("direct", "invalid_pretool_result"),
        ("other_field", "invalid_pretool_result"),
        ("empty", "invalid_pretool_result"),
        ("wrong_state", "invalid_pretool_result"),
        ("mutated_passthrough", "invalid_pretool_result"),
        ("wrong_identity", "invalid_compression_result"),
        ("failed", "cli_failed"),
        ("slow", "deadline_exceeded"),
    ] {
        let f = Fixture::new(mode);
        let reply = f.run(f.request("qoder"));
        assert_eq!(reply["error_code"], code, "{mode}: {reply}");
        assert!(reply.get("effects").is_none());
    }
    let f = Fixture::new("passthrough");
    assert_eq!(f.run(f.request("qoder"))["effects"], json!([]));
}

#[test]
fn non_shell_inputs_never_reach_the_cli() {
    let f = Fixture::new("failed");
    for (name, input) in [
        ("http", json!({"command":"git status"})),
        ("Bash", json!({})),
        ("Bash", json!({"command":7})),
    ] {
        let mut request = f.request("qoder");
        request["event"]["tool"]["name"] = json!(name);
        request["event"]["tool"]["input"] = input;
        assert_eq!(f.run(request)["effects"], json!([]));
    }
}

#[test]
#[ignore = "requires a built Tokenless Protocol v2 CLI; package CI runs this test"]
fn public_cli_pretool_and_posttool_share_the_executed_wrapper() {
    use aw_exec::{CommandSpec, Limits};
    use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
    let f = Fixture::new("unused");
    let bin = f.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let rtk = bin.join("rtk");
    fs::write(&rtk, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'rtk 0.49.0'; elif [ \"$1\" = rewrite ]; then case \"$2\" in sudo\\ *) printf 'sudo rtk %s\\n' \"${2#sudo }\";; *) printf 'rtk %s\\n' \"$2\";; esac; else exit 99; fi\n").unwrap();
    fs::set_permissions(&rtk, fs::Permissions::from_mode(0o700)).unwrap();
    let cli = PathBuf::from(
        std::env::var_os("AW_TOKENLESS_TEST_CLI").expect("set AW_TOKENLESS_TEST_CLI"),
    );
    let diagnostics = f.root.path().join("cli.stderr");
    let captured = f.root.path().join("cli.input");
    let wrapper = f.root.path().join("cli-wrapper");
    fs::write(
        &wrapper,
        "#!/bin/sh\ntee \"$AW_TOKENLESS_SMOKE_INPUT\" | \"$AW_TOKENLESS_REAL_CLI\" \"$@\" 2>\"$AW_TOKENLESS_SMOKE_STDERR\"\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    // Keep dependency discovery independent of runner/developer executables.
    environment.insert(
        "PATH".into(),
        std::env::join_paths([
            bin.as_path(),
            std::path::Path::new("/usr/bin"),
            std::path::Path::new("/bin"),
        ])
        .unwrap(),
    );
    environment.insert("TOKENLESS_STATS_ENABLED".into(), "0".into());
    environment.insert("TOKENLESS_SLS_ENABLED".into(), "0".into());
    environment.insert("AW_TOKENLESS_REAL_CLI".into(), cli.into_os_string());
    environment.insert(
        "AW_TOKENLESS_SMOKE_STDERR".into(),
        diagnostics.clone().into_os_string(),
    );
    environment.insert(
        "AW_TOKENLESS_SMOKE_INPUT".into(),
        captured.clone().into_os_string(),
    );
    let probe = CommandSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "command -v rtk && rtk --version".into()],
        cwd: f.root.path().into(),
        environment: environment.clone(),
    };
    let probe = aw_exec::run(
        &probe,
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 4096,
            stderr_bytes: 4096,
        },
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    assert_eq!(
        String::from_utf8(probe.stdout).unwrap(),
        format!("{}\nrtk 0.49.0\n", rtk.display())
    );
    let mut command = CommandSpec {
        program: env!("CARGO_BIN_EXE_aw-provider-tokenless").into(),
        args: vec![
            "--cli".into(),
            wrapper.into_os_string(),
            "--data-dir".into(),
            f.root.path().into(),
        ],
        cwd: f.root.path().into(),
        environment,
    };
    let state_link = f.root.path().join("state-link");
    std::os::unix::fs::symlink(f.root.path(), &state_link).unwrap();
    for state in [
        f.root.path().to_path_buf(),
        state_link.clone(),
        state_link.join("missing/nested"),
    ] {
        command.args[3] = state.into_os_string();
        let invoke = |value: Value| -> Value {
            let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
            let output = aw_exec::run(
                &command,
                &serde_json::to_vec(request.as_value()).unwrap(),
                Limits {
                    input_bytes: 1024 * 1024,
                    stdout_bytes: 1024 * 1024,
                    stderr_bytes: 65536,
                },
                Instant::now() + Duration::from_secs(10),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Protocol::new()
                .unwrap()
                .check_response(&request, &output.stdout)
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}; call={}; operation={}; diagnostic={}",
                        request.as_value()["event"]["tool"]["call_id"],
                        request.as_value()["operation"],
                        fs::read_to_string(&diagnostics).unwrap_or_default()
                    )
                });
            serde_json::from_slice(&output.stdout).unwrap()
        };
        for original in ["git status", "rg 'foo|bar' src", "sudo git status"] {
            let mut before = f.request("hermes");
            before["event"]["tool"]["input"]["command"] = json!(original);
            before["budget_ms"] = json!(5000);
            let reply = invoke(before.clone());
            assert_eq!(reply["effects"][0]["type"], "block", "{reply}");
            let retry: Value = serde_json::from_str(
                reply["effects"][0]["text"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("[tokenless:rewrite] Retry this tool with arguments: ")
                    .unwrap(),
            )
            .unwrap();
            assert!(retry["command"]
                .as_str()
                .unwrap()
                .contains(rtk.to_str().unwrap()));
            before["event"]["tool"]["input"] = retry;
            before["event"]["tool"]["call_id"] = json!("fresh-retry");
            assert_eq!(invoke(before.clone())["effects"], json!([]));
            before["operation"] = json!("post_tool");
            before["allowed_effects"] = json!(["replace_result"]);
            before["event"]["name"] = json!("tool.after");
            let text =
                serde_json::to_string_pretty(&json!({"values":(0..100).collect::<Vec<_>>()}))
                    .unwrap();
            before["event"]["tool"]["result"] = json!(text);
            before["event"]["tool"]["result_projection"] =
                json!({"text":text,"status":"success","content_origin":"api_response"});
            assert_eq!(invoke(before.clone())["effects"], json!([]));
            let wire: Value = serde_json::from_slice(&fs::read(&captured).unwrap()).unwrap();
            assert_eq!(wire["input"]["output_optimization"], "rtk");
            // The same synthetic output is compressible when it has no RTK owner.
            before["event"]["tool"]["input"]["command"] = json!(original);
            let reply = invoke(before);
            assert_eq!(reply["effects"][0]["type"], "replace_result", "{reply}");
        }
    }
    assert!(!f.root.path().join("missing").exists());
}
