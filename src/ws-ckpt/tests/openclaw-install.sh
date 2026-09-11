#!/usr/bin/env bash
# Behavioral tests for scripts/openclaw/install-openclaw.sh.

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_SCRIPT="$PROJECT_ROOT/scripts/openclaw/install-openclaw.sh"
REPO_ROOT="$(cd "$PROJECT_ROOT/../.." && pwd)"
PLUGIN_SRC="$PROJECT_ROOT/src/plugins/openclaw"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

FAKE_OPENCLAW="$TMPDIR_TEST/openclaw"
ARGV_LOG="$TMPDIR_TEST/argv.log"
STDERR_LOG="$TMPDIR_TEST/stderr.log"
WRITTEN_ALLOW="$TMPDIR_TEST/written-allow.json"
STATE_DIR="$TMPDIR_TEST/state"
DEFAULT_CONFIG="$STATE_DIR/openclaw.json"

FAKE_TARGET_DIR="$TMPDIR_TEST/staged-target"
mkdir -p "$FAKE_TARGET_DIR/share/anolisa/runtime/ws-ckpt/plugins/openclaw"

export ANOLISA_DRY_RUN=1
export ANOLISA_TARGET_DIR="$FAKE_TARGET_DIR"

ALL_TOOLS_JSON='["ws-ckpt-checkpoint","ws-ckpt-rollback","ws-ckpt-list","ws-ckpt-delete","ws-ckpt-diff","ws-ckpt-config","ws-ckpt-status"]'
PARTIAL_JSON='["custom-tool","ws-ckpt-list"]'
MERGED_CUSTOM_JSON='["custom-tool","ws-ckpt-checkpoint","ws-ckpt-rollback","ws-ckpt-list","ws-ckpt-delete","ws-ckpt-diff","ws-ckpt-config","ws-ckpt-status"]'
MERGED_PARTIAL_JSON='["custom-tool","ws-ckpt-list","ws-ckpt-checkpoint","ws-ckpt-rollback","ws-ckpt-delete","ws-ckpt-diff","ws-ckpt-config","ws-ckpt-status"]'

cat >"$FAKE_OPENCLAW" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$ARGV_LOG"

if [ -n "${EXPECT_CONFIG_PATH:-}" ] && [ "$1" = "config" ] \
    && [ "${OPENCLAW_CONFIG_PATH:-}" != "$EXPECT_CONFIG_PATH" ]; then
    echo "OPENCLAW_CONFIG_PATH was not preserved" >&2
    exit 9
fi

if [ "$*" = "plugins install --help" ]; then
    case "${INSTALL_HELP_MODE:-modern}" in
        modern) printf '%s\n' 'Usage: openclaw plugins install [--force] [--accept-capabilities]' ;;
        legacy) printf '%s\n' 'Usage: openclaw plugins install [--force]' ;;
        near-miss) printf '%s\n' 'Usage: openclaw plugins install [--accept-capabilities-only]' ;;
        failure) exit 2 ;;
    esac
    exit 0
fi

if [ "$*" = "config get tools.allow --json" ]; then
    case "${TOOLS_ALLOW_MODE:-unset-current}" in
        value) printf '%s\n' "$TOOLS_ALLOW_JSON" ;;
        unset-current)
            printf '%s\n' '{"ok":false,"error":"Config path is valid but unset: tools.allow"}'
            exit 1
            ;;
        unset-legacy)
            echo 'Config path not found: tools.allow' >&2
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
        unset-legacy)
            echo 'Config path not found: tools.alsoAllow' >&2
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
    elif [ "$#" -eq 5 ] && [ "$5" = "--expect-current-absent" ]; then
        if [ "${SUPPORTS_EXPECT:-1}" != "1" ] \
            || [[ "$current_mode" != unset-* ]]; then
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
    local install_help_mode="$1"
    local allow_mode="$2"
    local allow_json="$3"
    local config_path="${4:-}"
    local case_home="${5:-$HOME}"
    local supports_expect="${6:-1}"
    local tools_allow_mode="${7:-unset-current}"
    local tools_allow_json="${8:-[]}"
    : >"$ARGV_LOG"
    : >"$STDERR_LOG"
    rm -f "$WRITTEN_ALLOW"

    env_args=(
        -u ANOLISA_DRY_RUN
        -u ANOLISA_TARGET_DIR
        -u OPENCLAW_CONFIG_PATH
        ARGV_LOG="$ARGV_LOG"
        WRITTEN_ALLOW="$WRITTEN_ALLOW"
        INSTALL_HELP_MODE="$install_help_mode"
        ALLOW_MODE="$allow_mode"
        ALLOW_JSON="$allow_json"
        TOOLS_ALLOW_MODE="$tools_allow_mode"
        TOOLS_ALLOW_JSON="$tools_allow_json"
        SUPPORTS_EXPECT="$supports_expect"
        CONFIG_MODE="$CONFIG_MODE"
        HOME="$case_home"
        OPENCLAW_BIN="$FAKE_OPENCLAW"
        OPENCLAW_STATE_DIR="$STATE_DIR"
        ANOLISA_PROJECT_ROOT="$REPO_ROOT"
    )
    if [ -n "$config_path" ]; then
        env_args+=(OPENCLAW_CONFIG_PATH="$config_path" EXPECT_CONFIG_PATH="$config_path")
    fi
    env "${env_args[@]}" "$INSTALL_SCRIPT" >/dev/null 2>"$STDERR_LOG"
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

for mode in modern legacy near-miss failure; do
    setup_config absent
    run_case "$mode" unset-current '[]'
    install_expected="plugins install $PLUGIN_SRC --force"
    if [ "$mode" = "modern" ]; then
        install_expected="$install_expected --accept-capabilities"
    fi
    assert_calls "absent/$mode" \
        "plugins install --help" \
        "config get tools.allow --json" \
        "config get tools.alsoAllow --json" \
        "config set --help" \
        "config set tools.alsoAllow $ALL_TOOLS_JSON --expect-current-absent" \
        "$install_expected" \
        "plugins enable ws-ckpt"
    assert_written "absent/$mode" "$ALL_TOOLS_JSON"
done

setup_config absent
run_case modern unset-legacy '[]' '' "$HOME" 1 unset-legacy
assert_calls "legacy unset response" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $ALL_TOOLS_JSON --expect-current-absent" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "legacy unset response" "$ALL_TOOLS_JSON"

setup_config simple
run_case modern value "$PARTIAL_JSON"
assert_calls "merge tools.alsoAllow" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_PARTIAL_JSON --expect-current-json $PARTIAL_JSON" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "merge tools.alsoAllow" "$MERGED_PARTIAL_JSON"

setup_config simple
run_case modern unset-current '[]' '' "$HOME" 1 value "$PARTIAL_JSON"
assert_calls "merge existing tools.allow" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.allow $MERGED_PARTIAL_JSON --expect-current-json $PARTIAL_JSON" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "merge existing tools.allow" "$MERGED_PARTIAL_JSON"

setup_config simple
run_case modern value "$PARTIAL_JSON" '' "$HOME" 1 value '[]'
assert_calls "prefer non-empty tools.alsoAllow" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_PARTIAL_JSON --expect-current-json $PARTIAL_JSON" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "prefer non-empty tools.alsoAllow" "$MERGED_PARTIAL_JSON"

setup_config simple
run_case modern unset-current '[]' '' "$HOME" 1 value '[]'
assert_calls "preserve unrestricted empty tools.allow" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $ALL_TOOLS_JSON --expect-current-absent" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "preserve unrestricted empty tools.allow" "$ALL_TOOLS_JSON"

setup_config simple
run_case modern value "$PARTIAL_JSON" '' "$HOME" 0
assert_calls "legacy config set" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_PARTIAL_JSON --json" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "legacy config set" "$MERGED_PARTIAL_JSON"

setup_config include
run_case modern value '["custom-tool"]' '' "$HOME" 0
assert_calls "legacy include protection" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (legacy include protection): config was mutated" >&2
    exit 1
fi
grep -Fq "lacks safe conditional config writes" "$STDERR_LOG"

setup_config simple
run_case modern value "$PARTIAL_JSON" '' "$HOME" failure
assert_calls "config help failure" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (config help failure): config was mutated" >&2
    exit 1
fi
grep -Fq "could not determine OpenClaw config write capabilities" "$STDERR_LOG"

setup_config simple
run_case modern value "$ALL_TOOLS_JSON"
assert_calls "already complete" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"

setup_config malformed
run_case modern value '[]' '' "$HOME" 1 error
assert_calls "invalid tools.allow read" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
grep -Fq "could not read effective tools allowlists" "$STDERR_LOG"

setup_config malformed
run_case modern error '[]'
assert_calls "invalid tools.alsoAllow read" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
grep -Fq "could not read effective tools allowlists" "$STDERR_LOG"

setup_config include
run_case modern value '["custom-tool"]'
assert_calls "nested include-owned config" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_CUSTOM_JSON --expect-current-json [\"custom-tool\"]" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "nested include-owned config" "$MERGED_CUSTOM_JSON"

setup_config root-include
run_case modern value '["custom-tool"]'
assert_calls "root include refusal" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_CUSTOM_JSON --expect-current-json [\"custom-tool\"]" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (root include refusal): config was mutated" >&2
    exit 1
fi
grep -Fq "could not add ws-ckpt tools" "$STDERR_LOG"

setup_config write-failure
run_case modern value '["custom-tool"]'
assert_calls "config write failure" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_CUSTOM_JSON --expect-current-json [\"custom-tool\"]" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (config write failure): config was mutated" >&2
    exit 1
fi
grep -Fq "could not add ws-ckpt tools" "$STDERR_LOG"

CUSTOM_CONFIG="$TMPDIR_TEST/custom/openclaw.json"
setup_config simple "$CUSTOM_CONFIG"
run_case modern value "$PARTIAL_JSON" "$CUSTOM_CONFIG"
assert_calls "custom config path" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "config set tools.alsoAllow $MERGED_PARTIAL_JSON --expect-current-json $PARTIAL_JSON" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
assert_written "custom config path" "$MERGED_PARTIAL_JSON"

setup_config include "$CUSTOM_CONFIG"
run_case modern value '["custom-tool"]' "$CUSTOM_CONFIG" "$HOME" 0
assert_calls "legacy custom include protection" \
    "plugins install --help" \
    "config get tools.allow --json" \
    "config get tools.alsoAllow --json" \
    "config set --help" \
    "plugins install $PLUGIN_SRC --force --accept-capabilities" \
    "plugins enable ws-ckpt"
if [ -e "$WRITTEN_ALLOW" ]; then
    echo "FAIL (legacy custom include protection): config was mutated" >&2
    exit 1
fi
grep -Fq "$CUSTOM_CONFIG uses \$include" "$STDERR_LOG"

echo "OpenClaw install script tests passed"
