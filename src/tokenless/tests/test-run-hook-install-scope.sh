#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
RUN_HOOK="$SCRIPT_DIR/../adapters/tokenless/common/hooks/run-hook.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

CURRENT_ROOT="$TEST_DIR/current/adapters/tokenless"
STALE_HOME="$TEST_DIR/stale-home"
STALE_ROOT="$STALE_HOME/.local/share/anolisa/adapters/tokenless"
HOOK_NAME="tokenless-install-scope-probe.sh"

mkdir -p \
    "$CURRENT_ROOT/common/hooks" \
    "$CURRENT_ROOT/qwencode/hooks" \
    "$STALE_ROOT/common/hooks"

cp "$RUN_HOOK" "$CURRENT_ROOT/common/hooks/run-hook.sh"

cat >"$CURRENT_ROOT/common/hooks/$HOOK_NAME" <<'EOF'
#!/usr/bin/env bash
echo current
EOF

cat >"$STALE_ROOT/common/hooks/$HOOK_NAME" <<'EOF'
#!/usr/bin/env bash
echo stale
EOF

# Raw installs interpreter-loaded resources as 0644. Makefile preserves this
# relative symlink in the adapter tree, while Qwen links the bundle itself.
chmod 0644 \
    "$CURRENT_ROOT/common/hooks/run-hook.sh" \
    "$CURRENT_ROOT/common/hooks/$HOOK_NAME" \
    "$STALE_ROOT/common/hooks/$HOOK_NAME"
ln -s ../../common/hooks/run-hook.sh "$CURRENT_ROOT/qwencode/hooks/run-hook.sh"

output=$(
    HOME="$STALE_HOME" \
        bash "$CURRENT_ROOT/qwencode/hooks/run-hook.sh" "$HOOK_NAME"
)
[ "$output" = "current" ]

# Hook dispatch must remain fail-open even when a host launches it without HOME.
# The adapter-local candidate is still sufficient and should be selected before
# the optional user-level fallback.
output=$(
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        bash "$CURRENT_ROOT/qwencode/hooks/run-hook.sh" "$HOOK_NAME"
)
[ "$output" = "current" ]

# The same stripped environment must fail open when no candidate exists.
output=$(
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        bash "$CURRENT_ROOT/qwencode/hooks/run-hook.sh" "no-such-hook.sh"
)
[ "$output" = "{}" ]

# RPM's `install` command dereferences the source symlink into a regular file.
rm "$CURRENT_ROOT/qwencode/hooks/run-hook.sh"
cp "$RUN_HOOK" "$CURRENT_ROOT/qwencode/hooks/run-hook.sh"
chmod 0755 "$CURRENT_ROOT/qwencode/hooks/run-hook.sh"

output=$(
    HOME="$STALE_HOME" \
        bash "$CURRENT_ROOT/qwencode/hooks/run-hook.sh" "$HOOK_NAME"
)
[ "$output" = "current" ]

echo "run-hook install scope test passed"
