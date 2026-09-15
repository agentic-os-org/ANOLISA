#!/usr/bin/env bash
# install.sh — Register tokenless hooks in the Kimi config (config.toml).
#
# Kimi uses a flat TOML config with [[hooks]] entries rather than a plugin
# manifest. This script injects hook definitions that point to the shared
# tokenless hooks via the run-hook.sh dispatcher, into the data root the
# installed Kimi CLI actually reads (see _common.sh).
set -euo pipefail

AGENT="${ANOLISA_TARGET:-kimicode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

# Shared Kimi data-root resolution: Kimi Code reads ~/.kimi-code (KIMI_CODE_HOME)
# while the wound-down kimi-cli read ~/.kimi (KIMI_SHARE_DIR). See _common.sh.
SCRIPT_DIR="${BASH_SOURCE[0]%/*}"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"

KIMI_HOME="$(resolve_kimi_home)"
CONFIG_FILE="${KIMI_HOME}/config.toml"

DRY_RUN="${ANOLISA_DRY_RUN:-0}"
FORCE_INSTALL="${ANOLISA_FORCE_INSTALL:-0}"

echo "[${COMPONENT}] Installing ${AGENT} adapter..."

# Short-circuit when the Kimi CLI is missing so `make setup` does not create
# config files for users who have not installed Kimi Code. Can be overridden
# with ANOLISA_FORCE_INSTALL=1 for packaging scenarios.
if [ "$FORCE_INSTALL" != "1" ] && ! command -v kimi &>/dev/null; then
    echo "[${COMPONENT}] kimi CLI not found — skipping ${AGENT} adapter installation."
    echo "[${COMPONENT}] Install Kimi Code first, or re-run with ANOLISA_FORCE_INSTALL=1."
    exit 0
fi

echo "[${COMPONENT}] kimi data root: ${KIMI_HOME} ($(resolve_kimi_home_origin))"

if [ ! -d "$KIMI_HOME" ]; then
    mkdir -p "$KIMI_HOME"
    echo "[${COMPONENT}] created ${KIMI_HOME}"
fi

# Determine the absolute path to the Kimi-specific tool-ready wrapper.
# The wrapper translates the shared tool_ready_hook.sh output into Kimi Code's
# PreToolUse protocol (permissionDecision=deny + exit 2 on block).
HOOK_WRAPPER="${ADAPTER_DIR}/kimicode/hooks/tool-ready-kimi-wrapper.sh"
HOOK_DISPATCHER="${ADAPTER_DIR}/kimicode/hooks/run-hook.sh"
if [ ! -f "$HOOK_WRAPPER" ]; then
    echo "[${COMPONENT}] ERROR: hook wrapper not found: $HOOK_WRAPPER" >&2
    echo "[${COMPONENT}]        Ensure the kimicode adapter directory is intact." >&2
    exit 1
fi
if [ ! -f "$HOOK_DISPATCHER" ]; then
    echo "[${COMPONENT}] ERROR: hook dispatcher not found: $HOOK_DISPATCHER" >&2
    echo "[${COMPONENT}]        Ensure the kimicode adapter directory is intact." >&2
    exit 1
fi

# Make wrapper/dispatcher executable (check first to avoid EPERM in RPM scenarios)
[ -x "$HOOK_WRAPPER" ] || chmod +x "$HOOK_WRAPPER"
[ -x "$HOOK_DISPATCHER" ] || chmod +x "$HOOK_DISPATCHER"

# Convert to absolute path for TOML embedding
HOOK_WRAPPER_ABS="$(cd "$(dirname "$HOOK_WRAPPER")" && pwd)/$(basename "$HOOK_WRAPPER")"

# Define the hooks to install.
# Kimi Code's hook protocol only supports allow/block for PreToolUse and
# observation-only for PostToolUse — there is no input-replacement or
# output-replacement mechanism. Only tool-ready (env pre-check + auto-fix)
# is registered; rewrite and compress-response are not compatible.
declare -a HOOK_EVENTS=(
    "PreToolUse"
)

declare -a HOOK_MATCHERS=(
    ""
)

declare -a HOOK_SCRIPTS=(
    "tool_ready_hook.sh"
)

declare -a HOOK_TIMEOUTS=(
    "15"
)

declare -a HOOK_DESCRIPTIONS=(
    "tokenless-tool-ready: Pre-checks tool environment readiness"
)

# Python script to safely merge hooks into config.toml
python3 - "$CONFIG_FILE" "$HOOK_WRAPPER_ABS" "$DRY_RUN" <<'PYTHON_SCRIPT'
import sys
import os
from pathlib import Path

config_path = Path(sys.argv[1])
wrapper = sys.argv[2]
dry_run = sys.argv[3] == "1"

# Hook definitions — only tool-ready is compatible with Kimi Code's hook protocol.
# Kimi Code supports allow/block (PreToolUse) and observation-only (PostToolUse);
# it does not process tool_input/updatedInput or updatedToolOutput/additionalContext.
hook_events = ["PreToolUse"]
hook_matchers = [""]
hook_scripts = ["tool_ready_hook.sh"]
hook_timeouts = [15]
hook_descriptions = [
    "tokenless-tool-ready: Pre-checks tool environment readiness"
]

# Read existing config or start fresh
try:
    with open(config_path) as f:
        content = f.read()
except FileNotFoundError:
    content = ""

# Check for existing tokenless hooks and remove them
lines = content.split('\n')
new_lines = []
skip_until_next_hook = False

for i, line in enumerate(lines):
    if line.strip().startswith("[[hooks]]"):
        # Look ahead to see if this is a tokenless hook by checking command path
        is_tokenless = False
        for j in range(i+1, min(i+10, len(lines))):
            if lines[j].strip().startswith("[["):
                break
            # Match by tokenless marker: wrapper/dispatcher path or description.
            # This stays stable even if the wrapper/dispatcher filenames change.
            line_text = lines[j]
            if (
                "adapters/tokenless/kimicode/hooks/" in line_text
                or "tokenless-tool-ready" in line_text
            ):
                is_tokenless = True
                break
        
        if is_tokenless:
            skip_until_next_hook = True
            continue
    
    if skip_until_next_hook:
        if line.strip().startswith("[[hooks]]"):
            skip_until_next_hook = False
            new_lines.append(line)
        elif line.strip().startswith("["):
            skip_until_next_hook = False
            new_lines.append(line)
        continue
    
    new_lines.append(line)

content = '\n'.join(new_lines).rstrip()

# Build new hook entries
new_hooks = []
for event, matcher, script, timeout, desc in zip(
    hook_events, hook_matchers, hook_scripts, hook_timeouts, hook_descriptions
):
    # Escape backslashes and quotes for TOML basic string
    escaped_wrapper = wrapper.replace('\\', '\\\\').replace('"', '\\"')
    command = f'bash \\"{escaped_wrapper}\\"'
    hook_entry = f"""
[[hooks]]
event = "{event}"
matcher = "{matcher}"
command = "{command}"
timeout = {timeout}
# {desc}
"""
    new_hooks.append(hook_entry)

# Append new hooks
if content and not content.endswith('\n'):
    content += '\n'

content += '\n# Tokenless adapter hooks (auto-installed by tokenless)\n'
content += ''.join(new_hooks)

if dry_run:
    print(f"[DRY-RUN] Would write to {config_path}:")
    print(content)
    sys.exit(0)

# Write back
with open(config_path, 'w') as f:
    f.write(content)

print(f"[tokenless] Updated {config_path} with {len(new_hooks)} hooks")
PYTHON_SCRIPT

echo "[${COMPONENT}] ${AGENT} adapter installed."
echo "[${COMPONENT}] Restart kimi (or run /hooks) to activate."
