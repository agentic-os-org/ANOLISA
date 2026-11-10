"""Black-box tests of the secret gateway addon inside the standalone mitmdump.

Runs mitmdump in regular-proxy mode on loopback; a test-only addon routes
upstream connections to local mock servers, so no system network state is
touched. Set MITMDUMP to the pinned standalone binary to enable the suite.
"""

from __future__ import annotations

import base64
import gzip
import http.client
import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import ClassVar, Iterator

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_policy import DECISIONS, ROOT, VALUES, plan_for  # noqa: E402

MITMDUMP = os.environ.get("MITMDUMP", "")
ADDON = ROOT / "v2" / "gateway" / "addon" / "asc_gateway_addon.py"
REAL, FAKE, OTHER_FAKE = VALUES["real"], VALUES["fake"], VALUES["otherFake"]
ATTACKER_IP = "203.0.113.7"
MISNAMED_IP = "203.0.113.8"
H2_IP = "203.0.113.9"
BIG = 9 * 1024 * 1024

ROUTING_ADDON = f"""
import os
PORTS = {{
    "{ATTACKER_IP}": "ASC_TEST_UNTRUSTED_PORT",
    "{MISNAMED_IP}": "ASC_TEST_MISNAMED_PORT",
    "{H2_IP}": "ASC_TEST_H2_PORT",
}}
def server_connect(data):
    variable = PORTS.get(data.server.address[0], "ASC_TEST_MOCK_PORT")
    if variable == "ASC_TEST_H2_PORT":
        # Exercise HTTP/2 upstream even when the downstream client speaks HTTP/1.
        data.server.alpn_offers = (b"h2",)
    data.server.address = ("127.0.0.1", int(os.environ[variable]))
"""

# Only the standalone binary's bundled interpreter imports h2; the test runner is stdlib-only.
H2_UPSTREAM_ADDON = """
import base64
import gzip
import json
import os
import socket
import ssl
import threading
from pathlib import Path

from h2.config import H2Configuration
from h2.connection import H2Connection
from h2.events import DataReceived, RequestReceived, StreamEnded


class H2Upstream:
    def running(self):
        self.directory = Path(os.environ["ASC_TEST_H2_WORKDIR"])
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(
            str(self.directory / "leaf.pem"), str(self.directory / "leaf.key")
        )
        self.context.set_alpn_protocols(["h2"])
        self.stopped = threading.Event()
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", int(os.environ["ASC_TEST_H2_PORT"])))
        self.listener.listen(4)
        self.listener.settimeout(0.2)
        self.active = None
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while not self.stopped.is_set():
            try:
                raw, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with raw:
                raw.settimeout(2)
                try:
                    stream = self.context.wrap_socket(raw, server_side=True)
                except (OSError, ssl.SSLError):
                    # The readiness check opens TCP without a TLS handshake.
                    continue
                with stream:
                    self.active = stream
                    try:
                        self.handle(stream)
                    except Exception as error:
                        if not self.stopped.is_set():
                            (self.directory / "h2-error.json").write_text(
                                json.dumps({"error": type(error).__name__})
                            )
                    finally:
                        self.active = None

    def handle(self, stream):
        if stream.selected_alpn_protocol() != "h2":
            raise ValueError("HTTP/2 ALPN required")
        connection = H2Connection(
            config=H2Configuration(client_side=False, header_encoding="utf-8")
        )
        connection.initiate_connection()
        stream.sendall(connection.data_to_send())
        requests = {}
        while not self.stopped.is_set():
            try:
                data = stream.recv(65535)
            except socket.timeout:
                continue
            if not data:
                return
            for event in connection.receive_data(data):
                if isinstance(event, RequestReceived):
                    requests[event.stream_id] = dict(event.headers)
                elif isinstance(event, DataReceived):
                    connection.acknowledge_received_data(
                        event.flow_controlled_length, event.stream_id
                    )
                elif isinstance(event, StreamEnded):
                    headers = requests.pop(event.stream_id)
                    auth = headers.get("authorization", "")
                    path = headers[":path"]
                    trailers = path.startswith("/echo/h2-trailers/") or path == "/o/r.git/git-upload-pack"
                    encoding = path.rsplit("/", 1)[1]
                    proof = {"authorization": auth, "alpn": stream.selected_alpn_protocol()}
                    if trailers:
                        proof.update({"path": path, "method": headers[":method"]})
                    payload = json.dumps({"auth": auth}).encode()
                    if encoding == "gzip":
                        payload = gzip.compress(payload, mtime=0) if trailers else gzip.compress(payload)
                        split = len(payload) // 2
                    else:
                        # Split the actual received credential, not a preselected fake echo.
                        split = payload.index(auth.encode()) + len(auth) // 2
                    if trailers:
                        proof["body"] = base64.b64encode(payload).decode()
                    temporary = self.directory / ".h2-proof.tmp"
                    temporary.write_text(json.dumps(proof))
                    os.replace(temporary, self.directory / ("h2-proof-" + encoding + ".json"))
                    response_headers = [
                        (":status", "200"), ("content-type", "application/json"),
                        ("content-length", str(len(payload))),
                    ]
                    if encoding == "gzip":
                        response_headers.append(("content-encoding", "gzip"))
                    if trailers:
                        response_headers.append(("x-echo", auth))
                    connection.send_headers(event.stream_id, response_headers)
                    connection.send_data(event.stream_id, payload[:split], end_stream=False)
                    stream.sendall(connection.data_to_send())
                    # Legal zero-length DATA does not signal EOF without END_STREAM.
                    connection.send_data(event.stream_id, b"", end_stream=False)
                    stream.sendall(connection.data_to_send())
                    connection.send_data(event.stream_id, payload[split:], end_stream=not trailers)
                    if trailers:
                        connection.send_headers(
                            event.stream_id, [("x-echo-trailer", auth)], end_stream=True
                        )
                    stream.sendall(connection.data_to_send())
            pending = connection.data_to_send()
            if pending:
                stream.sendall(pending)

    def done(self):
        self.stopped.set()
        self.listener.close()
        active = self.active
        if active is not None:
            try:
                active.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.thread.join(timeout=5)


addons = [H2Upstream()]
"""

# Decode wire responses with bundled h2/HPACK, never gateway flow objects.
H2_CLIENT_ADDON = r"""
import asyncio
import base64
import json
import os
import socket
import ssl
from pathlib import Path

from h2.config import H2Configuration
from h2.connection import H2Connection
from h2.events import (
    ConnectionTerminated, DataReceived, ResponseReceived,
    StreamEnded, StreamReset, TrailersReceived,
)
from mitmproxy import ctx


def exchange(request, context):
    with socket.create_connection(("127.0.0.1", request["port"]), timeout=10) as raw:
        target = request["connect"] + ":443"
        raw.sendall(
            f"CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n".encode()
        )
        reply = bytearray()
        while not reply.endswith(b"\r\n\r\n"):
            chunk = raw.recv(1)
            if not chunk or len(reply) >= 8192:
                raise ConnectionError("CONNECT did not complete")
            reply.extend(chunk)
        if b" 200 " not in reply.split(b"\r\n", 1)[0]:
            raise ConnectionError("CONNECT rejected")
        with context.wrap_socket(raw, server_hostname=request["host"]) as stream:
            alpn = stream.selected_alpn_protocol()
            if alpn != "h2":
                raise ValueError("HTTP/2 ALPN required")
            connection = H2Connection(
                config=H2Configuration(client_side=True, header_encoding="utf-8")
            )
            connection.initiate_connection()
            connection.send_headers(1, [
                (":method", request["method"]), (":scheme", "https"),
                (":authority", request["host"]), (":path", request["path"]),
                ("authorization", request["authorization"]),
                ("accept-encoding", "gzip"),
            ], end_stream=True)
            stream.sendall(connection.data_to_send())
            result = {"alpn": alpn, "headers": {}, "trailers": {}}
            body = bytearray()
            while True:
                data = stream.recv(65535)
                if not data:
                    raise ConnectionError("HTTP/2 response ended before END_STREAM")
                for event in connection.receive_data(data):
                    if isinstance(event, (StreamReset, ConnectionTerminated)):
                        raise ConnectionError(type(event).__name__)
                    if getattr(event, "stream_id", None) != 1:
                        continue
                    if isinstance(event, ResponseReceived):
                        result["headers"] = dict(event.headers)
                    elif isinstance(event, TrailersReceived):
                        result["trailers"] = dict(event.headers)
                    elif isinstance(event, DataReceived):
                        body.extend(event.data)
                        connection.acknowledge_received_data(
                            event.flow_controlled_length, event.stream_id
                        )
                    elif isinstance(event, StreamEnded):
                        result["body"] = base64.b64encode(body).decode()
                        return result
                pending = connection.data_to_send()
                if pending:
                    stream.sendall(pending)


class H2Client:
    async def running(self):
        try:
            request = json.loads(os.environ["ASC_TEST_H2_REQUEST"])
            # The pinned standalone cannot create client TLS contexts in worker threads.
            context = ssl.create_default_context(cafile=request["ca"])
            context.set_alpn_protocols(["h2"])
            result = await asyncio.to_thread(exchange, request, context)
        except Exception as error:
            result = {"error": type(error).__name__, "reason": str(error)}
        try:
            Path(os.environ["ASC_TEST_H2_RESULT"]).write_text(json.dumps(result))
        finally:
            ctx.master.shutdown()


addons = [H2Client()]
"""


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def openssl(*args: str) -> None:
    subprocess.run(["openssl", *args], check=True, capture_output=True)


def make_certificates(directory: Path) -> None:
    openssl(
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "2",
        "-subj",
        "/CN=asc mock ca",
        "-keyout",
        str(directory / "ca.key"),
        "-out",
        str(directory / "ca.pem"),
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=keyCertSign",
    )
    openssl(
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-subj",
        "/CN=api.github.com",
        "-keyout",
        str(directory / "leaf.key"),
        "-out",
        str(directory / "leaf.csr"),
    )
    (directory / "ext").write_text("subjectAltName=DNS:api.github.com,DNS:github.com\n")
    openssl(
        "x509",
        "-req",
        "-days",
        "2",
        "-in",
        str(directory / "leaf.csr"),
        "-CA",
        str(directory / "ca.pem"),
        "-CAkey",
        str(directory / "ca.key"),
        "-CAcreateserial",
        "-out",
        str(directory / "leaf.pem"),
        "-extfile",
        str(directory / "ext"),
    )
    openssl(
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "2",
        "-subj",
        "/CN=api.github.com",
        "-addext",
        "subjectAltName=DNS:api.github.com",
        "-keyout",
        str(directory / "rogue.key"),
        "-out",
        str(directory / "rogue.pem"),
    )
    # Signed by the trusted CA but for another name: proves hostname verification.
    openssl(
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-subj",
        "/CN=evil.example",
        "-keyout",
        str(directory / "misnamed.key"),
        "-out",
        str(directory / "misnamed.csr"),
    )
    (directory / "misnamed.ext").write_text("subjectAltName=DNS:evil.example\n")
    openssl(
        "x509",
        "-req",
        "-days",
        "2",
        "-in",
        str(directory / "misnamed.csr"),
        "-CA",
        str(directory / "ca.pem"),
        "-CAkey",
        str(directory / "ca.key"),
        "-CAcreateserial",
        "-out",
        str(directory / "misnamed.pem"),
        "-extfile",
        str(directory / "misnamed.ext"),
    )


class QuietServer(ThreadingHTTPServer):
    def handle_error(self, request: object, client_address: object) -> None:
        # Failed handshakes are expected: the rogue upstream must be refused.
        pass


class MockUpstream:
    """HTTPS server recording every request it receives."""

    def __init__(self, cert: Path, key: Path) -> None:
        self.requests: list[dict[str, object]] = []
        self.progress: dict[str, tuple[int, int]] = {}
        self.finished: dict[str, threading.Event] = {}
        self.upload_started: dict[str, threading.Event] = {}
        self.stream_steps: dict[str, tuple[threading.Event, threading.Event]] = {}
        upstream = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args: object) -> None:
                pass

            def _handle(self) -> None:
                if self.path == "/echo/early-upload":
                    auth = (
                        self.headers.get("X-Api-Key")
                        or self.headers.get("Authorization")
                        or ""
                    )
                    upstream.requests.append({"path": self.path, "authorization": auth})
                    payload = json.dumps(
                        {"auth": auth, "padding": "x" * 200000}
                    ).encode()
                    self.send_response(413)
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                    self.wfile.flush()
                    self.close_connection = True
                    return
                length = int(self.headers.get("Content-Length") or 0)
                if self.path in upstream.upload_started:
                    body = bytearray()
                    if self.headers.get("Transfer-Encoding") == "chunked":
                        while size := int(self.rfile.readline().strip(), 16):
                            body.extend(self.rfile.read(size))
                            assert self.rfile.read(2) == b"\r\n"
                            upstream.upload_started[self.path].set()
                        assert self.rfile.readline() == b"\r\n"
                    else:
                        while len(body) < length:
                            body.extend(self.rfile.read(min(4096, length - len(body))))
                            upstream.upload_started[self.path].set()
                    body = bytes(body)
                else:
                    body = self.rfile.read(length) if length else b""
                auth = (
                    self.headers.get("X-Api-Key")
                    or self.headers.get("Authorization")
                    or ""
                )
                upstream.requests.append(
                    {
                        "method": self.command,
                        "path": self.path,
                        "authorization": auth,
                        "headers": dict(self.headers),
                        "body": body,
                        "accept_encoding": self.headers.get("Accept-Encoding"),
                    }
                )
                if self.path in upstream.stream_steps:
                    release, finish = upstream.stream_steps[self.path]
                    split = len(auth) // 2
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("X-Echo", auth)
                    self.end_headers()
                    prefix = b"data: " + auth[:split].encode()
                    self.wfile.write(f"{len(prefix):x}\r\n".encode() + prefix + b"\r\n")
                    self.wfile.flush()
                    if not release.wait(10):
                        self.close_connection = True
                        return
                    suffix = auth[split:].encode() + b"\n\n"
                    self.wfile.write(f"{len(suffix):x}\r\n".encode() + suffix + b"\r\n")
                    self.wfile.flush()
                    if not finish.wait(10):
                        self.close_connection = True
                        return
                    self.wfile.write(b"0\r\n\r\n")
                    self.close_connection = True
                    return
                if self.path.startswith("/invalid-headers/"):
                    raw_headers = (
                        auth.encode()
                    )  # No colon: the parser quotes this entire line.
                elif self.path.startswith("/invalid-content-length/"):
                    raw_headers = f"Content-Length: {auth}".encode()
                elif self.path.startswith("/invalid-header-name/"):
                    raw_headers = f"{auth}: x".encode()
                else:
                    raw_headers = None
                if raw_headers is not None:
                    self.connection.sendall(
                        b"HTTP/1.1 200 OK\r\n" + raw_headers + b"\r\n\r\n"
                    )
                    self.close_connection = True
                    return
                if self.path in (
                    "/no-body/204",
                    "/no-body/304",
                    "/no-body/200",
                    "/no-body/head",
                ):
                    status = (
                        200
                        if self.command == "HEAD"
                        else int(self.path.rsplit("/", 1)[1])
                    )
                    self.send_response(status)
                    self.send_header("X-Echo", auth)
                    if status != 204:
                        self.send_header(
                            "Content-Length",
                            (
                                str(BIG)
                                if status == 304 or self.command == "HEAD"
                                else "0"
                            ),
                        )
                    self.end_headers()
                    return
                if self.path == "/trailers":
                    payload = json.dumps({"auth": auth}).encode()
                    self.send_response(200)
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Trailer", "X-Echo-Trailer")
                    self.end_headers()
                    self.wfile.write(
                        f"{len(payload):x}\r\n".encode()
                        + payload
                        + f"\r\n0\r\nX-Echo-Trailer: {auth}\r\n\r\n".encode()
                    )
                    return
                if self.path in ("/large-chunked", "/gzip-large", "/tiny-chunked"):
                    compressed = self.path == "/gzip-large"
                    tiny = self.path == "/tiny-chunked"
                    payload = (
                        b"x" * 4096
                        if tiny
                        else auth.encode() + b"x" * (24 * 1024 * 1024)
                    )
                    if compressed:
                        payload = gzip.compress(payload)
                    finished = threading.Event()
                    upstream.finished[self.path] = finished
                    upstream.progress[self.path] = (0, len(payload))
                    self.send_response(200)
                    self.send_header("X-Echo", auth)
                    if compressed:
                        self.send_header("Content-Encoding", "gzip")
                        self.send_header("Content-Length", str(len(payload)))
                    else:
                        self.send_header("Transfer-Encoding", "chunked")
                    self.end_headers()
                    step = 1 if tiny else 128 if compressed else 64 * 1024
                    sent = 0
                    try:
                        for offset in range(0, len(payload), step):
                            chunk = payload[offset : offset + step]
                            wire = (
                                chunk
                                if compressed
                                else (f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                            )
                            self.wfile.write(wire)
                            self.wfile.flush()
                            sent += len(chunk)
                            upstream.progress[self.path] = (sent, len(payload))
                            time.sleep(0.001)
                        if not compressed:
                            self.wfile.write(b"0\r\n\r\n")
                    except (OSError, ssl.SSLError):
                        pass
                    finally:
                        self.close_connection = True
                        finished.set()
                    return
                headers = {
                    "Content-Type": "application/json",
                    "X-Echo": auth,
                    "ETag": '"e"',
                }
                if self.path.endswith("/git-upload-pack"):
                    payload, headers["Content-Type"] = (
                        auth.encode() + b"P" * (BIG - len(auth)),
                        "application/x-git-upload-pack-result",
                    )
                elif self.path.startswith("/large"):
                    payload = json.dumps({"auth": auth, "pad": "x" * BIG}).encode()
                elif self.path.startswith("/gzip"):
                    payload, headers["Content-Encoding"] = (
                        gzip.compress(json.dumps({"auth": auth}).encode(), mtime=0),
                        "gzip",
                    )
                elif self.path.startswith("/brotli"):
                    payload, headers["Content-Encoding"] = b"not brotli", "br"
                else:
                    payload = json.dumps({"auth": auth, "path": self.path}).encode()
                self.send_response(200)
                for name, value in headers.items():
                    self.send_header(name, value)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            do_GET = do_HEAD = do_POST = do_DELETE = _handle

        self.server = QuietServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(cert), str(key))
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class Response:
    def __init__(self, status: int, headers: dict[str, str], body: bytes) -> None:
        self.status, self.headers, self.body = status, headers, body

    def json(self) -> dict[str, object]:
        return json.loads(self.body)


def write_generation(
    runtime: Path,
    generation: int,
    listeners: list[dict[str, object]],
    secrets: dict[str, object],
    plans: dict[str, object] | None = None,
) -> None:
    secrets_name = f"secrets-{generation}.json"
    (runtime / secrets_name).write_text(json.dumps(secrets))
    entries = []
    for listener in listeners:
        plan_name = f"plan-{listener['scopeId']}-{generation}.json"
        scope_id = str(listener["scopeId"])
        if plans is not None and scope_id in plans:
            plan = plans[scope_id]
        else:
            plan = plan_for(scope_id)
            for rule in plan["rules"]:
                rule["responseRewrite"] = True
        (runtime / plan_name).write_text(json.dumps(plan))
        entries.append({**listener, "plan": plan_name})
    manifest = {
        "version": 1,
        "generation": generation,
        "secrets": secrets_name,
        "listeners": entries,
    }
    temporary = runtime / ".active.tmp"
    temporary.write_text(json.dumps(manifest))
    os.replace(temporary, runtime / "active.json")


@unittest.skipUnless(MITMDUMP and os.access(MITMDUMP, os.X_OK), "MITMDUMP is not set")
class GatewayCase(unittest.TestCase):
    workdir: ClassVar[Path]
    mock: ClassVar[MockUpstream]
    rogue: ClassVar[MockUpstream]
    misnamed: ClassVar[MockUpstream]
    process: ClassVar[subprocess.Popen[bytes]]
    ports: ClassVar[dict[str, int]]

    @classmethod
    def setUpClass(cls) -> None:
        cls.workdir = Path(tempfile.mkdtemp(prefix="asc-gateway-test-"))
        make_certificates(cls.workdir)
        cls.mock = MockUpstream(cls.workdir / "leaf.pem", cls.workdir / "leaf.key")
        cls.rogue = MockUpstream(cls.workdir / "rogue.pem", cls.workdir / "rogue.key")
        cls.misnamed = MockUpstream(
            cls.workdir / "misnamed.pem", cls.workdir / "misnamed.key"
        )
        runtime = cls.workdir / "runtime"
        (runtime / "status").mkdir(parents=True)
        cls.ports = {
            "00000000-0000-4000-8000-000000000001": free_port(),
            "00000000-0000-4000-8000-000000000002": free_port(),
            "unconfigured": free_port(),
        }
        listeners = [
            {
                "port": cls.ports["00000000-0000-4000-8000-000000000001"],
                "scopeId": "00000000-0000-4000-8000-000000000001",
            },
            {
                "port": cls.ports["00000000-0000-4000-8000-000000000002"],
                "scopeId": "00000000-0000-4000-8000-000000000002",
            },
        ]
        write_generation(runtime, 1, listeners, DECISIONS["secrets"])
        cls.process = cls.start_mitmdump()
        cls.wait_health(lambda health: health["loadedGeneration"] == 1)

    @classmethod
    def start_mitmdump(cls, *extra: str) -> subprocess.Popen[bytes]:
        routing = cls.workdir / "routing_addon.py"
        routing.write_text(ROUTING_ADDON)
        command = [MITMDUMP]
        for port in cls.ports.values():
            command += ["--mode", f"regular@127.0.0.1:{port}"]
        command += [
            "--set",
            f"confdir={cls.workdir / 'conf'}",
            "--set",
            f"ssl_verify_upstream_trusted_ca={cls.workdir / 'ca.pem'}",
            "--set",
            f"agent_sec_runtime_dir={cls.workdir / 'runtime'}",
            "--set",
            "flow_detail=0",
            "-s",
            str(ADDON),
            "-s",
            str(routing),
            *extra,
        ]
        environment = {
            **os.environ,
            "ASC_TEST_MOCK_PORT": str(cls.mock.port),
            "ASC_TEST_UNTRUSTED_PORT": str(cls.rogue.port),
            "ASC_TEST_MISNAMED_PORT": str(cls.misnamed.port),
            **(
                {"ASC_TEST_H2_PORT": str(cls.h2_port)}
                if hasattr(cls, "h2_port")
                else {}
            ),
            "TMPDIR": str(cls.workdir),
        }
        with open(cls.workdir / "mitmdump.log", "ab") as log:
            return subprocess.Popen(
                command, stdout=log, stderr=subprocess.STDOUT, env=environment
            )

    @classmethod
    def health(cls) -> dict[str, object]:
        return json.loads(
            (cls.workdir / "runtime" / "status" / "health.json").read_text()
        )

    @classmethod
    def wait_health(cls, ready: object, timeout: float = 30.0) -> dict[str, object]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if cls.process.poll() is not None:
                raise AssertionError("mitmdump exited: " + cls.log_text())
            try:
                health = cls.health()
                if ready(health):  # type: ignore[operator]
                    return health
            except (OSError, ValueError):
                pass
            time.sleep(0.1)
        raise AssertionError("gateway health did not converge: " + cls.log_text())

    @classmethod
    def log_text(cls) -> str:
        return (cls.workdir / "mitmdump.log").read_text(errors="replace")

    @classmethod
    def tearDownClass(cls) -> None:
        cls.process.terminate()
        cls.process.wait(timeout=10)
        log = cls.log_text()
        cls.mock.close()
        cls.rogue.close()
        cls.misnamed.close()
        shutil.rmtree(cls.workdir)
        for value in VALUES.values():
            assert value not in log, "mitmdump output contains a credential value"
        for request in cls.mock.requests:
            authorization = request["authorization"]
            if authorization:
                assert (
                    authorization not in log
                ), "mitmdump output contains upstream authorization"

    def client_context(self) -> ssl.SSLContext:
        return ssl.create_default_context(
            cafile=str(self.workdir / "conf" / "mitmproxy-ca-cert.pem")
        )

    def send(
        self,
        agent: str,
        method: str,
        path: str,
        *,
        host: str = "api.github.com",
        sni: str = "",
        connect: str | None = None,
        headers: list[tuple[str, str]] = (),  # type: ignore[assignment]
        body: bytes = b"",
        tls: bool = True,
    ) -> Response:
        with self.connection(
            agent, host=host, sni=sni, connect=connect, tls=tls
        ) as stream:
            target_line = path if tls else f"http://{host}{path}"
            return self._exchange(stream, method, target_line, host, headers, body)

    @contextmanager
    def connection(
        self,
        agent: str,
        *,
        host: str = "api.github.com",
        sni: str = "",
        connect: str | None = None,
        tls: bool = True,
        alpn: str = "http/1.1",
    ) -> Iterator[socket.socket]:
        sock = socket.create_connection(("127.0.0.1", self.ports[agent]), timeout=20)
        with sock:
            if tls:
                target = connect or host
                sock.sendall(
                    f"CONNECT {target}:443 HTTP/1.1\r\nHost: {target}:443\r\n\r\n".encode()
                )
                reply = b""
                while b"\r\n\r\n" not in reply:
                    chunk = sock.recv(4096)
                    if not chunk:
                        raise ConnectionError("proxy closed during CONNECT")
                    reply += chunk
                self.assertIn(b" 200 ", reply.split(b"\r\n")[0])
                server_name = host if sni == "" else sni
                context = self.client_context()
                context.set_alpn_protocols([alpn])
                stream = context.wrap_socket(sock, server_hostname=server_name)
            else:
                stream = sock
            with stream:
                yield stream

    def _exchange(
        self,
        stream: socket.socket,
        method: str,
        target_line: str,
        host: str,
        headers: list[tuple[str, str]],
        body: bytes,
    ) -> Response:
        lines = [
            f"{method} {target_line} HTTP/1.1",
            f"Host: {host}",
            "Connection: close",
        ]
        lines += [f"{name}: {value}" for name, value in headers]
        lines.append(f"Content-Length: {len(body)}")
        stream.sendall(("\r\n".join(lines) + "\r\n\r\n").encode() + body)
        response = http.client.HTTPResponse(stream)  # type: ignore[arg-type]
        response.begin()
        payload = response.read()
        response.close()
        return Response(response.status, dict(response.getheaders()), payload)

    @contextmanager
    def disabled_rewrite(self) -> Iterator[None]:
        """Publish an explicit false switch, then restore this class's true plans."""
        listeners = [
            {"scopeId": scope, "port": port}
            for scope, port in self.ports.items()
            if scope != "unconfigured"
        ]

        def publish(enabled: bool) -> None:
            generation = self.health()["loadedGeneration"] + 1
            secrets = json.loads(json.dumps(DECISIONS["secrets"]))
            secrets["generation"] = generation
            plans = {}
            for listener in listeners:
                plan = plan_for(listener["scopeId"])
                for rule in plan["rules"]:
                    rule["responseRewrite"] = enabled
                plans[listener["scopeId"]] = plan
            write_generation(
                self.workdir / "runtime", generation, listeners, secrets, plans
            )
            self.wait_health(lambda health: health["loadedGeneration"] == generation)

        try:
            publish(False)
            yield
        finally:
            publish(True)

    def recorded(self, path: str) -> list[dict[str, object]]:
        return [r for r in self.mock.requests if r["path"] == path]


class InjectionTest(GatewayCase):
    def test_ca_is_published(self) -> None:
        published = (self.workdir / "runtime" / "status" / "ca-cert.pem").read_bytes()
        self.assertEqual(
            published, (self.workdir / "conf" / "mitmproxy-ca-cert.pem").read_bytes()
        )

    def test_bearer_is_injected_and_echo_rewritten(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/echo/bearer",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(
            self.recorded("/echo/bearer")[0]["authorization"], f"Bearer {REAL}"
        )
        self.assertEqual(response.json()["auth"], f"Bearer {FAKE}")
        self.assertEqual(response.headers["X-Echo"], f"Bearer {FAKE}")
        self.assertNotIn("ETag", response.headers)
        self.assertNotIn(REAL.encode(), response.body)

    def test_bodyless_responses_preserve_connection_reuse(self) -> None:
        for method, status in (("GET", 204), ("GET", 304), ("GET", 200), ("HEAD", 200)):
            with self.subTest(method=method, status=status):
                path = f"/no-body/{'head' if method == 'HEAD' else status}"
                with self.connection("00000000-0000-4000-8000-000000000001") as stream:
                    stream.sendall(
                        f"{method} {path} HTTP/1.1\r\nHost: api.github.com\r\n"
                        f"Authorization: Bearer {FAKE}\r\n\r\n".encode()
                    )
                    headers = bytearray()
                    while not headers.endswith(b"\r\n\r\n"):
                        chunk = stream.recv(1)
                        self.assertTrue(chunk)
                        headers.extend(chunk)
                    self.assertIn(f" {status} ".encode(), headers)
                    self.assertIn(f"X-Echo: Bearer {FAKE}".encode(), headers)
                    self.assertNotIn(b"Transfer-Encoding", headers)
                    response = self._exchange(
                        stream,
                        "GET",
                        "/echo/reused",
                        "api.github.com",
                        [("Authorization", f"Bearer {FAKE}")],
                        b"",
                    )
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.json()["auth"], f"Bearer {FAKE}")
                self.assertEqual(
                    self.recorded(path)[0]["authorization"], f"Bearer {REAL}"
                )

    def test_unsupported_http1_trailers_abort_without_delivering_credentials(
        self,
    ) -> None:
        received = bytearray()
        with self.connection("00000000-0000-4000-8000-000000000001") as stream:
            stream.sendall(
                f"GET /trailers HTTP/1.1\r\nHost: api.github.com\r\n"
                f"Authorization: Bearer {FAKE}\r\nConnection: close\r\n\r\n".encode()
            )
            while chunk := stream.recv(4096):
                received.extend(chunk)
        self.assertEqual(
            self.recorded("/trailers")[0]["authorization"], f"Bearer {REAL}"
        )
        self.assertNotIn(REAL.encode(), received)

    def test_gzip_body_is_unchanged_while_headers_are_rewritten(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/gzip",
            headers=[
                ("Authorization", f"token {FAKE}"),
                ("Accept-Encoding", "gzip, br"),
            ],
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(
            json.loads(gzip.decompress(response.body))["auth"], f"token {REAL}"
        )
        self.assertEqual(
            response.body,
            gzip.compress(json.dumps({"auth": f"token {REAL}"}).encode(), mtime=0),
        )
        self.assertEqual(response.headers["X-Echo"], f"token {FAKE}")
        self.assertEqual(response.headers["Content-Encoding"], "gzip")
        self.assertEqual(int(response.headers["Content-Length"]), len(response.body))
        self.assertEqual(response.headers["ETag"], '"e"')
        self.assertEqual(self.recorded("/gzip")[0]["accept_encoding"], "gzip, br")

    def test_basic_password_is_injected_for_git(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        path = "/o/r.git/info/refs?service=git-upload-pack"
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            path,
            host="github.com",
            headers=[("Authorization", f"Basic {basic}")],
        )
        self.assertEqual(response.status, 200)
        sent = base64.b64decode(
            self.recorded(path)[0]["authorization"].split()[1]
        ).decode()
        self.assertEqual(sent, f"x-access-token:{REAL}")
        for echoed in (response.headers["X-Echo"], response.json()["auth"]):
            scheme, encoded = echoed.split()
            self.assertEqual(scheme, "Basic")
            self.assertEqual(
                base64.b64decode(encoded, validate=True).decode(),
                f"x-access-token:{FAKE}",
            )
        self.assertNotIn(REAL.encode(), response.body)

    def test_upstream_parse_errors_do_not_deliver_credentials(self) -> None:
        basic_fake = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        basic_real = base64.b64encode(f"x-access-token:{REAL}".encode()).decode()
        for failure in (
            "invalid-headers",
            "invalid-content-length",
            "invalid-header-name",
        ):
            for presented, injected in (
                (f"Bearer {FAKE}", f"Bearer {REAL}"),
                (f"Basic {basic_fake}", f"Basic {basic_real}"),
            ):
                with self.subTest(failure=failure, scheme=presented.split()[0]):
                    path = f"/{failure}/{presented.split()[0]}"
                    received = bytearray()
                    with self.connection(
                        "00000000-0000-4000-8000-000000000001"
                    ) as stream:
                        stream.sendall(
                            (
                                f"GET {path} HTTP/1.1\r\nHost: api.github.com\r\n"
                                f"Authorization: {presented}\r\nConnection: close\r\n\r\n"
                            ).encode()
                        )
                        while True:
                            try:
                                chunk = stream.recv(4096)
                            except (ConnectionResetError, ssl.SSLEOFError):
                                break
                            if not chunk:
                                break
                            received.extend(chunk)
                    self.assertEqual(
                        [r["authorization"] for r in self.recorded(path)], [injected]
                    )
                    # No dependency error page, including an encoded Basic password, may escape.
                    self.assertEqual(received, b"")

    def test_disabled_rewrite_preserves_headers_and_large_body(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        with self.disabled_rewrite():
            response = self.send(
                "00000000-0000-4000-8000-000000000001",
                "POST",
                "/o/r.git/git-upload-pack",
                host="github.com",
                headers=[("Authorization", f"Basic {basic}")],
                body=b"0000",
            )
        self.assertEqual(response.status, 200)
        self.assertEqual(len(response.body), BIG)
        injected = self.recorded("/o/r.git/git-upload-pack")[0]["authorization"]
        self.assertEqual(
            base64.b64decode(injected.split()[1]), f"x-access-token:{REAL}".encode()
        )
        self.assertEqual(response.headers["X-Echo"], injected)
        self.assertTrue(response.body.startswith(injected.encode()))
        self.assertEqual(response.headers["ETag"], '"e"')
        self.assertEqual(int(response.headers["Content-Length"]), BIG)

    def test_large_response_rewrites_without_a_total_body_limit(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/large",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(response.json(), {"auth": f"Bearer {FAKE}", "pad": "x" * BIG})
        self.assertNotIn("Content-Length", response.headers)

    def test_chunked_and_compressed_large_responses_complete(self) -> None:
        for path in ("/large-chunked", "/gzip-large"):
            with self.subTest(path=path):
                response = self.send(
                    "00000000-0000-4000-8000-000000000001",
                    "GET",
                    path,
                    headers=[("Authorization", f"Bearer {FAKE}")],
                )
                self.assertEqual(response.status, 200)
                compressed = path == "/gzip-large"
                body = gzip.decompress(response.body) if compressed else response.body
                expected = f"Bearer {REAL if compressed else FAKE}".encode()
                self.assertEqual(body, expected + b"x" * (24 * 1024 * 1024))
                self.assertEqual(response.headers["X-Echo"], f"Bearer {FAKE}")
                self.assertTrue(self.mock.finished[path].wait(10))
                sent, total = self.mock.progress[path]
                self.assertEqual(sent, total)
                self.assertIsNone(
                    self.process.poll(), "the shared proxy must remain usable"
                )

    def test_other_content_encoding_passes_through(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/brotli",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(response.body, b"not brotli")
        self.assertEqual(response.headers["Content-Encoding"], "br")
        self.assertEqual(response.headers["X-Echo"], f"Bearer {FAKE}")
        self.assertEqual(response.headers["ETag"], '"e"')

    def test_split_key_is_withheld_and_sse_event_arrives_before_eof(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        for authorization in (f"Bearer {FAKE}", f"Basic {basic}"):
            with self.subTest(scheme=authorization.split()[0]):
                path = f"/echo/stream-{authorization.split()[0]}"
                release, finish = threading.Event(), threading.Event()
                self.mock.stream_steps[path] = (release, finish)
                try:
                    with self.connection(
                        "00000000-0000-4000-8000-000000000001"
                    ) as stream:
                        stream.settimeout(3)
                        stream.sendall(
                            f"GET {path} HTTP/1.1\r\nHost: api.github.com\r\n"
                            f"Authorization: {authorization}\r\nConnection: close\r\n\r\n".encode()
                        )
                        response = http.client.HTTPResponse(stream)
                        response.begin()
                        self.assertEqual(response.status, 200)
                        self.assertEqual(response.read1(4096), b"data: ")
                        release.set()
                        self.assertEqual(
                            response.read1(4096), authorization.encode() + b"\n\n"
                        )
                        finish.set()
                        self.assertEqual(response.read(), b"")
                        response.close()
                    injected = self.recorded(path)[0]["authorization"]
                    self.assertNotEqual(injected, authorization)
                finally:
                    release.set()
                    finish.set()

    def test_upload_streams_before_eof_and_preserves_body_bytes(self) -> None:
        for label, authorization, encoding, chunked in [
            ("plain", "", "", False),
            ("managed", f"Bearer {FAKE}", "", True),
            ("encoded", f"Bearer {FAKE}", "gzip", False),
        ]:
            with self.subTest(label=label):
                path = f"/echo/upload-{label}"
                body = REAL.encode() + b"x" * (10 * 1024 * 1024)
                if encoding:
                    body = gzip.compress(body)
                started = threading.Event()
                self.mock.upload_started[path] = started
                with self.connection("00000000-0000-4000-8000-000000000001") as stream:
                    lines = [
                        f"POST {path} HTTP/1.1",
                        "Host: api.github.com",
                        "Connection: close",
                    ]
                    if authorization:
                        lines.append(f"Authorization: {authorization}")
                    if encoding:
                        lines.append(f"Content-Encoding: {encoding}")
                    lines.append(
                        "Transfer-Encoding: chunked"
                        if chunked
                        else f"Content-Length: {len(body)}"
                    )
                    stream.sendall(("\r\n".join(lines) + "\r\n\r\n").encode())
                    split = min(65536, len(body) // 2)
                    prefix = body[:split]
                    stream.sendall(
                        f"{len(prefix):x}\r\n".encode() + prefix + b"\r\n"
                        if chunked
                        else prefix
                    )
                    self.assertTrue(
                        started.wait(3), "upstream must receive bytes before upload EOF"
                    )
                    tail = body[split:]
                    stream.sendall(
                        f"{len(tail):x}\r\n".encode() + tail + b"\r\n0\r\n\r\n"
                        if chunked
                        else tail
                    )
                    response = http.client.HTTPResponse(stream)
                    response.begin()
                    payload = response.read()
                    self.assertEqual(response.status, 200)
                    if authorization:
                        self.assertNotIn(REAL.encode(), payload)
                        self.assertIn(FAKE.encode(), payload)
                [record] = self.recorded(path)
                self.assertEqual(record["body"], body)
                self.assertEqual(
                    record["authorization"], f"Bearer {REAL}" if authorization else ""
                )

    def test_early_response_finishes_without_upload_eof_and_still_rewrites(
        self,
    ) -> None:
        with self.connection("00000000-0000-4000-8000-000000000001") as stream:
            stream.sendall(
                f"POST /echo/early-upload HTTP/1.1\r\nHost: api.github.com\r\nAuthorization: Bearer {FAKE}\r\nContent-Length: 10485760\r\n\r\n".encode()
            )
            stream.settimeout(3)
            response = http.client.HTTPResponse(stream)
            response.begin()
            body = response.read()
            self.assertEqual(response.status, 413)
            self.assertEqual(json.loads(body)["auth"], f"Bearer {FAKE}")
            self.assertNotIn(REAL.encode(), body)
        self.assertEqual(
            self.recorded("/echo/early-upload")[0]["authorization"], f"Bearer {REAL}"
        )

    def test_request_without_managed_credential_passes_through(self) -> None:
        for host, authorization in [
            ("api.github.com", ""),
            ("github.com", ""),
            ("github.com", "Bearer unmanaged-token"),
        ]:
            with self.subTest(host=host, authorization=authorization):
                path = f"/echo/plain-{host}-{bool(authorization)}"
                headers = [("Authorization", authorization)] if authorization else []
                response = self.send(
                    "00000000-0000-4000-8000-000000000001",
                    "GET",
                    path,
                    host=host,
                    headers=headers,
                )
                self.assertEqual(response.status, 200)
                self.assertEqual(self.recorded(path)[0]["authorization"], authorization)


class CustomHeaderInjectionTest(GatewayCase):
    @classmethod
    def setUpClass(cls) -> None:
        super().setUpClass()
        secrets = json.loads(json.dumps(DECISIONS["secrets"]))
        secrets["generation"] = 2
        listeners = [
            {"scopeId": scope, "port": port}
            for scope, port in cls.ports.items()
            if scope != "unconfigured"
        ]
        plans = {}
        for listener in listeners:
            plan = plan_for(listener["scopeId"])
            for rule in plan["rules"]:
                rule.update(header="X-Api-Key", responseRewrite=True)
            plans[listener["scopeId"]] = plan
        write_generation(cls.workdir / "runtime", 2, listeners, secrets, plans)
        cls.wait_health(lambda health: health["loadedGeneration"] == 2)

    def test_custom_auto_and_basic_echoes_are_rewritten(self) -> None:
        for label, presented, injected in (
            ("auto", f"Vendor prefix={FAKE}; suffix", f"Vendor prefix={REAL}; suffix"),
            (
                "basic",
                "Basic " + base64.b64encode(f"user:{FAKE}".encode()).decode(),
                "Basic " + base64.b64encode(f"user:{REAL}".encode()).decode(),
            ),
        ):
            with self.subTest(label=label):
                path = f"/echo/custom-{label}"
                response = self.send(
                    "00000000-0000-4000-8000-000000000001",
                    "GET",
                    path,
                    headers=[("x-api-key", presented)],
                )
                self.assertEqual(response.status, 200)
                [record] = self.recorded(path)
                self.assertEqual(record["headers"]["X-Api-Key"], injected)
                self.assertNotIn("Authorization", record["headers"])
                self.assertEqual(response.headers["X-Echo"], presented)
                self.assertEqual(response.json()["auth"], presented)
                self.assertNotIn(injected.encode(), response.body)

    def test_custom_enabled_rewrite_redacts_headers_and_large_identity_body(
        self,
    ) -> None:
        path = "/o/r.git/git-upload-pack"
        encoded = base64.b64encode(f"user:{FAKE}".encode()).decode()
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "POST",
            path,
            host="github.com",
            headers=[("X-Api-Key", f"Basic {encoded}")],
            body=b"0000",
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(len(response.body), BIG)
        self.assertEqual(response.headers["X-Echo"], f"Basic {encoded}")
        self.assertEqual(
            response.body,
            f"Basic {encoded}".encode() + b"P" * (BIG - len(f"Basic {encoded}")),
        )
        self.assertNotIn("Content-Length", response.headers)
        self.assertNotIn("ETag", response.headers)
        self.assertEqual(
            base64.b64decode(
                self.recorded(path)[0]["headers"]["X-Api-Key"].split()[1]
            ).decode(),
            f"user:{REAL}",
        )

    def test_custom_origin_errors_close_without_dependency_error_pages(self) -> None:
        for format_word, presented in (
            ("auto", f"Vendor {FAKE}"),
            ("basic", "Basic " + base64.b64encode(f"user:{FAKE}".encode()).decode()),
        ):
            with self.subTest(format_word=format_word):
                path = f"/invalid-header-name/custom-{format_word}"
                received = bytearray()
                with self.connection("00000000-0000-4000-8000-000000000001") as stream:
                    stream.sendall(
                        (
                            f"GET {path} HTTP/1.1\r\nHost: api.github.com\r\n"
                            f"X-Api-Key: {presented}\r\nConnection: close\r\n\r\n"
                        ).encode()
                    )
                    while True:
                        try:
                            chunk = stream.recv(4096)
                        except (ConnectionResetError, ssl.SSLEOFError):
                            break
                        if not chunk:
                            break
                        received.extend(chunk)
                self.assertEqual(len(self.recorded(path)), 1)
                self.assertEqual(received, b"")


class RejectionTest(GatewayCase):
    def assert_rejected(
        self, response: Response, code: str, path: str, status: int = 403
    ) -> None:
        self.assertEqual(response.status, status)
        self.assertEqual(response.json()["error"], code)
        self.assertEqual(self.recorded(path), [], "upstream must not be called")

    def test_disallowed_method_and_path(self) -> None:
        auth = [("Authorization", f"Bearer {FAKE}")]
        self.assert_rejected(
            self.send(
                "00000000-0000-4000-8000-000000000001", "OPTIONS", "/x1", headers=auth
            ),
            "credential_not_allowed",
            "/x1",
        )
        self.assert_rejected(
            self.send(
                "00000000-0000-4000-8000-000000000001",
                "GET",
                "/x2",
                host="github.com",
                headers=auth,
            ),
            "credential_not_allowed",
            "/x2",
        )

    def test_rejected_upload_closes_before_receiving_body(self) -> None:
        with self.connection("00000000-0000-4000-8000-000000000001") as stream:
            stream.sendall(
                f"POST /x-upload HTTP/1.1\r\nHost: api.github.com\r\nAuthorization: Bearer {VALUES['retiredFake']}\r\nContent-Length: 10485760\r\n\r\n".encode()
            )
            stream.settimeout(3)
            try:
                self.assertEqual(stream.recv(4096), b"")
            except (ConnectionResetError, ssl.SSLEOFError):
                pass
        self.assertEqual(self.recorded("/x-upload"), [])

    def test_other_domain_fake(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000002",
            "GET",
            "/x3",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assert_rejected(response, "subject_mismatch", "/x3")

    def test_real_header_is_never_forwarded(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "POST",
            "/x4",
            headers=[("Authorization", f"Bearer {REAL}")],
        )
        self.assert_rejected(response, "real_credential_exposed", "/x4")

    def test_plaintext_fake(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/x5",
            tls=False,
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assert_rejected(response, "plaintext_credential", "/x5")

    def test_sni_host_mismatch(self) -> None:
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/x6",
            host="api.github.com",
            sni="github.com",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assert_rejected(response, "sni_host_mismatch", "/x6", 421)

    def test_unconfigured_listener_fails_closed(self) -> None:
        self.assert_rejected(
            self.send("unconfigured", "GET", "/x7"), "gateway_not_ready", "/x7"
        )

    def test_forged_destination_fails_upstream_verification(self) -> None:
        for destination, upstream in (
            (ATTACKER_IP, self.rogue),
            (MISNAMED_IP, self.misnamed),
        ):
            with self.subTest(destination=destination):
                try:
                    response = self.send(
                        "00000000-0000-4000-8000-000000000001",
                        "GET",
                        "/x8",
                        connect=destination,
                        sni="api.github.com",
                        headers=[("Authorization", f"Bearer {FAKE}")],
                    )
                except (OSError, http.client.HTTPException):
                    response = None
                if response is not None:
                    self.assertNotEqual(response.status, 200)
                self.assertEqual(upstream.requests, [])


class Http2InjectionTest(GatewayCase):
    @classmethod
    def start_mitmdump(cls, *extra: str) -> subprocess.Popen[bytes]:
        # Negotiate client HTTP/2 independently of the HTTP/1-only mock upstream.
        return super().start_mitmdump("--set", "connection_strategy=lazy", *extra)

    def test_http2_streamed_body_is_rewritten(self) -> None:
        for path, upload in [
            ("/echo/h2-streamed", False),
            ("/echo/early-upload", True),
        ]:
            with self.subTest(path=path):
                self._streamed_response(path, upload)

    def _streamed_response(self, path: str, upload: bool) -> None:
        block = bytearray()
        headers = [
            (":method", "POST" if upload else "GET"),
            (":scheme", "https"),
            (":authority", "api.github.com"),
            (":path", path),
            ("authorization", f"Bearer {FAKE}"),
        ]
        if upload:
            headers.append(("content-length", "10485760"))
        for name, value in headers:
            key, raw = name.encode(), value.encode()
            block.extend(bytes((0, len(key))) + key + bytes((len(raw),)) + raw)
        received = bytearray()
        body = bytearray()
        ended = False
        with self.connection(
            "00000000-0000-4000-8000-000000000001", alpn="h2"
        ) as stream:
            self.assertEqual(stream.selected_alpn_protocol(), "h2")
            stream.sendall(
                b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
                + b"\x00\x00\x00\x04\x00\x00\x00\x00\x00"
                + len(block).to_bytes(3, "big")
                + bytes((1, 4 if upload else 5))
                + b"\x00\x00\x00\x01"
                + block
            )
            stream.settimeout(3)
            while not ended:
                chunk = stream.recv(4096)
                self.assertTrue(chunk, "HTTP/2 response ended before END_STREAM")
                received.extend(chunk)
                while len(received) >= 9:
                    length = int.from_bytes(received[:3], "big")
                    if len(received) < 9 + length:
                        break
                    kind, flags = received[3], received[4]
                    if kind == 0:
                        body.extend(received[9 : 9 + length])
                        if length:
                            increment = length.to_bytes(4, "big")
                            stream.sendall(
                                b"\x00\x00\x04\x08\x00\x00\x00\x00\x00"
                                + increment
                                + b"\x00\x00\x04\x08\x00\x00\x00\x00\x01"
                                + increment
                            )
                    if kind in (0, 1) and flags & 1:
                        ended = True
                    del received[: 9 + length]
        self.assertEqual(json.loads(body)["auth"], f"Bearer {FAKE}")
        self.assertNotIn(REAL.encode(), body)
        self.assertEqual(self.recorded(path)[0]["authorization"], f"Bearer {REAL}")

    def test_stalled_http2_window_aborts_before_buffering_entire_response(self) -> None:
        # Gateway budgets: 8 MiB queued bytes, 1024 queued DATA items; plus socket slack.
        for path, bound in (
            ("/large-chunked", 16 * 1024 * 1024),
            ("/tiny-chunked", 2048),
        ):
            with self.subTest(path=path):
                self._stalled_http2_response(path, bound)

    def _stalled_http2_response(self, path: str, bound: int) -> None:
        block = bytearray()
        for name, value in (
            (":method", "GET"),
            (":scheme", "https"),
            (":authority", "api.github.com"),
            (":path", path),
            ("authorization", f"Bearer {FAKE}"),
        ):
            key, raw = name.encode(), value.encode()
            block.extend(bytes((0, len(key))) + key + bytes((len(raw),)) + raw)
        received = bytearray()
        with self.connection(
            "00000000-0000-4000-8000-000000000001", alpn="h2"
        ) as stream:
            stream.settimeout(5)
            stream.sendall(
                b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
                + b"\x00\x00\x06\x04\x00\x00\x00\x00\x00"
                + b"\x00\x04\x00\x00\x00\x00"
                + len(block).to_bytes(3, "big")
                + b"\x01\x05\x00\x00\x00\x01"
                + block
            )
            while True:
                try:
                    chunk = stream.recv(65536)
                except (ConnectionResetError, ssl.SSLEOFError):
                    break
                if not chunk:
                    break
                received.extend(chunk)
        self.assertNotIn(REAL.encode(), received)
        self.assertTrue(self.mock.finished[path].wait(10))
        sent, _ = self.mock.progress[path]
        self.assertLessEqual(
            sent, bound, "a stalled client must stop near the queue budget"
        )
        self.assertIsNone(self.process.poll(), "the shared gateway must survive")

    def test_http2_rejected_upload_without_length_closes_before_data(self) -> None:
        block = bytearray()
        for name, value in (
            (":method", "POST"),
            (":scheme", "https"),
            (":authority", "api.github.com"),
            (":path", "/h2-rejected-upload"),
            ("authorization", f"Bearer {VALUES['retiredFake']}"),
        ):
            key, raw = name.encode(), value.encode()
            block.extend(bytes((0, len(key))) + key + bytes((len(raw),)) + raw)
        with self.connection(
            "00000000-0000-4000-8000-000000000001", alpn="h2"
        ) as stream:
            stream.sendall(
                b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
                + b"\x00\x00\x00\x04\x00\x00\x00\x00\x00"
                + len(block).to_bytes(3, "big")
                + b"\x01\x04\x00\x00\x00\x01"
                + block
            )
            stream.settimeout(3)
            while True:
                try:
                    if not stream.recv(4096):
                        break
                except (ConnectionResetError, ssl.SSLEOFError):
                    break
        self.assertEqual(self.recorded("/h2-rejected-upload"), [])

    def test_http2_validation_errors_do_not_deliver_credentials(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        for presented in (f"Bearer {FAKE}", f"Basic {basic}"):
            with self.subTest(scheme=presented.split()[0]):
                path = f"/invalid-header-name/h2-{presented.split()[0]}"
                block = bytearray()
                for name, value in (
                    (":method", "GET"),
                    (":scheme", "https"),
                    (":authority", "api.github.com"),
                    (":path", path),
                    ("authorization", presented),
                ):
                    key, raw = name.encode(), value.encode()
                    self.assertLess(len(raw), 127)
                    # HPACK literal fields without indexing or Huffman coding.
                    block.extend(bytes((0, len(key))) + key + bytes((len(raw),)) + raw)
                received = bytearray()
                with self.connection(
                    "00000000-0000-4000-8000-000000000001", alpn="h2"
                ) as stream:
                    self.assertEqual(stream.selected_alpn_protocol(), "h2")
                    stream.sendall(
                        b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
                        + b"\x00\x00\x00\x04\x00\x00\x00\x00\x00"
                        + len(block).to_bytes(3, "big")
                        + b"\x01\x05\x00\x00\x00\x01"
                        + block
                    )
                    while True:
                        try:
                            chunk = stream.recv(4096)
                        except (ConnectionResetError, ssl.SSLEOFError):
                            break
                        if not chunk:
                            break
                        received.extend(chunk)
                sent = self.recorded(path)
                self.assertEqual(len(sent), 1)
                injected = sent[0]["authorization"]
                if presented.startswith("Basic "):
                    self.assertEqual(
                        base64.b64decode(injected.split()[1]).decode(),
                        f"x-access-token:{REAL}",
                    )
                else:
                    self.assertEqual(injected, f"Bearer {REAL}")
                self.assertNotIn(injected.encode(), received)
                while received:
                    self.assertGreaterEqual(len(received), 9)
                    length = int.from_bytes(received[:3], "big")
                    self.assertGreaterEqual(len(received), length + 9)
                    # SETTINGS are harmless; no response HEADERS or DATA may be sent.
                    self.assertNotIn(received[3], (0, 1))
                    del received[: length + 9]


class Http2UpstreamInjectionTest(GatewayCase):
    h2_port: ClassVar[int]

    @classmethod
    def start_mitmdump(cls, *extra: str) -> subprocess.Popen[bytes]:
        cls.h2_port = free_port()
        while cls.h2_port in cls.ports.values():
            cls.h2_port = free_port()
        # Keep downstream HTTP/1 negotiation independent of the HTTP/2-only origin.
        return super().start_mitmdump("--set", "connection_strategy=lazy", *extra)

    @contextmanager
    def h2_upstream(self) -> Iterator[None]:
        script = self.workdir / "h2_upstream_addon.py"
        script.write_text(H2_UPSTREAM_ADDON)
        proxy_port = free_port()
        while proxy_port in (*self.ports.values(), self.h2_port):
            proxy_port = free_port()
        process = subprocess.Popen(
            [
                MITMDUMP,
                "--mode",
                f"regular@127.0.0.1:{proxy_port}",
                "--set",
                f"confdir={self.workdir / 'conf'}",
                "--set",
                "flow_detail=0",
                "-s",
                str(script),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            cwd=self.workdir,
            env={
                **os.environ,
                "ASC_TEST_H2_PORT": str(self.h2_port),
                "ASC_TEST_H2_WORKDIR": str(self.workdir),
                "TMPDIR": str(self.workdir),
            },
        )
        try:
            deadline = time.monotonic() + 30
            while True:
                if process.poll() is not None:
                    output, _ = process.communicate()
                    self.fail("HTTP/2 upstream mitmdump exited: " + output.decode())
                try:
                    with socket.create_connection(
                        ("127.0.0.1", self.h2_port), timeout=0.2
                    ):
                        break
                except OSError:
                    if time.monotonic() >= deadline:
                        self.fail("HTTP/2 upstream did not become ready")
                    time.sleep(0.1)
            yield
        finally:
            process.terminate()
            try:
                process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate(timeout=10)

    def _h2_response(
        self, host: str, path: str, authorization: str, *, method: str = "GET"
    ) -> dict[str, object]:
        script = self.workdir / "h2_client_addon.py"
        script.write_text(H2_CLIENT_ADDON)
        result_path = self.workdir / "h2-client-result.json"
        result_path.unlink(missing_ok=True)
        client_port = free_port()
        while client_port in (*self.ports.values(), self.h2_port):
            client_port = free_port()
        result = subprocess.run(
            [
                MITMDUMP,
                "--mode",
                f"regular@127.0.0.1:{client_port}",
                "--set",
                f"confdir={self.workdir / 'conf'}",
                "--set",
                "flow_detail=0",
                "-s",
                str(script),
            ],
            check=False,
            timeout=40,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            cwd=self.workdir,
            env={
                **os.environ,
                "ASC_TEST_H2_REQUEST": json.dumps(
                    {
                        "port": self.ports["00000000-0000-4000-8000-000000000001"],
                        "connect": H2_IP,
                        "host": host,
                        "path": path,
                        "method": method,
                        "authorization": authorization,
                        "ca": str(self.workdir / "conf" / "mitmproxy-ca-cert.pem"),
                    }
                ),
                "ASC_TEST_H2_RESULT": str(result_path),
                "TMPDIR": str(self.workdir),
            },
        )
        self.assertEqual(result.returncode, 0, "HTTP/2 client mitmdump failed")
        response = json.loads(result_path.read_text())
        self.assertNotIn(
            "error", response, "HTTP/2 wire client failed: " + str(response)
        )
        self.assertEqual(response["alpn"], "h2")
        self.assertEqual(response["headers"][":status"], "200")
        self.assertFalse((self.workdir / "h2-error.json").exists())
        proof = json.loads(
            (self.workdir / f"h2-proof-{path.rsplit('/', 1)[1]}.json").read_text()
        )
        self.assertEqual(proof["alpn"], "h2")
        self.assertEqual((proof["path"], proof["method"]), (path, method))
        self.assertIsNone(self.process.poll(), "the gateway must remain usable")
        return response

    def test_http2_trailers_are_rewritten_with_headers_and_identity_body(self) -> None:
        with self.h2_upstream():
            response = self._h2_response(
                "api.github.com", "/echo/h2-trailers/identity", f"Bearer {FAKE}"
            )
            proof = json.loads((self.workdir / "h2-proof-identity.json").read_text())
            self.assertEqual(proof["authorization"], f"Bearer {REAL}")
            self.assertEqual(response["headers"]["x-echo"], f"Bearer {FAKE}")
            self.assertEqual(response["trailers"], {"x-echo-trailer": f"Bearer {FAKE}"})
            body = base64.b64decode(response["body"], validate=True)
            self.assertEqual(json.loads(body), {"auth": f"Bearer {FAKE}"})
            self.assertNotIn(REAL.encode(), body)

    def test_http2_disabled_rewrite_preserves_headers_body_and_trailers(self) -> None:
        presented = (
            "Basic " + base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        )
        injected = (
            "Basic " + base64.b64encode(f"x-access-token:{REAL}".encode()).decode()
        )
        with self.h2_upstream(), self.disabled_rewrite():
            response = self._h2_response(
                "github.com", "/o/r.git/git-upload-pack", presented, method="POST"
            )
            proof = json.loads(
                (self.workdir / "h2-proof-git-upload-pack.json").read_text()
            )
            self.assertEqual(proof["authorization"], injected)
            self.assertEqual(response["headers"]["x-echo"], injected)
            self.assertEqual(response["trailers"], {"x-echo-trailer": injected})
            body = base64.b64decode(response["body"], validate=True)
            self.assertEqual(body, json.dumps({"auth": injected}).encode())
            self.assertEqual(int(response["headers"]["content-length"]), len(body))

    def test_http2_gzip_preserves_body_but_rewrites_headers_and_trailers(self) -> None:
        with self.h2_upstream():
            response = self._h2_response(
                "api.github.com", "/echo/h2-trailers/gzip", f"Bearer {FAKE}"
            )
            proof = json.loads((self.workdir / "h2-proof-gzip.json").read_text())
            self.assertEqual(proof["authorization"], f"Bearer {REAL}")
            self.assertEqual(response["headers"]["x-echo"], f"Bearer {FAKE}")
            self.assertEqual(response["trailers"], {"x-echo-trailer": f"Bearer {FAKE}"})
            self.assertEqual(response["headers"]["content-encoding"], "gzip")
            body = base64.b64decode(response["body"], validate=True)
            self.assertEqual(body, base64.b64decode(proof["body"], validate=True))
            self.assertEqual(
                json.loads(gzip.decompress(body)), {"auth": f"Bearer {REAL}"}
            )
            self.assertEqual(int(response["headers"]["content-length"]), len(body))

    def test_empty_nonterminal_http2_data_preserves_rewrite_and_encoded_passthrough(
        self,
    ) -> None:
        with self.h2_upstream():
            for encoding in ("identity", "gzip"):
                with self.subTest(encoding=encoding):
                    response = self.send(
                        "00000000-0000-4000-8000-000000000001",
                        "GET",
                        f"/echo/h2-empty/{encoding}",
                        connect=H2_IP,
                        sni="api.github.com",
                        headers=[("Authorization", f"Bearer {FAKE}")],
                    )
                    proof = json.loads(
                        (self.workdir / f"h2-proof-{encoding}.json").read_text()
                    )
                    self.assertEqual(
                        proof, {"authorization": f"Bearer {REAL}", "alpn": "h2"}
                    )
                    self.assertFalse(
                        (self.workdir / "h2-error.json").exists(),
                        "HTTP/2 upstream failed",
                    )
                    self.assertEqual(response.status, 200)
                    body = response.body
                    if encoding == "gzip":
                        self.assertEqual(response.headers["content-encoding"], "gzip")
                        body = gzip.decompress(body)
                    expected = REAL if encoding == "gzip" else FAKE
                    self.assertEqual(json.loads(body), {"auth": f"Bearer {expected}"})
                    if encoding == "identity":
                        self.assertNotIn(REAL.encode(), body)
                    self.assertIsNone(
                        self.process.poll(), "the gateway must remain usable"
                    )


class LogProtectionTest(GatewayCase):
    def test_partial_client_headers_never_reach_logs(self) -> None:
        dummy_fake = "dummyfake-unmanaged-for-log-regression"
        dummy_real = "basicdummyreal-for-log-regression"
        encoded = base64.b64encode(f"synthetic-user:{dummy_real}".encode()).decode()
        retired = VALUES["retiredFake"]
        partial = FAKE[:16]
        with self.connection(
            "00000000-0000-4000-8000-000000000001", tls=False
        ) as stream:
            stream.sendall(
                (
                    "GET http://api.github.com/partial-headers HTTP/1.1\r\n"
                    "Host: api.github.com\r\n"
                    f"Authorization: Bearer {dummy_fake}\r\nX-Basic: Basic {encoded}\r\n"
                    f"X-Retired: {retired}\r\nX-Unfinished: {partial}"
                ).encode()
            )
            stream.shutdown(socket.SHUT_WR)
            while stream.recv(4096):
                pass
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/log-marker",
            tls=False,
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "plaintext_credential")
        )
        # Flush redirected stdout before asserting absence; this class owns its subprocess.
        self.process.terminate()
        self.process.wait(timeout=10)
        log = self.log_text()
        for value in (dummy_fake, dummy_real, encoded, retired, partial):
            self.assertNotIn(value, log)
        self.assertIn("agent-sec-gateway rejected request", log)


class ReloadTest(GatewayCase):
    def test_generation_switch_and_failed_load(self) -> None:
        runtime = self.workdir / "runtime"
        secrets = json.loads(json.dumps(DECISIONS["secrets"]))
        secrets["generation"] = 2
        secrets["retired"].append(
            {
                "fake": FAKE,
                "credential": "github",
            }
        )
        secrets["entries"] = [
            e
            for e in secrets["entries"]
            if "00000000-0000-4000-8000-000000000001" not in e["scopeIds"]
        ]
        write_generation(
            runtime,
            2,
            [
                {
                    "port": self.ports["00000000-0000-4000-8000-000000000002"],
                    "scopeId": "00000000-0000-4000-8000-000000000002",
                }
            ],
            secrets,
        )
        self.wait_health(lambda health: health["loadedGeneration"] == 2)
        response = self.send(
            "00000000-0000-4000-8000-000000000001",
            "GET",
            "/r1",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "gateway_not_ready")
        )
        response = self.send(
            "00000000-0000-4000-8000-000000000002",
            "GET",
            "/r2",
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "retired_credential")
        )

        broken = json.loads(json.dumps(secrets))
        write_generation(
            runtime,
            3,
            [
                {
                    "port": self.ports["00000000-0000-4000-8000-000000000002"],
                    "scopeId": "00000000-0000-4000-8000-000000000002",
                }
            ],
            broken,
        )
        health = self.wait_health(
            lambda health: health["lastAttempt"]["generation"] == 3
        )
        self.assertEqual(health["loadedGeneration"], 2)
        self.assertIn("generation", health["lastAttempt"]["error"])
        response = self.send(
            "00000000-0000-4000-8000-000000000002",
            "GET",
            "/r3",
            headers=[("Authorization", f"Bearer {OTHER_FAKE}")],
        )
        self.assertEqual(response.status, 200)

        (runtime / "active.json").unlink()
        self.wait_health(lambda health: health["loadedGeneration"] is None)
        response = self.send("00000000-0000-4000-8000-000000000002", "GET", "/r4")
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "gateway_not_ready")
        )


@unittest.skipUnless(MITMDUMP and os.access(MITMDUMP, os.X_OK), "MITMDUMP is not set")
class StartupTest(unittest.TestCase):
    def test_unsafe_options_are_refused(self) -> None:
        unsafe = [
            ("ssl_insecure=true", b"ssl_insecure=false"),
            ("stream_large_bodies=1m", b"stream_large_bodies"),
            ("store_streamed_bodies=true", b"store_streamed_bodies"),
            ("save_stream_file=flows.out", b"save_stream_file"),
            ("flow_detail=2", b"flow_detail=0"),
        ]
        for setting, message in unsafe:
            with (
                self.subTest(setting=setting),
                tempfile.TemporaryDirectory() as directory,
            ):
                # mitmdump refuses repeated options, so only add the safe default when needed.
                options = [setting]
                if not setting.startswith("flow_detail="):
                    options.insert(0, "flow_detail=0")
                command = [MITMDUMP, "--mode", f"regular@127.0.0.1:{free_port()}"]
                for option in [*options, f"confdir={directory}"]:
                    command += ["--set", option]
                command += [
                    "--set",
                    f"agent_sec_runtime_dir={directory}",
                    "-s",
                    str(ADDON),
                ]
                result = subprocess.run(
                    command,
                    capture_output=True,
                    check=False,
                    cwd=directory,
                    timeout=60,
                    env={**os.environ, "TMPDIR": directory},
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
