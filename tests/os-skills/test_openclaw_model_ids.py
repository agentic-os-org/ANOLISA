#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""All configured model references must identify one emitted provider model."""

import argparse
import importlib.util
import sys
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


def arguments(*options: str) -> argparse.Namespace:
    with mock.patch.object(sys, "argv", [str(SCRIPT), "--api-key", "test-key", *options]):
        return installer.parse_args()


class ModelIdTests(unittest.TestCase):
    def model_ids(self, *options: str) -> tuple[list[str], dict, dict]:
        config, metadata = installer.build_config(arguments(*options))
        provider = metadata["provider_id"]
        models = config["models"]["providers"][provider]["models"]
        model_ids = [model["id"] for model in models]
        self.assertEqual(len(model_ids), len(set(model_ids)))
        self.assertEqual(
            set(config["agents"]["defaults"]["models"]),
            {f"{provider}/{model_id}" for model_id in model_ids},
        )
        self.assertIn(metadata["primary_model"], config["agents"]["defaults"]["models"])
        return model_ids, config, metadata

    def test_prefixed_and_bare_extras_are_deduplicated_after_normalization(self) -> None:
        ids, _, _ = self.model_ids("--extra-model", "vendor/custom", "--extra-model", "custom")
        self.assertEqual(ids.count("custom"), 1)

    def test_prefixed_builtin_extra_does_not_duplicate_catalog_model(self) -> None:
        ids, _, _ = self.model_ids("--extra-model", "other/qwen3.6-plus")
        self.assertEqual(ids.count("qwen3.6-plus"), 1)

    def test_custom_provider_references_use_normalized_model_ids(self) -> None:
        ids, config, _ = self.model_ids(
            "--provider-id", "my-provider", "--extra-model", "old-provider/custom"
        )
        self.assertIn("custom", ids)
        self.assertIn("my-provider/custom", config["agents"]["defaults"]["models"])

    def test_selection_order_and_prefixed_primary_are_preserved(self) -> None:
        ids, _, metadata = self.model_ids(
            "--model-id",
            "old/custom-primary",
            "--extra-model",
            "old/alpha",
            "--extra-model",
            "beta",
            "--extra-model",
            "alpha",
        )
        self.assertEqual(ids[:3], ["custom-primary", "alpha", "beta"])
        self.assertEqual(metadata["primary_model"], "bailian/custom-primary")

    def test_unprefixed_selection_keeps_existing_model_metadata(self) -> None:
        ids, config, _ = self.model_ids("--extra-model", "custom")
        self.assertEqual(ids[:2], ["qwen3.6-plus", "custom"])
        custom = config["models"]["providers"]["bailian"]["models"][1]
        self.assertEqual(custom["contextWindow"], 1_000_000)
        self.assertEqual(custom["maxTokens"], 65_536)


if __name__ == "__main__":
    unittest.main()
