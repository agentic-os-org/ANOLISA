#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Keep billing selection and emitted model metadata stable across catalog moves."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/ai/install-openclaw/scripts/install_openclaw.py"
)
SPEC = importlib.util.spec_from_file_location("install_openclaw", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
installer = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(installer)


class CatalogTests(unittest.TestCase):
    def test_billing_aliases_generate_the_same_provider_and_primary(self) -> None:
        for billing, provider in (
            ("pay-as-you-go", "bailian"),
            ("coding-plan", "bailian-coding-plan"),
            ("token-plan", "bailian-token-plan"),
        ):
            with self.subTest(billing=billing):
                with mock.patch.object(
                    sys, "argv", [str(SCRIPT), "--billing", billing, "--api-key", "test-key"]
                ):
                    args = installer.parse_args()
                config, metadata = installer.build_config(args)
                self.assertEqual(metadata["provider_id"], provider)
                self.assertEqual(
                    config["agents"]["defaults"]["model"]["primary"], f"{provider}/qwen3.6-plus"
                )
                first = config["models"]["providers"][provider]["models"][0]
                self.assertEqual(first["id"], "qwen3.6-plus")
                self.assertEqual(first["contextWindow"], 1_000_000)
                self.assertEqual(first["maxTokens"], 65_536)
                self.assertEqual(first["input"], ["text", "image"])
                self.assertEqual(first["compat"], {"thinkingFormat": "openai"})

    def test_model_defaults_and_custom_fallback_are_unchanged(self) -> None:
        with mock.patch.object(
            sys, "argv", [str(SCRIPT), "--context-window", "8192", "--max-tokens", "2048"]
        ):
            args = installer.parse_args()
        known = installer.build_model("MiniMax-M2.5", args)
        self.assertEqual(known["contextWindow"], 204_800)
        self.assertEqual(known["maxTokens"], 131_072)
        self.assertEqual(known["input"], ["text"])
        self.assertNotIn("compat", known)
        custom = installer.build_model("custom-model", args)
        self.assertEqual(custom["contextWindow"], 8192)
        self.assertEqual(custom["maxTokens"], 2048)

    def test_region_aliases_keep_existing_endpoint_selection(self) -> None:
        self.assertEqual(installer.normalize_region("GLOBAL"), "singapore")
        self.assertEqual(installer.normalize_region("beijing"), "china")
        self.assertEqual(
            installer.BILLING_PLANS["payg"]["base_urls"]["singapore"],
            "https://dashscope-intl.aliyuncs.com/apps/anthropic",
        )

    def test_invalid_billing_and_region_remain_errors(self) -> None:
        with self.assertRaises(SystemExit):
            installer.normalize_billing("unknown-plan")
        with self.assertRaises(SystemExit):
            installer.normalize_region("unknown-region")

    def test_standalone_help_runs_from_unrelated_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--help"],
                cwd=directory,
                capture_output=True,
                encoding="utf-8",
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--extra-model", result.stdout)
        self.assertIn("--billing", result.stdout)


if __name__ == "__main__":
    unittest.main()
