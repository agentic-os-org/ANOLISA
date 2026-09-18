"""Installed V2 CLI coverage for the daemon-backed code scanner."""

import json


def test_scan_code_returns_the_daemon_result_with_v1_exit_semantics(daemon) -> None:
    result = daemon.cli("scan-code", "--code", "rm -rf /tmp/test")

    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    payload = json.loads(result.stdout)
    assert payload["ok"] is True
    assert payload["verdict"] == "warn"
    assert payload["language"] == "bash"
    assert payload["findings"]


def test_environment_socket_reaches_the_ci_daemon_for_code_scan(cli) -> None:
    result = cli("scan-code", "--code", "rm -rf /tmp/test")

    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    assert json.loads(result.stdout)["verdict"] == "warn"


def test_scan_code_error_results_remain_parseable_on_stdout(daemon) -> None:
    result = daemon.cli("scan-code", "--code", "echo hello", "--mode", "llm")

    assert result.returncode == 1
    assert result.stderr == ""
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert payload["verdict"] == "error"
    assert payload["summary"] == "scan error: LLM model not available"


def test_scan_code_rejects_empty_input_before_contacting_the_daemon(cli) -> None:
    result = cli("--socket", "/run/does-not-exist.sock", "scan-code")

    assert result.returncode == 1
    assert result.stdout == ""
    assert result.stderr == "Error: --code is required (use --code '<source>')\n"
