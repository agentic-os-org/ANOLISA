use std::ffi::OsString;

use asc_cli::{
    Cli, InputError,
    output::{render_policy, render_scan_code},
};
use asc_daemon_protocol::{DaemonResponse, RequestId};
use serde_json::{Value, json};

mod common;

#[test]
fn removed_probe_command_is_rejected() {
    assert!(Cli::parse_from(["agent-sec-cli", "agent-probe", "create"]).is_err());
}

#[test]
fn all_assignment_commands_match_frozen_wire_parameters() {
    let directory = common::Directory::new();
    let methods: Value = serde_json::from_str(common::METHODS).unwrap();
    let mut covered = std::collections::BTreeSet::new();
    for row in methods.as_array().unwrap() {
        let mut args = vec![OsString::from("agent-sec-cli")];
        args.extend(common::args_for(
            row,
            &directory.0,
            &directory.0.join("socket"),
        ));
        let actual =
            serde_json::to_value(Cli::parse_from(args).unwrap().request().unwrap()).unwrap();
        assert_eq!(
            actual,
            json!({"method": row["method"], "params": row["canonicalParams"]})
        );
        covered.insert(row["method"].as_str().unwrap());
    }
    assert_eq!(
        covered,
        asc_daemon_protocol::method::PAP_METHODS
            .into_iter()
            .collect()
    );
}

#[test]
fn equals_syntax_option_looking_values_and_awkward_paths_are_preserved() {
    // The file name carries a space and an `=` because both survive on every
    // filesystem this CLI runs on. Non-UTF-8 names cannot be created on macOS,
    // where the filesystem enforces UTF-8, so byte preservation for those is
    // asserted against the parsed path in the crate's own tests instead.
    let directory = common::Directory::new();
    let file = directory.0.join("policy=a b.json");
    std::fs::write(
        &file,
        br#"{"specVersion": "0.1", "rules": [{"effect": "block", "category": "file", "action": "write", "target": {"type": "file", "path": "/work/a b"}, "where": {"operation": {"eq": "delete"}}}]}"#,
    )
    .unwrap();
    let args: Vec<OsString> = vec![
        "agent-sec-cli".into(),
        "policy".into(),
        "create".into(),
        "--name=--help".into(),
        "--language=json".into(),
        "--file".into(),
        file.into_os_string(),
        "--socket=/run/asc.sock".into(),
    ];
    let request = Cli::parse_from(args).unwrap().request().unwrap();
    assert_eq!(request.params["policyName"], "--help");
    assert_eq!(
        request.params["template"]["rules"][0]["target"]["path"],
        "/work/a b"
    );
}

#[test]
fn scan_code_matches_the_v1_parameters_and_rejects_empty_input() {
    let cli = Cli::parse_from([
        "agent-sec-cli",
        "--socket",
        "/run/asc.sock",
        "scan-code",
        "--code",
        "echo hello",
    ])
    .unwrap();
    assert!(cli.is_scan_code());
    assert_eq!(
        serde_json::to_value(cli.request().unwrap()).unwrap(),
        json!({
            "method": "action.code_scan",
            "params": {
                "code": "echo hello",
                "language": "bash",
                "rules": null,
                "mode": "regex"
            }
        })
    );

    let empty = Cli::parse_from([
        "agent-sec-cli",
        "--socket",
        "/run/asc.sock",
        "scan-code",
        "--code",
        " \t ",
    ])
    .unwrap();
    assert!(matches!(empty.request(), Err(InputError::EmptyCode)));

    let hyphen_source = Cli::parse_from([
        "agent-sec-cli",
        "--socket",
        "/run/asc.sock",
        "scan-code",
        "--code",
        "--executable=$(which python3)",
    ])
    .unwrap();
    assert_eq!(
        hyphen_source.request().unwrap().params["code"],
        "--executable=$(which python3)"
    );

    let repeated = Cli::parse_from([
        "agent-sec-cli",
        "--socket",
        "/run/asc.sock",
        "scan-code",
        "--code",
        "echo a",
        "--language",
        "bash",
        "--code",
        "rm -rf /tmp/x",
        "--language",
        "python",
    ])
    .unwrap();
    let params = repeated.request().unwrap().params;
    assert_eq!(params["code"], "rm -rf /tmp/x");
    assert_eq!(params["language"], "python");
}

#[test]
fn invalid_and_ambiguous_options_are_usage_errors() {
    let cases = [
        vec!["policy", "get", "--policy-id", "p"],
        vec!["policy", "get", "--policy-id", "p", "--revision", "0"],
        vec!["policy", "get", "--policy-id", "", "--revision", "1"],
        vec!["policy", "list", "--limit", "1001"],
        vec!["policy", "list", "--limit", "0"],
        vec!["scope", "list", "--offset", "4294967296"],
        vec!["scope", "list", "--offset", "-1"],
        vec!["scope", "create"],
        vec!["scope", "create", "--pid", "0"],
        vec!["scope", "create", "--cgroup-id", "0"],
        vec!["scope", "create", "--pid", "1", "--cgroup-id", "2"],
        vec!["scope", "create", "--pid", "1", "--pid", "2"],
        vec!["policy", "list", "--timeout-ms", "0"],
        vec!["--timeout-ms", "100", "policy", "list", "--timeout-ms=200"],
        vec!["policy", "list", "--unknown"],
        vec!["policy", "list", "--role", "admin"],
        vec!["binding", "get", "--binding-id", "b", "--revision", "1"],
        vec!["policy", "create", "--name", "--help", "--file", "x"],
        vec!["policy", "list", "--socket", "/run/other.sock"],
    ];
    for case in cases {
        let mut args = vec!["agent-sec-cli", "--socket", "/run/asc.sock"];
        args.extend(case);
        let error = Cli::parse_from(args.clone()).unwrap_err();
        assert!(error.use_stderr(), "{args:?} unexpectedly produced help");
    }
    assert_eq!(
        Cli::parse_from(["agent-sec-cli", "policy", "list"])
            .unwrap()
            .socket(),
        Some(std::path::Path::new("/run/agent-sec-core/daemon.sock"))
    );
    assert!(
        Cli::parse_from([
            "agent-sec-cli",
            "--socket",
            "relative.sock",
            "policy",
            "list"
        ])
        .is_err()
    );
}

#[test]
fn repeated_socket_options_reject_non_utf8_inline_paths_at_every_level() {
    use std::os::unix::ffi::OsStringExt as _;

    let native = OsString::from_vec(b"--socket=/run/asc-\xff.sock".to_vec());
    let endpoints = [
        vec![native.clone()],
        vec!["--socket=/run/other.sock".into()],
        vec!["--socket".into(), "/run/other.sock".into()],
    ];
    for endpoint in endpoints {
        for (first, second) in [
            (vec![native.clone()], endpoint.clone()),
            (endpoint, vec![native.clone()]),
        ] {
            for position in [1, 2] {
                let mut args = first.clone();
                args.extend(["policy", "list"][..position].iter().map(OsString::from));
                args.extend(second.clone());
                args.extend(["policy", "list"][position..].iter().map(OsString::from));
                let error = Cli::parse_from(
                    std::iter::once(OsString::from("agent-sec-cli")).chain(args.clone()),
                )
                .unwrap_err();
                assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
                let output = common::run(&args);
                assert_eq!(output.status.code(), Some(2), "{args:?}");
                assert!(output.stdout.is_empty());
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("--socket may be specified only once")
                );
            }
        }
    }

    let cli = Cli::parse_from(vec![
        "agent-sec-cli".into(),
        native,
        "policy".into(),
        "list".into(),
    ])
    .unwrap();
    assert_eq!(
        cli.socket()
            .expect("policy list needs an endpoint")
            .as_os_str(),
        OsString::from_vec(b"/run/asc-\xff.sock".to_vec())
    );
}

#[test]
fn file_errors_duplicate_keys_and_oversized_inputs_are_local_failures() {
    let directory = common::Directory::new();
    let path = directory.0.join("template.json");
    for operation in ["create", "update"] {
        assert!(matches!(
            policy_file_cli(operation, &path, &["--language", "json"]).request(),
            Err(InputError::Read(_))
        ));
    }
    for bytes in [
        b"not-json".as_slice(),
        br#"{"specVersion":"0.1","rules":[],"rules":[]}"#,
        br#"{"specVersion":"0.1","specVersion":"0.1","rules":[]}"#,
        br#"{"specVersion":"0.1","rules":[{"effect":"block","category":"file","action":"write","target":{"type":"file","path":"/one","path":"/two"}}]}"#,
        br#"{"specVersion":"0.1","rules":[],"extra":true}"#,
        br#"{"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1},"text":"first","text":"second"}}"#,
        br#"{"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1,"revision":2},"text":"first"}}"#,
    ] {
        std::fs::write(&path, bytes).unwrap();
        for operation in ["create", "update"] {
            assert!(matches!(
                policy_file_cli(operation, &path, &["--language", "json"]).request(),
                Err(InputError::Json(_))
            ));
        }
    }
    std::fs::write(&path, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    for operation in ["create", "update"] {
        assert!(matches!(
            policy_file_cli(operation, &path, &["--language", "json"]).request(),
            Err(InputError::TooLarge)
        ));
    }
}

#[test]
fn policy_file_errors_retain_original_line_and_column() {
    let directory = common::Directory::new();
    let path = directory.0.join("template.json");
    for (source, message, line, column) in [
        ("{}", "missing field `specVersion`", 1, 2),
        ("\n  {\n}\n", "missing field `specVersion`", 3, 1),
        (
            "\n{\n\"language\":\"cedar\"\n}\n",
            "missing field `content`",
            4,
            1,
        ),
    ] {
        std::fs::write(&path, source).unwrap();
        for command in ["create", "update"] {
            let mut args = vec![
                "agent-sec-cli",
                "policy",
                command,
                "--name",
                "test",
                "--language",
                "json",
                "--file",
                path.to_str().unwrap(),
            ];
            if command == "update" {
                args.extend(["--policy-id", "policy-one"]);
            }
            let Err(InputError::Json(error)) = Cli::parse_from(args).unwrap().request() else {
                panic!("invalid policy file must fail JSON decoding");
            };
            assert_eq!((error.line(), error.column()), (line, column));
            assert_eq!(
                error.to_string(),
                format!("{message} at line {line} column {column}")
            );
        }
    }
}

#[test]
fn result_rendering_keeps_domains_and_errors_separate() {
    let response = DaemonResponse::success(
        RequestId::new("r1").unwrap(),
        json!({"status":{"phase":"PENDING_APPLY"}}),
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        render_policy(&response, &mut stdout, &mut stderr).unwrap(),
        0
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap(),
        json!({"status":{"phase":"PENDING_APPLY"}})
    );
    assert!(stderr.is_empty());
    stdout.clear();
    let response: DaemonResponse = DaemonResponse::error(
        RequestId::new("r2").unwrap(),
        "not_found",
        "Policy not found",
    );
    assert_eq!(
        render_policy(&response, &mut stdout, &mut stderr).unwrap(),
        1
    );
    assert!(stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&stderr).unwrap(),
        json!({"requestId":"r2","error":{"code":"not_found","message":"Policy not found"}})
    );
}

#[test]
fn scan_code_rendering_preserves_error_results_on_stdout() {
    let response = DaemonResponse::success(
        RequestId::new("scan-1").unwrap(),
        json!({
            "ok": false,
            "verdict": "error",
            "summary": "scan error: LLM model not available",
            "findings": [],
            "language": "bash",
            "engine_version": "0.12.0",
            "elapsed_ms": 1
        }),
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        render_scan_code(&response, &mut stdout, &mut stderr).unwrap(),
        1
    );
    assert!(stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["summary"],
        "scan error: LLM model not available"
    );
    assert_eq!(
        String::from_utf8(stdout.clone()).unwrap(),
        concat!(
            "{\n",
            "  \"ok\": false,\n",
            "  \"verdict\": \"error\",\n",
            "  \"summary\": \"scan error: LLM model not available\",\n",
            "  \"findings\": [],\n",
            "  \"language\": \"bash\",\n",
            "  \"engine_version\": \"0.12.0\",\n",
            "  \"elapsed_ms\": 1\n",
            "}\n"
        )
    );

    let response = DaemonResponse::error(
        RequestId::new("scan-2").unwrap(),
        "invalid_argument",
        "unsupported language: ruby",
    );
    stdout.clear();
    assert_eq!(
        render_scan_code(&response, &mut stdout, &mut stderr).unwrap(),
        1
    );
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"scan error: unsupported language: ruby\n");
}

#[test]
fn binary_help_version_and_failures_have_stable_exit_codes() {
    let mut help_cases = vec![vec!["--help"], vec!["--version"], vec!["-v"]];
    for resource in ["policy", "scope", "binding"] {
        help_cases.push(vec![resource, "--help"]);
        let operations: &[&str] = match resource {
            "policy" => &["create", "get", "list", "update", "delete"],
            "scope" => &["create", "get", "list", "delete", "retry"],
            _ => &["get", "list"],
        };
        for operation in operations {
            help_cases.push(vec![resource, operation, "--help"]);
        }
    }
    for args in help_cases {
        let output = common::run(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if args == ["--version"] || args == ["-v"] {
            assert_eq!(
                stdout,
                format!("agent-sec-cli {}\n", env!("CARGO_PKG_VERSION"))
            );
        } else {
            assert!(stdout.contains("Usage: agent-sec-cli"), "{stdout}");
        }
        assert!(output.stderr.is_empty());
    }
    let directory = common::Directory::new();
    let socket = directory.0.join("absent.sock");
    for (args, code, message) in [
        (
            vec!["--socket", socket.to_str().unwrap(), "policy", "list"],
            1,
            "connection unavailable",
        ),
        (
            vec![
                "--socket",
                socket.to_str().unwrap(),
                "scope",
                "create",
                "--pid",
                "0",
            ],
            2,
            "invalid value",
        ),
        (
            vec![
                "--socket",
                socket.to_str().unwrap(),
                "policy",
                "create",
                "--name",
                "x",
                "--file",
                "/no-such-policy-input",
            ],
            1,
            "cannot read Policy template",
        ),
    ] {
        let output = common::run(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message));
        if code == 1 {
            assert!(stderr.starts_with("agent-sec-cli: "), "{stderr}");
        }
    }
    assert!(!socket.exists());
}

#[test]
fn skillsec_and_legacy_alias_share_requests_and_deadlines() {
    let directory = common::Directory::new();
    let findings = directory.0.join("findings.json");
    std::fs::write(&findings, "[]").unwrap();
    let export = directory.0.join("export");
    let cases: &[&[&str]] = &[
        &["init", "--no-baseline"],
        &["check", "/skill with spaces"],
        &["analyze", "/skill"],
        &["scan", "--all", "--force"],
        &[
            "certify",
            "/skill",
            "--findings",
            findings.to_str().unwrap(),
        ],
        &["status", "--verbose"],
        &["audit", "/skill", "--verify-snapshots"],
        &["list-scanners"],
        &["decide", "/skill", "--action", "allow"],
        &["show", "/skill"],
        &["export", "/skill", "--output", export.to_str().unwrap()],
        &["activate", "/skill"],
    ];
    for arguments in cases {
        let parse = |name| {
            Cli::parse_from(
                ["agent-sec-cli", "--socket", "/run/asc.sock", name]
                    .into_iter()
                    .chain(arguments.iter().copied()),
            )
            .unwrap()
        };
        let current = parse("skillsec");
        let legacy = parse("skill-ledger");
        assert!(current.is_skill_sec());
        assert!(legacy.is_skill_sec());
        assert_eq!(current.timeout(), legacy.timeout());
        let request = serde_json::to_value(current.request().unwrap()).unwrap();
        assert_eq!(request["method"], "action.skill_sec");
        assert_eq!(
            request,
            serde_json::to_value(legacy.request().unwrap()).unwrap(),
            "{arguments:?}"
        );
    }
}

#[test]
fn skillsec_is_the_canonical_help_name_for_both_spellings() {
    let output = common::run(&["--help".into()]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("skillsec"), "{help}");
    assert!(!help.contains("skill-ledger"), "{help}");
    for name in ["skillsec", "skill-ledger"] {
        for subcommand in [None, Some("check")] {
            let mut args = vec![OsString::from(name)];
            args.extend(subcommand.map(OsString::from));
            args.push("--help".into());
            let output = common::run(&args);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("Usage: agent-sec-cli skillsec"), "{help}");
            assert!(!help.contains("skill-ledger"), "{help}");
        }
    }
}

#[test]
fn skill_sec_deadline_override_does_not_change_other_commands() {
    use std::time::Duration;

    let cli = |args: &[&str]| {
        Cli::parse_from(
            ["agent-sec-cli", "--socket", "/run/asc.sock"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .unwrap()
    };
    let skill = cli(&["skillsec", "status"]);
    assert_eq!(skill.timeout(), Duration::from_secs(60));
    assert_eq!(skill.request().unwrap().params["timeoutMs"], 60_000);
    let policy = cli(&["policy", "list"]);
    assert_eq!(policy.timeout(), Duration::from_secs(5));
    assert!(policy.request().unwrap().params.get("timeoutMs").is_none());
    let scan = cli(&["scan-code", "--code", "echo safe"]);
    assert_eq!(scan.timeout(), Duration::from_secs(5));
    let explicit = cli(&["--timeout-ms", "8000", "skillsec", "status"]);
    assert_eq!(explicit.timeout(), Duration::from_secs(8));
    assert_eq!(explicit.request().unwrap().params["timeoutMs"], 8000);
    let capped = cli(&["--timeout-ms", "180000", "skillsec", "status"]);
    assert_eq!(capped.timeout(), Duration::from_secs(180));
    assert_eq!(capped.request().unwrap().params["timeoutMs"], 120_000);
}

#[test]
fn skill_sec_uses_global_trace_context_without_business_fields() {
    let cli = Cli::parse_from([
        "agent-sec-cli",
        "--trace-context",
        r#"{"traceId":"skill-call","sessionId":"skill-session","agentName":"skill-agent"}"#,
        "--socket",
        "/run/asc.sock",
        "skillsec",
        "status",
    ])
    .unwrap();
    let request = cli.request().unwrap();
    assert_eq!(
        request.params,
        json!({"command":"status","verbose":false,"timeoutMs":60000})
    );
    assert!(request.trace_context.is_none());
    assert!(request.compatibility.is_none());
    // The common client injects the active Context when transmitting; business DTOs stay separate.
    let _attached = cli.context().attach();
    let snapshot = asc_observability::snapshot();
    assert_eq!(
        snapshot.compatibility.trace_id.as_deref(),
        Some("skill-call")
    );
    assert_eq!(
        snapshot.agent.get("session_id").map(String::as_str),
        Some("skill-session")
    );
    assert_eq!(
        snapshot.agent.get("agent_name").map(String::as_str),
        Some("skill-agent")
    );
}

#[test]
fn scope_create_excludes_unsupported_cgroup_selector() {
    for flag in ["-h", "--help"] {
        let output = common::run(&["scope".into(), "create".into(), flag.into()]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        for selector in ["--pid", "--process-name", "--executable", "--cmdline-arg"] {
            assert!(help.contains(selector), "{help}");
        }
        assert!(!help.contains("--cgroup-id"), "{help}");
    }

    for selector in [vec!["--cgroup-id", "1"], vec!["--cgroup-id=1"]] {
        let mut args = vec![
            "agent-sec-cli",
            "scope",
            "create",
            "--policy-id",
            "policy-one",
            "--policy-revision",
            "2",
        ];
        args.extend(selector);
        let error = Cli::parse_from(args.clone()).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        let output = common::run(&args[1..].iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--cgroup-id"));
    }
}

#[test]
fn scope_owner_uid_maps_to_rpc_and_rejects_root_or_pid_credential_selection() {
    let base = [
        "agent-sec-cli",
        "scope",
        "create",
        "--policy-id",
        "credential-policy",
        "--policy-revision",
        "1",
    ];
    for (flag, value) in [
        ("--process-name", "agent"),
        ("--executable", "/bin/agent"),
        ("--cmdline-arg", "/usr/local/bin/agent"),
    ] {
        let args = base.into_iter().chain([flag, value, "--owner-uid", "1000"]);
        let request = Cli::parse_from(args).unwrap().request().unwrap();
        assert_eq!(request.params["ownerUid"], 1000);
        assert_eq!(request.method, "policy.scopes.create");
    }
    for extra in [
        vec!["--process-name", "agent", "--owner-uid", "0"],
        vec!["--process-name", "agent", "--owner-uid", "-1"],
        vec!["--process-name", "agent", "--owner-uid", "4294967296"],
        vec!["--pid", "42", "--owner-uid", "1000"],
    ] {
        assert!(Cli::parse_from(base.into_iter().chain(extra)).is_err());
    }
}

#[test]
fn scope_create_encodes_name_and_path_assignments() {
    for (flag, value, matcher) in [
        (
            "--process-name",
            "scope-agent",
            json!({"processName":"scope-agent"}),
        ),
        (
            "--executable",
            "/opt/agent",
            json!({"executable":"/opt/agent"}),
        ),
        (
            "--cmdline-arg",
            "/usr/local/bin/agent",
            json!({"cmdlineArg":"/usr/local/bin/agent"}),
        ),
    ] {
        let request = Cli::parse_from([
            "agent-sec-cli",
            "scope",
            "create",
            flag,
            value,
            "--policy-id",
            "policy-one",
            "--policy-revision",
            "2",
        ])
        .unwrap()
        .request()
        .unwrap();
        assert_eq!(request.method, "policy.scopes.create");
        assert_eq!(
            request.params,
            json!({"selector":{"kind":"process","match":matcher}, "policyTemplates":[{"policyId":"policy-one","policyRevision":2}]})
        );
    }
    assert!(
        Cli::parse_from([
            "agent-sec-cli",
            "scope",
            "create",
            "--process-name",
            "agent",
            "--executable",
            "/bin/agent"
        ])
        .is_err()
    );
    assert!(
        Cli::parse_from([
            "agent-sec-cli",
            "scope",
            "create",
            "--executable",
            "/usr/bin/node",
            "--cmdline-arg",
            "/usr/local/bin/agent"
        ])
        .is_err()
    );
}

#[test]
fn gateway_transport_failure_never_falls_back_to_local_execution() {
    let directory = common::Directory::new();
    let socket = directory.0.join("absent.sock");
    for command in ["status", "ca"] {
        let args = vec![
            OsString::from("--socket"),
            socket.as_os_str().into(),
            "gateway".into(),
            command.into(),
        ];
        let cli =
            Cli::parse_from(std::iter::once(OsString::from("agent-sec-cli")).chain(args.clone()))
                .unwrap();
        assert!(matches!(cli.request(), Err(InputError::Gateway(_))));
        let output = common::run(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.starts_with("agent-sec-cli: "), "{error}");
        assert!(error.contains("connection unavailable"), "{error}");
    }
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_cli_preserves_minimal_results_and_conflicts() {
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    if !cfg!(target_os = "linux")
        || (rustix::process::geteuid().as_raw() != 0
            && std::env::var_os("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK").as_deref()
                != Some(std::ffi::OsStr::new("1")))
    {
        return;
    }
    let directory = common::Directory::new();
    let socket = directory.0.join("gateway.sock");
    let cases: &[(&[&str], &str, Value, Value)] = &[
        (&["enable"], "gateway.enable", json!({}), json!({})),
        (&["disable"], "gateway.disable", json!({}), json!({})),
        (
            &["status"],
            "gateway.status",
            json!({}),
            json!({"enabled":true,"ready":true,"scopes":[{"scopeId":"scope-1","bindings":["binding-1"]}]}),
        ),
        (
            &["key", "list"],
            "gateway.key.list",
            json!({}),
            json!({"pairids":["pair-1"]}),
        ),
        (
            &["key", "remove", "pair-1"],
            "gateway.key.remove",
            json!({"pairid":"pair-1"}),
            json!({"pairid":"pair-1"}),
        ),
        (
            &["ca"],
            "gateway.ca",
            json!({}),
            json!({"caBundlePath":"/run/agent-sec-gateway/ca-bundle.pem"}),
        ),
        (
            &["key", "remove", "pair-1"],
            "gateway.key.remove",
            json!({"pairid":"pair-1"}),
            Value::Null,
        ),
    ];
    for (arguments, method, params, result) in cases {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let mut args: Vec<OsString> = vec![
            "--socket".into(),
            socket.as_os_str().into(),
            "gateway".into(),
        ];
        args.extend(arguments.iter().map(OsString::from));
        let cli = tokio::task::spawn_blocking(move || common::run(&args));
        let response = if result.is_null() {
            DaemonResponse::error(
                RequestId::new("gateway-test").unwrap(),
                "conflict",
                "credential pair retirement has a publication conflict",
            )
        } else {
            DaemonResponse::success(RequestId::new("gateway-test").unwrap(), result.clone())
        };
        let response = serde_json::to_value(response).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], *method);
            assert_eq!(request["params"], *params);
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            stream.get_mut().write_all(&bytes).await.unwrap();
        })
        .await
        .unwrap();
        let output = cli.await.unwrap();
        assert_gateway_output(&output, result, &response);
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
    }
}

fn assert_gateway_output(output: &std::process::Output, result: &Value, response: &Value) {
    if result.is_null() {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap(),
            *response
        );
    } else {
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            *result
        );
    }
}

fn policy_file_cli(operation: &str, file: &std::path::Path, options: &[&str]) -> Cli {
    let mut params = json!({"policyName":"sandbox"});
    if operation == "update" {
        params["policyId"] = json!("sandbox-baseline");
    }
    let row = json!({"method":format!("policy.templates.{operation}"), "params":params});
    let mut args = vec![OsString::from("agent-sec-cli")];
    args.extend(common::args_for(
        &row,
        file.parent().unwrap(),
        &file.parent().unwrap().join("absent.sock"),
    ));
    args.extend([OsString::from("--file"), file.as_os_str().into()]);
    args.extend(options.iter().map(OsString::from));
    Cli::parse_from(args).unwrap()
}

#[test]
fn policy_commands_accept_raw_and_tagged_cedar_sources() {
    let directory = common::Directory::new();
    let file = directory.0.join("cedar-policy.json");
    let raw = directory.0.join("policy.cedar");
    for (schema_id, revision) in [
        ("seccore.system", 1),
        ("seccore.system", 2),
        ("custom.schema", 2),
    ] {
        let source = json!({"language":"cedar", "content":{
            "schema":{"schemaId":schema_id, "revision":revision},
            "text":"// Preserve this comment.\r\npermit(principal, action, resource);\n"
        }});
        std::fs::write(&file, serde_json::to_vec(&source).unwrap()).unwrap();
        std::fs::write(&raw, source["content"]["text"].as_str().unwrap()).unwrap();
        let revision = revision.to_string();
        for operation in ["create", "update"] {
            let request = policy_file_cli(operation, &file, &["--language", "json"])
                .request()
                .unwrap();
            let mut params = json!({"policyName":"sandbox", "template":source});
            if operation == "update" {
                params["policyId"] = json!("sandbox-baseline");
            }
            assert_eq!(request.params, params);
            assert_eq!(request.method, format!("policy.templates.{operation}"));
            let raw_request = policy_file_cli(
                operation,
                &raw,
                &[
                    "--language",
                    "cedar",
                    "--schema-id",
                    schema_id,
                    "--schema-revision",
                    &revision,
                ],
            )
            .request()
            .unwrap();
            assert_eq!(
                raw_request, request,
                "{operation} raw/envelope equivalent wire"
            );
        }
        let mut with_trailing_data = serde_json::to_vec(&source).unwrap();
        with_trailing_data.extend_from_slice(b" {}");
        std::fs::write(&file, with_trailing_data).unwrap();
        for operation in ["create", "update"] {
            assert!(matches!(
                policy_file_cli(operation, &file, &["--language", "json"]).request(),
                Err(InputError::Json(_))
            ));
        }
    }
}

#[test]
fn raw_cedar_create_and_update_preserve_text_and_schema_selection() {
    let directory = common::Directory::new();
    let file = directory.0.join("policy=a b.cedar");
    let text = "  // Preserve café and line endings.\r\npermit(principal, action, resource);\r\n\t";
    std::fs::write(&file, text).unwrap();
    let cases: &[(&[&str], &str, u32)] = &[
        (&["--language", "cedar"], "seccore.system", 1),
        (
            &["--language", "cedar", "--schema-revision", "2"],
            "seccore.system",
            2,
        ),
        (
            &["--language", "cedar", "--schema-id", "custom.schema"],
            "custom.schema",
            1,
        ),
        (
            &[
                "--language",
                "cedar",
                "--schema-id",
                "custom.schema",
                "--schema-revision",
                "2",
            ],
            "custom.schema",
            2,
        ),
    ];
    for (options, schema_id, revision) in cases {
        for operation in ["create", "update"] {
            let request = policy_file_cli(operation, &file, options)
                .request()
                .unwrap();
            let mut params = json!({"policyName":"sandbox", "template":{
                "language":"cedar", "content":{
                    "schema":{"schemaId":schema_id, "revision":revision}, "text":text
                }
            }});
            if operation == "update" {
                params["policyId"] = json!("sandbox-baseline");
            }
            assert_eq!(request.params, params, "{operation} {options:?}");
            assert_eq!(request.method, format!("policy.templates.{operation}"));
        }
    }
}

#[test]
fn raw_cedar_invalid_utf8_and_oversized_files_fail_locally() {
    let directory = common::Directory::new();
    let file = directory.0.join("policy.cedar");
    std::fs::write(
        &file,
        b"// invalid byte: \xff\npermit(principal, action, resource);",
    )
    .unwrap();
    for operation in ["create", "update"] {
        assert!(matches!(
            policy_file_cli(operation, &file, &["--language", "cedar"]).request(),
            Err(InputError::PolicyUtf8)
        ));
    }
    std::fs::write(&file, vec![b' '; asc_daemon_client::MAX_FRAME_BYTES + 1]).unwrap();
    for operation in ["create", "update"] {
        assert!(matches!(
            policy_file_cli(operation, &file, &["--language", "cedar"]).request(),
            Err(InputError::TooLarge)
        ));
    }
}

#[test]
fn json_policy_files_reject_raw_schema_options() {
    let directory = common::Directory::new();
    let file = directory.0.join("policy.json");
    let options: &[&[&str]] = &[
        &["--language", "json", "--schema-id", "seccore.system"],
        &["--language", "json", "--schema-revision", "2"],
        &[
            "--language",
            "json",
            "--schema-id",
            "custom.schema",
            "--schema-revision",
            "2",
        ],
    ];
    for source in [
        json!({"specVersion":"0.1", "rules":[]}),
        json!({"language":"cedar", "content":{
            "schema":{"schemaId":"seccore.system", "revision":1},
            "text":"permit(principal, action, resource);"
        }}),
    ] {
        std::fs::write(&file, serde_json::to_vec(&source).unwrap()).unwrap();
        for operation in ["create", "update"] {
            assert_eq!(
                policy_file_cli(operation, &file, &["--language", "json"])
                    .request()
                    .unwrap()
                    .params["template"],
                source
            );
            for options in options {
                assert!(
                    matches!(
                        policy_file_cli(operation, &file, options).request(),
                        Err(InputError::SchemaWithJson)
                    ),
                    "{operation} {options:?}"
                );
            }
        }
    }
}
