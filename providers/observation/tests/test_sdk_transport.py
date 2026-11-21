"""The acceptance runner must distinguish unsupported coverage from failed calls."""

from copy import deepcopy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from sdk_transport_cases import classify


class ClassificationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.result = {
            "sdk": "langchain",
            "model_request_verified": True,
            "client_result": {"completed": True},
            "transport_observed": True,
            "processes": [{"probe": "attached"}],
            "sources": [{"exit": 0, "summary": {"accounting_complete": True}}],
        }

    def test_observed_requires_completed_request_and_accounting(self) -> None:
        self.assertEqual(classify(self.result), "OBSERVED")
        for key, value in [
            ("model_request_verified", False),
            ("client_result", {"completed": False}),
        ]:
            row = deepcopy(self.result)
            row[key] = value
            self.assertEqual(classify(row), "FAIL")
        self.result["sources"][0]["summary"]["accounting_complete"] = False
        self.assertEqual(classify(self.result), "FAIL")

    def test_unsupported_requires_actual_request_and_worker_evidence(self) -> None:
        self.result.update(sdk="claude", transport_observed=False)
        self.assertEqual(classify(self.result), "FAIL")
        self.result["processes"].append({"probe": "unsupported_tls_library"})
        self.assertEqual(classify(self.result), "FAIL")
        self.result["processes"][-1]["model_sender"] = True
        self.assertEqual(classify(self.result), "UNSUPPORTED")
        self.result["model_request_verified"] = False
        self.assertEqual(classify(self.result), "FAIL")

    def test_qoder_auth_block_is_not_pass(self) -> None:
        self.result.update(sdk="qoder", model_request_verified=False)
        self.assertEqual(classify(self.result), "FAIL")
        self.result["client_result"]["blocked_reason"] = "offline_auth_initialization"
        self.assertEqual(classify(self.result), "BLOCKED")
        self.result["sources"][0]["exit"] = 1
        self.assertEqual(classify(self.result), "FAIL")

    def test_verified_account_gate_is_distinct_from_offline_auth(self) -> None:
        self.result.update(sdk="qoder", model_request_verified=False, custom_provider_allowed=False)
        self.assertEqual(classify(self.result), "BLOCKED")
        self.result["proxy_errors"] = ["SSLError"]
        self.assertEqual(classify(self.result), "FAIL")
        self.result["proxy_errors"] = []
        self.result["custom_provider_allowed"] = None
        self.assertEqual(classify(self.result), "FAIL")

    def test_authenticated_observation_requires_model_sender_digest(self) -> None:
        self.result["auth_closed_before_observation"] = True
        self.result["processes"][0].update(pid=1, model_sender=True)
        self.result["sources"][0]["pid"] = 1
        self.assertEqual(classify(self.result), "UNVERIFIED_TRANSPORT")
        self.result["sources"][0]["server_wire_digest_matches"] = 1
        self.assertEqual(classify(self.result), "OBSERVED")
        self.result["processes"][0]["model_sender"] = False
        self.assertEqual(classify(self.result), "UNVERIFIED_TRANSPORT")

    def test_collector_failure_cannot_be_unsupported(self) -> None:
        self.result.update(transport_observed=False)
        self.result["processes"].append({"probe": "unsupported_tls_library"})
        self.result["sources"][0]["exit"] = 1
        self.assertEqual(classify(self.result), "FAIL")


if __name__ == "__main__":
    unittest.main()
