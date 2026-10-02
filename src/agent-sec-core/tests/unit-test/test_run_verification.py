"""Unit tests for run_verification() output structure.

run_verification is the contract boundary between asset_verify and the
security middleware backend.  These tests mock verify_skill / load_trusted_keys
so we can validate the returned dict shape for every code path without
needing real GPG keys or signed skills.
"""

import argparse
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import call, patch

from agent_sec_cli.asset_verify.errors import (
    ErrHashMismatch,
    ErrManifestMissing,
    ErrSigInvalid,
)
from agent_sec_cli.asset_verify.verifier import (
    format_verification_result,
    main,
    run_verification,
)

_MOD = "agent_sec_cli.asset_verify.verifier"


@patch(f"{_MOD}.load_trusted_keys", return_value=["fake-key"])
class TestRunVerificationSingleSkill(unittest.TestCase):
    """Single-skill path: run_verification(skill=<path>)."""

    @patch(f"{_MOD}.verify_skill", return_value=(True, "my-skill"))
    def test_success_returns_passed_list(self, _mock_vs, _mock_keys):
        result = run_verification(skill="/opt/skills/my-skill")

        self.assertIsInstance(result["passed"], list)
        self.assertIsInstance(result["failed"], list)
        self.assertEqual(result["outcome"], "verified")
        self.assertEqual(result["checked"], 1)
        self.assertEqual(result["passed"], ["my-skill"])
        self.assertEqual(result["failed"], [])
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(f"{_MOD}.verify_skill", side_effect=ErrSigInvalid("bad-skill", "bad sig"))
    def test_failure_returns_failed_list(self, _mock_vs, _mock_keys):
        result = run_verification(skill="/opt/skills/bad-skill")

        self.assertIsInstance(result["passed"], list)
        self.assertIsInstance(result["failed"], list)
        self.assertEqual(result["outcome"], "failed")
        self.assertEqual(result["checked"], 1)
        self.assertEqual(result["passed"], [])
        self.assertEqual(len(result["failed"]), 1)
        self.assertEqual(result["failed"][0]["name"], "bad-skill")
        self.assertIn("bad sig", result["failed"][0]["error"])
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(f"{_MOD}.verify_skill", side_effect=ErrManifestMissing("no-manifest"))
    def test_missing_manifest_returns_failed_list(self, _mock_vs, _mock_keys):
        result = run_verification(skill="/opt/skills/no-manifest")

        self.assertEqual(result["passed"], [])
        self.assertEqual(result["outcome"], "failed")
        self.assertEqual(result["checked"], 1)
        self.assertEqual(len(result["failed"]), 1)
        self.assertIn("name", result["failed"][0])
        self.assertIn("error", result["failed"][0])

    def test_hash_manifest_and_signature_failures_are_never_skipped(self, _mock_keys):
        failures = [
            ErrHashMismatch("bad-skill", "main.py", "expected", "actual"),
            ErrManifestMissing("bad-skill"),
            ErrSigInvalid("bad-skill", "bad signature"),
        ]

        for failure in failures:
            with self.subTest(error=type(failure).__name__):
                with patch(f"{_MOD}.verify_skill", side_effect=failure):
                    result = run_verification(skill="/opt/skills/bad-skill")

                self.assertEqual(result["outcome"], "failed")
                self.assertEqual(result["checked"], 1)
                self.assertEqual(result["passed"], [])
                self.assertEqual(len(result["failed"]), 1)
                self.assertEqual(
                    result["checked"], len(result["passed"]) + len(result["failed"])
                )


@patch(f"{_MOD}.load_trusted_keys", return_value=["fake-key"])
class TestRunVerificationBatch(unittest.TestCase):
    """Batch path: run_verification(skill=None)."""

    @patch(f"{_MOD}.load_config", return_value={"skills_dirs": ["/opt/skills"]})
    @patch(
        f"{_MOD}.verify_skills_dir",
        return_value={
            "checked": 3,
            "passed": ["a", "b"],
            "failed": [{"name": "c", "error": "err"}],
        },
    )
    def test_batch_aggregates_results(self, _mock_vsd, _mock_cfg, _mock_keys):
        result = run_verification(skill=None)

        self.assertIsInstance(result["passed"], list)
        self.assertIsInstance(result["failed"], list)
        self.assertEqual(result["outcome"], "failed")
        self.assertEqual(result["checked"], 3)
        self.assertEqual(result["passed"], ["a", "b"])
        self.assertEqual(len(result["failed"]), 1)
        self.assertEqual(result["failed"][0]["name"], "c")
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(f"{_MOD}.load_config", return_value={"skills_dirs": []})
    @patch(f"{_MOD}.verify_skills_dir")
    def test_empty_root_config_is_no_candidates(
        self, mock_verify_dir, _mock_cfg, _mock_keys
    ):
        result = run_verification(skill=None)

        self.assertEqual(
            result,
            {"outcome": "no_candidates", "checked": 0, "passed": [], "failed": []},
        )
        mock_verify_dir.assert_not_called()

    @patch(
        f"{_MOD}.load_config",
        return_value={"skills_dirs": ["/usr/share/skills", "/usr/local/skills"]},
    )
    @patch(
        f"{_MOD}.verify_skills_dir",
        side_effect=[
            {"checked": 0, "passed": [], "failed": []},
            {"checked": 1, "passed": ["raw-skill"], "failed": []},
        ],
    )
    def test_missing_or_empty_root_is_best_effort(
        self, mock_verify_dir, _mock_cfg, _mock_keys
    ):
        result = run_verification(skill=None)

        self.assertEqual(result["outcome"], "verified")
        self.assertEqual(result["checked"], 1)
        self.assertEqual(result["passed"], ["raw-skill"])
        self.assertEqual(mock_verify_dir.call_count, 2)
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(
        f"{_MOD}.load_config",
        return_value={"skills_dirs": ["/usr/share/skills", "/usr/local/skills"]},
    )
    @patch(
        f"{_MOD}.verify_skills_dir",
        side_effect=[
            {"checked": 1, "passed": ["packaged-skill"], "failed": []},
            {"checked": 0, "passed": [], "failed": []},
        ],
    )
    def test_packaged_root_succeeds_when_raw_root_is_missing_or_empty(
        self, mock_verify_dir, _mock_cfg, _mock_keys
    ):
        result = run_verification(skill=None)

        self.assertEqual(result["outcome"], "verified")
        self.assertEqual(result["checked"], 1)
        self.assertEqual(result["passed"], ["packaged-skill"])
        self.assertEqual(mock_verify_dir.call_count, 2)
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(
        f"{_MOD}.load_config",
        return_value={"skills_dirs": ["/usr/share/skills", "/usr/local/skills"]},
    )
    @patch(
        f"{_MOD}.verify_skills_dir",
        side_effect=[
            {"checked": 0, "passed": [], "failed": []},
            {"checked": 0, "passed": [], "failed": []},
        ],
    )
    def test_two_empty_or_missing_roots_are_no_candidates(
        self, mock_verify_dir, _mock_cfg, _mock_keys
    ):
        result = run_verification(skill=None)

        self.assertEqual(
            result,
            {"outcome": "no_candidates", "checked": 0, "passed": [], "failed": []},
        )
        self.assertEqual(mock_verify_dir.call_count, 2)

    @patch(f"{_MOD}.load_config")
    def test_distinct_roots_each_verify_same_named_candidate(
        self, mock_load_config, _mock_keys
    ):
        with tempfile.TemporaryDirectory() as tmpdir:
            first_root = Path(tmpdir) / "rpm"
            second_root = Path(tmpdir) / "raw"
            first_skill = first_root / "shared-name"
            second_skill = second_root / "shared-name"
            first_skill.mkdir(parents=True)
            second_skill.mkdir(parents=True)
            mock_load_config.return_value = {
                "skills_dirs": [str(first_root), str(second_root)]
            }

            with patch(
                f"{_MOD}.verify_skill", return_value=(True, "shared-name")
            ) as mock_verify_skill:
                result = run_verification(skill=None)

        self.assertEqual(result["outcome"], "verified")
        self.assertEqual(result["checked"], 2)
        self.assertEqual(result["passed"], ["shared-name", "shared-name"])
        self.assertEqual(result["failed"], [])
        self.assertEqual(
            mock_verify_skill.call_args_list,
            [
                call(str(first_skill), ["fake-key"]),
                call(str(second_skill), ["fake-key"]),
            ],
        )
        self.assertEqual(
            result["checked"], len(result["passed"]) + len(result["failed"])
        )

    @patch(
        f"{_MOD}.load_config",
        return_value={"skills_dirs": ["/opt/skills", "/opt/../opt/skills"]},
    )
    @patch(
        f"{_MOD}.verify_skills_dir",
        return_value={"checked": 1, "passed": ["a"], "failed": []},
    )
    def test_canonical_duplicate_roots_are_scanned_once(
        self, mock_verify_dir, _mock_cfg, _mock_keys
    ):
        result = run_verification(skill=None)

        self.assertEqual(result["outcome"], "verified")
        mock_verify_dir.assert_called_once_with("/opt/skills", ["fake-key"])

    @patch(f"{_MOD}.load_config", return_value={"skills_dirs": ["/opt/skills"]})
    @patch(f"{_MOD}.verify_skills_dir", side_effect=PermissionError("denied"))
    def test_unreadable_existing_root_is_operation_error(
        self, _mock_verify_dir, _mock_cfg, _mock_keys
    ):
        with self.assertRaisesRegex(PermissionError, "denied"):
            run_verification(skill=None)


class TestVerificationFormatting(unittest.TestCase):
    def test_no_candidates_is_silent_and_skipped(self):
        output = format_verification_result(
            {"outcome": "no_candidates", "checked": 0, "passed": [], "failed": []}
        )

        self.assertIn("CHECKED: 0", output)
        self.assertIn("VERIFICATION SKIPPED: NO CANDIDATE SKILLS", output)
        self.assertNotIn("[WARN]", output)
        self.assertNotIn("VERIFICATION PASSED", output)

    def test_verified_and_failed_have_distinct_statuses(self):
        verified = format_verification_result(
            {"outcome": "verified", "checked": 1, "passed": ["a"], "failed": []}
        )
        failed = format_verification_result(
            {
                "outcome": "failed",
                "checked": 1,
                "passed": [],
                "failed": [{"name": "a", "error": "bad signature"}],
            }
        )

        self.assertIn("VERIFICATION PASSED", verified)
        self.assertIn("VERIFICATION FAILED", failed)


class TestStandaloneMain(unittest.TestCase):
    @patch(
        f"{_MOD}.argparse.ArgumentParser.parse_args",
        return_value=argparse.Namespace(skill=None),
    )
    @patch(
        f"{_MOD}.run_verification",
        return_value={
            "outcome": "no_candidates",
            "checked": 0,
            "passed": [],
            "failed": [],
        },
    )
    @patch("builtins.print")
    def test_no_candidates_prints_shared_output_and_exits_zero(
        self, mock_print, _mock_run, _mock_args
    ):
        self.assertEqual(main(), 0)
        rendered = format_verification_result(
            {"outcome": "no_candidates", "checked": 0, "passed": [], "failed": []}
        )
        mock_print.assert_called_once_with(rendered, end="")

    @patch(
        f"{_MOD}.argparse.ArgumentParser.parse_args",
        return_value=argparse.Namespace(skill="/missing/skill"),
    )
    @patch(
        f"{_MOD}.run_verification",
        return_value={
            "outcome": "failed",
            "checked": 1,
            "passed": [],
            "failed": [{"name": "skill", "error": "manifest missing"}],
        },
    )
    @patch("builtins.print")
    def test_explicit_skill_failure_exits_one(self, mock_print, _mock_run, _mock_args):
        self.assertEqual(main(), 1)
        _mock_run.assert_called_once_with("/missing/skill")
        rendered = format_verification_result(
            {
                "outcome": "failed",
                "checked": 1,
                "passed": [],
                "failed": [{"name": "skill", "error": "manifest missing"}],
            }
        )
        mock_print.assert_called_once_with(rendered, end="")

    @patch(
        f"{_MOD}.argparse.ArgumentParser.parse_args",
        return_value=argparse.Namespace(skill=None),
    )
    @patch(f"{_MOD}.run_verification", side_effect=RuntimeError("boom"))
    @patch("builtins.print")
    def test_operation_error_is_written_to_stderr(
        self, mock_print, _mock_run, _mock_args
    ):
        self.assertEqual(main(), 1)
        self.assertEqual(
            mock_print.call_args_list, [call("[ERROR] boom", file=sys.stderr)]
        )


if __name__ == "__main__":
    unittest.main()
