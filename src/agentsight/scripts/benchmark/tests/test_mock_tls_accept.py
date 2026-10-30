"""An idle TCP peer must not stop other benchmark TLS clients from connecting."""

import http.client
import socket
import ssl
import subprocess
import sys
import time
from pathlib import Path

import h2.config
import h2.connection
import h2.events
import pytest
from test_benchmark import mock_llm_server


@pytest.mark.parametrize("protocol", ["http/1.1", "h2"])
def test_idle_tcp_peer_does_not_block_tls_accept(tmp_path: Path, protocol: str) -> None:
    cert, key = tmp_path / "server.crt", tmp_path / "server.key"
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
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
    process = subprocess.Popen(
        [
            sys.executable,
            mock_llm_server.__file__,
            "--port",
            str(port),
            "--cert",
            str(cert),
            "--key",
            str(key),
            "--http2",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    idle = None
    try:
        deadline = time.monotonic() + 5
        while True:
            probe = http.client.HTTPSConnection(
                "127.0.0.1", port, timeout=0.2, context=ssl._create_unverified_context()
            )
            try:
                probe.request("GET", "/healthz")
                assert probe.getresponse().read() == b"OK"
                break
            except OSError:
                assert process.poll() is None
                assert time.monotonic() < deadline
                time.sleep(0.02)
            finally:
                probe.close()
        idle = socket.create_connection(("127.0.0.1", port), timeout=1)
        context = ssl._create_unverified_context()
        context.set_alpn_protocols([protocol])
        with context.wrap_socket(
            socket.create_connection(("127.0.0.1", port), timeout=1), server_hostname="localhost"
        ) as client:
            assert client.selected_alpn_protocol() == protocol
            if protocol == "http/1.1":
                client.sendall(
                    b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                )
                response = http.client.HTTPResponse(client)
                response.begin()
                assert response.status == 200 and response.read() == b"OK"
            else:
                connection = h2.connection.H2Connection(
                    config=h2.config.H2Configuration(client_side=True)
                )
                connection.initiate_connection()
                connection.send_headers(
                    1,
                    [
                        (":method", "GET"),
                        (":scheme", "https"),
                        (":authority", "localhost"),
                        (":path", "/healthz"),
                    ],
                    end_stream=True,
                )
                client.sendall(connection.data_to_send())
                body = b""
                ended = False
                while not ended:
                    data = client.recv(65535)
                    assert data
                    for event in connection.receive_data(data):
                        if isinstance(event, h2.events.DataReceived):
                            body += event.data
                        if isinstance(event, h2.events.StreamEnded):
                            ended = True
                    pending = connection.data_to_send()
                    if pending:
                        client.sendall(pending)
                assert body == b"OK"
        assert process.poll() is None
    finally:
        if idle is not None:
            idle.close()
        process.terminate()
        process.wait(timeout=5)
