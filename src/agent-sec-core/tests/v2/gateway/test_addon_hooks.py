"""Pure Python hook contracts; mitmproxy imports are isolated test doubles.

These tests exercise addon-owned rewriting and transport closure decisions, not
mitmproxy's parser, wire protocol, or standalone process integration.
"""

import base64
import copy
import gzip
import json
import sys
import tempfile
import unittest
from collections.abc import ItemsView
from pathlib import Path
from types import ModuleType, SimpleNamespace
from unittest.mock import Mock, call, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_policy import (  # noqa: E402
    API_FACTS,
    DECISIONS,
    PLAN,
    VALUES,
    facts,
    load,
    plan_for,
    policy,
    runtime_files,
)

MODULE_NAMES = (
    "mitmproxy",
    "mitmproxy.addons",
    "mitmproxy.addons.termlog",
    "mitmproxy.log",
    "mitmproxy.net",
    "mitmproxy.net.http",
    "mitmproxy.net.http.http1",
    "mitmproxy.proxy",
    "mitmproxy.proxy.layers",
    "mitmproxy.proxy.layers.http",
)
modules = {name: ModuleType(name) for name in MODULE_NAMES}
modules["mitmproxy"].ctx = SimpleNamespace()
modules["mitmproxy"].exceptions = SimpleNamespace()
modules["mitmproxy"].http = SimpleNamespace()
modules["mitmproxy.addons.termlog"].TermLogHandler = type("TermLogHandler", (), {})
modules["mitmproxy.log"].LegacyLogEvents = type("LegacyLogEvents", (), {})
modules["mitmproxy.net.http.http1"].expected_http_body_size = Mock(return_value=0)
modules["mitmproxy.proxy"].commands = SimpleNamespace()
modules["mitmproxy.proxy"].events = SimpleNamespace()
modules["mitmproxy.proxy"].layer = SimpleNamespace()
for name in ("Http2Server", "HttpLayer", "HttpStream", "ResponseData"):
    setattr(modules["mitmproxy.proxy.layers.http"], name, type(name, (), {}))

# Import state is restored after loading; doubles never replace another suite's dependency.
with patch.dict(sys.modules, modules):
    import asc_gateway_addon as addon


class RequestHeaders(dict[str, str]):
    def items(self, multi: bool = False) -> ItemsView[str, str]:
        return super().items()

    @property
    def fields(self) -> tuple[tuple[bytes, bytes], ...]:
        return tuple((name.encode(), value.encode()) for name, value in self.items())

    @fields.setter
    def fields(self, fields: tuple[tuple[bytes, bytes], ...]) -> None:
        self.clear()
        self.update((name.decode(), value.decode()) for name, value in fields)


class AddonHookTest(unittest.TestCase):
    def setUp(self) -> None:
        self.gateway = addon.SecretGateway()
        plan = plan_for(PLAN["scopeId"])
        for rule in plan["rules"]:
            rule.update(header="X-Api-Key", responseRewrite=True)
        self.gateway.snapshot = load(runtime_files(plans={plan["scopeId"]: plan}))
        self.original = f"Vendor prefix={VALUES['fake']}; suffix"
        self.injected = f"Vendor prefix={VALUES['real']}; suffix"
        self.client = Mock(tls_established=True, sni="api.github.com")
        self.client.proxy_mode.listen_port.return_value = 15001
        self.server = Mock()
        self.flow = SimpleNamespace(
            request=SimpleNamespace(
                host_header="api.github.com",
                authority="",
                method="GET",
                path="/echo",
                headers=RequestHeaders({"X-Api-Key": self.original}),
                is_http2=False,
                is_http3=False,
            ),
            client_conn=self.client,
            server_conn=self.server,
            response=None,
            metadata={},
            killable=True,
            live=True,
            kill=Mock(),
        )
        self.gateway.requestheaders(self.flow)
        self.decision = self.flow.metadata[addon.METADATA_KEY]

    def test_generic_header_write_and_injection_metadata(self) -> None:
        self.assertEqual(
            self.flow.request.headers,
            {"X-Api-Key": self.injected},
        )
        self.assertTrue(self.flow.request.stream)
        self.assertEqual(self.decision.action, policy.ACTION_INJECT)
        self.assertEqual(
            (
                self.decision.header,
                self.decision.header_value,
                self.decision.response_rewrite,
            ),
            ("X-Api-Key", self.injected, True),
        )
        self.assertIs(addon._injected(self.flow), self.decision)

    def test_custom_basic_hook_redacts_headers_trailers_and_streamed_payload(
        self,
    ) -> None:
        original = base64.b64encode(f"user:{VALUES['fake']}".encode()).decode()
        injected = base64.b64encode(f"user:{VALUES['real']}".encode()).decode()
        self.flow.request.headers = RequestHeaders({"X-Api-Key": f"bAsIc {original}"})
        self.gateway.requestheaders(self.flow)
        self.assertEqual(self.flow.request.headers["X-Api-Key"], f"bAsIc {injected}")
        echoed = f"bAsIc {injected}|{injected}|{VALUES['real']}"
        redacted = f"bAsIc {original}|{original}|{VALUES['fake']}"
        self.flow.response = SimpleNamespace(
            headers=RequestHeaders(
                {"X-Echo": echoed, "Content-Length": "1000", "ETag": "tag"}
            ),
            trailers=RequestHeaders({"X-Echo-Trailer": echoed}),
        )
        with patch.object(addon, "expected_http_body_size", return_value=-1):
            self.gateway.responseheaders(self.flow)
            self.assertEqual(
                self.flow.response.headers,
                {"X-Echo": redacted, "Transfer-Encoding": "chunked"},
            )
            rewrite = self.flow.response.stream
            actual = b"".join(rewrite(echoed.encode())) + b"".join(rewrite(b""))
            self.assertEqual(actual, redacted.encode())
            self.gateway.response(self.flow)
        self.assertEqual(self.flow.response.trailers, {"X-Echo-Trailer": redacted})
        self.flow.kill.assert_not_called()

    def test_unavailable_fake_hook_rejects_without_request_mutation(self) -> None:
        self.flow.metadata.clear()
        self.flow.request.headers = RequestHeaders(
            {"X-Api-Key": VALUES["unavailableFake"]}
        )
        rejected = SimpleNamespace()
        with patch.object(addon, "_reject_response", return_value=rejected) as response:
            self.gateway.requestheaders(self.flow)
        self.assertIs(self.flow.response, rejected)
        response.assert_called_once_with("credential_not_allowed", 403)
        self.assertEqual(
            self.flow.request.headers, {"X-Api-Key": VALUES["unavailableFake"]}
        )
        self.assertIsNone(addon._injected(self.flow))
        self.assertFalse(self.flow.request.stream)

    def test_response_and_trailer_fields_restore_names_and_values(self) -> None:
        for label in ("headers", "trailers"):
            with self.subTest(label=label):
                fields = SimpleNamespace(
                    fields=(
                        (b"X-Echo", self.injected.encode()),
                        (VALUES["real"].encode(), VALUES["real"].encode()),
                    )
                )
                if label == "headers":
                    addon._rewrite_fields(fields, self.decision)
                else:
                    self.flow.response = SimpleNamespace(
                        headers=RequestHeaders(), trailers=fields, stream=True
                    )
                    self.gateway.response(self.flow)
                self.assertEqual(
                    fields.fields,
                    (
                        (b"X-Echo", self.original.encode()),
                        (VALUES["fake"].encode(), VALUES["fake"].encode()),
                    ),
                )

    def test_identity_stream_withholds_split_key_but_releases_safe_bytes(self) -> None:
        rewrite = addon._StreamingResponse(self.flow, self.decision)
        body = self.injected.encode()
        split = len(body) // 2
        self.assertEqual(list(rewrite(b"data: " + body[:split])), [b"data: "])
        self.assertEqual(
            b"".join(rewrite(body[split:] + b"\n\n")),
            self.original.encode() + b"\n\n",
        )
        self.assertEqual(list(rewrite(b"")), [])
        self.flow.kill.assert_not_called()

    def test_streaming_failure_aborts_and_never_returns_origin_bytes(self) -> None:
        rewrite = addon._StreamingResponse(self.flow, self.decision)
        rewrite.rewriter = Mock(side_effect=ValueError("synthetic failure"))
        with patch.object(addon, "_abort_flow") as abort:
            self.assertEqual(list(rewrite(self.injected.encode())), [])
            self.assertEqual(list(rewrite(b"")), [])
            abort.assert_called_once_with(self.flow)

    def test_encoded_response_preserves_wire_body_and_framing(self) -> None:
        for encoding, wire in (
            ("gzip", gzip.compress(self.injected.encode(), mtime=0)),
            ("br", b"opaque encoded bytes"),
        ):
            with self.subTest(encoding=encoding):
                self.flow.response = SimpleNamespace(
                    headers=RequestHeaders(
                        {
                            "X-Echo": self.injected,
                            "Content-Encoding": encoding,
                            "Content-Length": str(len(wire)),
                            "ETag": "tag",
                        }
                    ),
                    trailers=RequestHeaders({"X-Echo-Trailer": self.injected}),
                    raw_content=wire,
                )
                with patch.object(addon, "expected_http_body_size", return_value=-1):
                    self.gateway.responseheaders(self.flow)
                    self.gateway.response(self.flow)
                self.assertEqual(
                    self.flow.response.headers,
                    {
                        "X-Echo": self.original,
                        "Content-Encoding": encoding,
                        "Content-Length": str(len(wire)),
                        "ETag": "tag",
                    },
                )
                self.assertEqual(
                    self.flow.response.trailers, {"X-Echo-Trailer": self.original}
                )
                self.assertIs(self.flow.response.stream, True)
                self.assertEqual(self.flow.response.raw_content, wire)
                self.flow.kill.assert_not_called()

    def test_disabled_rewrite_preserves_headers_trailers_and_body(self) -> None:
        plan = plan_for(PLAN["scopeId"])
        for rule in plan["rules"]:
            rule.update(header="X-Api-Key", responseRewrite=False)
        self.gateway.snapshot = load(runtime_files(plans={plan["scopeId"]: plan}))
        self.flow.request.headers = RequestHeaders(
            {"X-Api-Key": self.original, "Accept-Encoding": "br"}
        )
        self.gateway.requestheaders(self.flow)
        self.assertEqual(
            self.flow.request.headers,
            {"X-Api-Key": self.injected, "Accept-Encoding": "br"},
        )
        self.assertIs(self.flow.metadata[addon.METADATA_KEY].response_rewrite, False)
        self.flow.response = SimpleNamespace(
            headers=RequestHeaders({"X-Echo": self.injected}),
            trailers=RequestHeaders({"X-Echo-Trailer": self.injected}),
            raw_content=VALUES["real"].encode(),
        )
        self.gateway.responseheaders(self.flow)
        self.gateway.response(self.flow)
        self.assertEqual(self.flow.response.headers, {"X-Echo": self.injected})
        self.assertEqual(self.flow.response.trailers, {"X-Echo-Trailer": self.injected})
        self.assertIs(self.flow.response.stream, True)
        self.assertEqual(self.flow.response.raw_content, VALUES["real"].encode())

    def test_responseheaders_preserves_bodyless_and_protocol_framing_guards(
        self,
    ) -> None:
        for http2, size in ((False, 0), (False, -1), (True, -1)):
            with self.subTest(http2=http2, size=size):
                self.flow.request.is_http2 = http2
                self.flow.response = SimpleNamespace(
                    headers=RequestHeaders(
                        {
                            "X-Echo": self.injected,
                            "Content-Length": "100",
                            "Transfer-Encoding": "chunked",
                            "ETag": "tag",
                            "Content-MD5": "digest",
                        }
                    )
                )
                with patch.object(addon, "expected_http_body_size", return_value=size):
                    self.gateway.responseheaders(self.flow)
                expected = {"X-Echo": self.original}
                if size == 0:
                    expected.update(
                        {
                            "Content-Length": "100",
                            "ETag": "tag",
                            "Content-MD5": "digest",
                        }
                    )
                    self.assertIs(self.flow.response.stream, True)
                elif not http2:
                    expected["Transfer-Encoding"] = "chunked"
                self.assertEqual(self.flow.response.headers, expected)
                if size != 0:
                    self.assertEqual(list(self.flow.response.stream(b"")), [])
                self.flow.kill.assert_not_called()

    def test_injected_http1_and_http2_errors_close_both_transports_when_not_killable(
        self,
    ) -> None:
        for http2 in (False, True):
            with self.subTest(http2=http2):
                self.flow.request.is_http2 = http2
                self.flow.killable = False
                handler = Mock()
                writers = [Mock(), Mock()]
                handler.transports = {
                    self.client: SimpleNamespace(writer=writers[0]),
                    self.server: SimpleNamespace(writer=writers[1]),
                }
                proxy = SimpleNamespace(connections={self.client.id: handler})
                master = SimpleNamespace(addons=SimpleNamespace(get=lambda name: proxy))
                with patch.object(addon, "ctx", SimpleNamespace(master=master)):
                    self.gateway.error(self.flow)
                for writer in writers:
                    writer.close.assert_called_once_with()
                self.assertEqual(
                    handler.close_connection.call_args_list,
                    [call(self.client), call(self.server)],
                )
                self.flow.kill.assert_not_called()


class FocusLoaderTest(unittest.TestCase):
    def test_malformed_snapshot_rejects_startup_and_preserves_last_good_generation(
        self,
    ) -> None:
        for defect in ("live-owner", "retired", "unavailable", "length", "similarity"):
            for loaded in (False, True):
                with (
                    self.subTest(defect=defect, loaded=loaded),
                    tempfile.TemporaryDirectory(prefix="asc-focusloader-") as directory,
                ):
                    runtime = Path(directory)
                    (runtime / "status").mkdir()
                    gateway = addon.SecretGateway()
                    secrets = copy.deepcopy(DECISIONS["secrets"])
                    if defect == "live-owner":
                        item = copy.deepcopy(secrets["entries"][0])
                        item.update(fake="F" * 40, real="R" * 40)
                        item["scopeIds"] = [DECISIONS["listeners"][1]["scopeId"]]
                        secrets["entries"].append(item)
                    elif defect in ("retired", "unavailable"):
                        secrets[defect][0]["credential"] = "github"
                    elif defect == "length":
                        secrets["entries"][0]["real"] = "R" * 39
                    else:
                        secrets["entries"][0]["real"] = (
                            "R" + secrets["entries"][0]["fake"][1:]
                        )
                    generation = 2 if loaded else 1
                    secrets["generation"] = generation
                    with patch.object(
                        addon,
                        "ctx",
                        SimpleNamespace(
                            options=SimpleNamespace(agent_sec_runtime_dir=directory)
                        ),
                    ):
                        if loaded:
                            for name, data in runtime_files().items():
                                (runtime / name).write_bytes(data)
                            gateway.reload()
                        previous = gateway.snapshot
                        files = runtime_files(secrets=secrets, generation=generation)
                        for name, data in files.items():
                            (runtime / name).write_bytes(data)
                        gateway.reload()
                    self.assertIs(gateway.snapshot, previous)
                    health = json.loads(
                        (runtime / "status" / "health.json").read_bytes()
                    )
                    self.assertEqual(health["loadedGeneration"], 1 if loaded else None)
                    self.assertEqual(health["lastAttempt"]["generation"], generation)
                    self.assertIsInstance(health["lastAttempt"]["error"], str)
                    for value in VALUES.values():
                        self.assertNotIn(value, json.dumps(health))
                    for entry in secrets["entries"]:
                        for key in ("fake", "real"):
                            self.assertNotIn(entry[key], json.dumps(health))
                    decision = policy.decide_request(gateway.snapshot, facts(API_FACTS))
                    self.assertEqual(
                        (decision.action, decision.code),
                        (
                            ("inject", "credential_injected")
                            if loaded
                            else ("reject", "gateway_not_ready")
                        ),
                    )
                    if loaded:
                        self.assertEqual(
                            decision.header_value, f"Bearer {VALUES['real']}"
                        )
                    else:
                        self.assertIsNone(decision.header_value)
                        self.assertEqual(decision.replacements, ())


if __name__ == "__main__":
    unittest.main()
