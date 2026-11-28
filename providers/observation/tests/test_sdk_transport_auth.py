"""Auth fixture routes must reject unknown traffic before opening external sockets."""

import base64
import gzip
from http.client import HTTPConnection
import json
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fixtures"))
from sdk_transport_auth import AUTH_HOST, CATALOG_HOST, AuthBroker, AuthProxy, route


class AuthBoundaryTests(unittest.TestCase):
    def exchange(self, request: dict, connection: MagicMock) -> tuple:
        if connection.getresponse.return_value.getheader.side_effect is None:
            connection.getresponse.return_value.getheader.side_effect = (
                lambda name, default=None: default
            )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with AuthBroker(root / "broker.sock", root / "audit.jsonl") as broker:
                with patch(
                    "sdk_transport_auth.HTTPSConnection", return_value=connection
                ) as factory:
                    worker = threading.Thread(target=broker.handle_request)
                    worker.start()
                    with socket.socket(socket.AF_UNIX) as client:
                        client.settimeout(3)
                        client.connect(str(root / "broker.sock"))
                        client.sendall(json.dumps(request).encode() + b"\n")
                        with client.makefile("rb") as source:
                            result = json.loads(source.readline())
                    worker.join(3)
                    return result, factory.call_args_list, connection.request.call_args_list

    def test_unknown_auth_routes_never_open_external_connection(self) -> None:
        for method, path in [
            ("POST", "/v1/chat/completions"),
            ("GET", "/api/v1/userinfo?extra=1"),
            ("GET", "https://example.com/api/v1/userinfo"),
            ("DELETE", "/api/v1/userinfo"),
        ]:
            result, calls, _ = self.exchange({"method": method, "path": path}, MagicMock())
            self.assertEqual(result["status"], 502)
            self.assertEqual(calls, [])

    def test_redirect_is_rejected_without_following(self) -> None:
        connection = MagicMock()
        connection.getresponse.return_value.status = 302
        connection.getresponse.return_value.read.return_value = b""
        result, calls, requests = self.exchange(
            {"method": "GET", "path": "/api/v1/userinfo", "headers": {}, "body": ""}, connection
        )
        self.assertEqual(result["status"], 502)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].args, (AUTH_HOST,))
        self.assertEqual(len(requests), 1)

    def test_model_catalog_uses_only_its_declared_host(self) -> None:
        connection = MagicMock()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = b"{}"
        result, calls, requests = self.exchange(
            {
                "host": CATALOG_HOST,
                "method": "GET",
                "path": "/api/v2/model/list?Encode=1",
                "headers": {},
                "body": "",
            },
            connection,
        )
        self.assertEqual(result["status"], 200)
        self.assertEqual(calls[0].args, (CATALOG_HOST,))
        self.assertEqual(len(requests), 1)

    def test_encoded_response_preserves_headers_and_body(self) -> None:
        connection = MagicMock()
        payload = gzip.compress(b'{"featureSwitches":{"allow_byok":2}}')
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = payload
        response_headers = {"Content-Type": "application/json", "Content-Encoding": "gzip"}
        connection.getresponse.return_value.getheader.side_effect = response_headers.get
        result, calls, requests = self.exchange(
            {
                "method": "GET",
                "path": "/api/v3/user/status",
                "headers": {"Accept-Encoding": "gzip"},
                "body": "",
            },
            connection,
        )
        self.assertEqual(result["status"], 200)
        self.assertEqual(result["content_encoding"], "gzip")
        self.assertEqual(result["content_type"], "application/json")
        self.assertEqual(base64.b64decode(result["body"]), payload)
        self.assertEqual(requests[0].args[3]["Accept-Encoding"], "identity")
        self.assertEqual(len(calls), 1)

    def test_exact_auth_allowlist(self) -> None:
        self.assertEqual(route("POST", "/api/v1/jobToken/exchange"), "exchange")
        self.assertEqual(route("GET", "/api/v1/userinfo"), "userinfo")
        self.assertEqual(route("GET", "/api/v3/user/status"), "account_capabilities")
        self.assertIsNone(route("POST", "/api/v3/user/status"))
        self.assertEqual(route("GET", "/api/v2/model/list?Encode=1", CATALOG_HOST), "model_catalog")
        self.assertEqual(
            route("GET", "/algo/api/v2/model/list?Encode=1", CATALOG_HOST), "model_catalog"
        )
        self.assertIsNone(route("GET", "/algo/api/v2/model/list?Encode=2", CATALOG_HOST))
        self.assertIsNone(route("POST", "/api/v2/model/list?Encode=1", CATALOG_HOST))
        self.assertIsNone(route("POST", "/v1/chat/completions", CATALOG_HOST))
        self.assertIsNone(route("GET", "/api/v1/userinfo", "example.com"))
        self.assertEqual(route("GET", "/api/v1/organizations/synthetic/tags"), "organization_tags")
        self.assertIsNone(route("GET", "/api/v1/organizations/../userinfo/tags"))

    def test_proxy_rejects_unknown_host_and_closed_auth(self) -> None:
        with patch("sdk_transport_auth.ssl.SSLContext"):
            proxy = AuthProxy(Path("unused.sock"), Path("unused.pem"), Path("unused.key"), 1)
        try:
            for target in ["example.com:443", AUTH_HOST + ":444"]:
                client = HTTPConnection("127.0.0.1", proxy.server_port, timeout=3)
                client.request("CONNECT", target)
                self.assertEqual(client.getresponse().status, 403)
                client.close()
            proxy.disable_auth()
            client = HTTPConnection("127.0.0.1", proxy.server_port, timeout=3)
            client.request("CONNECT", AUTH_HOST + ":443")
            self.assertEqual(client.getresponse().status, 403)
            client.close()
            self.assertEqual(proxy.active, 0)
        finally:
            proxy.close()


if __name__ == "__main__":
    unittest.main()
