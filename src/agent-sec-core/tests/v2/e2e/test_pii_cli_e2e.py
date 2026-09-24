"""PII transport, process identity and startup configuration using real Rust binaries."""

import hashlib
import json
import os
import shutil
import socket
import subprocess

import pytest


def _events(environment):
    path = environment[0] / "security-events.jsonl"
    if not path.exists():
        return []
    return [
        event
        for line in path.read_text().splitlines()
        if (event := json.loads(line))["category"] == "pii_scan"
    ]


def _rpc(daemon, request):
    payload = request if isinstance(request, bytes) else json.dumps(request).encode() + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(daemon.socket_path))
        connection.sendall(payload)
        with connection.makefile("rb") as response:
            return json.loads(response.readline())


def _scan(daemon, *args, input_text=None):
    result = daemon.cli("scan-pii", *args, input_text=input_text)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_input_modes_unicode_redaction_and_one_event_per_scan(
    pii_daemon, pii_environment, tmp_path
):
    text = "备注🙂e\u0301 alice@company.cn password=SecretToken987"
    input_file = tmp_path / "input.txt"
    input_file.write_text(text)
    reports = []
    for args, stdin in [
        (["--text", text], None),
        (["--stdin"], text),
        (["--text-stdin"], text),
        (["--input", str(input_file)], None),
    ]:
        reports.append(
            _scan(
                pii_daemon,
                *args,
                "--source",
                "tool_input",
                "--raw-evidence",
                "--redact-output",
                input_text=stdin,
            )
        )
    for report in reports:
        assert set(report) == {
            "ok",
            "verdict",
            "summary",
            "findings",
            "elapsed_ms",
            "redacted_text",
        }
        assert report["ok"] and report["verdict"] == "deny"
        assert report["summary"]["source"] == "tool_input"
        assert report["summary"]["scanner_version"] == "2.0.0"
        assert report["summary"]["coverage"] == {"status": "complete", "reasons": []}
        assert not report["summary"].get("findings_truncated", False)
        assert not report["summary"].get("redacted_text_omitted", False)
        assert report["summary"]["input_sha256"] == hashlib.sha256(text.encode()).hexdigest()
        for finding in report["findings"]:
            span = finding["span"]
            assert text[span["start"] : span["end"]] == finding["raw_evidence"]
        assert "alice@company.cn" not in report["redacted_text"]
        assert "SecretToken987" not in report["redacted_text"]
        assert report["findings"] == reports[0]["findings"]
    assert input_file.read_text() == text
    events = _events(pii_environment)
    assert len(events) == 4
    for event in events:
        assert event["result"] == "succeeded"
        assert event["details"]["request"]["text_length"] == len(text)
        assert event["details"]["result"]["summary"]["scanner_version"] == "2.0.0"
    audit = json.dumps(events)
    for forbidden in [
        text,
        "alice@company.cn",
        "SecretToken987",
        "raw_evidence",
        "redacted_text",
    ]:
        assert forbidden not in audit


def test_empty_warning_low_confidence_and_text_output(pii_daemon):
    assert _scan(pii_daemon, "--text", "")["verdict"] == "pass"
    assert _scan(pii_daemon, "--text", "alice@company.cn")["verdict"] == "warn"
    assert _scan(pii_daemon, "--text", "alice@example.com")["verdict"] == "pass"
    low = _scan(pii_daemon, "--text", "alice@example.com", "--include-low-confidence")
    assert low["verdict"] == "warn"
    text = pii_daemon.cli(
        "scan-pii",
        "--text",
        "alice@company.cn",
        "--format",
        "text",
        "--raw-evidence",
        "--redact-output",
    )
    assert text.returncode == 0 and "Verdict: warn" in text.stdout
    assert "Redacted text:" in text.stdout and "alice@company.cn" not in text.stdout


def test_dense_report_retains_complete_verdict_and_one_terminal_event(pii_daemon, pii_environment):
    text = "a@b.co " * 20_000 + "\npassword=abcdefghijklmnop"
    result = pii_daemon.cli(
        "scan-pii", "--stdin", "--raw-evidence", "--redact-output", input_text=text
    )
    assert result.returncode == 0, result.stderr
    # The pretty CLI report stays within the response budget, below UDS limits.
    assert len(result.stdout.rstrip("\n").encode()) <= 512 * 1024
    report = json.loads(result.stdout)
    summary = report["summary"]
    assert report["ok"] and report["verdict"] == "deny"
    assert summary["total"] == 20_001
    assert summary["by_severity"] == {"deny": 1, "warn": 20_000}
    assert summary["by_type"] == {"email": 20_000, "generic_secret_field": 1}
    assert summary["coverage"] == {"status": "complete", "reasons": []}
    assert not summary["truncated"]
    assert summary["scanned_bytes"] == len(text.encode())
    assert summary["input_sha256"] == hashlib.sha256(text.encode()).hexdigest()
    assert summary["findings_truncated"] is True
    assert not summary.get("redacted_text_omitted", False)
    assert [finding["type"] for finding in report["findings"]] == [
        "email",
        "generic_secret_field",
    ]
    assert report["findings"][0]["span"] == {"start": 0, "end": 6}
    assert report["findings"][-1]["severity"] == "deny"
    assert report["findings"][-1]["span"]["end"] == len(text)
    for finding in report["findings"]:
        assert "raw_evidence" not in finding
        assert finding["metadata"]["evidence_omitted"] is True
    assert report["redacted_text"].count("a***@b.co") == 20_000
    assert "abcdefghijklmnop" not in result.stdout
    [event] = _events(pii_environment)
    audited = event["details"]["result"]
    assert event["result"] == "succeeded" and audited["verdict"] == "deny"
    assert audited["summary"] == summary
    assert event["details"]["request"]["text_sha256"] == summary["input_sha256"]
    assert "redacted_text" not in audited
    assert "raw_evidence" not in json.dumps(event)
    assert "abcdefghijklmnop" not in json.dumps(event)


def test_oversized_redacted_text_returns_a_marked_safe_replacement(pii_daemon, pii_environment):
    text = "x" * (600 * 1024) + "\npassword=abcdefghijklmnop"
    result = pii_daemon.cli("scan-pii", "--stdin", "--redact-output", input_text=text)
    assert result.returncode == 0, result.stderr
    assert len(result.stdout.rstrip("\n").encode()) <= 512 * 1024
    report = json.loads(result.stdout)
    summary = report["summary"]
    assert report["ok"] and report["verdict"] == "deny"
    assert summary["total"] == 1 and summary["by_severity"] == {"deny": 1}
    assert summary["coverage"] == {"status": "complete", "reasons": []}
    assert not summary["truncated"]
    assert summary["scanned_bytes"] == len(text.encode())
    assert summary["input_sha256"] == hashlib.sha256(text.encode()).hexdigest()
    assert summary["redacted_text_omitted"] is True
    assert report["redacted_text"] == "[REDACTED: output size limit]"
    assert report["findings"][0]["severity"] == "deny"
    assert "abcdefghijklmnop" not in result.stdout
    [event] = _events(pii_environment)
    audited = event["details"]["result"]
    assert event["result"] == "succeeded" and audited["verdict"] == "deny"
    assert audited["summary"] == summary
    assert "redacted_text" not in audited
    assert "raw_evidence" not in json.dumps(event)
    assert "abcdefghijklmnop" not in json.dumps(event)


def test_explicit_utf8_limit_attests_only_received_prefix(pii_daemon):
    report = _scan(pii_daemon, "--stdin", "--max-bytes", "7", input_text="备注🙂 secret tail")
    summary = report["summary"]
    assert summary["bytes_scanned"] == 7 and summary["scanned_bytes"] == 6
    assert summary["truncated"] and summary["coverage"]["status"] == "partial"
    assert summary["input_sha256"] == hashlib.sha256("备注".encode()).hexdigest()
    assert summary["input_sha256"] == summary["scanned_input_sha256"]
    large = "x" * 1_048_577
    unbounded = _scan(pii_daemon, "--stdin", input_text=large)
    assert unbounded["summary"]["scanned_bytes"] == len(large)
    assert not unbounded["summary"]["truncated"]


@pytest.mark.parametrize(
    "args",
    [
        [],
        ["--text", "a", "--stdin"],
        ["--stdin", "--input", "file"],
        ["--text", "", "--max-bytes", "0"],
        ["--text", "", "--source", "other"],
        ["--text", "", "--format", "yaml"],
    ],
)
def test_usage_errors_do_not_dispatch(pii_daemon, pii_environment, args):
    result = pii_daemon.cli("scan-pii", *args)
    assert result.returncode == 2 and result.stderr
    assert _events(pii_environment) == []


def test_invalid_utf8_and_frame_limits_fail_before_dispatch(pii_daemon, pii_environment):
    binary = shutil.which("agent-sec-cli")
    assert binary
    for data in [
        b"\xff",
        b"\xe4",
        b"x" * (4 * 1024 * 1024 + 1),
        b"x" * (4 * 1024 * 1024),
        b"\x01" * (1024 * 1024),
    ]:
        # The last payload fits as text but its JSON escapes exceed the frame.
        result = subprocess.run(
            [binary, "--socket", str(pii_daemon.socket_path), "scan-pii", "--stdin"],
            input=data,
            capture_output=True,
            timeout=30,
            check=False,
        )
        assert result.returncode == 1 and result.stderr
    assert _events(pii_environment) == []


def test_socket_environment_and_kernel_identity_override_trace_claims(
    pii_daemon,
    pii_environment,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", str(pii_daemon.socket_path))
    trace = {
        "trace_id": "  trace-123  ",
        "traceId": "ignored",
        "session_id": " ",
        "sessionId": "session-123",
        "runId": "run-123",
        "callId": "call-123",
        "toolCallId": "tool-123",
        "agentName": "🙂" * 300,
        "uid": 987654,
        "gid": 987654,
        "pid": 987654,
    }
    binary = shutil.which("agent-sec-cli")
    assert binary
    with subprocess.Popen(
        [binary, "--trace-context", json.dumps(trace), "scan-pii", "--text", ""],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    ) as process:
        stdout, stderr = process.communicate(timeout=30)
        assert process.returncode == 0, stderr
        assert json.loads(stdout)["verdict"] == "pass"
        peer_pid = process.pid
    [event] = _events(pii_environment)
    assert event["uid"] == os.getuid() and event["pid"] == peer_pid
    assert event["trace_id"] == "trace-123" and event["session_id"] == "session-123"
    assert event["run_id"] == "run-123" and event["call_id"] == "call-123"
    assert event["tool_call_id"] == "tool-123"
    agent_name = event["details"]["request"]["agent_name"]
    assert len(agent_name) == 256 and agent_name.endswith("...[truncated]")
    # An explicit socket wins over an unusable environment value.
    monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", "relative-invalid")
    assert _scan(pii_daemon, "--text", "")["ok"]


def test_trace_usage_and_absent_daemon_fail_without_fallback(pii_daemon, pii_environment, tmp_path):
    binary = shutil.which("agent-sec-cli")
    assert binary
    for trace in ["[]", "{broken", "null"]:
        result = subprocess.run(
            [
                binary,
                "--trace-context",
                trace,
                "--socket",
                str(pii_daemon.socket_path),
                "scan-pii",
                "--text",
                "",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        assert result.returncode == 1
    result = pii_daemon.cli("scan-pii", "--text", "", "--trace-context", "{}")
    assert result.returncode == 2
    binary = shutil.which("agent-sec-cli")
    assert binary
    result = subprocess.run(
        [binary, "--socket", str(tmp_path / "absent.sock"), "scan-pii", "--text", ""],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    assert result.returncode == 1
    assert not (tmp_path / "absent.sock").exists()
    assert _events(pii_environment) == []


@pytest.mark.parametrize(
    "params,code",
    [
        ({}, "invalid_request"),
        ({"text": 3}, "invalid_request"),
        ({"text": "", "source": "other"}, "invalid_argument"),
        ({"text": "", "rawEvidence": "yes"}, "invalid_request"),
        ({"text": "", "maxBytes": 0}, "invalid_argument"),
        ({"text": "", "maxBytes": -1}, "invalid_request"),
        ({"text": "", "inputBytesScanned": 99}, "invalid_argument"),
        ({"text": "", "traceContext": []}, "invalid_request"),
        ({"text": "", "traceContext": {"traceId": "UNTRUSTED_VALUE"}}, "invalid_request"),
        *[
            ({"text": "", key: "UNTRUSTED_VALUE"}, "invalid_request")
            for key in ["rulesPath", "input", "uid", "gid", "pid"]
        ],
    ],
)
def test_authorized_parameter_rejection_finalizes_once(pii_daemon, pii_environment, params, code):
    response = _rpc(
        pii_daemon,
        {
            "method": "action.pii_scan",
            "params": params,
            "traceContext": {"version": 1, "baggage": "agentsec.session.id=rejected-session"},
            "compatibility": {"version": 1, "traceId": "rejected-trace"},
        },
    )
    assert response["error"]["code"] == code
    assert response["error"]["message"] == "PII scan parameters are invalid"
    [event] = _events(pii_environment)
    assert event["result"] == "failed" and event["uid"] == os.getuid()
    assert event["pid"] == os.getpid()
    assert event["details"]["request"] == {}
    assert event["trace_id"] == "rejected-trace"
    assert event["session_id"] == "rejected-session"
    assert "UNTRUSTED_VALUE" not in json.dumps(event)


def test_envelope_and_method_rejections_are_outside_pii_finalization(pii_daemon, pii_environment):
    for request in [
        {"method": "action.pii.scan", "params": {"text": ""}},
        {"method": "action.pii_scan", "params": []},
        {"method": "action.pii_scan", "params": {"text": ""}, "traceContext": {}},
        {"method": "action.pii_scan", "params": {"text": ""}, "traceContext": {"version": 2}},
        {"method": "action.pii_scan", "params": {"text": ""}, "uid": 0},
        b'{"method":"action.pii_scan","params":{"text":"","text":""}}\n',
        b"not-json\n",
    ]:
        assert "error" in _rpc(pii_daemon, request)
    assert _events(pii_environment) == []
    result = _rpc(pii_daemon, {"method": "action.pii_scan", "params": {"text": ""}})
    assert result["result"]["ok"]
    assert len(_events(pii_environment)) == 1


def test_central_rules_are_immutable_until_restart(pii_environment, start_daemon, tmp_path):
    rules = tmp_path / "rules.yaml"
    rules.write_text('- type: internal_reference\n  regex: "REF-[0-9]{4}"\n  severity: deny\n')
    daemon = start_daemon(admin_uids=[], pii_rules=rules)
    first = _scan(daemon, "--text", "REF-1234 alice@company.cn")
    assert first["summary"]["custom_rules"]["status"] == "loaded"
    assert {f["type"] for f in first["findings"]} == {"internal_reference", "email"}
    rules.write_text('- type: second_reference\n  regex: "NEW-[0-9]{4}"\n  severity: warn\n')
    stable = _scan(daemon, "--text", "REF-1234")
    assert stable["verdict"] == "deny"
    assert stable["summary"]["ruleset_id"] == first["summary"]["ruleset_id"]
    daemon.process.terminate()
    daemon.process.wait(timeout=5)
    restarted = start_daemon(admin_uids=[], pii_rules=rules, name="restarted.sock")
    new = _scan(restarted, "--text", "REF-1234 NEW-4321")
    assert new["verdict"] == "warn" and new["findings"][0]["type"] == "second_reference"
    assert new["summary"]["ruleset_id"] != first["summary"]["ruleset_id"]


def test_legacy_home_rules_are_not_loaded_and_invalid_config_is_partial(
    pii_environment,
    start_daemon,
    tmp_path,
):
    legacy = pii_environment[1] / ".config/agent-sec/pii-checker/rules.yaml"
    legacy.parent.mkdir(parents=True)
    legacy.write_text('- type: legacy\n  regex: "legacy-token"\n')
    daemon = start_daemon(admin_uids=[])
    report = _scan(daemon, "--text", "legacy-token")
    assert report["verdict"] == "pass"
    assert report["summary"]["custom_rules"]["status"] == "absent"
    daemon.process.terminate()
    daemon.process.wait(timeout=5)
    missing = start_daemon(admin_uids=[], name="missing.sock", pii_rules=tmp_path / "missing.yaml")
    result = missing.cli("scan-pii", "--text", "alice@company.cn")
    assert result.returncode == 0 and "custom PII rules disabled" in result.stderr
    report = json.loads(result.stdout)
    assert report["verdict"] == "warn" and report["summary"]["coverage"]["status"] == "partial"
    assert report["summary"]["custom_rules"]["status"] == "invalid"


def test_business_and_propagation_budgets_are_independent(pii_daemon, pii_environment):
    request = {"method": "action.pii_scan", "params": {"text": ""}}
    overhead = len(json.dumps(request, separators=(",", ":")).encode()) + 1
    request["params"]["text"] = "x" * (4 * 1024 * 1024 - overhead)
    request["traceContext"] = {"version": 1, "baggage": "agentsec.session.id=near-limit"}
    payload = json.dumps(request, separators=(",", ":")).encode() + b"\n"
    assert len(payload) > 4 * 1024 * 1024
    response = _rpc(pii_daemon, payload)
    assert response["result"]["ok"]
    assert response["result"]["summary"]["coverage"]["status"] == "complete"
    [event] = _events(pii_environment)
    assert event["session_id"] == "near-limit"

    oversized_context = {
        "method": "action.pii_scan",
        "params": {"text": ""},
        "compatibility": {"version": 1, "traceId": "x" * (32 * 1024)},
    }
    assert _rpc(pii_daemon, oversized_context)["error"]["code"] == "invalid_request"
    assert len(_events(pii_environment)) == 1
