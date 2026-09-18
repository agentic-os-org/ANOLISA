#!/usr/bin/env bash

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$PROJECT_ROOT/../.." && pwd)"
COMPAT_LIB="$PROJECT_ROOT/scripts/openclaw/lib-openclaw.sh"
DETECT_SCRIPT="$PROJECT_ROOT/scripts/openclaw/detect-openclaw.sh"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

# shellcheck source=../scripts/openclaw/lib-openclaw.sh
source "$COMPAT_LIB"

assert_classification() {
    local version="$1"
    local expected="$2"
    local actual

    actual="$(classify_openclaw_version "$version")"
    if [ "$actual" != "$expected" ]; then
        echo "FAIL: expected $version to classify as $expected, got $actual" >&2
        exit 1
    fi
}

while read -r version expected; do
    assert_classification "$version" "$expected"
done <<'EOF'
2026.2.12 unsupported
2026.2.13-rc.1 unsupported
2026.2.13 plain-json
2026.2.13-1 plain-json
2026.9.0 plain-json
2026.9.1-rc.1 plain-json
2026.9.1 conditional
2026.9.1-1 conditional
2026.9.4+build.7 conditional
2027.1.1 conditional
EOF

for output in \
    "2026.2.13" \
    "v2026.2.13" \
    "OpenClaw 2026.9.4 (abcdefg)" \
    "OpenClaw version V2026.9.1-1" \
    "OpenClaw CLI version 2026.9.1"; do
    if ! parse_openclaw_version_output "$output" >/dev/null; then
        echo "FAIL: expected version output to parse: $output" >&2
        exit 1
    fi
done

for output in \
    "not-a-version" \
    "OpenClaw 2026.9" \
    "OpenClaw 2026.9.1 unexpected" \
    $'2026.9.1\n2026.9.2' \
    "warning: migration from 2026.2.13 failed"; do
    if parse_openclaw_version_output "$output" >/dev/null; then
        echo "FAIL: expected version output to be rejected: $output" >&2
        exit 1
    fi
done

BIN_DIR="$TMPDIR_TEST/bin"
STATE_DIR="$TMPDIR_TEST/state"
ARGV_LOG="$TMPDIR_TEST/argv.log"
STDOUT_LOG="$TMPDIR_TEST/stdout.log"
mkdir -p "$BIN_DIR" "$STATE_DIR"

cat >"$BIN_DIR/openclaw" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$ARGV_LOG"
case "$*" in
    --version)
        [ "${VERSION_EXIT:-0}" = "0" ] || exit "$VERSION_EXIT"
        printf '%s\n' "$VERSION_OUTPUT"
        ;;
    "plugins list --json") printf '%s\n' "$PLUGINS_JSON" ;;
    "plugins list") printf '%s\n' "$PLUGINS_TEXT" ;;
    *) exit 2 ;;
esac
EOF
cat >"$BIN_DIR/ws-ckpt" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$BIN_DIR/openclaw" "$BIN_DIR/ws-ckpt"

run_detect() {
    local version_output="$1"
    local expected_rc="$2"
    local plugins_json="${3:-}"
    local plugins_text="${4:-ws-ckpt enabled}"
    local actual_rc

    if [ -z "$plugins_json" ]; then
        plugins_json='{"plugins":[{"id":"ws-ckpt"}]}'
    fi
    : >"$ARGV_LOG"
    if env \
        ARGV_LOG="$ARGV_LOG" \
        VERSION_OUTPUT="$version_output" \
        PLUGINS_JSON="$plugins_json" \
        PLUGINS_TEXT="$plugins_text" \
        OPENCLAW_BIN="$BIN_DIR/openclaw" \
        OPENCLAW_STATE_DIR="$STATE_DIR" \
        ANOLISA_PROJECT_ROOT="$REPO_ROOT" \
        PATH="$BIN_DIR:$PATH" \
        "$DETECT_SCRIPT" >"$STDOUT_LOG" 2>&1; then
        actual_rc=0
    else
        actual_rc=$?
    fi
    if [ "$actual_rc" != "$expected_rc" ]; then
        echo "FAIL: expected detect exit $expected_rc, got $actual_rc for $version_output" >&2
        exit 1
    fi
}

run_detect "OpenClaw 2026.9.4 (abcdefg)" 0
mapfile -t calls <"$ARGV_LOG"
expected=("--version" "plugins list --json")
if [ "${calls[*]}" != "${expected[*]}" ]; then
    echo "FAIL: supported detect call order was: ${calls[*]}" >&2
    exit 1
fi
grep -Fq "2026.9.4 (conditional)" "$STDOUT_LOG"

run_detect "2026.9.1" 0 '{"plugins":[]}'
mapfile -t calls <"$ARGV_LOG"
expected=("--version" "plugins list --json" "plugins list")
if [ "${calls[*]}" != "${expected[*]}" ]; then
    echo "FAIL: detect text fallback call order was: ${calls[*]}" >&2
    exit 1
fi

for version in "2026.2.12" "2026.2.13-rc.1" "not-a-version"; do
    run_detect "$version" 2
    mapfile -t calls <"$ARGV_LOG"
    if [ "${#calls[@]}" -ne 1 ] || [ "${calls[0]}" != "--version" ]; then
        echo "FAIL: incompatible detect continued probing for $version" >&2
        exit 1
    fi
done

echo "OpenClaw compatibility tests passed"
