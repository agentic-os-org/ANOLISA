#!/usr/bin/env python3
"""Regression tests for install_openclaw.py --extra-model handling.

--model-id is passed through strip_provider_prefix, but --extra-model
values were not, so `--extra-model bailian/qwen3.6-flash` produced an
agents.defaults.models key of `bailian/bailian/qwen3.6-flash` (a double
provider prefix no agent could resolve) instead of
`bailian/qwen3.6-flash`.
"""

import argparse
import sys
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "ai"
    / "install-openclaw"
    / "scripts"
    / "install_openclaw.py"
)
sys.path.insert(0, str(SCRIPT.parent))

import install_openclaw  # noqa: E402


def make_args(**overrides):
    values = dict(
        billing="payg",
        region="china",
        provider_id="",
        base_url="",
        provider_api="anthropic-messages",
        model_id="",
        extra_model=[],
        api_key="sk-test",
        api_key_env="",
        qwen_api_key="",
        bailian_api_key="",
        dashscope_api_key="",
        modelstudio_api_key="",
        reasoning=False,
        context_window=1000,
        max_tokens=100,
        max_concurrent=4,
        subagent_max_concurrent=8,
        gateway_auth_mode="none",
        skills_extra_dir=[],
        dingtalk_client_id="",
        dingtalk_client_secret="",
        dingtalk_robot_code="",
        dingtalk_dm_policy="open",
        dingtalk_group_policy="open",
        dingtalk_message_type="markdown",
    )
    values.update(overrides)
    return argparse.Namespace(**values)


class TestExtraModelPrefix(unittest.TestCase):
    def test_extra_model_with_provider_prefix_gets_normalized(self):
        config, _ = install_openclaw.build_config(
            make_args(extra_model=["bailian/qwen3.6-flash"])
        )
        agent_model_keys = config["agents"]["defaults"]["models"]
        self.assertIn("bailian/qwen3.6-flash", agent_model_keys)
        self.assertNotIn("bailian/bailian/qwen3.6-flash", agent_model_keys)

    def test_plain_extra_model_still_works(self):
        config, metadata = install_openclaw.build_config(
            make_args(extra_model=["qwen3.6-flash"])
        )
        self.assertIn("bailian/qwen3.6-flash", config["agents"]["defaults"]["models"])
        provider_models = [
            m["id"]
            for m in config["models"]["providers"][metadata["provider_id"]]["models"]
        ]
        self.assertEqual(provider_models.count("qwen3.6-flash"), 1)


if __name__ == "__main__":
    unittest.main()
