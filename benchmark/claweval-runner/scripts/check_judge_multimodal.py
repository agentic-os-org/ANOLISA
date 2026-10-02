#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Check whether a judge model supports multimodal (vision) input.

Sends a tiny 1x1 red PNG and asks what color it is. If the model can
"see" the image it responds with a color name; otherwise "NO_IMAGE".

Usage:
    python scripts/check_judge_multimodal.py --config claw-eval/config.yaml
    python scripts/check_judge_multimodal.py --model qwen3.6-plus --api-key sk-xxx
    JUDGE_MODEL_ID=qwen-vl-max python scripts/check_judge_multimodal.py

Exit codes:
    0 - Judge model supports multimodal (vision) input
    1 - Judge model does NOT support multimodal input or error occurred
"""
from __future__ import annotations

import argparse
import os
import sys

# ── Tiny 1x1 red PNG (base64) ──────────────────────────────────────────
TINY_RED_PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg=="
)

PROBE_PROMPT = (
    "You are a vision capability probe. Look at the image and answer "
    "ONLY with the single word for its color. If you cannot see any "
    "image, answer ONLY: NO_IMAGE"
)


def check_multimodal(model: str, base_url: str, api_key: str,
                     client_factory=None) -> bool:
    """Test whether *model* can process image inputs.

    Args:
        model: Judge model ID.
        base_url: API base URL.
        api_key: API key.
        client_factory: Optional callable(model, base_url, api_key) -> client.
            Injected for testability; defaults to ``openai.OpenAI``.

    Returns True if the model appears to support vision, False otherwise.
    """
    if client_factory is None:
        try:
            from openai import OpenAI
        except ImportError:
            raise RuntimeError("openai package not available")
        client_factory = lambda m, b, k: OpenAI(api_key=k, base_url=b)  # noqa: E731

    client = client_factory(model, base_url, api_key)

    resp = client.chat.completions.create(
        model=model,
        messages=[{
            "role": "user",
            "content": [
                {"type": "text", "text": PROBE_PROMPT},
                {"type": "image_url",
                 "image_url": {"url": f"data:image/png;base64,{TINY_RED_PNG_B64}"}},
            ],
        }],
        temperature=0.0,
        max_tokens=16,
    )
    text = (resp.choices[0].message.content or "").strip().lower()

    if "no_image" in text:
        return False
    if not text:
        return False
    return True


def _load_config_yaml(config_path: str) -> dict:
    """Load judge config from a claw-eval config YAML file."""
    try:
        import yaml
    except ImportError:
        print("[ERROR] pyyaml required; install with: pip install pyyaml")
        sys.exit(1)
    with open(config_path) as f:
        data = yaml.safe_load(f) or {}
    judge = data.get("judge", {})
    return {
        "model": judge.get("model_id", ""),
        "base_url": judge.get("base_url", ""),
        "api_key": judge.get("api_key", ""),
    }


def main():
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="Check whether a judge model supports multimodal (vision) input",
    )
    parser.add_argument("--config", help="Path to claw-eval config YAML")
    parser.add_argument("--model", help="Judge model ID")
    parser.add_argument("--base-url", help="Judge API base URL")
    parser.add_argument("--api-key", help="Judge API key")
    args = parser.parse_args()

    if args.config:
        cfg = _load_config_yaml(args.config)
        model = args.model or cfg["model"]
        base_url = args.base_url or cfg["base_url"]
        api_key = args.api_key or cfg["api_key"]
    else:
        model = args.model or os.environ.get("JUDGE_MODEL_ID", "")
        base_url = args.base_url or os.environ.get("JUDGE_BASE_URL", "")
        api_key = args.api_key or os.environ.get("JUDGE_API_KEY", "")

    if not model:
        print("[ERROR] Judge model not specified.")
        sys.exit(1)
    if not api_key:
        print("[ERROR] API key not specified.")
        sys.exit(1)
    if not base_url:
        base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"

    print(f"Judge model: {model}")
    print(f"Base URL:    {base_url}")
    print()

    try:
        ok = check_multimodal(model, base_url, api_key)
    except Exception as exc:
        print(f"  Error: {exc}")
        sys.exit(1)

    print()
    if ok:
        print("PASS: Judge model supports multimodal (vision) input.")
        sys.exit(0)
    else:
        print("FAIL: Judge model does NOT support multimodal input.")
        print("  Visual grading will return 0.0 for all screenshot-based tasks.")
        sys.exit(1)


if __name__ == "__main__":
    main()
