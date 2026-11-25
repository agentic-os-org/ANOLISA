#!/usr/bin/env bash
# Detect whether the tokenless CLI is installed and functional.
#
# Output:
#   JSON on stdout:  {"installed": true, "status": "installable", "version": "0.1.0", "path": "/usr/bin/tokenless"}
#                    or {"installed": false, "status": "installable"}
#
#   "installed" describes the tokenless *binary*. "status" describes the codex
#   adapter and is what `tokenless init` reads: this script never asks codex
#   whether the plugin is registered, so it reports "installable" instead of
#   letting a binary on PATH read as a registered adapter. install.sh is
#   idempotent, so re-running it against an already registered host is safe.
#
# Exit code: 0 either way (fail-open — plugin activation is controlled by capabilities)

set -euo pipefail

TOKENLESS_BIN=""

# Search PATH first
if command -v tokenless >/dev/null 2>&1; then
    TOKENLESS_BIN="$(command -v tokenless)"
fi

# Fallback paths
if [[ -z "$TOKENLESS_BIN" ]]; then
    for fp in \
        "$HOME/.local/bin/tokenless" \
        "/usr/local/bin/tokenless" \
        "/usr/bin/tokenless" \
        "$HOME/.local/share/anolisa/tokenless/tokenless" \
        "$HOME/.local/lib/anolisa/tokenless/tokenless"; do
        if [[ -f "$fp" && -x "$fp" ]]; then
            TOKENLESS_BIN="$fp"
            break
        fi
    done
fi

if [[ -z "$TOKENLESS_BIN" ]]; then
    echo '{"installed": false, "status": "installable"}'
    exit 0
fi

# Verify it actually runs
VERSION="$("$TOKENLESS_BIN" --version 2>/dev/null || true)"
if [[ -z "$VERSION" ]]; then
    echo '{"installed": false, "status": "installable"}'
    exit 0
fi

# Extract version number
VERSION_CLEAN="$(echo "$VERSION" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")"

# Use jq --arg to safely embed variables into JSON — avoids broken output
# when paths contain characters that need JSON escaping.
if command -v jq &>/dev/null; then
  jq -n --arg ver "$VERSION_CLEAN" --arg p "$TOKENLESS_BIN" \
    '{installed: true, status: "installable", version: $ver, path: $p}'
else
  cat <<EOF
{
  "installed": true,
  "status": "installable",
  "version": "$VERSION_CLEAN",
  "path": "$TOKENLESS_BIN"
}
EOF
fi
