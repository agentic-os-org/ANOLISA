"""Self-contained e2e tests for the scan-pii CLI."""

import json
import os
import re
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

_MODES = ("binary", "module")


def _module_mode_available() -> bool:
    result = subprocess.run(
        [sys.executable, "-c", "import agent_sec_cli.cli"],
        capture_output=True,
        check=False,
        text=True,
        timeout=10,
    )
    return result.returncode == 0


def _command(mode: str) -> list[str]:
    if mode == "binary":
        return ["agent-sec-cli"]
    if mode == "module":
        if not _module_mode_available():
            pytest.skip(
                "module mode requires agent_sec_cli importable by this Python; "
                "RPM e2e validates the installed agent-sec-cli binary"
            )
        return [sys.executable, "-m", "agent_sec_cli.cli"]
    raise AssertionError(f"unknown CLI mode: {mode}")


def _run_cli(
    mode: str,
    *args: str,
    data_dir: Path,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    data_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["AGENT_SEC_DATA_DIR"] = str(data_dir)
    home_dir = data_dir / "home"
    home_dir.mkdir(parents=True, exist_ok=True)
    env["HOME"] = str(home_dir)
    try:
        return subprocess.run(
            [*_command(mode), *args],
            capture_output=True,
            text=True,
            input=input_text,
            check=False,
            timeout=30,
            env=env,
        )
    except FileNotFoundError as exc:
        raise AssertionError("agent-sec-cli binary not found on PATH") from exc


def _load_json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)
    assert isinstance(data, dict)
    return data


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_text_json(mode: str, tmp_path: Path) -> None:
    result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        "Contact alice@securecorp.cn for help.",
        "--source",
        "manual",
        "--format",
        "json",
        data_dir=tmp_path / mode / "text-json",
    )
    data = _load_json(result)

    assert data["ok"] is True
    assert data["verdict"] == "warn"
    assert data["summary"]["source"] == "manual"
    assert any(finding["type"] == "email" for finding in data["findings"])
    assert "redacted_text" not in data
    assert all("raw_evidence" not in finding for finding in data["findings"])


@pytest.mark.parametrize("mode", _MODES)
def test_bundled_pii_skill_commands(mode: str, tmp_path: Path) -> None:
    skill = Path(__file__).resolve().parents[3] / "skills/pii-checker/SKILL.md"
    commands = re.findall(r"```bash\n(.*?)\n```", skill.read_text(), flags=re.DOTALL)
    assert commands, "the bundled skill must provide executable CLI examples"
    text = "Contact alice@securecorp.cn; token=secret-value-1234567890"
    input_path = tmp_path / "report's input.txt"
    input_path.write_text(text, encoding="utf-8")

    for index, command in enumerate(commands):
        argv = shlex.split(command)
        assert argv[0] == "agent-sec-cli"
        args = [
            str(input_path) if arg == "/absolute/path/to/input.txt" else arg
            for arg in argv[1:]
        ]
        result = _run_cli(
            mode,
            *args,
            data_dir=tmp_path / mode / str(index),
            input_text=text if "--stdin" in args else None,
        )
        data = _load_json(result)
        assert data["ok"] is True
        assert data["verdict"] == "deny"
        assert data["summary"]["source"] == "manual"
        assert data["summary"]["truncated"] is False
        assert "secret-value-1234567890" not in result.stdout
        assert all("raw_evidence" not in finding for finding in data["findings"])
        if "--redact-output" in args:
            assert data["redacted_text"] != text
        else:
            assert "redacted_text" not in data
        assert input_path.read_text(encoding="utf-8") == text


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_filters_email_and_jwt_false_positives(
    mode: str, tmp_path: Path
) -> None:
    text = (
        "Call resolved.auth_source.as_deref(); then connect to "
        "ssh://deploy@securecorp.cn/home/deploy or contact alice@example.com"
    )
    default_result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        text,
        "--format",
        "json",
        data_dir=tmp_path / mode / "filtered-default",
    )
    default_data = _load_json(default_result)

    assert default_data["verdict"] == "pass"
    assert default_data["findings"] == []

    detailed_result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        text,
        "--format",
        "json",
        "--include-low-confidence",
        data_dir=tmp_path / mode / "filtered-detailed",
    )
    detailed_data = _load_json(detailed_result)

    assert detailed_data["verdict"] == "warn"
    assert detailed_data["summary"]["by_type"] == {"email": 2}
    assert {
        finding["metadata"]["context"] for finding in detailed_data["findings"]
    } == {
        "remote_identity",
        "reserved_domain",
    }


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_stdin_json(mode: str, tmp_path: Path) -> None:
    result = _run_cli(
        mode,
        "scan-pii",
        "--stdin",
        "--source",
        "manual",
        "--format",
        "json",
        data_dir=tmp_path / mode / "stdin-json",
        input_text="Contact alice@securecorp.cn for help.",
    )
    data = _load_json(result)

    assert data["ok"] is True
    assert data["verdict"] == "warn"
    assert data["summary"]["source"] == "manual"
    assert any(finding["type"] == "email" for finding in data["findings"])


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_stdin_max_bytes_truncates_before_scan(
    mode: str, tmp_path: Path
) -> None:
    max_bytes = len("备注".encode("utf-8")) + 1
    result = _run_cli(
        mode,
        "scan-pii",
        "--stdin",
        "--source",
        "manual",
        "--format",
        "json",
        "--max-bytes",
        str(max_bytes),
        data_dir=tmp_path / mode / "stdin-max-bytes",
        input_text="备注🙂 alice@example.com",
    )
    data = _load_json(result)

    assert data["ok"] is True
    assert data["summary"]["source"] == "manual"
    assert data["summary"]["truncated"] is True
    assert data["summary"]["bytes_scanned"] == max_bytes
    assert not any(finding["type"] == "email" for finding in data["findings"])


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_input_file_json(mode: str, tmp_path: Path) -> None:
    input_path = tmp_path / mode / "input.txt"
    input_path.parent.mkdir(parents=True, exist_ok=True)
    input_path.write_text("Phone: 13800138000\n", encoding="utf-8")

    result = _run_cli(
        mode,
        "scan-pii",
        "--input",
        str(input_path),
        "--source",
        "manual",
        "--format",
        "json",
        data_dir=tmp_path / mode / "input-json",
    )
    data = _load_json(result)

    assert data["ok"] is True
    assert data["verdict"] == "warn"
    assert any(finding["type"] == "phone_cn" for finding in data["findings"])
    assert all("raw_evidence" not in finding for finding in data["findings"])


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_redact_output_adds_redacted_text(mode: str, tmp_path: Path) -> None:
    secret = "password=supersecretvalue12345"
    result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        secret,
        "--source",
        "manual",
        "--format",
        "json",
        "--redact-output",
        data_dir=tmp_path / mode / "redact-output",
    )
    data = _load_json(result)

    assert data["verdict"] == "deny"
    assert "redacted_text" in data
    assert "supersecretvalue12345" not in data["redacted_text"]
    assert "password=" in data["redacted_text"]


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_raw_evidence_stays_out_of_security_events(
    mode: str, tmp_path: Path
) -> None:
    token = "abcdefghijklmnopqrstuvwx12345678"
    text = f"Authorization: Bearer {token}"
    data_dir = tmp_path / mode / "events-sanitized"

    scan_result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        text,
        "--source",
        "tool_output",
        "--format",
        "json",
        "--raw-evidence",
        "--redact-output",
        data_dir=data_dir,
    )
    scan_data = _load_json(scan_result)
    assert any("raw_evidence" in finding for finding in scan_data["findings"])

    events_result = _run_cli(
        mode,
        "events",
        "--category",
        "pii_scan",
        "--output",
        "json",
        data_dir=data_dir,
    )
    assert events_result.returncode == 0, events_result.stderr
    events = json.loads(events_result.stdout)
    assert isinstance(events, list)
    assert len(events) == 1

    event = events[0]
    details = event["details"]
    details_text = json.dumps(details, ensure_ascii=False)
    assert event["category"] == "pii_scan"
    assert details["request"]["source"] == "tool_output"
    assert "text" not in details["request"]
    assert "text_sha256" in details["request"]
    assert "redacted_text" not in details["result"]
    assert all(
        "raw_evidence" not in finding for finding in details["result"]["findings"]
    )
    assert text not in details_text
    assert token not in details_text


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_loads_fixed_custom_regex_rules(mode: str, tmp_path: Path) -> None:
    data_dir = tmp_path / mode / "custom-rules"
    rules_path = (
        data_dir / "home" / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"
    )
    rules_path.parent.mkdir(parents=True, exist_ok=True)
    rules_path.write_text(
        """
- type: dogfood_order_no
  regex: 'ORDER-[A-Z0-9]{8}'
  severity: warn
- type: dogfood_customer_token
  regex: 'DFT-[A-Z0-9]{16}'
  severity: deny
""".lstrip(),
        encoding="utf-8",
    )
    text = "order=ORDER-ABC12345 token=DFT-ABCDEF1234567890"

    result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        text,
        "--source",
        "tool_output",
        "--format",
        "json",
        "--redact-output",
        data_dir=data_dir,
    )
    data = _load_json(result)

    assert data["verdict"] == "deny"
    assert {
        "dogfood_order_no",
        "dogfood_customer_token",
    }.issubset({finding["type"] for finding in data["findings"]})
    assert data["summary"]["custom_rules"]["status"] == "loaded"
    assert data["summary"]["custom_rules"]["rule_count"] == 2
    assert "ORDER-ABC12345" not in data["redacted_text"]
    assert "DFT-ABCDEF1234567890" not in data["redacted_text"]

    events_result = _run_cli(
        mode,
        "events",
        "--category",
        "pii_scan",
        "--output",
        "json",
        data_dir=data_dir,
    )
    assert events_result.returncode == 0, events_result.stderr
    events = json.loads(events_result.stdout)
    assert len(events) == 1
    details = events[0]["details"]
    details_text = json.dumps(details, ensure_ascii=False)
    assert {
        "dogfood_order_no",
        "dogfood_customer_token",
    }.issubset({finding["type"] for finding in details["result"]["findings"]})
    assert details["result"]["summary"]["custom_rules"]["status"] == "loaded"
    assert text not in details_text
    assert "ORDER-[A-Z0-9]{8}" not in details_text
    assert "DFT-[A-Z0-9]{16}" not in details_text


@pytest.mark.parametrize("mode", _MODES)
def test_scan_pii_invalid_custom_rules_fail_open(mode: str, tmp_path: Path) -> None:
    data_dir = tmp_path / mode / "invalid-custom-rules"
    rules_path = (
        data_dir / "home" / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"
    )
    rules_path.parent.mkdir(parents=True, exist_ok=True)
    sensitive_pattern = "[private-business-pattern"
    rules_path.write_text(
        f"- type: dogfood_token\n  regex: '{sensitive_pattern}'\n",
        encoding="utf-8",
    )

    result = _run_cli(
        mode,
        "scan-pii",
        "--text",
        "alice@company.cn",
        "--format",
        "json",
        data_dir=data_dir,
    )
    data = _load_json(result)

    assert data["verdict"] == "warn"
    assert data["summary"]["custom_rules"]["status"] == "invalid"
    assert data["summary"]["custom_rules"]["error_code"] == "invalid_regex"
    assert "invalid_regex" in result.stderr
    assert sensitive_pattern not in result.stderr

    events_result = _run_cli(
        mode,
        "events",
        "--category",
        "pii_scan",
        "--output",
        "json",
        data_dir=data_dir,
    )
    assert events_result.returncode == 0, events_result.stderr
    events = json.loads(events_result.stdout)
    assert len(events) == 1
    details = events[0]["details"]
    details_text = json.dumps(details, ensure_ascii=False)
    custom_summary = details["result"]["summary"]["custom_rules"]
    assert custom_summary["status"] == "invalid"
    assert custom_summary["error_code"] == "invalid_regex"
    assert len(custom_summary["ruleset_sha256"]) == 64
    assert sensitive_pattern not in details_text
    assert "alice@company.cn" not in details_text
    assert ".config" not in details_text
