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

"""Configure model settings for claw-eval YAML config files.

Defaults are static values from config.yaml; only api_key is required
from user input. Interactive mode prompts for all values with defaults.

Usage:
    # Interactive (defaults from config.yaml static values)
    python3 scripts/configure_model.py

    # CLI mode
    python3 scripts/configure_model.py --api-key sk-xxx
"""

import argparse
import os
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("Error: pyyaml required. Install with: pip install pyyaml", file=sys.stderr)
    sys.exit(1)


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_DIR = SCRIPT_DIR.parent
DEFAULT_CONFIG_DIR = REPO_DIR / "claw-eval"

# Static defaults from config.yaml
_DEFAULT_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1"
_DEFAULT_MODEL_ID = "qwen3.6-plus"
_DEFAULT_JUDGE_MODEL_ID = "qwen3.6-plus"

CONFIG_FILES = [
    "config.yaml",
    "config_general.yaml",
    "config_multimodal.yaml",
    "config_user_agent.yaml",
]


def load_yaml(path: Path) -> dict:
    if not path.exists():
        return {}
    with open(path) as f:
        return yaml.safe_load(f) or {}


def save_yaml(path: Path, data: dict):
    with open(path, "w") as f:
        yaml.dump(data, f, default_flow_style=False, allow_unicode=True, sort_keys=False)


def prompt_input(label: str, default: str = "", hide_default: bool = False) -> str:
    """Prompt user for input with optional default."""
    display_default = "***" if hide_default and default else default
    if display_default:
        val = input(f"  {label} [{display_default}]: ").strip()
    else:
        val = input(f"  {label}: ").strip()
    return val if val else default


def update_config_file(path: Path, model: dict, judge: dict,
                       user_agent_model: dict = None):
    """Update a single YAML config file with new model settings."""
    data = load_yaml(path)

    data["model"] = {
        "api_key": model["api_key"],
        "base_url": model["base_url"],
        "model_id": model["model_id"],
        "input_modalities": ["text", "image"],
    }

    data["judge"] = {
        "api_key": judge["api_key"],
        "base_url": judge["base_url"],
        "model_id": judge["model_id"],
        "enabled": data.get("judge", {}).get("enabled", True),
    }

    if user_agent_model:
        if "user_agent_model" in data or path.name in ("config.yaml", "config_user_agent.yaml"):
            data["user_agent_model"] = {
                "api_key": user_agent_model["api_key"],
                "base_url": user_agent_model["base_url"],
                "model_id": user_agent_model["model_id"],
            }

    save_yaml(path, data)


def run_interactive(config_dir: Path, api_key: str = None):
    """Run interactive configuration."""
    print("=" * 60)
    print(" claw-eval Model Configuration")
    print("=" * 60)
    print("\nDefaults from config.yaml:")
    print(f"  Model:    {_DEFAULT_MODEL_ID}")
    print(f"  Judge:    {_DEFAULT_JUDGE_MODEL_ID}")
    print(f"  Base URL: {_DEFAULT_BASE_URL}")

    # API key is required
    if not api_key:
        print("\nAPI Key is required:")
        api_key = prompt_input("API Key", hide_default=False)
        if not api_key:
            print("Error: API key is required", file=sys.stderr)
            sys.exit(1)

    # Ask if all roles use same API key
    print("\nUse same API key for all roles? [Y/n]: ", end="")
    same_key = input().strip().lower()
    use_same_key = same_key in ("", "y", "yes")

    judge_api_key = api_key if use_same_key else prompt_input("Judge API Key", hide_default=True)
    ua_api_key = api_key if use_same_key else prompt_input("User Agent API Key", hide_default=True)

    # Model config with defaults
    print(f"\n-- Model (agent under test) --")
    model_id = prompt_input("Model ID", _DEFAULT_MODEL_ID)
    base_url = prompt_input("Base URL", _DEFAULT_BASE_URL)
    model = {"api_key": api_key, "base_url": base_url, "model_id": model_id}

    # Judge config with defaults
    print(f"\n-- Judge --")
    judge_default = os.environ.get("JUDGE_MODEL_ID", _DEFAULT_JUDGE_MODEL_ID)
    judge_model_id = prompt_input("Model ID", judge_default)
    judge_base_url = prompt_input("Base URL", _DEFAULT_BASE_URL)
    judge = {"api_key": judge_api_key, "base_url": judge_base_url, "model_id": judge_model_id}

    # User agent config with defaults
    print(f"\n-- User Agent --")
    ua_model_id = prompt_input("Model ID", _DEFAULT_MODEL_ID)
    ua_base_url = prompt_input("Base URL", _DEFAULT_BASE_URL)
    ua_model = {"api_key": ua_api_key, "base_url": ua_base_url, "model_id": ua_model_id}

    # Apply to all config files
    print(f"\nUpdating config files in {config_dir}...")
    for fname in CONFIG_FILES:
        fpath = config_dir / fname
        if fpath.exists() or fname == "config.yaml":
            update_config_file(fpath, model, judge, ua_model)
            print(f"  Updated: {fname}")
        else:
            print(f"  Skipped (not found): {fname}")

    print("\nConfiguration complete.")
    print_summary(model, judge, ua_model)


def run_cli(args):
    """Run CLI-based configuration."""
    config_dir = Path(args.config_dir)

    if not args.api_key:
        print("Error: --api-key is required in CLI mode", file=sys.stderr)
        sys.exit(1)

    model = {
        "api_key": args.api_key,
        "base_url": args.base_url or _DEFAULT_BASE_URL,
        "model_id": args.model_id or _DEFAULT_MODEL_ID,
    }

    judge = {
        "api_key": args.api_key,
        "base_url": args.base_url or _DEFAULT_BASE_URL,
        "model_id": args.judge_model_id or os.environ.get("JUDGE_MODEL_ID") or _DEFAULT_JUDGE_MODEL_ID,
        "enabled": True,
    }

    ua_model = {
        "api_key": args.api_key,
        "base_url": args.base_url or _DEFAULT_BASE_URL,
        "model_id": args.model_id or _DEFAULT_MODEL_ID,
    }

    # Apply
    updated = 0
    for fname in CONFIG_FILES:
        fpath = config_dir / fname
        if fpath.exists() or fname == "config.yaml":
            update_config_file(fpath, model, judge, ua_model)
            updated += 1
            print(f"Updated: {fpath}")

    if updated == 0:
        print(f"No config files found in {config_dir}", file=sys.stderr)
        sys.exit(1)

    print_summary(model, judge, ua_model)


def print_summary(model: dict, judge: dict, user_agent_model: dict):
    """Print a summary of the configured models."""
    print("\n" + "-" * 60)
    print(f"  Model (agent):      {model['model_id']} @ {model['base_url']}")
    print(f"  Judge (grader):     {judge['model_id']} @ {judge['base_url']}")
    print(f"  User Agent:         {user_agent_model['model_id']} @ {user_agent_model['base_url']}")
    print("-" * 60)


def main():
    parser = argparse.ArgumentParser(
        description="Configure model settings for claw-eval",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Interactive (static defaults, prompts for all values)
  %(prog)s

  # CLI with API key only (uses static defaults for rest)
  %(prog)s --api-key sk-xxx

  # Override model and base URL
  %(prog)s --api-key sk-xxx --model-id qwen-max --base-url https://api.example.com/v1
        """,
    )
    parser.add_argument("--config-dir", default=str(DEFAULT_CONFIG_DIR),
                        help=f"Path to claw-eval directory (default: {DEFAULT_CONFIG_DIR})")
    parser.add_argument("--api-key", help="API key (required)")
    parser.add_argument("--base-url", help=f"Base URL (default: {_DEFAULT_BASE_URL})")
    parser.add_argument("--model-id", help=f"Model ID (default: {_DEFAULT_MODEL_ID})")
    parser.add_argument("--judge-model-id", help=f"Judge model ID (default: {_DEFAULT_JUDGE_MODEL_ID})")

    args = parser.parse_args()

    has_cli_args = any([args.api_key, args.base_url, args.model_id, args.judge_model_id])

    if has_cli_args:
        run_cli(args)
    else:
        run_interactive(Path(args.config_dir), api_key=args.api_key)


if __name__ == "__main__":
    main()
