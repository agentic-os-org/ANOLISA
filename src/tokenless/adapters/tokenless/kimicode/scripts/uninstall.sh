#!/usr/bin/env bash
# uninstall.sh — Remove tokenless hooks from the Kimi config (config.toml).
#
# Removes every [[hooks]] entry that references tokenless hooks, from each Kimi
# data root still present on this host: Kimi Code migrates a legacy ~/.kimi
# config into ~/.kimi-code, so a hook installed before that migration exists in
# both files, and cleaning only the current one leaves a copy behind that a
# later migration re-imports. See _common.sh for the resolution order.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-kimicode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

# Shared Kimi data-root resolution: Kimi Code reads ~/.kimi-code (KIMI_CODE_HOME)
# while the wound-down kimi-cli read ~/.kimi (KIMI_SHARE_DIR). See _common.sh.
# Bash-only path expansion on purpose: the awk fallback below is exercised with
# a PATH that has no dirname(1).
SCRIPT_DIR="${BASH_SOURCE[0]%/*}"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"

KIMI_HOME="$(resolve_kimi_home)"

echo "[${COMPONENT}] Uninstalling ${AGENT} adapter..."
echo "[${COMPONENT}] kimi data root: ${KIMI_HOME} ($(resolve_kimi_home_origin))"

# Every existing config.toml this adapter may have written, resolved root first.
CONFIG_FILES=()
while IFS= read -r cleanup_root; do
    if [ -f "${cleanup_root}/config.toml" ]; then
        CONFIG_FILES+=("${cleanup_root}/config.toml")
    fi
done < <(kimi_home_cleanup_roots)

if [ "${#CONFIG_FILES[@]}" -eq 0 ]; then
    echo "[${COMPONENT}] config.toml not found — nothing to remove."
    exit 0
fi

# Check for python3 dependency
if ! command -v python3 &>/dev/null; then
    echo "[${COMPONENT}] WARNING: python3 not found — falling back to awk-based cleanup."
    echo "[${COMPONENT}] This removes tokenless hook blocks by matching dispatcher path."

    for CONFIG_FILE in "${CONFIG_FILES[@]}"; do
            # Fallback: use awk to remove [[hooks]] blocks containing tokenless hooks.
            # Any TOML table header (plain [table] or array [[table]]) ends the current
            # hook block, so we do not accidentally consume provider/API-key configs
            # that follow a tokenless hook. Rule order matters: the table-header
            # boundary rule must run before the in-hook append rule, otherwise the
            # header line is appended to the buffered hook and emitted twice on flush
            # (duplicate table headers are invalid TOML).
            awk '
            /^\[\[hooks\]\]/ {
                if (in_hook && !is_tokenless) printf "%s", hook_lines
                in_hook=1; hook_lines = $0 "\n"; is_tokenless=0
                next
            }
            /^\[/ {
                if (in_hook && !is_tokenless) printf "%s", hook_lines
                in_hook=0; hook_lines=""
                print
                next
            }
            in_hook {
                hook_lines = hook_lines $0 "\n"
                if (/adapters\/tokenless\/kimicode\/hooks\//) is_tokenless=1
                if (/tokenless-tool-ready/) is_tokenless=1
                next
            }
            { print }
            END { if (in_hook && !is_tokenless) printf "%s", hook_lines }
            ' "$CONFIG_FILE" > "${CONFIG_FILE}.tmp" && mv "${CONFIG_FILE}.tmp" "$CONFIG_FILE"
        echo "[${COMPONENT}] awk-based cleanup complete: ${CONFIG_FILE}"
    done

    echo "[${COMPONENT}] ${AGENT} adapter uninstalled."
    exit 0
fi

# Python script to remove tokenless hooks from every candidate config.toml
python3 - "${CONFIG_FILES[@]}" <<'PYTHON_SCRIPT'
import sys
from pathlib import Path

# One or more config.toml paths: the resolved Kimi data root first, then any
# other root that still exists (see kimi_home_cleanup_roots in _common.sh).
for arg in sys.argv[1:]:
    config_path = Path(arg)

    try:
        with open(config_path) as f:
            content = f.read()
    except FileNotFoundError:
        print(f"[tokenless] config.toml not found: {config_path}")
        continue

    # Parse and remove tokenless hooks
    lines = content.split('\n')
    new_lines = []
    skip_until_next_hook = False
    removed_count = 0

    for i, line in enumerate(lines):
        if line.strip().startswith("[[hooks]]"):
            # Look ahead to see if this is a tokenless hook by checking command path
            is_tokenless = False
            for j in range(i+1, min(i+15, len(lines))):
                if lines[j].strip().startswith("[["):
                    break
                # Match by tokenless marker: wrapper/dispatcher path or description.
                line_text = lines[j]
                if (
                    "adapters/tokenless/kimicode/hooks/" in line_text
                    or "tokenless-tool-ready" in line_text
                ):
                    is_tokenless = True
                    break
        
            if is_tokenless:
                skip_until_next_hook = True
                removed_count += 1
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

    # Remove the "Tokenless adapter hooks" comment if it exists
    lines_after = content.split('\n')
    lines_after = [l for l in lines_after if l.strip() != "# Tokenless adapter hooks (auto-installed by tokenless)"]
    content = '\n'.join(lines_after)

    # Clean up trailing blank lines
    while content.endswith('\n\n'):
        content = content[:-1]

    if not content.endswith('\n'):
        content += '\n'

    # Write back
    with open(config_path, 'w') as f:
        f.write(content)

    print(f"[tokenless] Removed {removed_count} hook entries from {config_path}")
PYTHON_SCRIPT

echo "[${COMPONENT}] ${AGENT} adapter uninstalled."
