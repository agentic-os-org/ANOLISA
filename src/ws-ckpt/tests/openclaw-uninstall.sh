#!/usr/bin/env bash
# Behavioral tests for scripts/openclaw/uninstall-openclaw.sh.

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNINSTALL_SCRIPT="$PROJECT_ROOT/scripts/openclaw/uninstall-openclaw.sh"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

FAKE_OPENCLAW="$TMPDIR_TEST/openclaw"
ARGV_LOG="$TMPDIR_TEST/argv.log"
STDERR_LOG="$TMPDIR_TEST/stderr.log"
WRITTEN_ALLOW="$TMPDIR_TEST/written-allow.json"
STATE_DIR="$TMPDIR_TEST/state"
DEFAULT_CONFIG="$STATE_DIR/openclaw.json"
MIXED_JSON='["custom-tool","ws-ckpt-list","ws-ckpt-status"]'
FILTERED_JSON='["custom-tool"]'

cat >"$FAKE_OPENCLAW" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$ARGV_LOG"

if [ "$*" = "--version" ]; then
    if [ "${VERSION_EXIT:-0}" != "0" ]; then
        exit "$VERSION_EXIT"
    fi
    printf '%s\n' "$VERSION_OUTPUT"
    exit 0
fi

if [ -n "${EXPECT_CONFIG_PATH:-}" ] && [ "$1" = "config" ] \
    && [ "${OPENCLAW_CONFIG_PATH:-}" != "$EXPECT_CONFIG_PATH" ]; then
    echo "OPENCLAW_CONFIG_PATH was not preserved" >&2
    exit 9
fi

if [ "$*" = "plugins uninstall ws-ckpt --force" ] \
    && [ "${UNINSTALL_FAIL:-0}" = "1" ]; then
    echo "plugin uninstall failed" >&2
    exit 7
fi

if [ "$*" = "config get tools.allow --json" ]; then
    case "${TOOLS_ALLOW_MODE:-unset-current}" in
        value) printf '%s\n' "$TOOLS_ALLOW_JSON" ;;
        unset-current)
            printf '%s\n' '{"ok":false,"error":"Config path is valid but unset: tools.allow"}'
            exit 1
            ;;
        error)
            echo 'Config invalid: parse failure' >&2
            exit 1
            ;;
    esac
    exit 0
fi

if [ "$*" = "config get tools.alsoAllow --json" ]; then
    case "${ALLOW_MODE:-value}" in
        value) printf '%s\n' "$ALLOW_JSON" ;;
        unset-current)
            printf '%s\n' '{"ok":false,"error":"Config path is valid but unset: tools.alsoAllow"}'
            exit 1
            ;;
        error)
            echo 'Config invalid: parse failure' >&2
            exit 1
            ;;
    esac
    exit 0
fi

if [ "$*" = "config set --help" ]; then
    case "${SUPPORTS_EXPECT:-1}" in
        1) printf '%s\n' 'Usage: openclaw config set [--expect-current-absent] [--expect-current-json <json>]' ;;
        0) printf '%s\n' 'Usage: openclaw config set [--json]' ;;
        failure) exit 2 ;;
    esac
    exit 0
fi

if [ "$1 $2" = "config set" ] \
    && { [ "$3" = "tools.allow" ] || [ "$3" = "tools.alsoAllow" ]; }; then
    if [ "${CONFIG_MODE:-simple}" = "root-include" ]; then
        echo 'Config write would flatten $include-owned config at <root>' >&2
        exit 8
    fi
    if [ "${CONFIG_MODE:-simple}" = "write-failure" ]; then
        echo 'simulated config write failure' >&2
        exit 8
    fi
    if [ "$3" = "tools.allow" ]; then
        current_mode="${TOOLS_ALLOW_MODE:-unset-current}"
        current_json="${TOOLS_ALLOW_JSON:-[]}"
    else
        current_mode="${ALLOW_MODE:-value}"
        current_json="$ALLOW_JSON"
    fi
    if [ "$#" -eq 6 ] && [ "$5" = "--expect-current-json" ]; then
        if [ "${SUPPORTS_EXPECT:-1}" != "1" ] \
            || [ "$current_mode" != "value" ] \
            || [ "$6" != "$current_json" ]; then
            echo "conditional config set expectation did not match" >&2
            exit 8
        fi
    elif [ "$#" -eq 5 ] && [ "$5" = "--json" ]; then
        if [ "${SUPPORTS_EXPECT:-1}" != "0" ]; then
            echo "legacy config set used with modern help" >&2
            exit 8
        fi
    else
        echo "config set received unsupported arguments" >&2
        exit 8
    fi
    printf '%s\n' "$4" >"$WRITTEN_ALLOW"
fi
exit 0
EOF
chmod +x "$FAKE_OPENCLAW"

setup_config() {
    local mode="$1"
    local config_path="${2:-$DEFAULT_CONFIG}"
    CONFIG_MODE="$mode"
    rm -rf "$STATE_DIR"
    mkdir -p "$(dirname "$config_path")"
    case "$mode" in
        absent) rm -f "$config_path" ;;
        simple|write-failure) printf '%s\n' '{"tools":{"alsoAllow":[]}}' >"$config_path" ;;
        malformed) printf '%s\n' '{not json' >"$config_path" ;;
        include) printf '%s\n' '{"tools":{"$include":"tools.json"}}' >"$config_path" ;;
        root-include) printf '%s\n' '{"$include":"tools.json"}' >"$config_path" ;;
        *) echo "FAIL: unknown setup mode: $mode" >&2; exit 1 ;;
    esac
}

run_case() {
    local allow_mode="$1"
    local allow_json="$2"
    local config_path="${CASE_CONFIG_PATH:-}"
    local supports_expect="${CASE_SUPPORTS_EXPECT:-1}"
    local uninstall_fail="${CASE_UNINSTALL_FAIL:-0}"
    local tools_allow_mode="${CASE_TOOLS_ALLOW_MODE:-unset-current}"
    local tools_allow_json="${CASE_TOOLS_ALLOW_JSON:-[]}"
    local version_output="${CASE_VERSION_OUTPUT:-}"
    local version_exit="${CASE_VERSION_EXIT:-0}"
    if [ -z "$version_output" ]; then
        if [ "$supports_expect" = "0" ]; then
            version_output="2026.9.0"
        else
            version_output="OpenClaw 2026.9.4 (abcdefg)"
        fi
    fi
    : >"$ARGV_LOG"
    : >"$STDERR_LOG"
    rm -f "$WRITTEN_ALLOW"

    env_args=(
        -u ANOLISA_DRY_RUN
        -u OPENCLAW_CONFIG_PATH
        ARGV_LOG="$ARGV_LOG"
        WRITTEN_ALLOW="$WRITTEN_ALLOW"
        ALLOW_MODE="$allow_mode"
        ALLOW_JSON="$allow_json"
        TOOLS_ALLOW_MODE="$tools_allow_mode"
        TOOLS_ALLOW_JSON="$tools_allow_json"
        SUPPORTS_EXPECT="$supports_expect"
        VERSION_OUTPUT="$version_output"
        VERSION_EXIT="$version_exit"
        CONFIG_MODE="$CONFIG_MODE"
        UNINSTALL_FAIL="$uninstall_fail"
        OPENCLAW_BIN="$FAKE_OPENCLAW"
        OPENCLAW_STATE_DIR="$STATE_DIR"
    )
    if [ -n "$config_path" ]; then
        env_args+=(OPENCLAW_CONFIG_PATH="$config_path" EXPECT_CONFIG_PATH="$config_path")
    fi
    env "${env_args[@]}" "$UNINSTALL_SCRIPT" >/dev/null 2>"$STDERR_LOG"
}

assert_calls() {
    local desc="$1"; shift
    local expected=("$@")
    mapfile -t calls <"$ARGV_LOG"

    if [ "${#calls[@]}" -ne "${#expected[@]}" ]; then
        echo "FAIL ($desc): expected ${#expected[@]} openclaw invocations, got ${#calls[@]}:" >&2
        printf '  %s\n' "${calls[@]}" >&2
        exit 1
    fi
    local i
    for i in "${!expected[@]}"; do
        if [ "${calls[$i]}" != "${expected[$i]}" ]; then
            echo "FAIL ($desc): call #$((i + 1)) mismatch" >&2
            echo "  expected: ${expected[$i]}" >&2
            echo "  actual:   ${calls[$i]}" >&2
            exit 1
        fi
    done
}

assert_written() {
    local desc="$1"
    local expected="$2"
    local actual
    actual="$(<"$WRITTEN_ALLOW")"
    if [ "$actual" != "$expected" ]; then
        echo "FAIL ($desc): persisted allowlist mismatch" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

setup_config simple
mkdir -p "$STATE_DIR/extensions/ws-ckpt" "$STATE_DIR/skills/ws-ckpt"
CASE_VERSION_OUTPUT=2026.2.12 run_case value "$MIXED_JSON"
assert_calls "unsupported version skips config mutations" \
    "--version"
if [ -d "$STATE_DIR/extensions/ws-ckpt" ] || [ -d "$STATE_DIR/skills/ws-ckpt" ]; then
    echo "FAIL (unsupported version): local plugin or skill files remain" >&2
    exit 1
fi
grep -Fq "older than the minimum supported version" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

setup_config simple
mkdir -p "$STATE_DIR/extensions/ws-ckpt" "$STATE_DIR/skills/ws-ckpt"
CASE_VERSION_OUTPUT=not-a-version run_case value "$MIXED_JSON"
assert_calls "unparseable version skips config mutations" \
    "--version"
if [ -d "$STATE_DIR/extensions/ws-ckpt" ] || [ -d "$STATE_DIR/skills/ws-ckpt" ]; then
    echo "FAIL (unparseable version): local plugin or skill files remain" >&2
    exit 1
fi
grep -Fq "could not parse an unambiguous version" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

setup_config simple
mkdir -p "$STATE_DIR/extensions/ws-ckpt" "$STATE_DIR/skills/ws-ckpt"
CASE_VERSION_EXIT=1 run_case value "$MIXED_JSON"
assert_calls "failed version probe skips config mutations" \
    "--version"
if [ -d "$STATE_DIR/extensions/ws-ckpt" ] || [ -d "$STATE_DIR/skills/ws-ckpt" ]; then
    echo "FAIL (failed version probe): local plugin or skill files remain" >&2
    exit 1
fi
grep -Fq "version probe exited with status 1" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

setup_config simple
run_case value "$MIXED_JSON"
assert_calls "filter tools.alsoAllow" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --expect-current-json $MIXED_JSON"
assert_written "filter tools.alsoAllow" "$FILTERED_JSON"

setup_config simple
CASE_TOOLS_ALLOW_MODE=value CASE_TOOLS_ALLOW_JSON="$MIXED_JSON" \
    run_case unset-current '[]'
assert_calls "filter tools.allow" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.allow $FILTERED_JSON --expect-current-json $MIXED_JSON"
assert_written "filter tools.allow" "$FILTERED_JSON"

setup_config simple
CASE_SUPPORTS_EXPECT=0 run_case value "$MIXED_JSON"
assert_calls "legacy config set" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --json"
assert_written "legacy config set" "$FILTERED_JSON"

setup_config simple
CASE_SUPPORTS_EXPECT=0 CASE_VERSION_OUTPUT=2026.9.1 \
    run_case value "$MIXED_JSON"
assert_calls "conditional version rejects legacy-only help" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (conditional version legacy-only help): config was mutated" >&2
    exit 1
fi
grep -Fq "lacks the conditional config flags" "$STDERR_LOG"

setup_config include
mkdir -p "$STATE_DIR/extensions/ws-ckpt" "$STATE_DIR/skills/ws-ckpt"
CASE_SUPPORTS_EXPECT=0 run_case value "$MIXED_JSON"
assert_calls "legacy include protection" \
    "--version"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (legacy include protection): config was mutated" >&2
    exit 1
fi
if [ -d "$STATE_DIR/extensions/ws-ckpt" ] || [ -d "$STATE_DIR/skills/ws-ckpt" ]; then
    echo "FAIL (legacy include protection): local plugin or skill files remain" >&2
    exit 1
fi
grep -Fq "cannot update it safely" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

setup_config simple
CASE_SUPPORTS_EXPECT=failure run_case value "$MIXED_JSON"
assert_calls "config help failure" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (config help failure): config was mutated" >&2
    exit 1
fi
grep -Fq "could not determine OpenClaw config write capabilities" "$STDERR_LOG"

setup_config simple
CASE_UNINSTALL_FAIL=1 run_case value '["custom-tool"]'
assert_calls "plugin uninstall failure" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json"
grep -Fq "could not unregister the ws-ckpt plugin" "$STDERR_LOG"

setup_config include
run_case value "$MIXED_JSON"
assert_calls "nested include-owned config" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --expect-current-json $MIXED_JSON"
assert_written "nested include-owned config" "$FILTERED_JSON"

setup_config root-include
run_case value "$MIXED_JSON"
assert_calls "root include refusal" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --expect-current-json $MIXED_JSON"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (root include refusal): config was mutated" >&2
    exit 1
fi
grep -Fq "could not remove ws-ckpt entries" "$STDERR_LOG"

setup_config write-failure
run_case value "$MIXED_JSON"
assert_calls "config write failure" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --expect-current-json $MIXED_JSON"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (config write failure): config was mutated" >&2
    exit 1
fi
grep -Fq "could not remove ws-ckpt entries" "$STDERR_LOG"

CUSTOM_CONFIG="$TMPDIR_TEST/custom/openclaw.json"
setup_config simple "$CUSTOM_CONFIG"
CASE_CONFIG_PATH="$CUSTOM_CONFIG" run_case value "$MIXED_JSON"
assert_calls "custom config path" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $FILTERED_JSON --expect-current-json $MIXED_JSON"
assert_written "custom config path" "$FILTERED_JSON"

setup_config include "$CUSTOM_CONFIG"
CASE_CONFIG_PATH="$CUSTOM_CONFIG" CASE_SUPPORTS_EXPECT=0 \
    run_case value "$MIXED_JSON"
assert_calls "legacy custom include protection" \
    "--version"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (legacy custom include protection): config was mutated" >&2
    exit 1
fi
grep -Fq "$CUSTOM_CONFIG uses \$include" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

setup_config malformed
CASE_TOOLS_ALLOW_MODE=error run_case value '[]'
assert_calls "invalid tools.allow read" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json"
grep -Fq "could not read effective tools allowlists" "$STDERR_LOG"

setup_config malformed
run_case error '[]'
assert_calls "invalid tools.alsoAllow read" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json"
grep -Fq "could not read effective tools allowlists" "$STDERR_LOG"

setup_config absent
run_case unset-current '[]'
assert_calls "unset allowlists" \
    "--version" \
    "plugins uninstall ws-ckpt --force" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json"

# Missing openclaw CLI: config cleanup is skipped but must warn loudly with
# manual-cleanup guidance instead of silently leaving ws-ckpt-* entries.
setup_config simple
: >"$ARGV_LOG"
: >"$STDERR_LOG"
rm -f "$WRITTEN_ALLOW"
env -u ANOLISA_DRY_RUN -u OPENCLAW_CONFIG_PATH \
    ARGV_LOG="$ARGV_LOG" WRITTEN_ALLOW="$WRITTEN_ALLOW" \
    CONFIG_MODE=simple \
    OPENCLAW_BIN="$TMPDIR_TEST/no-such-openclaw" \
    OPENCLAW_STATE_DIR="$STATE_DIR" \
    "$UNINSTALL_SCRIPT" >/dev/null 2>"$STDERR_LOG"
if [ -s "$ARGV_LOG" ] || [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (missing CLI): openclaw was invoked or config was mutated" >&2
    exit 1
fi
grep -Fq "openclaw CLI" "$STDERR_LOG"
grep -Fq "remove its ws-ckpt plugin registration" "$STDERR_LOG"

echo "OpenClaw uninstall script tests passed"
