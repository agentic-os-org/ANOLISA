"""Check native model settings controls without a CLI or installed SDK."""

from copy import deepcopy
from pathlib import Path
from unittest.mock import patch
import sys
from types import ModuleType, SimpleNamespace
import unittest

sys.path.insert(0, str(Path(sys.argv.pop(1))))
sdk = ModuleType("claude_agent_sdk")
sdk.ClaudeAgentOptions = sdk.Transport = object
sys.modules["claude_agent_sdk"] = sdk
sys.modules["claude_agent_sdk._internal.transport.subprocess_cli"] = SimpleNamespace(
    SubprocessCLITransport=object,
)
sys.modules["aw_claude"] = SimpleNamespace(CallbackFailure=RuntimeError)

from aw_transport import ManagedTransport


class Query:
    def __init__(self, responses):
        self.responses = iter(responses)
        self.requests = []

    async def _send_control_request(self, request):
        self.requests.append(request)
        response = next(self.responses)
        if isinstance(response, Exception):
            raise response
        return response


class Contracts(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.transport = object.__new__(ManagedTransport)

    async def test_effective_native_environment_is_snapshotted(self):
        environment = {"ANTHROPIC_BASE_URL": "https://fixture.invalid", "BUSINESS": "retained"}
        response = {"effective": {"env": environment}, "sources": ["native-owned"]}
        original = deepcopy(response)
        query = Query([response])
        result = await self.transport.model_environment(query)
        self.assertEqual(result, environment)
        result["BUSINESS"] = "changed"
        self.assertEqual(response, original)
        self.assertEqual(query.requests, [{"subtype": "get_settings"}])

    async def test_missing_native_environment_is_empty(self):
        self.assertEqual(await self.transport.model_environment(Query([{"effective": {}}])), {})

    async def test_malformed_native_settings_are_rejected(self):
        for response in (None, {}, {"effective": []}, {"effective": {"env": None}},
                         {"effective": {"env": {"key": 1}}}):
            with self.subTest(response=response), self.assertRaisesRegex(RuntimeError, "effective settings"):
                await self.transport.model_environment(Query([response]))

    async def test_route_updates_only_session_environment_and_verifies_adoption(self):
        endpoint = "http://127.0.0.1:1234/fixture"
        route = {"ANTHROPIC_BASE_URL": endpoint, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                 "NO_PROXY": "127.0.0.1:1234", "no_proxy": "127.0.0.1:1234"}
        query = Query([{"effective": {}}, {}, {"effective": {"env": route}}])
        with patch.dict("os.environ", {}, clear=True):
            await self.transport.route_model(query, endpoint)
        self.assertEqual(query.requests, [
            {"subtype": "get_settings"},
            {"subtype": "apply_flag_settings", "settings": {"env": route}},
            {"subtype": "get_settings"},
        ])

    async def test_merge_preserves_inherited_and_native_exclusions(self):
        endpoint = "http://127.0.0.1:1234/fixture"
        base = {"HTTP_PROXY": "http://proxy.invalid", "NO_PROXY": "one.invalid, two.invalid",
                "no_proxy": "local.invalid"}
        effective = {"NO_PROXY": "two.invalid,settings.invalid", "no_proxy": "*.example.org"}
        merged = "one.invalid,two.invalid,local.invalid,settings.invalid,*.example.org,127.0.0.1:1234"
        query = Query([{}, {"effective": {"env": {
            "ANTHROPIC_BASE_URL": endpoint, "no_proxy": merged, "NO_PROXY": merged,
        }}}])
        await self.transport.route_model(query, endpoint, base, effective)
        flags = query.requests[0]["settings"]["env"]
        self.assertEqual((flags["no_proxy"], flags["NO_PROXY"]), (merged, merged))

    async def test_managed_proxy_overrides_are_checked_before_dispatch(self):
        endpoint = "http://127.0.0.1:1234/fixture"
        relay = "127.0.0.1:1234"
        for proxy in ("HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"):
            for lower in ("example.org", "127.0.0.1:99", ".0.0.1:1234", "27.0.0.1",
                          "*.0.0.1", "..127.0.0.1", "example.org 127.0.0.1", "127.0.0.1\u00a0"):
                overrides = {"NO_PROXY": relay, "no_proxy": lower}
                with self.subTest(proxy=proxy, overrides=overrides):
                    query = Query([{}, {"effective": {"env": {"ANTHROPIC_BASE_URL": endpoint, **overrides}}}])
                    with self.assertRaisesRegex(RuntimeError, "relay proxy bypass") as error:
                        await self.transport.route_model(query, endpoint, {proxy: "private-proxy"}, {})
                    self.assertNotIn("private", str(error.exception))
                    self.assertNotIn(endpoint, str(error.exception))

    async def test_effective_proxy_uses_lowercase_then_uppercase_exclusions(self):
        endpoint = "http://127.0.0.1:1234/fixture"
        for lower, upper in (("127.0.0.1:1234", "example.org"), ("", "127.0.0.1"),
                             ("example.org,*", ""), (".127.0.0.1", ""), (" .127.0.0.1:1234 ", ""),
                             ("example.org,0.0.1", ""), (".0.0.1", "")):
            env = {"ANTHROPIC_BASE_URL": endpoint, "HTTP_PROXY": "http://proxy.invalid",
                   "no_proxy": lower, "NO_PROXY": upper}
            await self.transport.route_model(Query([{}, {"effective": {"env": env}}]), endpoint, {}, {})
        env["no_proxy"] = "example.org"
        with self.assertRaisesRegex(RuntimeError, "relay proxy bypass"):
            await self.transport.route_model(Query([{}, {"effective": {"env": env}}]), endpoint, {}, {})

    async def test_all_proxy_alone_allows_direct_native_model_requests(self):
        endpoint = "http://127.0.0.1:1234/fixture"
        env = {"ANTHROPIC_BASE_URL": endpoint, "no_proxy": "example.org"}
        for key in ("ALL_PROXY", "all_proxy"):
            with self.subTest(key=key):
                query = Query([{}, {"effective": {"env": env}}])
                await self.transport.route_model(query, endpoint, {key: "http://proxy.invalid"}, {})

    async def test_unadopted_route_is_rejected_without_disclosing_settings(self):
        query = Query([{}, {"effective": {"env": {"ANTHROPIC_BASE_URL": "private-upstream"}}}])
        with self.assertRaisesRegex(RuntimeError, "override the required AW model relay") as raised:
            await self.transport.route_model(query, "http://127.0.0.1:1234/private-relay", {}, {})
        self.assertNotIn("private", str(raised.exception))

    async def test_failed_control_requests_have_no_direct_fallback(self):
        query = Query([RuntimeError("unsupported native settings control")])
        with self.assertRaisesRegex(RuntimeError, "unsupported native settings control"):
            await self.transport.route_model(query, "http://127.0.0.1:1234/fixture")
        self.assertEqual(len(query.requests), 1)


if __name__ == "__main__":
    unittest.main()
