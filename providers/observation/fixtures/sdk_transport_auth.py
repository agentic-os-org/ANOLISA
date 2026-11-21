"""Private test relay: exact Qoder auth routes, no redirects or stored bodies.

The host broker owns external sockets. A loopback-only SDK namespace reaches it
through a private Unix socket; its CONNECT proxy allows only auth and one model.
"""

import argparse
import base64
from http.client import HTTPSConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import resource
import select
import socket
import socketserver
import ssl
import threading
import time
from urllib.parse import urlsplit, parse_qsl

AUTH_HOST = "openapi.qoder.sh"
CATALOG_HOST = "api2.qoder.sh"
MAX_BODY = 1 << 20


def route(method: str, path: str, host: str = AUTH_HOST) -> str | None:
    if (
        host == CATALOG_HOST
        and method == "GET"
        and path in {"/api/v2/model/list?Encode=1", "/algo/api/v2/model/list?Encode=1"}
    ):
        return "model_catalog"
    if host != AUTH_HOST:
        return None
    if method == "POST" and path == "/api/v1/jobToken/exchange":
        return "exchange"
    if method == "GET" and path == "/api/v1/userinfo":
        return "userinfo"
    if method == "GET" and path == "/api/v3/user/status":
        return "account_capabilities"
    if method == "GET" and re.fullmatch(r"/api/v1/organizations/[A-Za-z0-9_-]+/tags", path):
        return "organization_tags"
    return None


class AuthBroker(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True

    def __init__(self, path: Path, audit: Path) -> None:
        self.audit = audit
        self.lock = threading.Lock()
        super().__init__(str(path), AuthRequest)
        os.chmod(path, 0o600)

    def record(
        self, kind: str, status: int, capability: bool | None = None, encoding: str | None = None
    ) -> None:
        with self.lock, self.audit.open("a") as destination:
            entry = {"route": kind, "status": status}
            if encoding is not None:
                entry["encoding"] = (
                    encoding if encoding in {"gzip", "br", "deflate", "identity"} else "other"
                )
            if capability is not None:
                entry["custom_provider_allowed"] = capability
            destination.write(json.dumps(entry) + "\n")


class AuthRequest(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        self.request.settimeout(20)
        connection = None
        kind = "rejected"
        try:
            raw = self.rfile.readline(MAX_BODY + 1)
            if len(raw) > MAX_BODY:
                raise ValueError("request_limit")
            request = json.loads(raw)
            host = request.get("host", AUTH_HOST)
            kind = route(request["method"], request["path"], host)
            if kind is None:
                raise ValueError("auth_route_forbidden")
            headers = {
                key: value
                for key, value in request["headers"].items()
                if key.lower()
                not in {
                    "host",
                    "connection",
                    "proxy-authorization",
                    "proxy-connection",
                    "content-length",
                    "transfer-encoding",
                    "accept-encoding",
                }
            }
            headers["Accept-Encoding"] = "identity"
            body = base64.b64decode(request["body"], validate=True)
            connection = HTTPSConnection(host, timeout=15, context=ssl.create_default_context())
            connection.request(request["method"], request["path"], body, headers)
            response = connection.getresponse()
            data = response.read(MAX_BODY + 1)
            if len(data) > MAX_BODY or 300 <= response.status < 400:
                raise ValueError("auth_response_rejected")
            result = {
                "status": response.status,
                "body": base64.b64encode(data).decode(),
                "content_type": response.getheader("Content-Type", "application/json"),
                "content_encoding": response.getheader("Content-Encoding"),
            }
            capability = None
            if kind == "account_capabilities" and response.status == 200:

                def inspect(value: object) -> bool | None:
                    if isinstance(value, dict):
                        if isinstance(value.get("allow_byok"), (int, float)):
                            return value["allow_byok"] >= 2
                        for nested in value.values():
                            found = inspect(nested)
                            if found is not None:
                                return found
                    return None

                try:
                    capability = inspect(json.loads(data))
                except (ValueError, UnicodeError):
                    pass
            result["custom_provider_allowed"] = capability
            self.server.record(
                kind,
                response.status,
                capability,
                response.getheader("Content-Encoding", "identity"),
            )
        except (OSError, ValueError, KeyError):
            result = {
                "status": 502,
                "body": base64.b64encode(b'{"error":"auth_relay_failed"}').decode(),
            }
            self.server.record(kind or "rejected", 502)
        finally:
            if connection:
                connection.close()
        self.wfile.write(json.dumps(result).encode() + b"\n")


class AuthProxy(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, broker: Path, certificate: Path, key: Path, model_port: int) -> None:
        self.broker = broker
        self.model_port = model_port
        self.enabled = True
        self.active = 0
        self.condition = threading.Condition()
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certificate, key)
        self.context.set_alpn_protocols(["http/1.1"])
        self.blocked = 0
        self.rejections = {}
        self.denied_routes = []
        self.proxy_errors = []
        self.model_peer_port = None
        self.custom_provider_allowed = None
        super().__init__(("127.0.0.1", 0), ProxyRequest)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def disable_auth(self) -> None:
        deadline = time.monotonic() + 20
        with self.condition:
            self.enabled = False
            while self.active:
                left = deadline - time.monotonic()
                if left <= 0:
                    raise TimeoutError("authentication_drain_deadline")
                self.condition.wait(left)

    def close(self) -> None:
        self.disable_auth()
        self.shutdown()
        self.server_close()
        self.thread.join(5)


class ProxyRequest(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        pass

    def do_CONNECT(self) -> None:
        self.connection.settimeout(20)
        authority = self.path
        if self.path == f"localhost:{self.server.model_port}":
            self.model_tunnel()
            return
        with self.server.condition:
            if (
                self.path not in (AUTH_HOST + ":443", CATALOG_HOST + ":443")
                or not self.server.enabled
            ):
                self.server.blocked += 1
                label = {
                    "openapi.qoder.sh:443": "auth_closed",
                    "center.qoder.sh:443": "endpoint_configuration",
                    "api2.qoder.sh:443": "service",
                    "api2-v2.qoder.sh:443": "remote_model",
                }.get(self.path, "other_external")
                self.server.rejections[label] = self.server.rejections.get(label, 0) + 1
                self.send_error(403)
                return
            self.server.active += 1
        try:
            self.send_response(200, "Connection established")
            self.end_headers()
            connection = self.server.context.wrap_socket(self.connection, server_side=True)
            self.connection = connection
            self.rfile = connection.makefile("rb")
            self.wfile = connection.makefile("wb")
            self.raw_requestline = self.rfile.readline(65537)
            if len(self.raw_requestline) > 65536 or not self.parse_request():
                if len(self.server.proxy_errors) < 8:
                    self.server.proxy_errors.append("request_line_or_parse_rejected")
                return
            self.close_connection = True
            if route(self.command, self.path, authority.rsplit(":", 1)[0]) is None:
                parsed = urlsplit(self.path)
                if len(self.server.denied_routes) < 8:
                    segments = [
                        part if re.fullmatch(r"[A-Za-z_-]{1,24}|v[0-9]", part) else ":opaque"
                        for part in parsed.path.split("/")
                        if part
                    ]
                    self.server.denied_routes.append(
                        {
                            "host_role": (
                                "catalog" if authority.startswith(CATALOG_HOST) else "auth"
                            ),
                            "method": (
                                self.command
                                if self.command in {"GET", "POST", "PUT", "DELETE"}
                                else "other"
                            ),
                            "path_template": "/" + "/".join(segments[:12]),
                            "query_keys": [
                                key if re.fullmatch(r"[A-Za-z_]{1,32}", key) else "opaque"
                                for key, _ in parse_qsl(parsed.query)
                            ][:12],
                        }
                    )
                self.server.blocked += 1
                self.send_error(403)
                return
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 <= length <= 32768 or self.headers.get("Transfer-Encoding"):
                self.send_error(413)
                return
            body = self.rfile.read(length)
            if len(body) != length:
                raise ValueError("short_auth_body")
            request = {
                "host": authority.rsplit(":", 1)[0],
                "method": self.command,
                "path": self.path,
                "headers": dict(self.headers.items()),
                "body": base64.b64encode(body).decode(),
            }
            with socket.socket(socket.AF_UNIX) as relay:
                relay.settimeout(20)
                relay.connect(str(self.server.broker))
                relay.sendall(json.dumps(request).encode() + b"\n")
                with relay.makefile("rb") as response:
                    result = json.loads(response.readline(MAX_BODY * 2))
            if isinstance(result.get("custom_provider_allowed"), bool):
                self.server.custom_provider_allowed = result["custom_provider_allowed"]
            data = base64.b64decode(result["body"], validate=True)
            self.send_response(result["status"])
            self.send_header("Content-Type", result.get("content_type", "application/json"))
            if result.get("content_encoding"):
                self.send_header("Content-Encoding", result["content_encoding"])
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()
        except (OSError, ValueError, KeyError) as error:
            if len(self.server.proxy_errors) < 8:
                self.server.proxy_errors.append(type(error).__name__)
        finally:
            self.close_connection = True
            self.connection.close()
            with self.server.condition:
                self.server.active -= 1
                self.server.condition.notify_all()

    def model_tunnel(self) -> None:
        self.server.model_peer_port = self.client_address[1]
        self.send_response(200, "Connection established")
        self.end_headers()
        with socket.create_connection(("127.0.0.1", self.server.model_port), timeout=20) as model:
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                ready, _, _ = select.select([self.connection, model], [], [], 1)
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    (model if source is self.connection else self.connection).sendall(data)
        self.close_connection = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--audit", type=Path, required=True)
    options = parser.parse_args()
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    os.umask(0o077)
    with AuthBroker(options.socket, options.audit) as broker:
        try:
            broker.serve_forever()
        finally:
            options.socket.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
