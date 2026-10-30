//! Package-owned RTK selection never discovers an unrelated user executable.

use aw_exec::{CommandSpec, Limits};
use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt},
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
        fixture.script(
            "managed/rtk",
            "#!/bin/sh\ntouch \"$AW_MANAGED_MARKER\"\necho 'rtk 0.49.0'\n",
        );
        fixture.script(
            "foreign/rtk",
            "#!/bin/sh\ntouch \"$AW_FOREIGN_MARKER\"\necho 'rtk 0.49.0'\n",
        );
        fixture.script(
            "home/.local/bin/rtk",
            "#!/bin/sh\ntouch \"$AW_FOREIGN_MARKER\"\necho 'rtk 0.49.0'\n",
        );
        fixture.script(
            "foreign/sh",
            "#!/bin/sh\ntouch \"$AW_FOREIGN_MARKER\"\nexit 1\n",
        );
        fixture.script("tokenless", r#"#!/usr/bin/python3
import json, os, pathlib, sys
r = json.load(sys.stdin)
pathlib.Path(__file__).with_suffix('.seen').write_text(os.environ['PATH'])
if r['operation'] == 'pre_tool':
    expected = str(pathlib.Path(__file__).parent / 'managed')
    assert os.environ['PATH'] == expected + ':/usr/bin:/bin'
    args = r['input']['arguments'].copy()
    args['command'] = expected + '/rtk git status'
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
    fn command(&self, cli: &Path, rtk: &Path, checksum: &str) -> CommandSpec {
        let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        environment.insert("PATH".into(), self.path("foreign").into_os_string());
        environment.insert("HOME".into(), self.path("home").into_os_string());
        environment.insert(
            "AW_FOREIGN_MARKER".into(),
            self.path("foreign.seen").into_os_string(),
        );
        environment.insert(
            "AW_MANAGED_MARKER".into(),
            self.path("managed.seen").into_os_string(),
        );
        environment.insert("TOKENLESS_STATS_ENABLED".into(), "0".into());
        environment.insert("TOKENLESS_SLS_ENABLED".into(), "0".into());
        environment.insert("TOKENLESS_COMPRESSION_ENABLED".into(), "1".into());
        CommandSpec {
            program: env!("CARGO_BIN_EXE_aw-provider-tokenless").into(),
            args: vec![
                "--cli".into(),
                cli.into(),
                "--data-dir".into(),
                self.path("state").into(),
                "--rtk".into(),
                rtk.into(),
                "--rtk-sha256".into(),
                checksum.into(),
            ],
            cwd: self.root.path().into(),
            environment,
        }
    }
    fn invoke(&self, command: &CommandSpec, value: Value) -> Value {
        let request = Protocol::new().unwrap().bind_invocation(value).unwrap();
        let output = aw_exec::run(
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
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn managed_path_is_child_only_and_rejects_missing_corrupt_or_unusable_payloads() {
    for mode in [
        "valid",
        "group-writable-file",
        "group-writable-directory",
        "world-writable-file",
        "world-writable-directory",
        "missing",
        "corrupt",
        "nonexecutable",
        "symlink",
        "symlink-directory",
        "wrong-version",
        "execution-failure",
    ] {
        let f = Fixture::new();
        let rtk = f.path("managed/rtk");
        if mode == "wrong-version" {
            f.script("managed/rtk", "#!/bin/sh\necho 'rtk 9.99.0'\n");
        }
        if mode == "execution-failure" {
            f.script("managed/rtk", "#!/bin/sh\nexit 1\n");
        }
        let checksum = format!("{:x}", Sha256::digest(fs::read(&rtk).unwrap()));
        match mode {
            "missing" => fs::remove_file(&rtk).unwrap(),
            "corrupt" => fs::write(&rtk, "corrupted").unwrap(),
            "nonexecutable" => {
                fs::set_permissions(&rtk, fs::Permissions::from_mode(0o600)).unwrap()
            }
            "group-writable-file" => {
                fs::set_permissions(&rtk, fs::Permissions::from_mode(0o775)).unwrap()
            }
            "group-writable-directory" => {
                fs::set_permissions(rtk.parent().unwrap(), fs::Permissions::from_mode(0o775))
                    .unwrap()
            }
            "world-writable-file" => {
                fs::set_permissions(&rtk, fs::Permissions::from_mode(0o777)).unwrap()
            }
            "world-writable-directory" => {
                fs::set_permissions(rtk.parent().unwrap(), fs::Permissions::from_mode(0o777))
                    .unwrap()
            }
            "symlink" => {
                fs::remove_file(&rtk).unwrap();
                symlink(f.path("foreign/rtk"), &rtk).unwrap();
            }
            "symlink-directory" => {
                fs::rename(f.path("managed"), f.path("linked-managed")).unwrap();
                symlink(f.path("linked-managed"), f.path("managed")).unwrap();
            }
            _ => {}
        }
        let command = f.command(&f.path("tokenless"), &rtk, &checksum);
        let reply = f.invoke(&command, f.request(true));
        if matches!(
            mode,
            "valid" | "group-writable-file" | "group-writable-directory"
        ) {
            assert_eq!(reply["effects"][0]["reason_code"], "tokenless_retry");
            assert!(reply["effects"][0]["text"]
                .as_str()
                .unwrap()
                .contains(rtk.to_str().unwrap()));
        } else {
            assert_eq!(
                reply["error_code"], "managed_rtk_unavailable",
                "{mode}: {reply}"
            );
            assert!(!f.path("tokenless.seen").exists());
            if matches!(
                mode,
                "world-writable-file" | "world-writable-directory" | "symlink-directory"
            ) {
                assert!(!f.path("managed.seen").exists());
            }
        }
        assert!(!f.path("foreign.seen").exists());
        assert_eq!(
            command
                .environment
                .get(std::ffi::OsStr::new("PATH"))
                .unwrap(),
            f.path("foreign").as_os_str()
        );
        assert_eq!(
            f.invoke(&command, f.request(false))["effects"],
            json!([]),
            "{mode}"
        );
        assert!(!f.path("foreign.seen").exists());
    }
}

#[test]
#[ignore = "requires the package-built Tokenless CLI and pinned patched RTK"]
fn real_managed_rtk_rewrites_executes_and_preserves_attributed_after() {
    for adapter in ["qoder", "claude-sdk"] {
        exercise_real_managed_rtk(adapter);
    }
}

fn exercise_real_managed_rtk(adapter: &str) {
    let f = Fixture::new();
    let cli = PathBuf::from(
        std::env::var_os("AW_TOKENLESS_TEST_CLI").expect("set AW_TOKENLESS_TEST_CLI"),
    );
    let source = PathBuf::from(
        std::env::var_os("AW_TOKENLESS_TEST_RTK").expect("set AW_TOKENLESS_TEST_RTK"),
    );
    assert!(
        cli.is_file(),
        "AW_TOKENLESS_TEST_CLI is not a file: {}",
        cli.display()
    );
    let rtk = f.path("managed/rtk");
    fs::copy(&source, &rtk).unwrap();
    // Model installation: Cargo's artifact mode depends on its build umask,
    // whereas aw-package sets installed executables to the manifest's 0755.
    fs::set_permissions(&rtk, fs::Permissions::from_mode(0o755)).unwrap();
    let checksum = format!("{:x}", Sha256::digest(fs::read(&rtk).unwrap()));
    let mut command = f.command(&cli, &rtk, &checksum);
    // Native commands retain their normal PATH; only the Provider's CLI child
    // receives the managed dependency directory ahead of system executables.
    command.environment.insert(
        "PATH".into(),
        std::env::join_paths([f.path("foreign"), "/usr/bin".into(), "/bin".into()]).unwrap(),
    );
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
    let reply = f.invoke(&command, before.clone());
    let mut diagnostic = String::new();
    if reply["effects"][0]["reason_code"] != "tokenless_retry" {
        // A shell may normalize the environment, so only retry for diagnostics;
        // the assertion below always evaluates the original direct invocation.
        let stderr_path = f.path("cli.stderr");
        let wrapper = f.path("cli-wrapper");
        f.script(
            "cli-wrapper",
            &format!(
                "#!/bin/sh\nexec {} \"$@\" 2>{}\n",
                shlex::try_quote(cli.to_str().unwrap()).unwrap(),
                shlex::try_quote(stderr_path.to_str().unwrap()).unwrap()
            ),
        );
        let mut retry = command.clone();
        retry.args[1] = wrapper.into_os_string();
        let retry_reply = f.invoke(&retry, before.clone());
        let mut stderr = Vec::new();
        if let Ok(file) = fs::File::open(&stderr_path) {
            file.take(16 * 1024).read_to_end(&mut stderr).unwrap();
        }
        let metadata = |path: &Path| match fs::symlink_metadata(path) {
            Ok(meta) => format!(
                "uid={} mode={:o} file={} directory={} symlink={}",
                meta.uid(),
                meta.mode() & 0o7777,
                meta.is_file(),
                meta.is_dir(),
                meta.is_symlink()
            ),
            Err(error) => format!("{error}"),
        };
        let version = aw_exec::run(
            &CommandSpec {
                program: rtk.clone(),
                args: vec!["--version".into()],
                cwd: command.cwd.clone(),
                environment: command.environment.clone(),
            },
            b"",
            Limits {
                input_bytes: 0,
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
            Instant::now() + Duration::from_secs(3),
            &AtomicBool::new(false),
        );
        diagnostic = format!(
            "source {source:?}: {}; managed {rtk:?}: {}; parent: {}; \
             expected digest={checksum}, actual digest={:x}; version={version:?}; \
             diagnostic retry: {retry_reply}; CLI stderr (up to 16 KiB): {}",
            metadata(&source),
            metadata(&rtk),
            metadata(rtk.parent().unwrap()),
            Sha256::digest(fs::read(&rtk).unwrap()),
            String::from_utf8_lossy(&stderr)
        );
    }
    assert_eq!(
        reply["effects"][0]["reason_code"], "tokenless_retry",
        "{reply}; {diagnostic}"
    );
    let arguments: Value = serde_json::from_str(
        reply["effects"][0]["text"]
            .as_str()
            .unwrap()
            .strip_prefix("[tokenless:rewrite] Retry this tool with arguments: ")
            .unwrap(),
    )
    .unwrap();
    assert!(arguments["command"]
        .as_str()
        .unwrap()
        .contains(rtk.to_str().unwrap()));
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
