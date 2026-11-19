//! Issue #9518: a stdio MCP server declared only in the writable user config
//! must not be auto-spawned at startup without an explicit trust/approval
//! decision. A user-level SessionStart hook marker is the control that proves
//! the runtime actually initialized, so an absent MCP marker is an
//! authorization decision rather than a harness that never reached startup.

use std::io::Write;
use std::process::Stdio;

use serde_json::Value;

mod common;

const MCP_SERVER_SCRIPT: &str = r#"
import sys, json, os
open(os.environ["PROBE_STARTED"], "w").write("started")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    method = msg.get("method")
    ident = msg.get("id")
    if method == "initialize":
        version = (msg.get("params") or {}).get("protocolVersion", "2024-11-05")
        print(json.dumps({"jsonrpc": "2.0", "id": ident, "result": {"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": {"name": "probe", "version": "0.0.1"}}}), flush=True)
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        print(json.dumps({"jsonrpc": "2.0", "id": ident, "result": {"tools": [{"name": "probe_touch", "description": "x", "inputSchema": {"type": "object", "properties": {}}}]}}), flush=True)
    elif method == "tools/call":
        open(os.environ["PROBE_PWNED"], "w").write("pwned")
        print(json.dumps({"jsonrpc": "2.0", "id": ident, "result": {"content": [{"type": "text", "text": "ok"}]}}), flush=True)
"#;

struct Workspace {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    started: std::path::PathBuf,
    pwned: std::path::PathBuf,
    hook_marker: std::path::PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(home.join(".copilot-shell")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let started = temp.path().join("mcp-started");
        let pwned = temp.path().join("mcp-pwned");
        let hook_marker = temp.path().join("hook-ran");
        std::fs::write(
            home.join(".copilot-shell/config.toml"),
            config_toml(&hook_marker, MCP_SERVER_SCRIPT, &started, &pwned),
        )
        .unwrap();
        let project = std::fs::canonicalize(&project).unwrap();
        Self {
            _temp: temp,
            home,
            project,
            started,
            pwned,
            hook_marker,
        }
    }

    fn rewrite_probe_script(&self, script: &str) {
        std::fs::write(
            self.home.join(".copilot-shell/config.toml"),
            config_toml(&self.hook_marker, script, &self.started, &self.pwned),
        )
        .unwrap();
    }

    fn rewrite_probe_pwned(&self, new_pwned: &std::path::Path) {
        std::fs::write(
            self.home.join(".copilot-shell/config.toml"),
            config_toml(
                &self.hook_marker,
                MCP_SERVER_SCRIPT,
                &self.started,
                new_pwned,
            ),
        )
        .unwrap();
    }

    fn run_mcp_cli(&self, args: &[&str]) -> Value {
        let output = common::cosh_core_command(&self.home)
            .current_dir(&self.project)
            .args(["--workspace", self.project.to_str().unwrap()])
            .arg("mcp")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run cosh-core mcp");
        assert!(
            output.status.success(),
            "mcp {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("mcp cli emits json")
    }

    fn run_mcp_cli_raw(&self, args: &[&str]) -> std::process::Output {
        common::cosh_core_command(&self.home)
            .current_dir(&self.project)
            .args(["--workspace", self.project.to_str().unwrap()])
            .arg("mcp")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run cosh-core mcp")
    }

    fn run_initialize(&self) -> Vec<Value> {
        let mut command = common::cosh_core_command(&self.home);
        command
            .current_dir(&self.project)
            .args(["--workspace", self.project.to_str().unwrap()])
            .env("PROBE_STARTED", &self.started)
            .env("PROBE_PWNED", &self.pwned)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn cosh-core");
        {
            let stdin = child.stdin.as_mut().unwrap();
            writeln!(
                stdin,
                r#"{{"type":"control_request","request_id":"init-1","request":{{"subtype":"initialize"}}}}"#
            )
            .unwrap();
            writeln!(
                stdin,
                r#"{{"type":"control_request","request_id":"shut-1","request":{{"subtype":"shutdown"}}}}"#
            )
            .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn config_toml(
    hook: &std::path::Path,
    script: &str,
    started: &std::path::Path,
    pwned: &std::path::Path,
) -> String {
    format!(
        r#"[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"

[hooks]
enabled = true

[[hooks.SessionStart]]
command = "touch {hook}"
name = "user-probe"

[mcp.servers.probe]
command = "python3"
args = ["-c", {script}]
startup_timeout_ms = 3000

[mcp.servers.probe.env]
PROBE_STARTED = "{started}"
PROBE_PWNED = "{pwned}"
"#,
        hook = hook.display(),
        script = toml_string(script),
        started = started.display(),
        pwned = pwned.display(),
    )
}

#[test]
fn untrusted_user_config_mcp_server_must_not_auto_spawn() {
    let workspace = Workspace::new();
    let messages = workspace.run_initialize();
    assert!(
        messages
            .iter()
            .any(|message| message["type"] == "system" && message["subtype"] == "init"),
        "runtime must initialize: {messages:?}"
    );
    assert!(
        workspace.hook_marker.exists(),
        "control: user SessionStart hook must fire so startup is proven"
    );
    assert!(
        !workspace.started.exists(),
        "issue #9518: user-config MCP stdio server auto-spawned without approval"
    );
    assert!(
        !workspace.pwned.exists(),
        "issue #9518: MCP tool handler ran without a startup trust decision"
    );
}

#[test]
fn trusted_user_config_mcp_server_spawns_and_registers_tools() {
    let workspace = Workspace::new();
    let shown = workspace.run_mcp_cli(&["show", "probe"]);
    assert_eq!(shown["command"], "python3");
    assert_eq!(shown["trusted"], false);
    let trusted = workspace.run_mcp_cli(&["trust", "probe", "--yes"]);
    assert_eq!(trusted["trusted"], true);
    assert_eq!(trusted["fingerprint"], shown["fingerprint"]);

    let messages = workspace.run_initialize();
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    assert!(
        workspace.started.exists(),
        "trusted server must spawn at startup"
    );
    assert!(
        init["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .any(|tool| tool == "mcp__probe__probe_touch"),
        "trusted server tools must register: {init:?}"
    );

    workspace.run_mcp_cli(&["untrust", "probe"]);
    assert_eq!(workspace.run_mcp_cli(&["show", "probe"])["trusted"], false);
}

#[test]
fn changed_command_invalidates_startup_trust() {
    let workspace = Workspace::new();
    workspace.run_mcp_cli(&["trust", "probe", "--yes"]);
    assert!(workspace
        .run_initialize()
        .iter()
        .any(|message| { message["type"] == "system" && message["subtype"] == "init" }));
    assert!(workspace.started.exists(), "trusted server spawns first");

    std::fs::remove_file(&workspace.started).unwrap();
    workspace.rewrite_probe_script(&format!("{MCP_SERVER_SCRIPT}\n# changed args"));
    workspace.run_initialize();
    assert!(
        !workspace.started.exists(),
        "changed args must invalidate the trust pin and re-gate startup"
    );
}

#[test]
fn changed_env_invalidates_startup_trust() {
    let workspace = Workspace::new();
    workspace.run_mcp_cli(&["trust", "probe", "--yes"]);
    workspace.run_initialize();
    assert!(workspace.started.exists(), "trusted server spawns first");

    std::fs::remove_file(&workspace.started).unwrap();
    let moved = workspace.pwned.with_file_name("mcp-pwned-moved");
    workspace.rewrite_probe_pwned(&moved);
    workspace.run_initialize();
    assert!(
        !workspace.started.exists(),
        "changed env must invalidate the trust pin and re-gate startup"
    );
}

#[test]
fn connect_requires_startup_trust() {
    let workspace = Workspace::new();
    // Connect must not grant persistent trust itself (it would bypass the consent gate).
    let refused = workspace.run_mcp_cli_raw(&["connect", "probe"]);
    assert!(
        !refused.status.success(),
        "untrusted connect must refuse: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not trusted"),
        "stderr={}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(workspace.run_mcp_cli(&["show", "probe"])["trusted"], false);

    // After explicit trust, connect succeeds and the session registers its tools.
    workspace.run_mcp_cli(&["trust", "probe", "--yes"]);
    workspace.run_mcp_cli(&["connect", "probe"]);
    let messages = workspace.run_initialize();
    let init = messages
        .iter()
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .expect("system init");
    assert!(workspace.started.exists(), "trusted server must auto-start");
    assert!(
        init["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .any(|tool| tool == "mcp__probe__probe_touch"),
        "trusted server tools must register: {init:?}"
    );
}

#[test]
fn non_interactive_trust_without_yes_fails_closed() {
    let workspace = Workspace::new();
    let output = workspace.run_mcp_cli_raw(&["trust", "probe"]);
    assert!(
        !output.status.success(),
        "piped trust without --yes must refuse: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interactive confirmation"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(workspace.run_mcp_cli(&["show", "probe"])["trusted"], false);
    workspace.run_initialize();
    assert!(
        !workspace.started.exists(),
        "refused trust must not enable startup spawn"
    );
}

#[test]
fn trust_binds_previewed_fingerprint() {
    let workspace = Workspace::new();
    let shown = workspace.run_mcp_cli(&["show", "probe"]);
    let fingerprint = shown["fingerprint"].as_str().expect("fingerprint");

    let refused = workspace.run_mcp_cli_raw(&[
        "trust",
        "probe",
        "--yes",
        "--expect-fingerprint",
        "sha256:stale",
    ]);
    assert!(
        !refused.status.success(),
        "a stale preview fingerprint must be refused"
    );
    assert_eq!(workspace.run_mcp_cli(&["show", "probe"])["trusted"], false);

    let trusted = workspace.run_mcp_cli(&[
        "trust",
        "probe",
        "--yes",
        "--expect-fingerprint",
        fingerprint,
    ]);
    assert_eq!(trusted["trusted"], true);
    assert_eq!(trusted["fingerprint"], fingerprint);
}

#[test]
fn relative_script_arg_resolves_against_workspace_not_core_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace_dir = temp.path().join("workspace");
    let core_cwd_dir = temp.path().join("core-cwd");
    let other_dir = temp.path().join("other");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::create_dir_all(&core_cwd_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();
    let workspace = std::fs::canonicalize(&workspace_dir).unwrap();
    let core_cwd = std::fs::canonicalize(&core_cwd_dir).unwrap();
    let other_workspace = std::fs::canonicalize(&other_dir).unwrap();
    std::fs::create_dir_all(home.join(".copilot-shell")).unwrap();
    let marker_workspace = temp.path().join("marker-workspace");
    let marker_planted = temp.path().join("marker-planted");
    std::fs::write(
        workspace.join("run.py"),
        format!(
            "import pathlib; pathlib.Path({:?}).write_text('ws')",
            marker_workspace.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        core_cwd.join("run.py"),
        format!(
            "import pathlib; pathlib.Path({:?}).write_text('planted')",
            marker_planted.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        home.join(".copilot-shell/config.toml"),
        r#"[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"

[mcp.servers.probe]
command = "python3"
args = ["./run.py"]
startup_timeout_ms = 3000
"#,
    )
    .unwrap();

    let trust = common::cosh_core_command(&home)
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "mcp",
            "trust",
            "probe",
            "--yes",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        trust.status.success(),
        "trust stderr={}",
        String::from_utf8_lossy(&trust.stderr)
    );

    // Run the core from a different cwd (planted run.py) but the trusted workspace.
    let mut child = common::cosh_core_command(&home)
        .current_dir(&core_cwd)
        .args(["--workspace", workspace.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"type":"control_request","request_id":"init-1","request":{{"subtype":"initialize"}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"type":"control_request","request_id":"shut-1","request":{{"subtype":"shutdown"}}}}"#
        )
        .unwrap();
    }
    let _ = child.wait_with_output().unwrap();
    assert!(
        marker_workspace.exists(),
        "the workspace run.py must execute (child cwd anchored to workspace)"
    );
    assert!(
        !marker_planted.exists(),
        "the planted core-cwd run.py must NOT execute"
    );

    // The same config is untrusted from a different workspace.
    let show = common::cosh_core_command(&home)
        .args([
            "--workspace",
            other_workspace.to_str().unwrap(),
            "mcp",
            "show",
            "probe",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(show.status.success());
    let show_json: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["trusted"], false);
}
