"""Unit tests for security_middleware.backends.asset_verify — AssetVerifyBackend.

run_verification is mocked — we test the backend's orchestration and error handling.
"""

import unittest
from unittest.mock import patch

from agent_sec_cli.security_middleware.backends.asset_verify import (
    AssetVerifyBackend,
)
from agent_sec_cli.security_middleware.context import RequestContext

_PATCH_TARGET = (
    "agent_sec_cli.security_middleware.backends.asset_verify.run_verification"
)


class TestAssetVerifyBackend(unittest.TestCase):
    def setUp(self):
        self.backend = AssetVerifyBackend()
        self.ctx = RequestContext(action="verify")

    @patch(_PATCH_TARGET)
    def test_single_skill_pass(self, mock_run):
        mock_run.return_value = {
            "outcome": "verified",
            "checked": 1,
            "passed": ["my-skill"],
            "failed": [],
        }
        result = self.backend.execute(self.ctx, skill="/path/to/my-skill")

        mock_run.assert_called_once_with("/path/to/my-skill")
        self.assertTrue(result.success)
        self.assertEqual(result.data["passed"], 1)
        self.assertEqual(result.data["failed"], 0)
        self.assertEqual(result.data["outcome"], "verified")
        self.assertEqual(result.data["checked"], 1)
        self.assertEqual(result.exit_code, 0)
        self.assertIn("[OK]", result.stdout)

    @patch(_PATCH_TARGET)
    def test_single_skill_fail(self, mock_run):
        mock_run.return_value = {
            "outcome": "failed",
            "checked": 1,
            "passed": [],
            "failed": [{"name": "bad-skill", "error": "signature mismatch"}],
        }
        result = self.backend.execute(self.ctx, skill="/path/to/bad-skill")

        self.assertFalse(result.success)
        self.assertEqual(result.data["failed"], 1)
        self.assertEqual(result.data["outcome"], "failed")
        self.assertEqual(result.exit_code, 1)
        self.assertIn("[ERROR]", result.stdout)
        self.assertEqual(result.error_type, "")

    @patch(_PATCH_TARGET)
    def test_full_scan(self, mock_run):
        mock_run.return_value = {
            "outcome": "failed",
            "checked": 3,
            "passed": ["skill-a", "skill-b"],
            "failed": [{"name": "skill-c", "error": "bad sig"}],
        }
        result = self.backend.execute(self.ctx)

        mock_run.assert_called_once_with(None)
        self.assertFalse(result.success)
        self.assertEqual(result.data["passed"], 2)
        self.assertEqual(result.data["failed"], 1)

    @patch(_PATCH_TARGET)
    def test_no_candidates_is_successful_skip(self, mock_run):
        mock_run.return_value = {
            "outcome": "no_candidates",
            "checked": 0,
            "passed": [],
            "failed": [],
        }

        result = self.backend.execute(self.ctx)

        self.assertTrue(result.success)
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(
            result.data,
            {"outcome": "no_candidates", "checked": 0, "passed": 0, "failed": 0},
        )
        self.assertIn("VERIFICATION SKIPPED", result.stdout)
        self.assertNotIn("VERIFICATION PASSED", result.stdout)
        self.assertNotIn("[WARN]", result.stdout)

    @patch(_PATCH_TARGET)
    def test_verification_exception(self, mock_run):
        mock_run.side_effect = RuntimeError("no module")
        result = self.backend.execute(self.ctx)

        self.assertFalse(result.success)
        self.assertEqual(result.data, {})
        self.assertEqual(result.exit_code, 1)
        self.assertIn("Verification error", result.error)
        self.assertEqual(result.error_type, "RuntimeError")


if __name__ == "__main__":
    unittest.main()
