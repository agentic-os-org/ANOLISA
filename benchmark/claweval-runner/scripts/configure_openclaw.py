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

"""Configure openclaw settings.
   
Sets:
  1. contextWindow = 256000 on all model definitions
  2. reserveTokensFloor = 40000 under agents.defaults.compaction (env: CE_RESERVE_TOKENS)
  3. heartbeat disabled (every = "0m")
  4. temperature = 0 under agents.defaults.params
  5. "image" added to model input arrays (idempotent — safe to call multiple times)
  6. gateway.http.endpoints.chatCompletions.enabled = true
  7. images.allowUrl = true with standard allowedMimes
  8. agents.defaults.skipBootstrap = true
  9. gateway.port = 18789 (if not configured)
"""

import json
import os
from pathlib import Path

OPENCLAW_CONFIG = Path.home() / ".openclaw" / "openclaw.json"

_ALLOWED_IMAGE_MIMES = ["image/jpeg", "image/png", "image/gif", "image/webp"]


def main() -> None:
    with open(OPENCLAW_CONFIG) as f:
        config = json.load(f)

    changes: list[str] = []

    # 1. Set contextWindow = 256000 on all model definitions
    providers = config.setdefault("models", {}).setdefault("providers", {})
    for provider_name, provider in providers.items():
        models = provider.get("models", [])
        for model_entry in models:
            old_cw = model_entry.get("contextWindow")
            model_entry["contextWindow"] = 256000
            model_id = model_entry.get("id", "?")
            changes.append(
                f"  models.providers.{provider_name}.models[{model_id}].contextWindow: "
                f"{old_cw} -> 256000"
            )

    # 2. Set reserveTokensFloor = 256000 under agents.defaults.compaction
    defaults = config.setdefault("agents", {}).setdefault("defaults", {})
    compaction = defaults.setdefault("compaction", {})
    reserve_tokens = int(os.environ.get("CE_RESERVE_TOKENS", "40000"))
    old_rtf = compaction.get("reserveTokensFloor", "not set")
    compaction["reserveTokensFloor"] = reserve_tokens
    changes.append(
        f"  agents.defaults.compaction.reserveTokensFloor: {old_rtf} -> {reserve_tokens}"
    )

    # 3. Disable heartbeat
    old_hb = defaults.get("heartbeat", {}).get("every", "not set") if "heartbeat" in defaults else "not set"
    defaults["heartbeat"] = {"every": "0m"}
    changes.append(f"  agents.defaults.heartbeat.every: {old_hb} -> '0m'")

    # 4. Set temperature = 0
    params = defaults.setdefault("params", {})
    old_temp = params.get("temperature", "not set")
    params["temperature"] = 0
    changes.append(f"  agents.defaults.params.temperature: {old_temp} -> 0")

    # 5. Add "image" to model input arrays (idempotent)
    for provider_name, provider in providers.items():
        models = provider.get("models", [])
        for model_entry in models:
            model_input = model_entry.setdefault("input", ["text"])
            if "image" not in model_input:
                model_input.append("image")
                model_id = model_entry.get("id", "?")
                changes.append(
                    f"  models.providers.{provider_name}.models[{model_id}].input: "
                    f"added 'image'"
                )

    # 6. Enable chatCompletions endpoint
    gateway = config.setdefault("gateway", {})
    http = gateway.setdefault("http", {})
    endpoints = http.setdefault("endpoints", {})
    cc = endpoints.setdefault("chatCompletions", {})
    old_cc = cc.get("enabled", "not set")
    cc["enabled"] = True
    changes.append(f"  gateway.http.endpoints.chatCompletions.enabled: {old_cc} -> true")

    # 7. Enable images.allowUrl with allowedMimes
    cc_images = cc.setdefault("images", {})
    old_allow = cc_images.get("allowUrl", "not set")
    cc_images["allowUrl"] = True
    old_mimes = cc_images.get("allowedMimes", "not set")
    cc_images["allowedMimes"] = _ALLOWED_IMAGE_MIMES
    changes.append(f"  gateway.http.endpoints.chatCompletions.images.allowUrl: {old_allow} -> true")
    changes.append(f"  gateway.http.endpoints.chatCompletions.images.allowedMimes: "
                   f"{old_mimes} -> {_ALLOWED_IMAGE_MIMES}")

    # 8. Set agents.defaults.skipBootstrap = true
    old_sb = defaults.get("skipBootstrap", "not set")
    defaults["skipBootstrap"] = True
    changes.append(f"  agents.defaults.skipBootstrap: {old_sb} -> true")

    # 9. Enable /mcp and /plugins commands
    commands = config.setdefault("commands", {})
    old_mcp = commands.get("mcp", "not set")
    commands["mcp"] = True
    changes.append(f"  commands.mcp: {old_mcp} -> true")
    old_plugins = commands.get("plugins", "not set")
    commands["plugins"] = True
    changes.append(f"  commands.plugins: {old_plugins} -> true")

    # 10. Configure gateway port if not set
    gateway = config.setdefault("gateway", {})
    old_port = gateway.get("port", "not set")
    if old_port == "not set":
        gateway["port"] = 18789
        changes.append(f"  gateway.port: {old_port} -> 18789")
    else:
        changes.append(f"  gateway.port: {old_port} (already configured)")

    # Write back
    with open(OPENCLAW_CONFIG, "w") as f:
        json.dump(config, f, indent=2, ensure_ascii=False)

    print(f"Updated {OPENCLAW_CONFIG}:")
    for c in changes:
        print(c)


if __name__ == "__main__":
    main()
