#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$SCRIPT_DIR/../adapters/tokenless/common/hooks/tool_ready_hook.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

SPEC="$TEST_DIR/tool-ready-spec.json"
FIXER="$TEST_DIR/tokenless-env-fix.sh"
MARKER="$TEST_DIR/fixer-called"

cat > "$SPEC" <<'EOF'
{"TestMissing":{"required":[{"binary":"tokenless-missing-for-test","package":"tokenless-missing-for-test","manager":"rpm"}],"recommended":[],"permissions":[]}}
EOF

cat > "$FIXER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

[ "${1:-}" = "fix-all" ]
cat >/dev/null
touch "$TOKENLESS_FIX_MARKER"
EOF
chmod 0644 "$FIXER"

OUTPUT=$(
    echo '{"tool_name":"TestMissing","tool_input":{"command":"test"}}' \
        | TOKENLESS_TOOL_READY_ENABLED=1 \
          TOKENLESS_TOOL_READY_SPEC="$SPEC" \
          TOKENLESS_ENV_FIX_SCRIPT="$FIXER" \
          TOKENLESS_FIX_MARKER="$MARKER" \
          bash "$HOOK"
)

[ ! -e "$MARKER" ]
[ "$OUTPUT" = "{}" ]

echo "tool-ready hard bypass test passed"

# Exercise only dormant path selection; the production hook must stay bypassed.
RESOLVER="$TEST_DIR/resolve-paths.sh"
awk '/^VERBOSE=/{copy=1} /^# --- Read input/{copy=0} copy' "$HOOK" > "$RESOLVER"
ADAPTERS="$TEST_DIR/adapters"
USER_DATA="$TEST_DIR/data"
mkdir -p "$ADAPTERS/common" "$USER_DATA/tokenless/adapters/common"
for directory in "$ADAPTERS/common" "$USER_DATA/tokenless/adapters/common"; do
    cp "$SPEC" "$directory/tool-ready-spec.json"
    cp "$FIXER" "$directory/tokenless-env-fix.sh"
done

assert_resource_paths() {
    EXPECTED_SPEC="$1" EXPECTED_FIXER="$2" bash -e -c '
        source "$1"
        [ "$SPEC_FILE" = "$EXPECTED_SPEC" ]
        [ "$FIX_SCRIPT" = "$EXPECTED_FIXER" ]
    ' bash "$RESOLVER"
}

HOME="$TEST_DIR" XDG_DATA_HOME="$USER_DATA" ANOLISA_ADAPTER_DIR="$ADAPTERS" \
    TOKENLESS_TOOL_READY_SPEC="$SPEC" TOKENLESS_ENV_FIX_SCRIPT="$FIXER" \
    assert_resource_paths "$SPEC" "$FIXER"
HOME="$TEST_DIR" XDG_DATA_HOME="$USER_DATA" ANOLISA_ADAPTER_DIR="$ADAPTERS" \
    TOKENLESS_TOOL_READY_SPEC= TOKENLESS_ENV_FIX_SCRIPT= \
    assert_resource_paths "$ADAPTERS/common/tool-ready-spec.json" "$ADAPTERS/common/tokenless-env-fix.sh"
HOME="$TEST_DIR" XDG_DATA_HOME="$USER_DATA" ANOLISA_ADAPTER_DIR= \
    TOKENLESS_TOOL_READY_SPEC= TOKENLESS_ENV_FIX_SCRIPT= \
    assert_resource_paths "$USER_DATA/tokenless/adapters/common/tool-ready-spec.json" \
        "$USER_DATA/tokenless/adapters/common/tokenless-env-fix.sh"

echo "tool-ready resource precedence test passed"
