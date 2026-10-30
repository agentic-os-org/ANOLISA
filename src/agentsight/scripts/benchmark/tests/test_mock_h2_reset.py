"""Cancelling a flow-controlled response must not kill its HTTP/2 connection."""

import socket
import threading
from types import SimpleNamespace

import h2.config
import h2.connection
import h2.events
import h2.settings
import pytest
from test_benchmark import mock_llm_server


@pytest.mark.parametrize("window", [1024, 4096])
def test_reset_blocked_response_keeps_connection_usable(window: int) -> None:
    server, client = socket.socketpair()
    errors: list[BaseException] = []

    def serve() -> None:
        try:
            mock_llm_server.serve_h2(
                server,
                SimpleNamespace(chunks=1, chunk_bytes=100000, chunk_delay=0, sse=True),
                False,
            )
        except BaseException as error:
            errors.append(error)

    thread = threading.Thread(target=serve)
    thread.start()
    connection = h2.connection.H2Connection(config=h2.config.H2Configuration(client_side=True))
    connection.initiate_connection()
    connection.update_settings({h2.settings.SettingCodes.INITIAL_WINDOW_SIZE: window})
    headers = [
        (":method", "POST"),
        (":scheme", "https"),
        (":authority", "localhost"),
        (":path", "/v1/chat/completions"),
    ]
    connection.send_headers(1, headers)
    connection.send_data(1, b'{"request_id":"cancelled"}', end_stream=True)
    client.settimeout(3)
    client.sendall(connection.data_to_send())
    try:
        while True:
            events = connection.receive_data(client.recv(65535))
            if any(isinstance(event, h2.events.DataReceived) for event in events):
                break
            client.sendall(connection.data_to_send())
        connection.reset_stream(1)
        client.sendall(connection.data_to_send())
        client.settimeout(0.2)
        try:
            data = client.recv(65535)
            assert data, f"RST_STREAM closed the entire connection: {errors}"
            connection.receive_data(data)
        except TimeoutError:
            pass
        connection.send_headers(
            3,
            [
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "localhost"),
                (":path", "/healthz"),
            ],
            end_stream=True,
        )
        client.sendall(connection.data_to_send())
        client.settimeout(3)
        body = b""
        ended = False
        while not ended:
            data = client.recv(65535)
            assert data, f"healthy stream lost after reset: {errors}"
            for event in connection.receive_data(data):
                if isinstance(event, h2.events.DataReceived) and event.stream_id == 3:
                    body += event.data
                if isinstance(event, h2.events.StreamEnded) and event.stream_id == 3:
                    ended = True
            pending = connection.data_to_send()
            if pending:
                client.sendall(pending)
        assert body == b"OK"
        assert not errors
    finally:
        client.close()
        thread.join(timeout=5)
        assert not thread.is_alive()
