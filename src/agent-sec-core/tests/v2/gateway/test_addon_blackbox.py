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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import ClassVar

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_policy import DECISIONS, ROOT, VALUES, plan_for  # noqa: E402

MITMDUMP = os.environ.get("MITMDUMP", "")
ADDON = ROOT / "v2" / "gateway" / "addon" / "asc_gateway_addon.py"
REAL, FAKE, OTHER_FAKE = VALUES["real"], VALUES["fake"], VALUES["otherFake"]
ATTACKER_IP = "203.0.113.7"
MISNAMED_IP = "203.0.113.8"
BIG = 9 * 1024 * 1024

ROUTING_ADDON = f"""
import os
PORTS = {{"{ATTACKER_IP}": "ASC_TEST_UNTRUSTED_PORT", "{MISNAMED_IP}": "ASC_TEST_MISNAMED_PORT"}}
def server_connect(data):
    variable = PORTS.get(data.server.address[0], "ASC_TEST_MOCK_PORT")
    data.server.address = ("127.0.0.1", int(os.environ[variable]))
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
        upstream = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args: object) -> None:
                pass

            def _handle(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                auth = self.headers.get("Authorization") or ""
                upstream.requests.append(
                    {
                        "method": self.command,
                        "path": self.path,
                        "authorization": auth,
                        "body": body,
                    }
                )
                headers = {
                    "Content-Type": "application/json",
                    "X-Echo": auth,
                    "ETag": '"e"',
                }
                if self.path.endswith("/git-upload-pack"):
                    payload, headers["Content-Type"] = (
                        b"P" * BIG,
                        "application/x-git-upload-pack-result",
                    )
                elif self.path.startswith("/large"):
                    payload = json.dumps({"auth": auth, "pad": "x" * BIG}).encode()
                elif self.path.startswith("/gzip"):
                    payload, headers["Content-Encoding"] = (
                        gzip.compress(json.dumps({"auth": auth}).encode()),
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

            do_GET = do_POST = do_DELETE = _handle

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
) -> None:
    secrets_name = f"secrets-{generation}.json"
    (runtime / secrets_name).write_text(json.dumps(secrets))
    entries = []
    for listener in listeners:
        plan_name = f"plan-{listener['agent']}-{generation}.json"
        (runtime / plan_name).write_text(json.dumps(plan_for(str(listener["agent"]))))
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
            "dev-agent": free_port(),
            "other-agent": free_port(),
            "unconfigured": free_port(),
        }
        listeners = [
            {"port": cls.ports["dev-agent"], "agent": "dev-agent"},
            {"port": cls.ports["other-agent"], "agent": "other-agent"},
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
        for value in (REAL, FAKE, OTHER_FAKE):
            assert value not in log, "mitmdump output contains a credential value"

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
                stream = self.client_context().wrap_socket(
                    sock, server_hostname=server_name
                )
                target_line = path
            else:
                stream = sock
                target_line = f"http://{host}{path}"
            with stream:
                return self._exchange(stream, method, target_line, host, headers, body)

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
            "dev-agent",
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

    def test_gzip_body_is_rewritten(self) -> None:
        response = self.send(
            "dev-agent", "GET", "/gzip", headers=[("Authorization", f"token {FAKE}")]
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(
            json.loads(gzip.decompress(response.body))["auth"], f"token {FAKE}"
        )

    def test_basic_password_is_injected_for_git(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        path = "/o/r.git/info/refs?service=git-upload-pack"
        response = self.send(
            "dev-agent",
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
        self.assertNotIn(REAL.encode(), response.body)

    def test_upload_pack_streams_beyond_the_buffer_limit(self) -> None:
        basic = base64.b64encode(f"x-access-token:{FAKE}".encode()).decode()
        response = self.send(
            "dev-agent",
            "POST",
            "/o/r.git/git-upload-pack",
            host="github.com",
            headers=[("Authorization", f"Basic {basic}")],
            body=b"0000",
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(len(response.body), BIG)

    def test_oversized_buffered_response_is_not_delivered(self) -> None:
        try:
            response = self.send(
                "dev-agent",
                "GET",
                "/large",
                headers=[("Authorization", f"Bearer {FAKE}")],
            )
        except (OSError, http.client.HTTPException):
            return  # the killed flow closes the connection
        self.assertNotEqual(response.status, 200)
        self.assertNotIn(REAL.encode(), response.body)

    def test_unsupported_encoding_is_rejected(self) -> None:
        response = self.send(
            "dev-agent", "GET", "/brotli", headers=[("Authorization", f"Bearer {FAKE}")]
        )
        self.assertEqual(response.status, 502)
        self.assertEqual(response.json()["error"], "response_encoding_unsupported")

    def test_request_without_credential_passes_through(self) -> None:
        response = self.send("dev-agent", "GET", "/echo/plain")
        self.assertEqual(response.status, 200)
        self.assertEqual(self.recorded("/echo/plain")[0]["authorization"], "")


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
            self.send("dev-agent", "OPTIONS", "/x1", headers=auth),
            "credential_not_allowed",
            "/x1",
        )
        self.assert_rejected(
            self.send("dev-agent", "GET", "/x2", host="github.com", headers=auth),
            "credential_not_allowed",
            "/x2",
        )

    def test_other_domain_fake(self) -> None:
        response = self.send(
            "other-agent", "GET", "/x3", headers=[("Authorization", f"Bearer {FAKE}")]
        )
        self.assert_rejected(response, "subject_mismatch", "/x3")

    def test_real_value_is_never_forwarded(self) -> None:
        response = self.send(
            "dev-agent", "POST", "/x4", body=json.dumps({"t": REAL}).encode()
        )
        self.assert_rejected(response, "real_credential_exposed", "/x4")

    def test_plaintext_fake(self) -> None:
        response = self.send(
            "dev-agent",
            "GET",
            "/x5",
            tls=False,
            headers=[("Authorization", f"Bearer {FAKE}")],
        )
        self.assert_rejected(response, "plaintext_credential", "/x5")

    def test_sni_host_mismatch(self) -> None:
        response = self.send(
            "dev-agent",
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
                        "dev-agent",
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


class ReloadTest(GatewayCase):
    def test_generation_switch_and_failed_load(self) -> None:
        runtime = self.workdir / "runtime"
        secrets = json.loads(json.dumps(DECISIONS["secrets"]))
        secrets["generation"] = 2
        secrets["retired"].append(
            {"fake": FAKE, "credential": "github-token", "agent": "dev-agent"}
        )
        secrets["entries"] = [
            e for e in secrets["entries"] if e["agent"] != "dev-agent"
        ]
        write_generation(
            runtime,
            2,
            [{"port": self.ports["other-agent"], "agent": "other-agent"}],
            secrets,
        )
        self.wait_health(lambda health: health["loadedGeneration"] == 2)
        response = self.send(
            "dev-agent", "GET", "/r1", headers=[("Authorization", f"Bearer {FAKE}")]
        )
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "gateway_not_ready")
        )
        response = self.send(
            "other-agent", "GET", "/r2", headers=[("Authorization", f"Bearer {FAKE}")]
        )
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "retired_credential")
        )

        broken = json.loads(json.dumps(secrets))
        write_generation(
            runtime,
            3,
            [{"port": self.ports["other-agent"], "agent": "other-agent"}],
            broken,
        )
        health = self.wait_health(
            lambda health: health["lastAttempt"]["generation"] == 3
        )
        self.assertEqual(health["loadedGeneration"], 2)
        self.assertIn("generation", health["lastAttempt"]["error"])
        response = self.send(
            "other-agent",
            "GET",
            "/r3",
            headers=[("Authorization", f"Bearer {OTHER_FAKE}")],
        )
        self.assertEqual(response.status, 200)

        (runtime / "active.json").unlink()
        self.wait_health(lambda health: health["loadedGeneration"] is None)
        response = self.send("other-agent", "GET", "/r4")
        self.assertEqual(
            (response.status, response.json()["error"]), (403, "gateway_not_ready")
        )


@unittest.skipUnless(MITMDUMP and os.access(MITMDUMP, os.X_OK), "MITMDUMP is not set")
class StartupTest(unittest.TestCase):
    def test_unsafe_options_are_refused(self) -> None:
        unsafe = [
            ("ssl_insecure=true", b"ssl_insecure=false"),
            ("stream_large_bodies=1m", b"stream_large_bodies"),
            ("save_stream_file=flows.out", b"save_stream_file"),
            ("flow_detail=2", b"flow_detail=0"),
        ]
        for setting, message in unsafe:
            with self.subTest(
                setting=setting
            ), tempfile.TemporaryDirectory() as directory:
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
