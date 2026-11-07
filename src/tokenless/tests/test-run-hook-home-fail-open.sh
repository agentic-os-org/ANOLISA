#!/usr/bin/env bash
# run-hook.sh must honour its documented fail-open contract when the host omits
# HOME. The dispatcher runs under `set -u`, so expanding ${HOME} while building
# the candidate list aborts the script before it can reach fail_open: the host
# sees exit 1 with an empty stdout instead of `{}` with exit 0, and every hook
# registered in hooks.json fails on every tool call. The user-local candidate is
# therefore appended conditionally. Plain bash over a mktemp sandbox; no cargo
# build output, node, jq, rtk, python3 or network.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
RUN_HOOK="$SCRIPT_DIR/../adapters/tokenless/common/hooks/run-hook.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

HOOK_NAME="tokenless-home-fail-open-probe.sh"
CURRENT_ROOT="$TEST_DIR/current/adapters/tokenless"
USER_HOME="$TEST_DIR/user-home"
USER_HOOKS="$USER_HOME/.local/share/anolisa/adapters/tokenless/common/hooks"

mkdir -p "$CURRENT_ROOT/common/hooks" "$CURRENT_ROOT/qwencode/hooks" "$USER_HOOKS"
cp "$RUN_HOOK" "$CURRENT_ROOT/common/hooks/run-hook.sh"
cat >"$CURRENT_ROOT/common/hooks/$HOOK_NAME" <<'EOF'
#!/usr/bin/env bash
echo current
EOF
cat >"$USER_HOOKS/$HOOK_NAME" <<'EOF'
#!/usr/bin/env bash
echo user-local
EOF
ln -s ../../common/hooks/run-hook.sh "$CURRENT_ROOT/qwencode/hooks/run-hook.sh"

RUNNER="$CURRENT_ROOT/common/hooks/run-hook.sh"

# Capture stdout, stderr and the exit status of one dispatcher run.
run() {
    status=0
    stdout=$(env "$@" 2>"$TEST_DIR/stderr") || status=$?
    stderr=$(cat "$TEST_DIR/stderr")
}

# 1. HOME unset: the adapter-tree hook still resolves and runs.
run -u HOME bash "$CURRENT_ROOT/qwencode/hooks/run-hook.sh" "$HOOK_NAME"
[ "$status" = 0 ] || { echo "HOME-less run of a present hook exited $status"; exit 1; }
[ "$stdout" = "current" ] || { echo "HOME-less run printed '$stdout', expected 'current'"; exit 1; }
[ -z "$stderr" ] || { echo "HOME-less run wrote to stderr: $stderr"; exit 1; }

# 2. HOME unset and no hook anywhere: fail open with {} and exit 0.
run -u HOME bash "$RUNNER" tokenless-home-fail-open-missing.sh
[ "$status" = 0 ] || { echo "HOME-less fail-open exited $status"; exit 1; }
[ "$stdout" = "{}" ] || { echo "HOME-less fail-open printed '$stdout', expected '{}'"; exit 1; }

# 3. HOME set: the user-local fallback still resolves when the adapter tree and
#    the FHS paths cannot answer.
rm "$CURRENT_ROOT/common/hooks/$HOOK_NAME"
run HOME="$USER_HOME" bash "$RUNNER" "$HOOK_NAME"
[ "$status" = 0 ] || { echo "HOME fallback exited $status"; exit 1; }
[ "$stdout" = "user-local" ] || { echo "HOME fallback printed '$stdout', expected 'user-local'"; exit 1; }

# 4. HOME unset with only the user-local copy present: the candidate is skipped,
#    not expanded, so the dispatcher still fails open.
run -u HOME bash "$RUNNER" "$HOOK_NAME"
[ "$status" = 0 ] || { echo "HOME-less skip exited $status"; exit 1; }
[ "$stdout" = "{}" ] || { echo "HOME-less skip printed '$stdout', expected '{}'"; exit 1; }

echo "run-hook HOME fail-open test passed"
