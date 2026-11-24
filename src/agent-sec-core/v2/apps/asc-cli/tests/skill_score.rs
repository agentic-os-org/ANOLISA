//! Native local assessment validates inputs without invoking the retired Python CLI.

mod common;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use asc_cli::{Cli, InputError, Plan};
use serde_json::{Value, json};

fn installation() -> common::Directory {
    let binary = Path::new(env!("CARGO_BIN_EXE_agent-sec-cli"));
    let directory = common::Directory(
        binary
            .parent()
            .unwrap()
            .join(format!("score-install-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(directory.0.join("bin")).unwrap();
    // A parallel child must not inherit a writable copy descriptor for another test's executable.
    std::fs::hard_link(binary, directory.0.join("bin/agent-sec-cli")).unwrap();
    directory
}

fn command(prefix: &Path) -> Command {
    let mut command = Command::new(prefix.join("bin/agent-sec-cli"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("AGENT_SEC_DAEMON_SOCKET", "relative-invalid-socket")
        .env(
            "AGENT_SEC_MODEL_CONFIG",
            prefix.join("no-model-config.json"),
        )
        .args(["skillsec", "score"]);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

#[test]
fn score_is_explicitly_local_and_never_constructs_a_daemon_request() {
    let cli = Cli::parse_from(["cli", "skillsec", "score", "plan", "skill"]).unwrap();
    assert!(matches!(cli.plan(), Plan::SkillScore(_)));
    assert!(cli.socket().is_none());
    assert!(!cli.is_skill_sec());
    assert!(matches!(cli.request(), Err(InputError::LocalCommand)));
}

#[test]
fn old_and_new_skill_names_keep_the_same_daemon_contract() {
    let parse =
        |name| Cli::parse_from(["cli", "--socket", "/run/fixture.sock", name, "status"]).unwrap();
    let old = parse("skill-ledger");
    let new = parse("skillsec");
    assert!(matches!(new.plan(), Plan::Daemon { .. }));
    assert!(new.is_skill_sec());
    assert_eq!(
        serde_json::to_value(old.request().unwrap()).unwrap(),
        serde_json::to_value(new.request().unwrap()).unwrap()
    );
    assert_eq!(old.timeout(), new.timeout());
}

#[test]
fn score_rejects_daemon_and_context_options_instead_of_ignoring_them() {
    for (option, value) in [
        ("--socket", "/run/fixture.sock"),
        ("--timeout-ms", "9000"),
        ("--otel-context", "{}"),
        ("--trace-context", "{}"),
    ] {
        let error =
            Cli::parse_from(["cli", option, value, "skillsec", "score", "run", "-"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert!(error.to_string().contains(option), "{error}");
    }
    let error = Cli::parse_from([
        "cli",
        "skillsec",
        "score",
        "plan",
        "skill",
        "--timeout-ms=9000",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn public_help_and_usage_errors_do_not_need_a_payload() {
    let prefix = installation();
    for arguments in [
        vec!["--help"],
        vec!["plan", "--help"],
        vec!["run", "--help"],
    ] {
        let output = command(&prefix.0).args(arguments).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("skillsec score"));
    }
    let output = Command::new(prefix.0.join("bin/agent-sec-cli"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("skillsec"), "{help}");
    assert!(!help.contains("skill-ledger"), "{help}");
    for arguments in [
        vec![],
        vec!["plan"],
        vec!["run"],
        vec!["unknown"],
        vec!["run", "-", "--unknown"],
        vec!["plan", "skill", "--evidence-dir", "evidence"],
    ] {
        let output = command(&prefix.0).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("scoring payload"));
    }
}

#[test]
fn malformed_task_rejects_inline_file_and_stdin_before_runtime_or_model() {
    let prefix = installation();
    let input = r#"{"schema_version":"agent-sec-skill-score/task-v1","skill_path":"ignored","content_hash":"invalid","audit":{}}"#;
    let path = prefix.0.join("input.json");
    std::fs::write(&path, input).unwrap();
    for argument in [input, "-", path.to_str().unwrap()] {
        let mut child = command(&prefix.0)
            .args(["run", argument])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if argument == "-" {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("content_hash"));
    }
}

fn task_for(skill: &Path) -> Value {
    json!({
        "schema_version":"agent-sec-skill-score/task-v1", "skill_path":skill,
        "content_hash":"0".repeat(64),
        "audit":{"objective":"Do not echo private task contents", "allowed_boundary":"Read SKILL.md",
        "findings":[],"probes":[{"id":"baseline","template":"baseline","entry_path":"SKILL.md",
        "turns":["Describe this Skill."],"activation":"Read its instructions","expected_result":"A description"}]}
    })
}

#[test]
fn missing_task_reports_stage_and_escaped_path_without_a_report() {
    let prefix = installation();
    let missing = prefix.0.join("missing task\nforged-log.json");
    let report = prefix.0.join("report.json");
    for path in [missing.clone(), prefix.0.join("dangling-task.json")] {
        if path != missing {
            std::os::unix::fs::symlink(&missing, &path).unwrap();
        }
        let result = command(&prefix.0)
            .arg("run")
            .arg(&path)
            .arg("--output-file")
            .arg(&report)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&result.stderr);
        assert_eq!(result.status.code(), Some(1), "{error}");
        assert!(error.contains("stage=task.resolve"), "{error}");
        assert!(error.contains(&format!("path={path:?}")), "{error}");
        assert!(error.contains("os error 2"), "{error}");
        assert_eq!(error.lines().count(), 2, "{error}");
        let event = error
            .lines()
            .last()
            .unwrap()
            .strip_prefix("[skill-score] Command failed: diagnostic=")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(event).unwrap(),
            json!({"stage":"task.resolve","reason":"io","errno":2})
        );

        assert!(result.stdout.is_empty() && !report.exists());
    }
}

#[test]
fn missing_skill_is_resolved_relative_to_task_without_echoing_its_contents() {
    let prefix = installation();
    let task_path = prefix.0.join("task.json");
    std::fs::write(&task_path, task_for(Path::new("missing skill")).to_string()).unwrap();
    let report = prefix.0.join("report.json");
    let result = command(&prefix.0)
        .arg("run")
        .arg(&task_path)
        .arg("--output-file")
        .arg(&report)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{error}");
    assert!(error.contains("stage=task.skill.resolve"), "{error}");
    assert!(
        error.contains(&format!("path={:?}", prefix.0.join("missing skill"))),
        "{error}"
    );
    assert!(error.contains("os error 2"), "{error}");
    assert!(
        !error.contains("Do not echo private task contents"),
        "{error}"
    );
    assert!(result.stdout.is_empty() && !report.exists());
}

#[test]
fn missing_temporary_directory_identifies_evidence_preparation() {
    let prefix = installation();
    let skill = prefix.0.join("skill");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
    let missing = prefix.0.join("missing temporary directory");
    let report = prefix.0.join("report.json");
    let result = command(&prefix.0)
        .env("TMPDIR", &missing)
        .arg("run")
        .arg(task_for(&skill).to_string())
        .arg("--output-file")
        .arg(&report)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{error}");
    assert!(error.contains("stage=evidence.temp.create"), "{error}");
    assert!(error.contains(&format!("path={missing:?}")), "{error}");
    assert!(error.contains("os error 2"), "{error}");
    assert!(result.stdout.is_empty() && !report.exists());
}

#[test]
fn missing_paths_redact_the_configured_model_credential() {
    let prefix = installation();
    let config = prefix.0.join("model.json");
    std::fs::write(
        &config,
        json!({"schemaVersion":1,"remote":{
        "provider":"bailian-payg-anthropic","baseUrl":"https://dashscope.aliyuncs.com/apps/anthropic",
        "defaultModel":"fixture","apiKeyEnv":"TEST_KEY"}})
        .to_string(),
    )
    .unwrap();
    for key in ["fixture-opaque-key", r#"fixture"opaque\key"#] {
        let missing = prefix.0.join(key);
        for (argument, stage) in [
            (missing.to_string_lossy().into_owned(), "task.resolve"),
            (task_for(&missing).to_string(), "task.skill.resolve"),
        ] {
            let result = command(&prefix.0)
                .env("AGENT_SEC_MODEL_CONFIG", &config)
                .env("TEST_KEY", key)
                .arg("run")
                .arg(argument)
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&result.stderr);
            assert_eq!(result.status.code(), Some(1), "{error}");
            assert!(
                error.contains(&format!("stage={stage}")) && error.contains("path="),
                "{error}"
            );
            assert!(!error.contains(key), "{error}");
            let escaped = format!("{key:?}");
            assert!(!error.contains(&escaped[1..escaped.len() - 1]), "{error}");
            assert!(error.contains("[REDACTED]"), "{error}");
            assert!(error.contains("os error 2"), "{error}");
            assert!(result.stdout.is_empty());
        }
    }
}

#[test]
fn native_plan_does_not_invoke_retired_payload_and_rejects_output_in_skill() {
    use std::os::unix::fs::PermissionsExt as _;
    let prefix = installation();
    let skill = prefix.0.join("skill with spaces");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
    let retired = prefix
        .0
        .join("lib/anolisa/sec-core/skill-score/cli/bin/agent-sec-cli");
    std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
    std::fs::write(
        &retired,
        "#!/bin/sh
exit 77
",
    )
    .unwrap();
    std::fs::set_permissions(retired, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = command(&prefix.0)
        .arg("plan")
        .arg(&skill)
        .arg("--output-file")
        .arg(skill.join("task.json"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("outside"));
    let result = command(&prefix.0).arg("plan").arg(&skill).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("model"));
}

#[derive(Clone, Copy)]
enum PlanReply {
    Valid,
    GatewayLimit(u32),
    EmptyProbes,
    NonObject(&'static str),
    Truncated,
    Chunked(&'static [u8]),
    MissingText,
    MissingUsage,
}

fn respond_to_plan(
    mut stream: std::net::TcpStream,
    variant: PlanReply,
    scratch: Option<&Path>,
) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(&stream);
    let mut length = 0;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header).unwrap() == 0 {
            return false;
        }
        if header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    assert!((1..=2_000_000).contains(&length));
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    if let PlanReply::GatewayLimit(limit) = variant {
        assert_eq!(request["model"], "deepseek-v4.1-flash-1017");
        if request["max_tokens"].as_u64().unwrap() > u64::from(limit) {
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            return true;
        }
        assert_eq!(request["max_tokens"], limit);
    }
    let prompt = request["messages"][0]["content"].as_str().unwrap();
    let payload = prompt.split("\nRequired JSON schema:").next().unwrap();
    let payload: Value = serde_json::from_str(payload).unwrap();
    let reviewed = |field: &str| -> serde_json::Map<String, Value> {
        payload[field].as_object().unwrap().keys().map(|id| {
            (id.clone(), json!({"status":"not_applicable", "reason":"This example only describes input."}))
        }).collect()
    };
    if let Some(scratch) = scratch {
        // A valid plan only needs its output directory after scanning.
        std::fs::rename(scratch, scratch.with_extension("used")).unwrap();
        std::fs::write(scratch, "temporary storage is unavailable").unwrap();
    }
    let probes = if matches!(variant, PlanReply::EmptyProbes) {
        json!([])
    } else {
        json!([{"id":"baseline", "template":"baseline", "finding_ids":[], "input_path":"/app/work/folder/input.txt",
            "fixtures":{"folder/input.txt":"hello"}, "entry_path":"SKILL.md","turns":["Describe this Skill."],
            "activation":"Read the instructions","expected_result":"A description"}])
    };
    let audit = if payload.get("failed_probes").is_some() {
        json!({"probes":probes})
    } else {
        json!({"objective":"Describe the supplied Skill", "allowed_boundary":"Read SKILL.md",
            "findings":[], "pattern_checks":reviewed("risk_catalog"),
            "hygiene_review":reviewed("hygiene_checks"), "probes":probes})
    };
    let mut envelope = json!({"content":[{"type":"text","text":audit.to_string()}],
        "usage":{"input_tokens":1,"output_tokens":1},"stop_reason":"end_turn"});
    match variant {
        PlanReply::Chunked(wire) => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.write_all(wire).unwrap();
            return true;
        }
        PlanReply::NonObject(text) => envelope["content"][0]["text"] = text.into(),
        PlanReply::MissingText => {
            envelope["content"] = json!([{"type":"thinking","thinking":"private-fixture-text"}]);
        }
        PlanReply::MissingUsage => {
            envelope.as_object_mut().unwrap().remove("usage");
        }
        PlanReply::Valid
        | PlanReply::GatewayLimit(_)
        | PlanReply::EmptyProbes
        | PlanReply::Truncated => {}
    }
    let mut reply = serde_json::to_vec(&envelope).unwrap();
    let declared = reply.len();
    if matches!(variant, PlanReply::Truncated) {
        reply.truncate(declared / 2);
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.write_all(&reply).unwrap();
    true
}

fn run_fixture_plan(
    prefix: &Path,
    skill: &Path,
    output: &Path,
    replies: &'static [PlanReply],
    scratch: Option<&Path>,
    judge_cap: Option<u32>,
) -> (std::process::Output, usize) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut server_scratch = scratch.map(Path::to_owned);
    let server = std::thread::spawn(move || {
        let mut count = 0;
        for stream in listener.incoming() {
            let reply = replies[count.min(replies.len() - 1)];
            if !respond_to_plan(stream.unwrap(), reply, server_scratch.take().as_deref()) {
                break;
            }
            count += 1;
        }
        count
    });
    let config = prefix.join("model.json");
    let mut model = json!({"schemaVersion":1,"remote":{
        "provider":"anthropic-compatible","baseUrl":format!("http://{address}"),
        "defaultModel":"deepseek-v4.1-flash-1017","apiKeyEnv":"FIXTURE_MODEL_KEY"}});
    if let Some(cap) = judge_cap {
        model["remote"]["judgeMaxTokens"] = cap.into();
    }
    std::fs::write(&config, model.to_string()).unwrap();
    let mut plan = command(prefix);
    if let Some(scratch) = scratch {
        plan.env("TMPDIR", scratch);
    }
    let plan = plan
        .env("AGENT_SEC_MODEL_CONFIG", config)
        .env("FIXTURE_MODEL_KEY", "fixture-credential")
        .arg("plan")
        .arg(skill)
        .arg("--output-file")
        .arg(output)
        .output()
        .unwrap();
    // An empty connection wakes the listener even if the CLI made no requests.
    drop(std::net::TcpStream::connect(address).unwrap());
    (plan, server.join().unwrap())
}

#[test]
fn plan_honors_a_compatible_gateways_judge_output_cap() {
    const LIMITED: &[PlanReply] = &[PlanReply::GatewayLimit(24_000)];
    const FULL: &[PlanReply] = &[PlanReply::GatewayLimit(393_216)];
    for (cap, gateway, success, calls) in [
        (None, LIMITED, false, 1),
        (Some(24_000), LIMITED, true, 1),
        (Some(500_000), FULL, true, 1),
        (Some(0), LIMITED, false, 0),
    ] {
        let prefix = installation();
        let skill = prefix.0.join("skill");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
        let task = prefix.0.join("task.json");
        let (plan, count) = run_fixture_plan(&prefix.0, &skill, &task, gateway, None, cap);
        let stderr = String::from_utf8_lossy(&plan.stderr);
        assert_eq!(plan.status.success(), success, "cap={cap:?}: {stderr}");
        assert_eq!(count, calls, "cap={cap:?}: {stderr}");
        assert_eq!(task.exists(), success);
        if cap.is_none() {
            assert!(stderr.contains("request_rejected"), "{stderr}");
        }
    }
}

#[test]
fn plan_distinguishes_unusable_responses_and_recovers_an_interrupted_body_once() {
    use PlanReply::{Chunked, MissingText, MissingUsage, Truncated, Valid};

    const MISSING_TERMINATOR: PlanReply = Chunked(b"2\r\n{}\r\n");
    const PARTIAL_CHUNK: PlanReply = Chunked(b"2\r\n{");
    const COMPLETE_BAD_JSON: PlanReply = Chunked(b"1\r\n{\r\n0\r\n\r\n");

    for (replies, expected, calls, success) in [
        (&[Truncated, Valid][..], "body_truncated", 2, true),
        (&[Truncated][..], "body_truncated", 2, false),
        (&[MISSING_TERMINATOR, Valid][..], "body_truncated", 2, true),
        (&[PARTIAL_CHUNK, Valid][..], "body_truncated", 2, true),
        (
            &[MISSING_TERMINATOR, PARTIAL_CHUNK][..],
            "body_truncated",
            2,
            false,
        ),
        (&[COMPLETE_BAD_JSON][..], "invalid_json", 1, false),
        (&[MissingText][..], "missing_text", 1, false),
        (&[MissingUsage][..], "missing_usage", 1, true),
    ] {
        let prefix = installation();
        let skill = prefix.0.join("skill");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "# Example\nDescribe the supplied input.",
        )
        .unwrap();
        let task = prefix.0.join("task.json");
        let (plan, count) = run_fixture_plan(&prefix.0, &skill, &task, replies, None, None);
        let stderr = String::from_utf8(plan.stderr).unwrap();
        assert_eq!(count, calls, "{stderr}");
        assert_eq!(plan.status.success(), success, "{stderr}");
        assert_eq!(task.is_file(), success, "{stderr}");
        assert!(!stderr.contains("private-fixture-text") && !stderr.contains("fixture-credential"));
        let diagnostics: Vec<Value> = stderr
            .lines()
            .filter(|line| {
                line.starts_with("[skill-score] Judge call failed: code=invalid_response")
            })
            .map(|line| serde_json::from_str(line.split_once(" diagnostic=").unwrap().1).unwrap())
            .collect();
        assert_eq!(diagnostics.len(), calls - usize::from(success), "{stderr}");
        if expected == "missing_usage" {
            let warnings: Vec<Value> = stderr
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("[skill-score] Judge usage warning: diagnostic=")
                })
                .map(|fields| serde_json::from_str(fields).unwrap())
                .collect();
            assert_eq!(
                warnings,
                [json!({"stage":"audit", "reason":"missing_usage"})]
            );
        }
        for diagnostic in diagnostics {
            assert_eq!(diagnostic["stage"], "audit", "{stderr}");
            assert_eq!(diagnostic["reason"], expected, "{stderr}");
            assert_eq!(diagnostic["http_status"], 200, "{stderr}");
            let received = diagnostic["received_bytes"].as_u64().unwrap();
            if matches!(replies[0], Chunked(_)) {
                assert!(diagnostic.get("declared_bytes").is_none(), "{stderr}");
            } else {
                let declared = diagnostic["declared_bytes"].as_u64().unwrap();
                assert_eq!(
                    received < declared,
                    expected == "body_truncated",
                    "{stderr}"
                );
            }
        }
    }
}

#[test]
fn plan_reports_unresolved_repairs_and_preserves_partial_plans() {
    use PlanReply::{EmptyProbes, Valid};
    for (replies, failures, recovered) in [
        (&[EmptyProbes, EmptyProbes, Valid][..], 1, true),
        (&[EmptyProbes; 4][..], 3, false),
    ] {
        let prefix = installation();
        let skill = prefix.0.join("skill");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
        let task = prefix.0.join("task.json");
        let (plan, calls) = run_fixture_plan(&prefix.0, &skill, &task, replies, None, None);
        let stderr = String::from_utf8(plan.stderr).unwrap();
        assert!(plan.status.success(), "{stderr}");
        assert_eq!(calls, replies.len());
        let events: Vec<Value> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("[skill-score] Judge output invalid: diagnostic="))
            .map(|fields| serde_json::from_str(fields).unwrap())
            .collect();
        let expected: Vec<_> = (1..=failures)
            .map(|n| json!({"stage":"probe_repair","reason":"output_contract","attempt":n}))
            .collect();
        assert_eq!(events, expected);
        let task: Value = serde_json::from_slice(&std::fs::read(task).unwrap()).unwrap();
        let audit = &task["audit"];
        assert_eq!(
            audit["probes"].as_array().unwrap().len(),
            usize::from(recovered)
        );
        assert_eq!(audit["planning_errors"]["baseline"].is_null(), recovered);
    }
}

#[test]
fn plan_retries_non_object_judge_outputs_and_reports_each_attempt() {
    use PlanReply::{NonObject, Valid};
    for (replies, failures, exit) in [
        (&[NonObject("[]"), Valid][..], 1, 0),
        (
            &[
                NonObject("[]"),
                NonObject("[]"),
                NonObject("[]"),
                NonObject("[]"),
                NonObject("[]"),
                Valid,
            ][..],
            5,
            0,
        ),
        (
            &[
                NonObject("false"),
                NonObject("null"),
                NonObject(r#""private-fixture-text""#),
                NonObject("[]"),
                NonObject("[]"),
                NonObject("[]"),
            ][..],
            6,
            1,
        ),
    ] {
        let prefix = installation();
        let skill = prefix.0.join("skill");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
        let task = prefix.0.join("task.json");
        let (plan, count) = run_fixture_plan(&prefix.0, &skill, &task, replies, None, None);
        let stderr = String::from_utf8(plan.stderr).unwrap();
        assert_eq!(count, replies.len(), "{stderr}");
        assert_eq!(plan.status.code(), Some(exit), "{stderr}");
        assert_eq!(task.is_file(), exit == 0);
        assert!(!stderr.contains("private-fixture-text"));
        let diagnostics: Vec<Value> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("[skill-score] Judge output invalid: diagnostic="))
            .map(|payload| serde_json::from_str(payload).unwrap())
            .collect();
        let expected: Vec<_> = (1..=failures)
            .map(|attempt| json!({"stage":"audit","reason":"output_contract","attempt":attempt}))
            .collect();
        assert_eq!(diagnostics, expected, "{stderr}");
    }
}

#[test]
fn installed_cli_writes_a_bound_plan_and_reports_the_missing_runtime() {
    let prefix = installation();
    let skill = prefix.0.join("skill");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "# Example\nDescribe the supplied input.",
    )
    .unwrap();
    let output = prefix.0.join("reports/task.json");
    let scratch = prefix.0.join("planning-tmp");
    std::fs::create_dir(&scratch).unwrap();
    let (plan, calls) = run_fixture_plan(
        &prefix.0,
        &skill,
        &output,
        &[PlanReply::Valid],
        Some(&scratch),
        None,
    );
    assert!(plan.status.success(), "{plan:?}");
    assert_eq!(calls, 1);
    assert_eq!(plan.stdout, std::fs::read(&output).unwrap());
    let task: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(
        task["skill_path"].as_str(),
        skill.canonicalize().unwrap().to_str()
    );
    assert_eq!(task["audit"]["probes"][0]["id"], "baseline");
    if let Some(python) = std::env::var_os("ASC_TEST_WORKER_PYTHON") {
        let validation = Command::new(python)
            .arg("-c")
            .arg("import json, sys; from agent_sec_cli.skill_score.contracts import Probe; probe = Probe.model_validate(json.load(sys.stdin)['audit']['probes'][0]); assert probe.fixtures['folder/input.txt'] == 'hello'")
            .stdin(std::fs::File::open(&output).unwrap())
            .output().unwrap();
        assert!(validation.status.success(), "{validation:?}");
    }
    let evidence = prefix.0.join("evidence");
    let run = command(&prefix.0)
        .arg("run")
        .arg(output)
        .arg("--evidence-dir")
        .arg(&evidence)
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1), "{run:?}");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("install-runtime"),
        "{run:?}"
    );
    assert!(evidence.is_dir());
    assert!(run.stdout.is_empty());
}

fn assert_runtime_failure(prefix: &Path, task: &Path, expected: &Value, code: i32) {
    let run = command(prefix).arg("run").arg(task).output().unwrap();
    let stderr = String::from_utf8(run.stderr).unwrap();
    assert_eq!(run.status.code(), Some(code), "{stderr}");
    let payload = stderr
        .lines()
        .last()
        .unwrap()
        .strip_prefix("[skill-score] Command failed: diagnostic=")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(payload).unwrap(),
        *expected,
        "{stderr}"
    );
}

fn assert_active_manifest_failures(prefix: &Path, task: &Path, path: &Path, recipe: &Value) {
    let mut incompatible = recipe.clone();
    incompatible["protocol_version"] = json!("unsupported");
    for (content, stage, reason, code) in [
        (None, "runtime.active-manifest.read", "io", 1),
        (
            Some("{".to_owned()),
            "runtime.active-manifest.parse",
            "invalid_json",
            2,
        ),
        (
            Some(incompatible.to_string()),
            "runtime.active-manifest.verify",
            "runtime_error",
            1,
        ),
    ] {
        let mut expected = json!({"stage":stage,"reason":reason,"file":"active-manifest.json"});
        if let Some(content) = content {
            std::fs::write(path, content).unwrap();
        } else {
            expected["errno"] = json!(2);
        }
        assert_runtime_failure(prefix, task, &expected, code);
    }
}

#[test]
fn run_identifies_invalid_runtime_artifacts_and_preserves_exit_status() {
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::PermissionsExt as _;
    let dir = installation();
    let prefix = dir.0.join("opt/anolisa");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::hard_link(
        dir.0.join("bin/agent-sec-cli"),
        prefix.join("bin/agent-sec-cli"),
    )
    .unwrap();
    let skill = dir.0.join("skill");
    let task = dir.0.join("task.json");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
    let (plan, _) = run_fixture_plan(&prefix, &skill, &task, &[PlanReply::Valid], None, None);
    assert!(plan.status.success(), "{plan:?}");
    let verify = |stage: &str, reason: &str, file: Option<&str>, code| {
        let mut expected = json!({"stage":stage,"reason":reason});
        if let Some(file) = file {
            expected["file"] = file.into();
        }
        assert_runtime_failure(&prefix, &task, &expected, code);
    };
    let root = prefix.join("lib/anolisa/sec-core-skill-score");
    let lock = root.join(".lifecycle.lock");
    let requests = root.join("requests");
    std::fs::create_dir_all(&requests).unwrap();
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o1700)).unwrap();
    let manifest_file = Some("runtime-manifest.json");
    let receipt_file = Some("image-build.json");
    let manifest_path = root.join("runtime-manifest.json");
    std::fs::write(&manifest_path, "{").unwrap();
    verify("runtime.manifest.parse", "invalid_json", manifest_file, 2);

    let manifest = json!({"schema":"agent-sec-skill-score/runtime-v2","component":"sec-core-skill-score",
        "protocol_version":"agent-sec-skill-score/worker-v1","version":"1","source_commit":"test"});
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let digest = format!("{:x}", Sha256::digest(&manifest_bytes));
    std::fs::write(&manifest_path, &manifest_bytes).unwrap();
    let active_path = root.join("active-manifest.json");
    assert_active_manifest_failures(&prefix, &task, &active_path, &manifest);
    std::fs::write(&active_path, &manifest_bytes).unwrap();
    let env = root.join("environments").join(&digest);
    std::fs::create_dir_all(env.join("python/bin")).unwrap();
    let ready_path = env.join("ready.json");
    let mut ready = json!({"schema":"agent-sec-skill-score/ready-v1","manifest_sha256":digest,
        "version":"1","source_commit":"test","protocol_version":manifest["protocol_version"],"images":[]});
    std::fs::write(&ready_path, ready.to_string()).unwrap();
    let base = dir
        .0
        .join("var/lib/anolisa/component-manifests/sec-core/component.toml");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, "").unwrap();
    verify("runtime.base-manifest.verify", "runtime_error", None, 1);
    std::fs::write(&base, "version = \"2\"").unwrap();
    verify("runtime.manifest.verify", "runtime_error", manifest_file, 1);
    std::fs::write(&base, "version = \"1\"").unwrap();
    let mut wrong_ready = ready.clone();
    wrong_ready["source_commit"] = "stale".into();
    std::fs::write(&ready_path, wrong_ready.to_string()).unwrap();
    verify(
        "runtime.ready.verify",
        "runtime_error",
        Some("ready.json"),
        1,
    );

    let receipt_path = env.join("image-build.json");
    std::fs::write(&receipt_path, "{").unwrap();
    std::fs::write(&ready_path, ready.to_string()).unwrap();
    verify(
        "runtime.image-receipt.verify",
        "runtime_error",
        receipt_file,
        1,
    );
    ready["image_build_sha256"] = format!("{:x}", Sha256::digest(b"{")).into();
    std::fs::write(&ready_path, ready.to_string()).unwrap();
    verify(
        "runtime.image-receipt.verify",
        "invalid_json",
        receipt_file,
        2,
    );

    let receipt = json!({"schema":"agent-sec-skill-score/image-build-v1","manifest_sha256":digest,
        "source_commit":"test","version":"1","images":[]})
    .to_string();
    ready["image_build_sha256"] = format!("{:x}", Sha256::digest(receipt.as_bytes())).into();
    std::fs::write(receipt_path, receipt).unwrap();
    std::fs::write(ready_path, ready.to_string()).unwrap();
    std::fs::write(env.join("python/bin/python3.12"), "").unwrap();
    std::fs::write(env.join("worker-entry.py"), "").unwrap();
    verify("runtime.image.inspect", "runtime_error", receipt_file, 1);
}

#[test]
fn plan_rejects_credentials_in_omitted_binary_paths_before_contacting_the_model() {
    let prefix = installation();
    let skill = prefix.0.join("skill");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
    std::fs::write(skill.join("fixture-opaque-key.bin"), [0_u8, 255]).unwrap();
    let config = prefix.0.join("model.json");
    std::fs::write(
        &config,
        json!({"schemaVersion":1,"remote":{
        "provider":"anthropic-compatible","baseUrl":"http://127.0.0.1:1",
        "defaultModel":"fixture","apiKeyEnv":"TEST_KEY"}})
        .to_string(),
    )
    .unwrap();
    let output = prefix.0.join("task.json");
    let result = command(&prefix.0)
        .env("AGENT_SEC_MODEL_CONFIG", config)
        .env("TEST_KEY", "fixture-opaque-key")
        .arg("plan")
        .arg(skill)
        .arg("--output-file")
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2), "{result:?}");
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("credential") && !error.contains("fixture-opaque-key"),
        "{error}"
    );
    assert!(result.stdout.is_empty() && !output.exists());
}

#[test]
fn run_rejects_conflicting_report_paths_before_creating_evidence() {
    let prefix = installation();
    let skill = prefix.0.join("skill");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Example").unwrap();
    let task = serde_json::json!({
        "schema_version":"agent-sec-skill-score/task-v1", "skill_path":skill,
        "content_hash":"0".repeat(64),
        "audit":{"objective":"Describe the Skill","allowed_boundary":"Read SKILL.md",
        "findings":[],"probes":[{"id":"baseline","template":"baseline","entry_path":"SKILL.md",
        "turns":["Describe this Skill."],"activation":"Read its instructions","expected_result":"A description"}]}
    }).to_string();
    let parent = prefix.0.join("results");
    let evidence = parent.join("evidence");
    for output in [
        evidence.join("source-redaction.json"),
        evidence.clone(),
        parent.clone(),
    ] {
        let result = command(&prefix.0)
            .args(["run", &task, "--evidence-dir"])
            .arg(&evidence)
            .arg("--output-file")
            .arg(output)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("evidence"),
            "{result:?}"
        );
        assert!(!parent.exists());
    }
}

#[test]
fn cancellation_interrupts_open_stdin_without_waiting_for_eof() {
    let prefix = installation();
    let mut child = command(&prefix.0)
        .args(["run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            panic!("native stdin did not respond to cancellation");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
