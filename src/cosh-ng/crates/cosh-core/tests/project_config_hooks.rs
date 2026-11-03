//! Project `.copilot-shell/config.toml` hook definitions never run in Core.
//!
//! cosh-shell's `trusted-project-hooks` store records consent for the external
//! `.cosh/hooks` tree only: a fingerprinted entry pins those bytes and a bare
//! root pins no content. Neither covers hook definitions in the project config,
//! so the real Core binary must not run them for any state of that store,
//! including after the user trusts, revokes, or edits the config. Every run also
//! carries a user-level SessionStart hook: it proves the hook pipeline fired,
//! so an absent project hook is an authorization decision rather than a harness
//! that never observed hooks.

use std::io::Write;
use std::process::Stdio;

use serde_json::Value;

mod common;

const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn hook_toml(name: &str) -> String {
    format!(
        "[hooks]\nenabled = true\n\n[[hooks.SessionStart]]\nname = \"{name}\"\n\
         command = \"echo '{{\\\"system_message\\\":\\\"{name}-ran\\\"}}'\"\n"
    )
}

struct Workspace {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    store: std::path::PathBuf,
}

impl Workspace {
    fn new(project_hook: &str) -> Self {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(home.join(".copilot-shell")).unwrap();
        std::fs::create_dir_all(project.join(".copilot-shell")).unwrap();
        std::fs::write(
            home.join(".copilot-shell/config.toml"),
            hook_toml("user-probe"),
        )
        .unwrap();
        let project = std::fs::canonicalize(&project).unwrap();
        let store = temp.path().join("trusted-project-hooks");
        let workspace = Self {
            _temp: temp,
            home,
            project,
            store,
        };
        workspace.set_project_hook(project_hook);
        workspace
    }

    fn set_project_hook(&self, name: &str) {
        std::fs::write(
            self.project.join(".copilot-shell/config.toml"),
            hook_toml(name),
        )
        .unwrap();
    }

    /// Writes the store the way cosh-shell records consent: a header comment,
    /// then the project root, optionally followed by ` <digest>`.
    fn trust(&self, digest: Option<&str>) {
        let mut content = String::from("# cosh-shell trusted project hook roots\n");
        content.push_str(&self.project.to_string_lossy());
        if let Some(digest) = digest {
            content.push(' ');
            content.push_str(digest);
        }
        content.push('\n');
        std::fs::write(&self.store, content).unwrap();
    }

    /// Rewrites the store without the project root, as `/hooks untrust-project` does.
    fn untrust(&self) {
        std::fs::write(&self.store, "# cosh-shell trusted project hook roots\n").unwrap();
    }

    /// Runs the real `cosh-core` through initialize/shutdown and returns the
    /// names of the SessionStart hooks that actually executed.
    fn session_start_hooks(&self) -> Vec<String> {
        let mut command = common::cosh_core_command(&self.home);
        command
            .current_dir(&self.project)
            .args(["--workspace", self.project.to_str().unwrap()])
            .env("COSH_SHELL_PROJECT_TRUST_STORE", &self.store)
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
            .filter(|message| {
                message["type"] == "system" && message["subtype"] == "hook_notification"
            })
            .filter_map(|message| message["hook_name"].as_str().map(str::to_string))
            .collect()
    }
}

fn assert_project_hook_blocked(hooks: &[String], project_hook: &str, case: &str) {
    // Checked first: an applied project layer replaces the user SessionStart
    // list, so the control below would otherwise mask the real failure.
    assert!(
        !hooks.iter().any(|name| name == project_hook),
        "{case}: project config hook `{project_hook}` ran, saw {hooks:?}"
    );
    assert!(
        hooks.iter().any(|name| name == "user-probe"),
        "{case}: user-level hook must keep running, saw {hooks:?}"
    );
}

#[test]
fn project_config_hook_is_blocked_without_trust_store() {
    let workspace = Workspace::new("project-probe");
    let hooks = workspace.session_start_hooks();
    assert_project_hook_blocked(&hooks, "project-probe", "no store");
}

#[test]
fn fingerprinted_consent_does_not_cover_project_config_hooks() {
    let workspace = Workspace::new("project-probe");
    workspace.trust(Some(DIGEST));
    let hooks = workspace.session_start_hooks();
    assert_project_hook_blocked(&hooks, "project-probe", "fingerprinted entry");
}

#[test]
fn legacy_bare_root_does_not_cover_project_config_hooks() {
    let workspace = Workspace::new("project-probe");
    workspace.trust(None);
    let hooks = workspace.session_start_hooks();
    assert_project_hook_blocked(&hooks, "project-probe", "legacy bare entry");
}

#[test]
fn edited_project_config_hook_is_not_authorized_by_earlier_consent() {
    for (form, digest) in [("fingerprinted", Some(DIGEST)), ("legacy bare", None)] {
        let workspace = Workspace::new("project-probe-v1");
        workspace.trust(digest);
        let before = workspace.session_start_hooks();
        workspace.set_project_hook("project-probe-v2");
        let after = workspace.session_start_hooks();
        assert_project_hook_blocked(&after, "project-probe-v2", &format!("{form} edited v2"));
        assert_project_hook_blocked(&before, "project-probe-v1", &format!("{form} v1"));
    }
}

#[test]
fn trust_revoke_and_retrust_never_enable_project_config_hooks() {
    // With no store state able to enable project config hooks, there is
    // nothing loaded that a later revocation would have to take back.
    let workspace = Workspace::new("project-probe");
    for (step, change) in [
        ("legacy trust", Some(None)),
        ("revoke", None),
        ("fingerprinted retrust", Some(Some(DIGEST))),
    ] {
        match change {
            Some(digest) => workspace.trust(digest),
            None => workspace.untrust(),
        }
        let hooks = workspace.session_start_hooks();
        assert_project_hook_blocked(&hooks, "project-probe", step);
    }
}

fn prompt_hook_toml(name: &str, marker: &std::path::Path) -> String {
    format!(
        "[hooks]\nenabled = true\n\n[[hooks.UserPromptSubmit]]\nname = \"{name}\"\n\
         command = '''printf x >> '{}'; echo '{{\"system_message\":\"{name}-ran\"}}' '''\n",
        marker.display()
    )
}

fn marker_count(path: &std::path::Path) -> usize {
    std::fs::read(path).map(|bytes| bytes.len()).unwrap_or(0)
}

#[test]
fn revoking_trust_in_a_running_core_leaves_no_project_config_hook_running() {
    // One persistent Core serves two turns. Trust is present when it loads its
    // config and revoked between the turns, exactly as `/hooks untrust-project`
    // does during a shell session. The user-level prompt hook is the control
    // that each turn really fired the hook pipeline.
    let workspace = Workspace::new("unused");
    let user_marker = workspace.home.join("user-hook.count");
    let project_marker = workspace.home.join("project-hook.count");
    std::fs::write(
        workspace.home.join(".copilot-shell/config.toml"),
        format!(
            "[ai]\nactive_provider = \"mock\"\n\n[ai.providers.mock]\ntype = \"mock\"\n\n{}",
            prompt_hook_toml("user-prompt-probe", &user_marker)
        ),
    )
    .unwrap();
    std::fs::write(
        workspace.project.join(".copilot-shell/config.toml"),
        prompt_hook_toml("project-prompt-probe", &project_marker),
    )
    .unwrap();
    workspace.trust(None);

    let mut child = common::cosh_core_command(&workspace.home)
        .current_dir(&workspace.project)
        .args(["--workspace", workspace.project.to_str().unwrap()])
        .env("COSH_SHELL_PROJECT_TRUST_STORE", &workspace.store)
        .env("COSH_STATES_DIR", workspace.home.join("states"))
        .env_remove("COSH_AI_PROVIDER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cosh-core");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let wait_for = |predicate: &dyn Fn(&Value) -> bool, what: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = line_rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("cosh-core did not emit {what}"));
            if serde_json::from_str::<Value>(&line).is_ok_and(|message| predicate(&message)) {
                return;
            }
        }
    };
    let send = |stdin: &mut std::process::ChildStdin, line: &str| {
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    };
    let is_result = |message: &Value| message["type"] == "result";

    send(
        &mut stdin,
        r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#,
    );
    wait_for(&|message| message["subtype"] == "init", "system init");
    send(
        &mut stdin,
        r#"{"type":"user","message":{"role":"user","content":"first turn"},"parent_tool_use_id":null}"#,
    );
    wait_for(&is_result, "the first turn result");
    let after_first = (marker_count(&user_marker), marker_count(&project_marker));

    workspace.untrust();
    send(
        &mut stdin,
        r#"{"type":"user","message":{"role":"user","content":"second turn"},"parent_tool_use_id":null}"#,
    );
    wait_for(&is_result, "the second turn result");
    let after_second = (marker_count(&user_marker), marker_count(&project_marker));
    send(
        &mut stdin,
        r#"{"type":"control_request","request_id":"shut-1","request":{"subtype":"shutdown"}}"#,
    );
    drop(stdin);
    let _ = child.wait();

    assert_eq!(
        after_second.1, 0,
        "project config hook ran in a running Core: first turn {after_first:?}, after revoke {after_second:?}"
    );
    assert_eq!(
        (after_first.0, after_second.0),
        (1, 2),
        "the user-level prompt hook must run once per turn"
    );
}

#[test]
fn project_config_still_cannot_widen_core_approval() {
    // Dropping project hooks must not loosen the other project-layer strips:
    // the real load path still refuses project approval_mode and allowed_tools
    // for every state of the trust store.
    let workspace = Workspace::new("project-probe");
    std::fs::write(
        workspace.project.join(".copilot-shell/config.toml"),
        "[agent]\napproval_mode = \"trust\"\nallowed_tools = [\"shell\", \"write_file\"]\n",
    )
    .unwrap();
    for (form, digest) in [
        ("no store", None),
        ("legacy bare", Some(None)),
        ("fingerprinted", Some(Some(DIGEST))),
    ] {
        match digest {
            Some(digest) => workspace.trust(digest),
            None => workspace.untrust(),
        }
        let mut command = common::cosh_core_command(&workspace.home);
        let output = command
            .current_dir(&workspace.project)
            .args(["--workspace", workspace.project.to_str().unwrap()])
            .env("COSH_SHELL_PROJECT_TRUST_STORE", &workspace.store)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                writeln!(
                    child.stdin.as_mut().unwrap(),
                    r#"{{"type":"control_request","request_id":"shut-1","request":{{"subtype":"shutdown"}}}}"#
                )?;
                child.wait_with_output()
            })
            .expect("run cosh-core");
        let stderr = String::from_utf8_lossy(&output.stderr);
        for field in ["agent.approval_mode", "agent.allowed_tools"] {
            assert!(
                stderr.contains(&format!("ignoring {field} from project config")),
                "{form}: project {field} was not stripped: {stderr}"
            );
        }
    }
}
