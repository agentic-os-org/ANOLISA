#!/usr/bin/env bash
# Detect whether the tokenless CLI is installed and functional.
#
# Output:
#   JSON on stdout:  {"installed": true, "status": "installable", "version": "0.1.0", "path": "/usr/bin/tokenless"}
#                    or {"installed": false, "status": "missing_prereqs"}
#
#   "installed" describes the tokenless *binary*. "status" describes the codex
#   adapter and is what `tokenless init` reads:
#     installable      a codex CLI can be resolved, so install.sh will register
#     missing_prereqs  no codex CLI can be resolved, so install.sh would print
#                      "skipping plugin registration" and exit 0 having changed
#                      nothing - offering that as installable reads as success
#   This script never asks codex whether the plugin is registered, so
#   "installable" is the most it can claim; install.sh is idempotent, so
#   re-running it against an already registered host is safe.
#
# Exit code: 0 either way (fail-open — plugin activation is controlled by capabilities)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"

# The same resolution install.sh uses, so "installable" cannot promise a CLI the
# installer would then fail to find. An explicit CODEX_BIN is authoritative and
# an unusable pin resolves to "not found" rather than to some other codex.
# Resolved in a subshell with HOME defaulted: resolve_codex probes
# "$HOME/.local/bin", and a host without HOME must still get an answer instead
# of dying on an unbound variable.
CODEX_BIN="${CODEX_BIN:-}"
CODEX_CLI="$(HOME="${HOME:-}"; resolve_codex)"

STATUS="installable"
if [[ -z "$CODEX_CLI" ]]; then
    STATUS="missing_prereqs"
    echo "[tokenless] codex: no codex CLI resolved; the adapter cannot be registered" >&2
fi

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
    echo "{\"installed\": false, \"status\": \"${STATUS}\"}"
    exit 0
fi

# Verify it actually runs
VERSION="$("$TOKENLESS_BIN" --version 2>/dev/null || true)"
if [[ -z "$VERSION" ]]; then
    echo "{\"installed\": false, \"status\": \"${STATUS}\"}"
    exit 0
fi

# Extract version number
VERSION_CLEAN="$(echo "$VERSION" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")"

# Escape a value for a JSON string. jq does this on the branch below; without it
# a path holding a quote, a backslash or a control character produced invalid
# JSON, and a caller that cannot parse the status learns nothing about the
# adapter. Pure shell, so this branch still needs no external tool.
json_escape() {
    local text="$1" char oct hex code
    text="${text//\\/\\\\}"   # backslash first, or it re-escapes the rest
    text="${text//\"/\\\"}"     # double quote
    text="${text//$'\b'/\\b}"    # the five control characters JSON names
    text="${text//$'\f'/\\f}"
    text="${text//$'\n'/\\n}"
    text="${text//$'\r'/\\r}"
    text="${text//$'\t'/\\t}"
    # The rest of U+0001-U+001F may not appear raw in a JSON string and has no
    # short form, so it becomes \u00XX. A NUL cannot live in a shell variable.
    # Builtins only: no fork per code point.
    for ((code = 1; code < 32; code++)); do
        printf -v oct '%03o' "$code"
        printf -v char "\\$oct"
        printf -v hex '%04x' "$code"
        text="${text//"$char"/\\u$hex}"
    done
    printf '%s' "$text"
}

# Use jq --arg to safely embed variables into JSON when it is available.
if command -v jq &>/dev/null; then
  jq -n --arg ver "$VERSION_CLEAN" --arg p "$TOKENLESS_BIN" --arg st "$STATUS" \
    '{installed: true, status: $st, version: $ver, path: $p}'
else
  # No jq: escape by hand and emit with the shell's own printf, so this branch
  # depends on no external tool and stays parseable for any path. A path holding
  # a quote used to be interpolated raw, and the exit 0 that followed let a
  # caller read a registration nobody had verified.
  printf '{\n  "installed": true,\n  "status": "%s",\n  "version": "%s",\n  "path": "%s"\n}\n' \
    "$(json_escape "$STATUS")" \
    "$(json_escape "$VERSION_CLEAN")" \
    "$(json_escape "$TOKENLESS_BIN")"
fi
