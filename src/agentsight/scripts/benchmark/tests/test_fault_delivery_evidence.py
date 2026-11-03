"""A healthy server cannot turn unsent fault attempts into passing evidence."""

import json
import os
import sys
from pathlib import Path
from typing import Any

import pytest
from test_benchmark import campaign_evidence, complete_summary, fault_injector
from test_campaign_evidence import thresholds


class Connection:
    def __init__(self, phase: str) -> None:
        self.phase = phase

    def __enter__(self) -> "Connection":
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    def close(self) -> None:
        pass

    def sendall(self, payload: bytes) -> None:
        if self.phase == "send":
            raise BrokenPipeError("fixture send interrupted")

    def recv(self, size: int) -> bytes:
        if self.phase == "response":
            raise ConnectionResetError("fixture peer rejected malformed request")
        return b"HTTP/1.1 400 Bad Request\r\n\r\n"


def fault_run(monkeypatch: pytest.MonkeyPatch, phase: str) -> dict[str, Any]:
    attempts = 0

    def connect(*args: object) -> Connection:
        nonlocal attempts
        attempts += 1
        if attempts == 1 and phase == "connect":
            raise ConnectionRefusedError("fixture listener not ready")
        return Connection(phase if attempts == 1 else "ok")

    monkeypatch.setattr(fault_injector, "tls_socket", connect)
    monkeypatch.setattr(fault_injector, "cases", lambda _: {"invalid_json": (b"bad request", True)})
    monkeypatch.setattr(fault_injector, "health_check", lambda *args: True)
    result = fault_injector.inject("localhost", 1, 1, 1, 1, os.getpid())
    for name in campaign_evidence.FAULT_CASES - result["outcomes"].keys():
        result["outcomes"][name] = {"closed_without_response": 1}
    result["outcomes"]["truncated_sse"] = {"truncated_sse_after_data": 1}
    result["sse_truncation_verified"] = True
    return result


@pytest.mark.parametrize("phase", ["connect", "send", "response", "ok"])
def test_delivery_phase_reaches_campaign_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, phase: str
) -> None:
    result = fault_run(monkeypatch, phase)
    path = tmp_path / "run-result.json"
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    (measurement / "fault-results.json").write_text(json.dumps(result))
    outcome = campaign_evidence.fault_outcome(
        path, {"summary": complete_summary()}, {"repetitions_per_case": 1}, thresholds()
    )
    delivered = phase in {"response", "ok"}
    assert outcome["verdict"] == ("PASS" if delivered else "FAIL")
    assert ("delivery:invalid_json" in outcome["failed"]) is (not delivered)


@pytest.mark.parametrize("phase", ["connect", "send", "response", "ok"])
def test_injector_exit_status_requires_delivered_faults(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, phase: str
) -> None:
    report = fault_run(monkeypatch, phase)
    monkeypatch.setattr(fault_injector, "inject", lambda *args: report)
    path = tmp_path / "fault.json"
    monkeypatch.setattr(sys, "argv", ["fault_injector.py", "--output", str(path)])
    assert fault_injector.main() == (0 if phase in {"response", "ok"} else 1)
    assert json.loads(path.read_text())["outcomes"] == report["outcomes"]


def test_tls_connection_fault_failure_is_not_success(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    report = fault_run(monkeypatch, "ok")
    report["outcomes"]["tls_connect_then_close"] = {"failed:TimeoutError": 1}
    folder = tmp_path / "measurement"
    folder.mkdir()
    (folder / "fault-results.json").write_text(json.dumps(report))
    result = campaign_evidence.fault_outcome(
        tmp_path / "run-result.json",
        {"summary": complete_summary()},
        {"repetitions_per_case": 1},
        thresholds(),
    )
    assert result["verdict"] == "FAIL"
    assert "delivery:tls_connect_then_close" in result["failed"]


@pytest.mark.parametrize("count", [0, 1])
def test_zero_failure_bucket_is_not_a_failed_attempt(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, count: int
) -> None:
    report = fault_run(monkeypatch, "ok")
    report["outcomes"]["invalid_json"] = {"not_sent:TimeoutError": count, "HTTP/1.1 400": 1 - count}
    folder = tmp_path / "measurement"
    folder.mkdir()
    (folder / "fault-results.json").write_text(json.dumps(report))
    result = campaign_evidence.fault_outcome(
        tmp_path / "run-result.json",
        {"summary": complete_summary()},
        {"repetitions_per_case": 1},
        thresholds(),
    )
    assert result["verdict"] == ("FAIL" if count else "PASS")
