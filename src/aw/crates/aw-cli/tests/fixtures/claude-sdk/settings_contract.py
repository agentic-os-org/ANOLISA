"""Check native model settings controls without a CLI or installed SDK."""

from copy import deepcopy
from pathlib import Path
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
        query = Query([{}, {"effective": {"env": {"ANTHROPIC_BASE_URL": endpoint}}}])
        await self.transport.route_model(query, endpoint)
        self.assertEqual(query.requests, [
            {"subtype": "apply_flag_settings", "settings": {"env": {
                "ANTHROPIC_BASE_URL": endpoint, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            }}},
            {"subtype": "get_settings"},
        ])

    async def test_unadopted_route_is_rejected_without_disclosing_settings(self):
        query = Query([{}, {"effective": {"env": {"ANTHROPIC_BASE_URL": "private-upstream"}}}])
        with self.assertRaisesRegex(RuntimeError, "override the required AW model relay") as raised:
            await self.transport.route_model(query, "private-relay")
        self.assertNotIn("private", str(raised.exception))

    async def test_failed_control_requests_have_no_direct_fallback(self):
        query = Query([RuntimeError("unsupported native settings control")])
        with self.assertRaisesRegex(RuntimeError, "unsupported native settings control"):
            await self.transport.route_model(query, "http://127.0.0.1:1234/fixture")
        self.assertEqual(len(query.requests), 1)


if __name__ == "__main__":
    unittest.main()
