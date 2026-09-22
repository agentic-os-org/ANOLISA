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

"""Quickly verify model API keys are usable.

By default, reads claw-eval/config.yaml and tests every distinct
(api_key, base_url, model_id) triple referenced by ``model``, ``judge``,
and ``user_agent_model``. Each triple is exercised with a minimal
chat.completions request via the OpenAI SDK.

Usage:
    # Test all roles configured in claw-eval/config.yaml
    python3 scripts/check_api_key.py

    # Test a specific config file
    python3 scripts/check_api_key.py --config claw-eval/config_general.yaml

    # Test arbitrary credentials directly (skips yaml)
    python3 scripts/check_api_key.py --api-key sk-xxx \\
        --base-url https://dashscope.aliyuncs.com/compatible-mode/v1 \\
        --model-id qwen3.6-plus

Exit codes:
    0 - All tested keys responded successfully
    1 - At least one key failed
    2 - Invalid arguments / config not found
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path
from typing import Iterable

try:
    import yaml
except ImportError:
    print("Error: pyyaml required. Install with: pip install pyyaml", file=sys.stderr)
    sys.exit(2)


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_DIR = SCRIPT_DIR.parent
DEFAULT_CONFIG = REPO_DIR / "claw-eval" / "config.yaml"

ROLE_KEYS = ("model", "judge", "user_agent_model")


def _mask(api_key: str) -> str:
    if not api_key:
        return "<empty>"
    if len(api_key) <= 8:
        return "***"
    return f"{api_key[:4]}...{api_key[-4:]}"


def collect_targets_from_config(path: Path) -> list[dict]:
    """Return one entry per role in the yaml that has all required fields."""
    if not path.exists():
        raise FileNotFoundError(path)
    with open(path) as f:
        data = yaml.safe_load(f) or {}

    targets: list[dict] = []
    for role in ROLE_KEYS:
        section = data.get(role) or {}
        api_key = section.get("api_key")
        base_url = section.get("base_url")
        model_id = section.get("model_id")
        if not (api_key and base_url and model_id):
            continue
        targets.append({
            "role": role,
            "api_key": api_key,
            "base_url": base_url,
            "model_id": model_id,
        })
    return targets


def dedupe_targets(targets: Iterable[dict]) -> list[dict]:
    """Keep one entry per unique (api_key, base_url, model_id), merging roles."""
    seen: dict[tuple, dict] = {}
    for t in targets:
        key = (t["api_key"], t["base_url"], t["model_id"])
        if key in seen:
            seen[key]["role"] += f"+{t['role']}"
        else:
            seen[key] = dict(t)
    return list(seen.values())


def test_one(target: dict, timeout: float, max_tokens: int) -> dict:
    """Send a tiny chat completion. Returns a result dict."""
    try:
        from openai import OpenAI
    except ImportError:
        return {
            "ok": False,
            "error": "openai package not installed (pip install openai)",
            "latency_ms": 0,
            "reply": "",
        }

    client = OpenAI(
        api_key=target["api_key"],
        base_url=target["base_url"],
        timeout=timeout,
    )
    start = time.monotonic()
    try:
        resp = client.chat.completions.create(
            model=target["model_id"],
            messages=[{"role": "user", "content": "ping"}],
            max_tokens=max_tokens,
            temperature=0,
        )
        latency_ms = int((time.monotonic() - start) * 1000)
        reply = (resp.choices[0].message.content or "").strip()
        return {"ok": True, "error": "", "latency_ms": latency_ms, "reply": reply}
    except Exception as exc:
        latency_ms = int((time.monotonic() - start) * 1000)
        return {
            "ok": False,
            "error": f"{type(exc).__name__}: {exc}",
            "latency_ms": latency_ms,
            "reply": "",
        }


def run(targets: list[dict], timeout: float, max_tokens: int) -> int:
    if not targets:
        print("No targets to test.", file=sys.stderr)
        return 2

    failures = 0
    for t in targets:
        header = (
            f"[{t['role']}] {t['model_id']} @ {t['base_url']} "
            f"(key={_mask(t['api_key'])})"
        )
        print(header)
        result = test_one(t, timeout=timeout, max_tokens=max_tokens)
        if result["ok"]:
            preview = result["reply"][:60].replace("\n", " ")
            print(f"  OK    {result['latency_ms']} ms  reply={preview!r}")
        else:
            failures += 1
            print(f"  FAIL  {result['latency_ms']} ms  {result['error']}")
        print()

    total = len(targets)
    passed = total - failures
    print(f"Summary: {passed}/{total} passed")
    return 0 if failures == 0 else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Quickly verify model API keys with a tiny chat completion.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Test all roles in claw-eval/config.yaml
  %(prog)s

  # Test a specific yaml
  %(prog)s --config claw-eval/config_general.yaml

  # Direct credentials (skips yaml)
  %(prog)s --api-key sk-xxx --base-url https://api.example.com/v1 --model-id qwen3.6-plus
        """,
    )
    parser.add_argument("--config", default=str(DEFAULT_CONFIG),
                        help=f"YAML config to read (default: {DEFAULT_CONFIG})")
    parser.add_argument("--api-key", help="Test this key directly (skips YAML).")
    parser.add_argument("--base-url", help="Base URL when using --api-key.")
    parser.add_argument("--model-id", help="Model ID when using --api-key.")
    parser.add_argument("--timeout", type=float, default=15.0,
                        help="Per-request timeout in seconds (default: 15).")
    parser.add_argument("--max-tokens", type=int, default=5,
                        help="max_tokens for the probe (default: 5).")
    parser.add_argument("--no-dedupe", action="store_true",
                        help="Test every role even if credentials are identical.")
    args = parser.parse_args(argv)

    if args.api_key:
        if not (args.base_url and args.model_id):
            print("Error: --base-url and --model-id are required with --api-key",
                  file=sys.stderr)
            return 2
        targets = [{
            "role": "cli",
            "api_key": args.api_key,
            "base_url": args.base_url,
            "model_id": args.model_id,
        }]
    else:
        try:
            targets = collect_targets_from_config(Path(args.config))
        except FileNotFoundError as exc:
            print(f"Error: config not found: {exc}", file=sys.stderr)
            return 2
        if not targets:
            print(f"Error: no model/judge/user_agent_model entries in {args.config}",
                  file=sys.stderr)
            return 2
        if not args.no_dedupe:
            targets = dedupe_targets(targets)

    return run(targets, timeout=args.timeout, max_tokens=args.max_tokens)


if __name__ == "__main__":
    sys.exit(main())
