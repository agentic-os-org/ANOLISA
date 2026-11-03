"""The SSE fault must observe response data before deliberately closing TLS."""

import http.client
import json
import os
import ssl
import subprocess
import sys
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler
from pathlib import Path
from types import SimpleNamespace

import pytest
from test_benchmark import campaign_data, campaign_evidence, fault_injector, mock_llm_server


@pytest.fixture
def peer(tmp_path: Path) -> Iterator[tuple[int, dict[str, object]]]:
    cert, key = tmp_path / "cert.pem", tmp_path / "key.pem"
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
            str(key),
            "-out",
            str(cert),
        ],
        check=True,
        capture_output=True,
    )
    state: dict[str, object] = {
        "kind": "sse",
        "sent": threading.Event(),
        "release": threading.Event(),
    }

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args: object) -> None:
            pass

        def do_GET(self) -> None:
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"OK")

        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(200)
            self.send_header(
                "Content-Type", "text/plain" if state["kind"] == "plain" else "text/event-stream"
            )
            self.end_headers()
            try:
                if state["kind"] == "sse":
                    self.wfile.write(b'data: {"choices":[{"delta":{"content":"first"}}]}\n\n')
                    self.wfile.flush()
                    state["sent"].set()
                    state["release"].wait(3)
                self.wfile.write(b"data: [DONE]\n\n")
            except OSError:
                pass

    server = mock_llm_server.BenchmarkHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield server.server_address[1], state
    finally:
        state["release"].set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@pytest.mark.parametrize("kind", ["sse", "done", "plain"])
def test_injector_verifies_partial_sse_on_real_tls(
    peer: tuple[int, dict[str, object]], monkeypatch: pytest.MonkeyPatch, kind: str
) -> None:
    port, state = peer
    state["kind"] = kind
    original = fault_injector.cases
    monkeypatch.setattr(
        fault_injector, "cases", lambda size: {"truncated_sse": original(size)["truncated_sse"]}
    )
    report = fault_injector.inject("127.0.0.1", port, 1, 8, 0.5, os.getpid())
    assert report.get("sse_truncation_verified") is (kind == "sse")
    if kind == "sse":
        assert state["sent"].is_set()
        assert report["outcomes"]["truncated_sse"] == {"truncated_sse_after_data": 1}
    assert report["server_healthy_after"] is True


@pytest.mark.parametrize("verified", [None, False, True])
def test_fault_campaign_requires_truncation_evidence(tmp_path: Path, verified: bool | None) -> None:
    run_path = tmp_path / "run-result.json"
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    fault = {
        "outcomes": {name: {"handled": 1} for name in campaign_evidence.FAULT_CASES},
        "server_healthy_after": True,
        "process_alive_before": True,
        "process_alive_after": True,
    }
    if verified is not None:
        fault["sse_truncation_verified"] = verified
    (measurement / "fault-results.json").write_text(json.dumps(fault))
    result = campaign_evidence.fault_outcome(
        run_path,
        {
            "summary": {
                "http_success_rate": 1,
                "trace_completeness": 1,
                "token_accuracy": 1,
                "process_survived": True,
                "runtime_clean": True,
            }
        },
        {"repetitions_per_case": 1},
        campaign_data(tmp_path)["thresholds"],
    )
    assert result["verdict"] == (
        "INCONCLUSIVE" if verified is None else "FAIL" if not verified else "PASS"
    )


@pytest.mark.parametrize("verified", [None, False, True])
def test_injector_exit_requires_verified_sse(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, verified: bool | None
) -> None:
    report = {"server_healthy_after": True, "process_alive_after": True, "outcomes": {}}
    if verified is not None:
        report["sse_truncation_verified"] = verified
    monkeypatch.setattr(fault_injector, "inject", lambda *args: report)
    output = tmp_path / "fault.json"
    monkeypatch.setattr(sys, "argv", ["fault_injector.py", "--output", str(output)])
    assert fault_injector.main() == (0 if verified is True else 1)
    assert json.loads(output.read_text()) == report


@pytest.mark.parametrize("normal_sse", [False, True])
def test_real_mock_omits_done_and_chunk_terminator(
    tmp_path: Path, peer: tuple[int, dict[str, object]], normal_sse: bool
) -> None:
    server = mock_llm_server.BenchmarkHTTPServer(("127.0.0.1", 0), mock_llm_server.BenchmarkHandler)
    server.settings = SimpleNamespace(chunks=3, chunk_bytes=8, chunk_delay=0, sse=normal_sse)
    server.verbose = False
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(tmp_path / "cert.pem", tmp_path / "key.pem")
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        port = server.server_address[1]
        payload, _ = fault_injector.cases(8)["truncated_sse"]
        with fault_injector.tls_socket("127.0.0.1", port, 1) as client:
            client.sendall(payload)
            response = http.client.HTTPResponse(client)
            response.begin()
            assert response.status == 200
            assert response.getheader("Content-Type") == "text/event-stream"
            with pytest.raises(http.client.IncompleteRead) as error:
                response.read()
            partial = error.value.partial
            assert partial.count(b"data:") == 1
            assert b"[DONE]" not in partial
            assert b"usage" not in partial
        assert fault_injector.health_check("127.0.0.1", port, 1)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
