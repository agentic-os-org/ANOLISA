#!/usr/bin/env python3
"""
Non-interactive OpenClaw installer/configuration helper for Alibaba Cloud Model Studio.

The script prepares Node.js/OpenClaw, writes ~/.openclaw/openclaw.json using
the OpenClaw Anthropic provider shape documented by Alibaba Cloud Model Studio,
and starts the local gateway service.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path


DEFAULT_CONFIG_PATH = Path("~/.openclaw/openclaw.json").expanduser()

BILLING_ALIASES = {
    "payg": "payg",
    "standard": "payg",
    "dashscope": "payg",
    "postpaid": "payg",
    "pay-as-you-go": "payg",
    "coding": "coding",
    "coding-plan": "coding",
    "token": "token",
    "token-plan": "token",
}

REGION_ALIASES = {
    "china": "china",
    "cn": "china",
    "beijing": "china",
    "global": "singapore",
    "intl": "singapore",
    "international": "singapore",
    "singapore": "singapore",
    "sg": "singapore",
}

BILLING_PLANS = {
    "payg": {
        "name": "Pay-as-you-go",
        "provider_id": "bailian",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://help.aliyun.com/zh/model-studio/get-api-key",
        "model_catalog_url": "https://bailian.console.aliyun.com/?tab=model#/model-market",
        "base_urls": {
            "china": "https://dashscope.aliyuncs.com/apps/anthropic",
            "singapore": "https://dashscope-intl.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.6-plus",
            "MiniMax-M2.5",
            "glm-5",
            "deepseek-v3.2",
        ],
        "key_env": ["BAILIAN_API_KEY", "DASHSCOPE_API_KEY", "QWEN_API_KEY"],
    },
    "coding": {
        "name": "Coding Plan",
        "provider_id": "bailian-coding-plan",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan",
        "model_catalog_url": "https://help.aliyun.com/zh/model-studio/coding-plan",
        "base_urls": {
            "china": "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.6-plus",
            "qwen3.5-plus",
            "qwen3-max-2026-01-23",
            "qwen3-coder-next",
            "qwen3-coder-plus",
            "MiniMax-M2.5",
            "glm-5",
            "glm-4.7",
            "kimi-k2.5",
        ],
        "key_env": ["CODING_PLAN_API_KEY", "QWEN_API_KEY", "BAILIAN_API_KEY"],
    },
    "token": {
        "name": "Token Plan",
        "provider_id": "bailian-token-plan",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/overview",
        "model_catalog_url": "https://help.aliyun.com/zh/model-studio/token-plan-overview",
        "base_urls": {
            "china": "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.7-max",
            "qwen3.6-plus",
            "qwen3.6-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v3.2",
            "kimi-k2.6",
            "kimi-k2.5",
            "glm-5.1",
            "glm-5",
            "MiniMax-M2.5",
        ],
        "key_env": ["BAILIAN_TOKEN_PLAN_API_KEY", "TOKEN_PLAN_API_KEY", "BAILIAN_API_KEY"],
    },
}

MODEL_DEFAULTS = {
    "qwen3.7-max": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3.6-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3.6-flash": {"contextWindow": 1_000_000, "maxTokens": 32_768},
    "qwen3.5-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-max-2026-01-23": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-coder-next": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-coder-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "deepseek-v4-pro": {"contextWindow": 163_840, "maxTokens": 32_768},
    "deepseek-v4-flash": {"contextWindow": 163_840, "maxTokens": 16_384},
    "deepseek-v3.2": {"contextWindow": 163_840, "maxTokens": 16_384},
    "kimi-k2.6": {"contextWindow": 262_144, "maxTokens": 32_768},
    "kimi-k2.5": {"contextWindow": 262_144, "maxTokens": 16_384},
    "glm-5.1": {"contextWindow": 202_752, "maxTokens": 16_384},
    "glm-5": {"contextWindow": 202_752, "maxTokens": 16_384},
    "glm-4.7": {"contextWindow": 128_000, "maxTokens": 16_384},
    "MiniMax-M2.5": {"contextWindow": 204_800, "maxTokens": 131_072},
}

VISION_MODELS = {
    "qwen3.6-plus",
    "qwen3.6-flash",
    "qwen3.5-plus",
    "qwen3-coder-plus",
    "kimi-k2.6",
    "kimi-k2.5",
}

OPENAI_THINKING_FORMAT_MODELS = {
    "qwen3.7-max",
    "qwen3.6-plus",
    "qwen3.6-flash",
    "qwen3.5-plus",
    "qwen3-max-2026-01-23",
    "qwen3-coder-next",
    "qwen3-coder-plus",
    "deepseek-v3.2",
    "kimi-k2.6",
    "kimi-k2.5",
    "glm-5.1",
    "glm-5",
    "glm-4.7",
}

DINGTALK_PLUGIN_PACKAGE = "@soimy/dingtalk"
DINGTALK_CHANNEL_ID = "dingtalk"


def deep_merge(base, override):
    result = dict(base)
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result


def ordered_unique(items):
    result = []
    for item in items:
        if item and item not in result:
            result.append(item)
    return result


def merge_plugin_allow(existing, merged):
    existing_allow = existing.get("plugins", {}).get("allow", [])
    merged_allow = merged.get("plugins", {}).get("allow")
    if merged_allow is None:
        return merged

    merged.setdefault("plugins", {})["allow"] = ordered_unique(
        [*existing_allow, *merged_allow]
    )
    return merged


def normalize_billing(value):
    key = (value or "payg").strip().lower()
    if key not in BILLING_ALIASES:
        raise SystemExit(
            f"Unsupported billing plan: {value}. Choose payg, coding, or token."
        )
    return BILLING_ALIASES[key]


def normalize_region(value):
    key = (value or "china").strip().lower()
    if key not in REGION_ALIASES:
        raise SystemExit(
            f"Unsupported region: {value}. Choose china or singapore/global."
        )
    return REGION_ALIASES[key]


def strip_provider_prefix(model_id):
    return model_id.split("/", 1)[1] if "/" in model_id else model_id


def resolve_api_key(args, plan):
    if args.api_key_env:
        value = os.environ.get(args.api_key_env, "")
        if not value:
            raise SystemExit(
                f"Environment variable {args.api_key_env} is empty or missing. "
                f"Get the {plan['name']} API key from: {plan['api_key_url']}"
            )
        return value

    candidates = [
        args.api_key,
        args.qwen_api_key,
        args.bailian_api_key,
        args.dashscope_api_key,
        args.modelstudio_api_key,
    ]
    candidates.extend(os.environ.get(name, "") for name in plan["key_env"])
    candidates.extend(
        os.environ.get(name, "")
        for name in [
            "ALIYUN_API_KEY",
            "BAILIAN_API_KEY",
            "DASHSCOPE_API_KEY",
            "QWEN_API_KEY",
            "MODELSTUDIO_API_KEY",
        ]
    )
    for candidate in candidates:
        if candidate:
            return candidate
    env_hint = ", ".join(plan["key_env"])
    raise SystemExit(
        "API key is required. "
        f"Pass --api-key or set one of: {env_hint}. "
        f"Get the {plan['name']} API key from: {plan['api_key_url']}"
    )


def resolve_base_url(args, plan, region):
    if args.base_url:
        return args.base_url
    base_urls = plan["base_urls"]
    if region not in base_urls:
        supported = ", ".join(sorted(base_urls))
        raise SystemExit(
            f"{plan['name']} does not define region {region}. "
            f"Supported regions: {supported}. Use --base-url to override if needed."
        )
    return base_urls[region]


def validate_api_key_for_billing(billing, api_key, plan):
    if billing == "coding" and api_key.startswith("sk-") and not api_key.startswith("sk-sp-"):
        raise SystemExit(
            "The provided API key does not look like a Coding Plan dedicated key. "
            "Use --billing payg for a normal Alibaba Cloud Model Studio/DashScope key, "
            f"or get the Coding Plan key from: {plan['api_key_url']}"
        )


def build_model(model_id, args):
    model_id = strip_provider_prefix(model_id)
    defaults = MODEL_DEFAULTS.get(
        model_id,
        {
            "contextWindow": args.context_window,
            "maxTokens": args.max_tokens,
        },
    )
    model = {
        "id": model_id,
        "name": model_id,
        "reasoning": args.reasoning,
        "input": ["text", "image"] if model_id in VISION_MODELS else ["text"],
        "contextWindow": defaults["contextWindow"],
        "maxTokens": defaults["maxTokens"],
        "cost": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
        },
    }
    if model_id in OPENAI_THINKING_FORMAT_MODELS:
        model["compat"] = {"thinkingFormat": "openai"}
    return model


def build_dingtalk_channel(args):
    return {
        "enabled": True,
        "clientId": args.dingtalk_client_id,
        "clientSecret": args.dingtalk_client_secret,
        "robotCode": args.dingtalk_robot_code or args.dingtalk_client_id,
        "dmPolicy": args.dingtalk_dm_policy,
        "groupPolicy": args.dingtalk_group_policy,
        "messageType": args.dingtalk_message_type,
    }


def build_config(args):
    billing = normalize_billing(args.billing)
    region = normalize_region(args.region)
    plan = BILLING_PLANS[billing]
    provider_id = args.provider_id or plan["provider_id"]
    base_url = resolve_base_url(args, plan, region)
    api_key = resolve_api_key(args, plan)
    validate_api_key_for_billing(billing, api_key, plan)
    model_id = strip_provider_prefix(args.model_id or plan["default_model"])
    model_refs = ordered_unique([model_id, *args.extra_model, *plan["models"]])

    primary_ref = f"{provider_id}/{model_id}"
    config = {
        "models": {
            "mode": "merge",
            "providers": {
                provider_id: {
                    "baseUrl": base_url,
                    "apiKey": api_key,
                    "api": args.provider_api,
                    "models": [build_model(model, args) for model in model_refs],
                },
            },
        },
        "agents": {
            "defaults": {
                "model": {
                    "primary": primary_ref,
                },
                "models": {f"{provider_id}/{model}": {} for model in model_refs},
                "maxConcurrent": args.max_concurrent,
                "subagents": {
                    "maxConcurrent": args.subagent_max_concurrent,
                },
            }
        },
        "commands": {
            "native": "auto",
            "nativeSkills": "auto",
            "restart": True,
            "ownerDisplay": "raw",
        },
        "session": {
            "dmScope": "per-channel-peer",
        },
        "gateway": {
            "mode": "local",
            "bind": "loopback",
        },
    }

    if args.gateway_auth_mode != "keep":
        config["gateway"]["auth"] = {"mode": args.gateway_auth_mode}

    if args.skills_extra_dir:
        config["skills"] = {
            "load": {
                "extraDirs": ordered_unique(args.skills_extra_dir),
            }
        }

    if args.dingtalk_client_id or args.dingtalk_client_secret:
        if not args.dingtalk_client_id or not args.dingtalk_client_secret:
            raise SystemExit(
                "Both --dingtalk-client-id and --dingtalk-client-secret are required when configuring DingTalk."
            )
        config["plugins"] = {
            "enabled": True,
            "allow": [DINGTALK_CHANNEL_ID],
            "entries": {
                DINGTALK_CHANNEL_ID: {
                    "enabled": True,
                }
            },
        }
        config["channels"] = {
            DINGTALK_CHANNEL_ID: build_dingtalk_channel(args),
        }

    return config, {
        "billing": billing,
        "region": region,
        "provider_id": provider_id,
        "base_url": base_url,
        "api": args.provider_api,
        "primary_model": primary_ref,
        "model_id": model_id,
        "api_key": api_key,
        "api_key_url": plan["api_key_url"],
        "model_catalog_url": plan["model_catalog_url"],
    }


def anthropic_messages_url(base_url):
    base = base_url.rstrip("/")
    if base.endswith("/v1/messages"):
        return base
    if base.endswith("/v1"):
        return f"{base}/messages"
    return f"{base}/v1/messages"


def extract_error_message(body):
    if not body:
        return ""
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError:
        return body.strip()[:500]
    error = parsed.get("error") if isinstance(parsed, dict) else None
    if isinstance(error, dict):
        for key in ("message", "msg", "description"):
            if error.get(key):
                return str(error[key])
    for key in ("message", "msg", "description", "request_id"):
        if isinstance(parsed, dict) and parsed.get(key):
            return str(parsed[key])
    return json.dumps(parsed, ensure_ascii=False)[:500]


def preflight_hint(status, metadata):
    plan_name = BILLING_PLANS[metadata["billing"]]["name"]
    if status in (401, 403):
        return (
            "Authentication failed. Check that the API key belongs to "
            f"{plan_name} and matches baseUrl {metadata['base_url']}. "
            f"Get the correct key from: {metadata['api_key_url']}"
        )
    if status == 404:
        return (
            "Endpoint or model was not found. Check --base-url and --model-id; "
            f"model catalog: {metadata['model_catalog_url']}"
        )
    if status == 400:
        return (
            "The endpoint rejected the request shape or model id. Check that "
            f"model {metadata['model_id']} is available for {plan_name}."
        )
    if status == 429:
        return (
            "The endpoint is reachable but returned rate limit or quota errors. "
            "Check quota, billing status, or retry later."
        )
    return "Check API key, baseUrl, model id, and network reachability."


def preflight_model_call(args, metadata):
    if args.skip_preflight:
        print("\n--- Skipping model pre-flight check (--skip-preflight) ---\n")
        return
    if metadata["api"] != "anthropic-messages":
        print(
            "\n--- Skipping model pre-flight check "
            f"(unsupported provider api: {metadata['api']}) ---\n"
        )
        return

    print("\n--- Model endpoint pre-flight check ---\n")
    url = anthropic_messages_url(metadata["base_url"])
    payload = {
        "model": metadata["model_id"],
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    }
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": "install-openclaw-preflight/1.0",
            "anthropic-version": "2023-06-01",
            "x-api-key": metadata["api_key"],
        },
    )

    print(f"  endpoint={url}")
    print(f"  model={metadata['model_id']}")
    try:
        with urllib.request.urlopen(request, timeout=args.preflight_timeout) as response:
            response.read(1024)
    except urllib.error.HTTPError as exc:
        error_body = exc.read().decode("utf-8", errors="replace")
        message = extract_error_message(error_body)
        hint = preflight_hint(exc.code, metadata)
        detail = f"\n  provider message: {message}" if message else ""
        raise SystemExit(
            "Model pre-flight check failed before writing OpenClaw config.\n"
            f"  HTTP {exc.code} from {url}{detail}\n"
            f"  hint: {hint}\n"
            "Fix the values or rerun with --skip-preflight only if you want "
            "to defer validation to OpenClaw Gateway startup."
        ) from exc
    except (urllib.error.URLError, TimeoutError, socket.timeout) as exc:
        reason = getattr(exc, "reason", exc)
        raise SystemExit(
            "Model pre-flight check failed before writing OpenClaw config.\n"
            f"  endpoint: {url}\n"
            f"  error: {reason}\n"
            "Check --base-url, DNS/proxy/network access, or rerun with "
            "--skip-preflight to defer validation."
        ) from exc

    print("  [OK] API key, baseUrl, and model id accepted by endpoint")


def apply_config(config, config_path, *, dry_run=False):
    print("\n--- Writing OpenClaw config ---\n")
    if dry_run:
        print(f"  # dry-run: would write OpenClaw config to {config_path}")
        for key in config:
            print(f"  [OK] {key}")
        return

    existing = {}
    if config_path.exists():
        try:
            with config_path.open("r", encoding="utf-8") as fh:
                existing = json.load(fh)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"Invalid JSON in {config_path}: {exc}") from exc

    merged = deep_merge(existing, config)
    merged = merge_plugin_allow(existing, merged)

    config_path.parent.mkdir(parents=True, exist_ok=True)
    if config_path.exists():
        backup_path = config_path.with_name(config_path.name + ".bak")
        backup_path.write_bytes(config_path.read_bytes())

    tmp_path = config_path.with_name(config_path.name + ".tmp")
    with tmp_path.open("w", encoding="utf-8") as fh:
        json.dump(merged, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    os.replace(tmp_path, config_path)

    for key in config:
        print(f"  [OK] {key}")

    print(f"\nConfig written: {config_path}")


def run_command(cmd, *, env=None, dry_run=False, check=True, timeout=None, capture_output=False):
    print("  $ " + " ".join(cmd), flush=True)
    if dry_run:
        return subprocess.CompletedProcess(cmd, 0)
    return subprocess.run(
        cmd,
        env=env,
        check=check,
        timeout=timeout,
        capture_output=capture_output,
        text=capture_output,
    )


def find_command(name):
    search_dirs = os.environ.get("PATH", "").split(os.pathsep)
    search_dirs.extend(["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])
    for directory in search_dirs:
        if not directory:
            continue
        candidate = Path(directory) / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return ""


def command_exists(name):
    return bool(find_command(name))


def command_output(cmd):
    try:
        result = subprocess.run(
            cmd,
            check=False,
            capture_output=True,
            text=True,
            timeout=8,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return ""
    return (result.stdout or result.stderr or "").strip()


def node_major():
    value = command_output(["node", "-p", "Number(process.versions.node.split('.')[0])"])
    try:
        return int(value)
    except ValueError:
        return 0


def sudo_command(cmd):
    if os.geteuid() == 0:
        return cmd
    if not command_exists("sudo"):
        raise SystemExit("sudo is required for package installation.")
    return ["sudo", *cmd]


def has_package_manager():
    return command_exists("dnf") or command_exists("yum")


def dependency_precheck(args):
    print("\n--- Dependency precheck ---\n")

    issues = []
    actions = []

    node_ok = command_exists("node") and node_major() >= 22
    npm_ok = command_exists("npm")
    openclaw_ok = command_exists("openclaw") and not args.force_install_openclaw
    can_sudo = os.geteuid() == 0 or command_exists("sudo")

    node_status = (
        f"{command_output(['node', '--version'])} major={node_major()}"
        if command_exists("node")
        else "missing"
    )
    npm_status = command_output(["npm", "--version"]) if command_exists("npm") else "missing"
    openclaw_status = (
        command_output(["openclaw", "--version"]) if command_exists("openclaw") else "missing"
    )
    package_manager = "dnf" if command_exists("dnf") else "yum" if command_exists("yum") else "missing"

    print(f"  python={sys.version.split()[0]}")
    print(f"  node={node_status}")
    print(f"  npm={npm_status}")
    print(f"  openclaw={openclaw_status}")
    print(f"  package_manager={package_manager}")
    print(f"  sudo={'available' if can_sudo else 'missing'}")
    print(f"  npm_registry={args.npm_registry or 'default'}")

    if args.skip_install_openclaw:
        print("  install_openclaw=skipped")
    else:
        if node_ok and npm_ok:
            print("  node_npm=ok")
        elif args.no_install_node:
            issues.append("Node.js v22+ and npm are required, but --no-install-node was passed.")
        elif not has_package_manager():
            issues.append("Node.js/npm are missing or too old, and no dnf/yum package manager was found.")
        elif not can_sudo:
            issues.append("Node.js/npm installation needs root or sudo.")
        else:
            actions.append("Install Node.js/npm with dnf/yum.")

        npm_available_after_actions = npm_ok or any("Node.js/npm" in action for action in actions)
        if openclaw_ok:
            print("  openclaw_cli=ok")
        elif not npm_available_after_actions:
            issues.append("OpenClaw is missing and npm is not available to install it.")
        elif not can_sudo:
            issues.append("OpenClaw global npm installation needs root or sudo.")
        else:
            actions.append(f"Install {args.openclaw_package} with npm.")

    if not args.skip_gateway:
        if command_exists("ps"):
            print("  ps=ok")
        else:
            issues.append("ps is required for gateway port ownership checks.")
        if command_exists("fuser") or command_exists("lsof"):
            print("  port_tools=ok")
        else:
            print("  port_tools=missing; stale gateway port inspection will be limited")

    if actions:
        print("\n  Planned automatic fixes:")
        for action in actions:
            print(f"  - {action}")

    if issues:
        print("\n  Missing dependencies that cannot be fixed automatically:")
        for issue in issues:
            print(f"  - {issue}")
        raise SystemExit("Dependency precheck failed. Fix the items above and rerun.")

    print("\nDependency precheck passed.")


def print_api_key_guidance(args):
    billing = normalize_billing(args.billing)
    plan = BILLING_PLANS[billing]
    print("\nNext step:")
    print(f"  Get the {plan['name']} API key from: {plan['api_key_url']}")
    print("  Then rerun with --api-key or --api-key-env.")


def install_node_with_system_package_manager(args):
    if command_exists("dnf"):
        run_command(
            sudo_command(["dnf", "install", "-y", "nodejs", "nodejs-npm"]),
            dry_run=args.dry_run,
        )
        return
    if command_exists("yum"):
        run_command(
            sudo_command(["yum", "install", "-y", "nodejs", "nodejs-npm"]),
            dry_run=args.dry_run,
        )
        return
    raise SystemExit("Node.js is missing and no dnf/yum package manager was found.")


def ensure_node_and_npm(args):
    print("\n--- Checking Node.js and npm ---\n")
    if command_exists("node"):
        print(f"  node={command_output(['node', '--version']) or 'unknown'} major={node_major()}")
    else:
        print("  node=missing")

    if command_exists("npm"):
        print(f"  npm={command_output(['npm', '--version']) or 'unknown'}")
    else:
        print("  npm=missing")

    if node_major() >= 22 and command_exists("npm"):
        print("  [OK] Node.js/npm requirement satisfied")
        return

    if args.no_install_node:
        raise SystemExit("Node.js v22+ and npm are required.")

    print("  # installing Node.js/npm with system package manager")
    install_node_with_system_package_manager(args)
    if args.dry_run:
        print("  # dry-run: skipped Node.js/npm post-install verification")
        return

    if not command_exists("node") or not command_exists("npm"):
        raise SystemExit("Node.js/npm is still missing after package installation.")
    if node_major() < 22:
        raise SystemExit(f"Node.js v22+ is required, found {command_output(['node', '--version'])}.")
    print("  [OK] Node.js/npm installed")


def npm_install_openclaw(args):
    env = os.environ.copy()
    if args.npm_registry:
        env["NPM_CONFIG_REGISTRY"] = args.npm_registry

    if os.geteuid() == 0:
        run_command(
            ["npm", "install", "-g", args.openclaw_package],
            env=env,
            dry_run=args.dry_run,
        )
        return

    if not command_exists("sudo"):
        raise SystemExit("sudo is required for global npm install.")

    sudo_env = []
    if args.npm_registry:
        sudo_env.append(f"NPM_CONFIG_REGISTRY={args.npm_registry}")
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]:
        value = os.environ.get(name)
        if value:
            sudo_env.append(f"{name}={value}")

    run_command(
        ["sudo", "env", *sudo_env, "npm", "install", "-g", args.openclaw_package],
        dry_run=args.dry_run,
    )


def ensure_openclaw(args):
    print("\n--- Checking OpenClaw ---\n")
    if command_exists("openclaw"):
        print(f"  openclaw={command_output(['openclaw', '--version']) or 'unknown'}")
    else:
        print("  openclaw=missing")

    if command_exists("openclaw") and not args.force_install_openclaw:
        print("  [OK] OpenClaw requirement satisfied")
        return

    if not command_exists("npm"):
        raise SystemExit("npm is required to install OpenClaw.")

    print("  # installing OpenClaw")
    npm_install_openclaw(args)
    if args.dry_run:
        print("  # dry-run: skipped OpenClaw post-install verification")
        return

    if not command_exists("openclaw"):
        raise SystemExit("openclaw is still missing after npm install.")
    print(f"  [OK] OpenClaw installed: {command_output(['openclaw', '--version']) or 'unknown'}")


def tail_file(path, max_bytes=4000):
    if not path.exists():
        return ""
    with path.open("rb") as fh:
        try:
            fh.seek(-max_bytes, os.SEEK_END)
        except OSError:
            fh.seek(0)
        return fh.read().decode("utf-8", errors="replace")


def gateway_port_listeners(args):
    pids = []
    fuser = find_command("fuser")
    if fuser:
        fuser_cmd = [fuser, "-v", "-n", "tcp", str(args.gateway_port)]
    else:
        fuser_cmd = []
    try:
        result = (
            run_command(
                fuser_cmd,
                check=False,
                timeout=args.gateway_status_timeout,
                capture_output=True,
            )
            if fuser_cmd
            else None
        )
    except FileNotFoundError:
        result = None

    output = ""
    if result is not None:
        output = (result.stdout or "") + (result.stderr or "")
    for token in output.split():
        if token.isdigit() and token not in pids:
            pids.append(token)
    if pids:
        return pids

    lsof = find_command("lsof")
    if not lsof:
        if fuser:
            print("  # no listener found by fuser; lsof not found")
        else:
            print("  # neither fuser nor lsof found; skipped stale gateway port inspection")
        return []
    try:
        result = run_command(
            [lsof, "-tiTCP:" + str(args.gateway_port), "-sTCP:LISTEN"],
            check=False,
            timeout=args.gateway_status_timeout,
            capture_output=True,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return []
    for token in ((result.stdout or "") + (result.stderr or "")).split():
        if token.isdigit() and token not in pids:
            pids.append(token)
    return pids


def process_command(pid):
    try:
        result = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return ""
    return (result.stdout or "").strip()


def clear_openclaw_gateway_port(args):
    if args.dry_run:
        return

    pids = gateway_port_listeners(args)
    if not pids:
        return

    non_openclaw = []
    for pid in pids:
        command = process_command(pid)
        if "openclaw" not in command.lower():
            non_openclaw.append((pid, command or "unknown"))

    if non_openclaw:
        print(f"Port {args.gateway_port} is already in use by a non-OpenClaw process:")
        for pid, command in non_openclaw:
            print(f"  pid {pid}: {command}")
        raise SystemExit(
            "Stop that process or rerun with --gateway-port to choose another port."
        )

    for pid in pids:
        command = process_command(pid)
        print(f"  # stopping stale OpenClaw listener pid {pid}: {command}")
        try:
            os.kill(int(pid), 15)
        except ProcessLookupError:
            pass
    time.sleep(1)


def print_gateway_logs(args):
    try:
        result = run_command(
            ["journalctl", "--user", "-u", "openclaw-gateway.service", "-n", "120", "--no-pager"],
            check=False,
            timeout=args.gateway_status_timeout,
            capture_output=True,
        )
        output = (result.stdout or "") + (result.stderr or "")
        if output.strip():
            print(output.strip())
            return
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    log_path = Path(args.gateway_log).expanduser()
    if log_path.exists():
        print(tail_file(log_path))


def wait_gateway_ready(args):
    if args.dry_run:
        return

    deadline = time.monotonic() + args.gateway_ready_timeout
    last_output = ""
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(
                ("127.0.0.1", args.gateway_port),
                timeout=min(2, args.gateway_status_timeout),
            ):
                print("Gateway port is listening.")
                return
        except OSError as exc:
            last_output = str(exc)

        time.sleep(2)

    print("Gateway port did not listen before timeout. Recent socket output:")
    print(last_output.strip() or "(no status output)")
    try:
        result = run_command(
            ["openclaw", "gateway", "status", "--deep"],
            check=False,
            timeout=args.gateway_status_timeout,
            capture_output=True,
        )
        status_output = (result.stdout or "") + (result.stderr or "")
        if status_output.strip():
            print("Recent gateway status:")
            print(status_output.strip())
    except subprocess.TimeoutExpired:
        print("openclaw gateway status --deep timed out")
    print("Recent gateway service logs:")
    print_gateway_logs(args)


def completed_output(result):
    return (result.stdout or "") + (result.stderr or "")


def gateway_smoke_succeeded(result):
    output = completed_output(result).lower()
    blocked_markers = [
        "embedded fallback",
        "missing scope",
        "pairing required",
        "scope upgrade pending approval",
        "gateway disconnected",
    ]
    return result.returncode == 0 and not any(marker in output for marker in blocked_markers)


def gateway_scope_blocked(result):
    output = completed_output(result).lower()
    return any(
        marker in output
        for marker in [
            "missing scope: operator.write",
            "pairing required",
            "scope upgrade pending approval",
        ]
    )


def device_auth_path(args):
    return Path(args.config).expanduser().parent / "identity" / "device-auth.json"


def paired_devices_path(args):
    return Path(args.config).expanduser().parent / "devices" / "paired.json"


def has_write_scope(scopes):
    return "operator.write" in scopes or "operator.admin" in scopes


def device_operator_scopes(device):
    scopes = []
    for key in ("scopes", "approvedScopes"):
        value = device.get(key)
        if isinstance(value, list):
            scopes.extend(value)
    tokens = device.get("tokens")
    if isinstance(tokens, dict):
        operator = tokens.get("operator")
        if isinstance(operator, dict) and isinstance(operator.get("scopes"), list):
            scopes.extend(operator["scopes"])
    return scopes


def clear_read_only_operator_pairings(args):
    path = paired_devices_path(args)
    if not path.exists():
        return
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return
    if not isinstance(data, dict):
        return

    changed = False
    for device_id, device in list(data.items()):
        if not isinstance(device, dict):
            continue
        roles = device.get("roles") if isinstance(device.get("roles"), list) else []
        is_operator = device.get("role") == "operator" or "operator" in roles
        if is_operator and not has_write_scope(device_operator_scopes(device)):
            del data[device_id]
            changed = True

    if changed:
        tmp_path = path.with_name(path.name + ".tmp")
        with tmp_path.open("w", encoding="utf-8") as fh:
            json.dump(data, fh, indent=2, ensure_ascii=False)
            fh.write("\n")
        os.replace(tmp_path, path)
        print(f"  cleared read-only operator pairing: {path}")


def cached_operator_has_write_scope(args):
    path = device_auth_path(args)
    if not path.exists():
        return False
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False

    scopes = []
    if isinstance(data.get("tokens"), dict):
        operator = data["tokens"].get("operator")
        if isinstance(operator, dict) and isinstance(operator.get("scopes"), list):
            scopes = operator["scopes"]
    elif data.get("role") == "operator" and isinstance(data.get("scopes"), list):
        scopes = data["scopes"]
    return has_write_scope(scopes)


def clear_cached_operator_device_auth(args):
    path = device_auth_path(args)
    if not path.exists():
        return
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        backup = path.with_name(path.name + ".bak")
        backup.write_bytes(path.read_bytes())
        path.unlink()
        print(f"  cleared unreadable cached device auth: {path}")
        return

    changed = False
    if isinstance(data.get("tokens"), dict) and "operator" in data["tokens"]:
        del data["tokens"]["operator"]
        changed = True
    elif data.get("role") == "operator":
        path.unlink()
        print(f"  cleared cached operator device auth: {path}")
        return

    if changed:
        tmp_path = path.with_name(path.name + ".tmp")
        with tmp_path.open("w", encoding="utf-8") as fh:
            json.dump(data, fh, indent=2, ensure_ascii=False)
            fh.write("\n")
        os.replace(tmp_path, path)
        print(f"  cleared cached operator device auth: {path}")


def run_gateway_write_smoke(args, timeout):
    cmd = ["openclaw", "agent", "--message", "hello", "--agent", "main"]
    try:
        return run_command(
            cmd,
            check=False,
            timeout=timeout,
            capture_output=True,
        )
    except subprocess.TimeoutExpired as exc:
        output = exc.output or ""
        stderr = exc.stderr or f"Gateway write-scope smoke test timed out after {timeout}s"
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        return subprocess.CompletedProcess(cmd, 124, stdout=output, stderr=stderr)


def print_smoke_output(result):
    output = completed_output(result).strip()
    if output:
        print(output)


def ensure_gateway_write_scope(args):
    if args.dry_run:
        return
    if args.skip_gateway_write_check:
        print("\n--- Skipping gateway write-scope check (--skip-gateway-write-check) ---\n")
        return

    print("\n--- Verifying Gateway operator write scope ---\n")

    if not cached_operator_has_write_scope(args):
        clear_cached_operator_device_auth(args)
        clear_read_only_operator_pairings(args)

    deadline = time.monotonic() + args.gateway_write_check_timeout
    result = subprocess.CompletedProcess([], 1, stdout="", stderr="smoke test not run")
    pairing_reset = False

    while time.monotonic() < deadline:
        remaining = max(1, int(deadline - time.monotonic()))
        result = run_gateway_write_smoke(args, timeout=min(30, remaining))
        if gateway_smoke_succeeded(result):
            print("Gateway write-scope smoke test passed.")
            return

        if gateway_scope_blocked(result) and not pairing_reset:
            print("Gateway write scope is blocked by stale local operator pairing; resetting it.")
            clear_cached_operator_device_auth(args)
            clear_read_only_operator_pairings(args)
            pairing_reset = True

        if time.monotonic() + 2 >= deadline:
            break
        time.sleep(2)

    print("Gateway write-scope smoke test failed. Recent output:")
    print_smoke_output(result)
    raise SystemExit(
        "OpenClaw Gateway port is listening, but the local operator device cannot "
        "send messages before the write-scope check timeout."
    )


def install_openclaw(args):
    ensure_node_and_npm(args)
    ensure_openclaw(args)


def find_tokenless_adapter_dir():
    """Locate the tokenless adapter directory containing agent plugin install scripts."""
    candidates = [
        os.environ.get("ANOLISA_TOKENLESS_ADAPTER_DIR", ""),
        "/usr/share/anolisa/adapters/tokenless",
        os.path.expanduser("~/.local/share/anolisa/adapters/tokenless"),
    ]
    # Also try relative to the script's own location (source tree layout)
    script_dir = Path(__file__).resolve().parent
    source_candidate = script_dir / ".." / ".." / ".." / ".." / "tokenless" / "adapters" / "tokenless"
    candidates.append(str(source_candidate))

    for candidate in candidates:
        if not candidate:
            continue
        p = Path(candidate)
        if p.is_dir() and (p / "manifest.json").is_file():
            return p
    return None


def install_tokenless_plugin(args):
    """Install the tokenless OpenClaw plugin via the tokenless adapter install script."""
    if args.skip_tokenless:
        print("\n--- Skipping tokenless plugin (--skip-tokenless) ---\n")
        return

    print("\n--- Installing tokenless OpenClaw plugin ---\n")

    adapter_dir = find_tokenless_adapter_dir()
    if adapter_dir is None:
        print("  tokenless adapter not found — skipping tokenless plugin installation.")
        print("  Install tokenless first, then run:")
        print("    make -C src/tokenless openclaw-install")
        return

    install_script = adapter_dir / "openclaw" / "scripts" / "install.sh"
    if not install_script.is_file():
        print(f"  tokenless install script not found: {install_script}")
        return

    print(f"  tokenless adapter: {adapter_dir}")
    env = os.environ.copy()
    env["ANOLISA_ADAPTER_DIR"] = str(adapter_dir)
    env["ANOLISA_COMPONENT"] = "tokenless"
    env["ANOLISA_TARGET"] = "openclaw"
    try:
        run_command(
            ["bash", str(install_script)],
            env=env,
            dry_run=args.dry_run,
        )
    except subprocess.CalledProcessError as exc:
        print(f"  tokenless plugin installation failed: {exc}")
        print("  You can install it later with:")
        print(f"    ANOLISA_ADAPTER_DIR={adapter_dir} bash {install_script}")


def install_dingtalk_plugin(args):
    env = os.environ.copy()
    if args.npm_registry:
        env["NPM_CONFIG_REGISTRY"] = args.npm_registry
    run_command(
        ["openclaw", "plugins", "install", DINGTALK_PLUGIN_PACKAGE],
        env=env,
        dry_run=args.dry_run,
    )


def start_gateway(args):
    if args.doctor_fix:
        run_command(
            ["openclaw", "doctor", "--fix"],
            dry_run=args.dry_run,
            check=False,
            timeout=args.gateway_command_timeout,
        )
    run_command(
        ["openclaw", "gateway", "stop"],
        dry_run=args.dry_run,
        check=False,
        timeout=args.gateway_command_timeout,
    )
    clear_openclaw_gateway_port(args)
    run_command(
        ["openclaw", "gateway", "install", "--port", str(args.gateway_port)],
        dry_run=args.dry_run,
        check=False,
        timeout=args.gateway_command_timeout,
    )
    run_command(
        ["openclaw", "gateway", "restart"],
        dry_run=args.dry_run,
        check=False,
        timeout=args.gateway_command_timeout,
    )
    wait_gateway_ready(args)
    ensure_gateway_write_scope(args)


def print_summary(metadata, args):
    print("\nSelected Alibaba Cloud Model Studio plan:")
    print(f"  billing: {metadata['billing']}")
    print(f"  region: {metadata['region']}")
    print(f"  provider: {metadata['provider_id']}")
    print(f"  api: {metadata['api']}")
    print(f"  baseUrl: {metadata['base_url']}")
    print(f"  primary: {metadata['primary_model']}")
    print(f"  API key source: {metadata['api_key_url']}")
    print(f"  model catalog: {metadata['model_catalog_url']}")

    print("\nUseful checks:")
    print("  openclaw models list")
    print('  openclaw agent --message "hello" --agent main')
    print("  openclaw gateway health")
    print("  openclaw status")
    print("  # If agent output says EMBEDDED FALLBACK, rerun the installer or fix gateway.")
    print("\nFirst real task tip:")
    print("  openclaw onboard --skip-bootstrap")
    print(
        "  # Run this if you want first real tasks to start without the "
        "BOOTSTRAP.md introduction flow interrupting them."
    )
    print(
        "  # You can still fill in or adjust IDENTITY.md, USER.md, and SOUL.md later "
        "under the OpenClaw workspace."
    )
    if args.dingtalk_client_id and args.dingtalk_client_secret:
        print("  openclaw channels status --probe")


def parse_args():
    parser = argparse.ArgumentParser(
        description="Install and configure OpenClaw non-interactively for Alibaba Cloud Model Studio."
    )

    parser.add_argument("--config", default=str(DEFAULT_CONFIG_PATH))
    parser.add_argument(
        "--billing",
        default="payg",
        choices=sorted(BILLING_ALIASES),
        help="Alibaba Cloud Model Studio billing mode.",
    )
    parser.add_argument("--region", default="china")
    parser.add_argument("--provider-id", default="")
    parser.add_argument("--base-url", default="")
    parser.add_argument("--provider-api", default="anthropic-messages")
    parser.add_argument("--model-id", default="")
    parser.add_argument("--extra-model", action="append", default=[])

    parser.add_argument("--api-key", default="")
    parser.add_argument(
        "--api-key-env",
        default="",
        help="Read API key from the named environment variable.",
    )
    parser.add_argument("--bailian-api-key", default="")
    parser.add_argument("--qwen-api-key", default="")
    parser.add_argument("--modelstudio-api-key", default="")
    parser.add_argument("--dashscope-api-key", default="")

    parser.add_argument("--context-window", type=int, default=1_000_000)
    parser.add_argument("--max-tokens", type=int, default=65_536)
    parser.add_argument("--reasoning", action="store_true")
    parser.add_argument("--max-concurrent", type=int, default=4)
    parser.add_argument("--subagent-max-concurrent", type=int, default=8)
    parser.add_argument(
        "--gateway-auth-mode",
        default="none",
        choices=["none", "token", "keep"],
        help="Use none for local single-machine setup; use keep to preserve existing gateway.auth.",
    )
    parser.add_argument("--skills-extra-dir", action="append", default=[])

    parser.add_argument("--skip-install-openclaw", action="store_true")
    parser.add_argument("--force-install-openclaw", action="store_true")
    parser.add_argument("--no-install-node", action="store_true")
    parser.add_argument("--openclaw-package", default="openclaw@latest")
    parser.add_argument(
        "--npm-registry",
        default=os.environ.get("NPM_CONFIG_REGISTRY", "https://registry.npmmirror.com"),
    )
    parser.add_argument("--install-dingtalk-plugin", action="store_true")
    parser.add_argument("--skip-gateway", action="store_true")
    parser.add_argument("--gateway-port", type=int, default=18789)
    parser.add_argument("--gateway-command-timeout", type=int, default=30)
    parser.add_argument("--gateway-ready-timeout", type=int, default=30)
    parser.add_argument("--gateway-status-timeout", type=int, default=8)
    parser.add_argument("--gateway-write-check-timeout", type=int, default=60)
    parser.add_argument(
        "--skip-gateway-write-check",
        action="store_true",
        help="Skip the post-start local operator write-scope smoke test.",
    )
    parser.add_argument(
        "--gateway-log",
        default=str(Path("~/.openclaw/setup-gateway-start.log").expanduser()),
    )
    parser.add_argument("--doctor-fix", action="store_true")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help=(
            "Preview the full flow without local changes. Model pre-flight "
            "still runs unless --skip-preflight is set."
        ),
    )
    parser.add_argument(
        "--precheck-only",
        action="store_true",
        help="Only check basic environment dependencies; do not require API keys or install anything.",
    )
    parser.add_argument(
        "--skip-tokenless",
        action="store_true",
        help="Do not install the tokenless OpenClaw plugin after OpenClaw installation.",
    )
    parser.add_argument(
        "--skip-preflight",
        action="store_true",
        help="Skip the model endpoint API check before writing config and starting Gateway.",
    )
    parser.add_argument(
        "--preflight-timeout",
        type=int,
        default=20,
        help="Seconds to wait for the model endpoint pre-flight API call.",
    )

    parser.add_argument("--dingtalk-client-id", default="")
    parser.add_argument("--dingtalk-client-secret", default="")
    parser.add_argument("--dingtalk-robot-code", default="")
    parser.add_argument("--dingtalk-dm-policy", default="open")
    parser.add_argument("--dingtalk-group-policy", default="open")
    parser.add_argument("--dingtalk-message-type", default="markdown")

    return parser.parse_args()


def main():
    args = parse_args()

    dependency_precheck(args)
    if args.precheck_only:
        print_api_key_guidance(args)
        return

    config, metadata = build_config(args)
    preflight_model_call(args, metadata)

    if not args.skip_install_openclaw:
        install_openclaw(args)
        install_tokenless_plugin(args)

    apply_config(config, Path(args.config).expanduser(), dry_run=args.dry_run)

    if args.install_dingtalk_plugin:
        install_dingtalk_plugin(args)

    if not args.skip_gateway:
        start_gateway(args)

    print_summary(metadata, args)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as exc:
        print(f"Command failed with exit code {exc.returncode}: {exc.cmd}", file=sys.stderr)
        raise SystemExit(exc.returncode)
