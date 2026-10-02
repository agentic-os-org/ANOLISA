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
