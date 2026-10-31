//! RTK linked into the Tokenless CLI never discovers an unrelated user executable.

use aw_exec::{CommandSpec, Limits};
use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tokenless-validation");
        fs::create_dir_all(&target).unwrap();
        let root = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        let fixture = Self { root };
        for decoy in ["legacy/rtk", "foreign/rtk", "home/.local/bin/rtk"] {
            fixture.script(
                decoy,
                "#!/bin/sh\ntouch \"$AW_FOREIGN_MARKER\"\necho 'rtk 0.49.0'\n",
            );
        }
        fixture.script("tokenless", r#"#!/usr/bin/python3
import json, os, pathlib, sys
r = json.load(sys.stdin)
here = pathlib.Path(__file__)
here.with_suffix('.seen').write_text(os.environ['PATH'])
if r['operation'] == 'pre_tool':
    args = r['input']['arguments'].copy()
    args['command'] = str(here) + ' rtk git status'
    result = {'action':'block_and_suggest','arguments':args,'output_optimization':'rtk'}
else:
    result = {'disposition':'passthrough','output':r['input']['content']}
print(json.dumps({'protocol_version':2,'operation':r['operation'],'attribution':r['attribution'],'result':result}))
"#);
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn script(&self, name: &str, content: &str) {
        let path = self.path(name);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(&path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn request(&self, before: bool) -> Value {
        let mut value = json!({"api_version":VERSION,"method":"invoke","request_id":"request","operation":if before {"pre_tool"} else {"post_tool"},"config":{"version":1},"config_revision":"b".repeat(64),"budget_ms":3000,"allowed_effects":[if before {"block"} else {"replace_result"}],"event":{"name":if before {"tool.before"} else {"tool.after"},"agent":{"adapter":"qoder","binding_id":"target","instance_id":"instance"},"session_id":"session","tool":{"name":"Bash","native_name":"Bash","call_id":"call","input":{"command":"git status"},"result":"original output","result_projection":{"text":"original output","status":"success","content_origin":"command_output"}},"native":{}}});
        if before {
            value["event"]["tool"]["result"] = Value::Null;
            value["event"]["tool"]
                .as_object_mut()
                .unwrap()
                .remove("result_projection");
        }
        value
    }
    fn command(&self, cli: &Path, extra: &[&str]) -> CommandSpec {
        let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        environment.insert(
            "PATH".into(),
            std::env::join_paths([self.path("foreign"), "/usr/bin".into(), "/bin".into()]).unwrap(),
        );
        environment.insert("HOME".into(), self.path("home").into_os_string());
        environment.insert(
            "AW_FOREIGN_MARKER".into(),
            self.path("foreign.seen").into_os_string(),
        );
        environment.insert("TOKENLESS_STATS_ENABLED".into(), "0".into());
        environment.insert("TOKENLESS_SLS_ENABLED".into(), "0".into());
        environment.insert("TOKENLESS_COMPRESSION_ENABLED".into(), "1".into());
        let mut args = vec![
            "--cli".into(),
            cli.into(),
            "--data-dir".into(),
            self.path("state").into(),
        ];
        args.extend(extra.iter().map(Into::into));
        CommandSpec {
            program: env!("CARGO_BIN_EXE_aw-provider-tokenless").into(),
            args,
            cwd: self.root.path().into(),
            environment,
        }
    }
    fn execute(&self, command: &CommandSpec, value: Value) -> aw_exec::Output {
        let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
        aw_exec::run(
            command,
            &serde_json::to_vec(request.as_value()).unwrap(),
            Limits {
                input_bytes: 1024 * 1024,
                stdout_bytes: 1024 * 1024,
                stderr_bytes: 65536,
            },
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    fn invoke(&self, command: &CommandSpec, value: Value) -> Value {
        let output = self.execute(command, value);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn retry_command(reply: &Value) -> Value {
    assert_eq!(
        reply["effects"][0]["reason_code"], "tokenless_retry",
        "{reply}"
    );
    serde_json::from_str(
        reply["effects"][0]["text"]
            .as_str()
            .unwrap()
            .strip_prefix("[tokenless:rewrite] Retry this tool with arguments: ")
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn legacy_rtk_arguments_are_accepted_but_never_read_or_executed() {
    let f = Fixture::new();
    let legacy = f.path("legacy/rtk");
    let missing = f.path("missing/rtk");
    for extra in [
        vec![],
        vec!["--rtk", legacy.to_str().unwrap(), "--rtk-sha256", "stale"],
        vec!["--rtk", missing.to_str().unwrap(), "--rtk-sha256", "stale"],
    ] {
        let command = f.command(&f.path("tokenless"), &extra);
        let retry = retry_command(&f.invoke(&command, f.request(true)));
        assert_eq!(
            retry["command"],
            format!("{} rtk git status", f.path("tokenless").display())
        );
        // The CLI child keeps the caller's PATH; no RTK directory is injected.
        assert_eq!(
            fs::read_to_string(f.path("tokenless.seen")).unwrap(),
            command
                .environment
                .get(std::ffi::OsStr::new("PATH"))
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert_eq!(f.invoke(&command, f.request(false))["effects"], json!([]));
        assert!(!f.path("foreign.seen").exists(), "{extra:?}");
    }
    let legacy = legacy.to_str().unwrap();
    for (view, extra) in [
        (false, vec!["--rtk", legacy, "--other", "stale"]),
        (false, vec!["--other", legacy, "--rtk-sha256", "stale"]),
        (true, vec!["--rtk", legacy, "--rtk-sha256", "stale"]),
    ] {
        let mut command = f.command(&f.path("tokenless"), &extra);
        if view {
            command.args.insert(0, "--view".into());
        }
        let output = f.execute(&command, f.request(true));
        assert!(!output.status.success(), "{extra:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    assert!(!f.path("foreign.seen").exists());
}

#[test]
#[ignore = "requires the package-built Tokenless CLI with its linked RTK"]
fn real_linked_rtk_rewrites_executes_and_preserves_attributed_after() {
    for adapter in ["qoder", "claude-sdk"] {
        exercise_real_linked_rtk(adapter);
    }
}

fn exercise_real_linked_rtk(adapter: &str) {
    let f = Fixture::new();
    let cli = PathBuf::from(
        std::env::var_os("AW_TOKENLESS_TEST_CLI").expect("set AW_TOKENLESS_TEST_CLI"),
    );
    assert!(
        cli.is_file(),
        "AW_TOKENLESS_TEST_CLI is not a file: {}",
        cli.display()
    );
    let command = f.command(&cli, &[]);
    let git = std::process::Command::new("/usr/bin/git")
        .args(["init", "--quiet"])
        .current_dir(f.root.path())
        .status()
        .unwrap();
    assert!(git.success());
    let mut before = f.request(true);
    before["event"]["agent"]["adapter"] = json!(adapter);
    if adapter == "claude-sdk" {
        before["event"]["tool"]["name"] = json!("bash");
        before["event"]["tool"]["native_name"] = json!("Bash");
    }
    let arguments = retry_command(&f.invoke(&command, before.clone()));
    let words = shlex::split(arguments["command"].as_str().unwrap()).unwrap();
    let launcher = cli.canonicalize().unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair[0] == launcher.to_str().unwrap() && pair[1] == "rtk"),
        "{arguments}"
    );
    // Native execution keeps the foreign decoy first on PATH.
    let output = aw_exec::run(
        &CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), arguments["command"].as_str().unwrap().into()],
            cwd: f.root.path().into(),
            environment: command.environment.clone(),
        },
        b"",
        Limits {
            input_bytes: 0,
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
    before["event"]["tool"]["input"] = arguments;
    before["event"]["tool"]["call_id"] = json!("new-call");
    assert_eq!(f.invoke(&command, before.clone())["effects"], json!([]));
    before["operation"] = json!("post_tool");
    before["allowed_effects"] = json!(["replace_result"]);
    before["event"]["name"] = json!("tool.after");
    let text = String::from_utf8(output.stdout).unwrap();
    before["event"]["tool"]["result"] = json!(text);
    before["event"]["tool"]["result_projection"] =
        json!({"text": text, "status":"success", "content_origin":"command_output"});
    assert_eq!(f.invoke(&command, before)["effects"], json!([]));
    assert!(!f.path("foreign.seen").exists());
}
