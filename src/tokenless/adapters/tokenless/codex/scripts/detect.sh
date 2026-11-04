#!/usr/bin/env bash
# Detect whether the tokenless CLI is installed and functional.
#
# Output:
#   JSON on stdout:  {"installed": true, "version": "0.1.0", "path": "/usr/bin/tokenless"}
#                    or {"installed": false}
# Exit code: 0 either way (fail-open — plugin activation is controlled by capabilities)

set -euo pipefail

# Escape a value for interpolation into a JSON string.
#
# The jq branch at the bottom gets this from `jq --arg`; the no-jq fallback has
# to do it itself. A filesystem path may legally contain double quotes,
# backslashes and control characters, and interpolating one of those verbatim
# yields JSON that either fails to parse or silently decodes to a *different*
# path (a binary under a directory named `a\tb` came back as `a<TAB>b`).
# Builtins only, so the fallback still introduces no runtime dependency, and
# it produces the same escapes jq does: the five short forms JSON defines,
# \u00XX for the remaining C0 controls and for DEL, everything else passed
# through — which keeps multi-byte UTF-8 intact, because no UTF-8 continuation
# byte is below 0x80.
json_escape() {
    local s=$1
    local i ch hex

    # Backslash first: every substitution below introduces backslashes of its
    # own, and those must not be escaped a second time.
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\b'/\\b}
    s=${s//$'\f'/\\f}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    # The remaining C0 controls have no short form. Start at 1: a bash string
    # cannot hold NUL, and an empty search pattern matches at every position.
    for (( i = 1; i < 32; i++ )); do
        case $i in 8 | 9 | 10 | 12 | 13) continue ;; esac
        printf -v ch '%b' "$(printf '\\%03o' "$i")"
        printf -v hex '\\u%04x' "$i"
        s=${s//"$ch"/$hex}
    done
    s=${s//$'\177'/\\u007f}

    printf '%s' "$s"
}

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
    echo '{"installed": false}'
    exit 0
fi

# Verify it actually runs
VERSION="$("$TOKENLESS_BIN" --version 2>/dev/null || true)"
if [[ -z "$VERSION" ]]; then
    echo '{"installed": false}'
    exit 0
fi

# Extract version number
VERSION_CLEAN="$(echo "$VERSION" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")"

# Use jq --arg to safely embed variables into JSON — avoids broken output
# when paths contain characters that need JSON escaping. The fallback below
# promises the same JSON, so it escapes through json_escape instead of
# interpolating the raw values.
if command -v jq &>/dev/null; then
  jq -n --arg ver "$VERSION_CLEAN" --arg p "$TOKENLESS_BIN" \
    '{installed: true, version: $ver, path: $p}'
else
  VERSION_JSON="$(json_escape "$VERSION_CLEAN")"
  PATH_JSON="$(json_escape "$TOKENLESS_BIN")"
  cat <<EOF
{
  "installed": true,
  "version": "$VERSION_JSON",
  "path": "$PATH_JSON"
}
EOF
fi
