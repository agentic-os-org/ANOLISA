#!/usr/bin/env bash
set -euo pipefail

# Regression tests for tests/run-all-tests.sh path handling.
#
# The runner must work regardless of the caller's working directory and of
# the order in which component suites execute: component `cd`s stay inside
# subshells, and the agent-sec-core e2e script is invoked through an
# absolute path instead of a path relative to whatever directory a previous
# suite happened to leave behind.

RUNNER_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run-all-tests.sh"

TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

# ── Fixture repository with the directory shape the runner expects ──────
mkdir -p \
    "$TEST_TMP/tests" \
    "$TEST_TMP/deprecated/copilot-shell" \
    "$TEST_TMP/src/agent-sec-core/tests/e2e/linux-sandbox" \
    "$TEST_TMP/src/agentsight" \
    "$TEST_TMP/src/tokenless" \
    "$TEST_TMP/src/agent-memory"
cp "$RUNNER_SRC" "$TEST_TMP/tests/run-all-tests.sh"

E2E_MARKER="$TEST_TMP/src/agent-sec-core/tests/e2e/linux-sandbox/e2e_test.py"
cat > "$E2E_MARKER" << 'PYEOF'
import os
import sys

with open(os.environ["E2E_INVOCATION_LOG"], "a", encoding="utf-8") as fh:
    fh.write(sys.argv[0] + "\n")
PYEOF

# ── Command stubs that record how they were reached ────────────────────
STUB_BIN="$TEST_TMP/stubs"
mkdir -p "$STUB_BIN"

cat > "$STUB_BIN/python3" << 'SHEOF'
#!/usr/bin/env bash
# Only the e2e script is intercepted; anything else runs the real binary.
for arg in "$@"; do
    case "$arg" in
        *e2e_test.py)
            printf '%s\n' "$arg" >> "$E2E_INVOCATION_LOG"
            exit 0
            ;;
    esac
done
exec /usr/bin/python3 "$@"
SHEOF

cat > "$STUB_BIN/npm" << 'SHEOF'
#!/usr/bin/env bash
printf 'npm cwd=%s\n' "$PWD" >> "$NPM_CWD_LOG"
exit 0
SHEOF

# A PATH-visible sandbox stub so the runner's e2e branch actually executes
# in this fixture (production hosts rely on /usr/local/bin, which a test
# cannot create without privileges).
cat > "$STUB_BIN/linux-sandbox" << 'SHEOF'
#!/usr/bin/env bash
exit 0
SHEOF

cat > "$STUB_BIN/make" << 'SHEOF'
#!/usr/bin/env bash
printf 'make args=%s cwd=%s\n' "$*" "$PWD" >> "$MAKE_LOG"
exit 0
SHEOF

cat > "$STUB_BIN/cargo" << 'SHEOF'
#!/usr/bin/env bash
printf 'cargo args=%s cwd=%s\n' "$*" "$PWD" >> "$CARGO_LOG"
exit 0
SHEOF

chmod +x "$STUB_BIN/python3" "$STUB_BIN/npm" "$STUB_BIN/make" "$STUB_BIN/cargo" "$STUB_BIN/linux-sandbox"
export PATH="$STUB_BIN:$PATH"
export E2E_INVOCATION_LOG="$TEST_TMP/e2e-invocations.log"
export NPM_CWD_LOG="$TEST_TMP/npm-cwd.log"
export MAKE_LOG="$TEST_TMP/make.log"
export CARGO_LOG="$TEST_TMP/cargo.log"
touch "$E2E_INVOCATION_LOG" "$NPM_CWD_LOG" "$MAKE_LOG" "$CARGO_LOG"

fail_count=0

fail() {
    echo "not ok - $*" >&2
    fail_count=$((fail_count + 1))
}

# ── 1. --filter sec from an unrelated working directory ────────────────
# The e2e script must still be reached: previously the relative path
# `tests/e2e/linux-sandbox/e2e_test.py` resolved against the caller's cwd
# (or against deprecated/copilot-shell after the shell suite), neither of
# which contains that path.
(cd /tmp && bash "$TEST_TMP/tests/run-all-tests.sh" --filter sec >/dev/null 2>&1)
if grep -q "^$TEST_TMP/src/agent-sec-core/tests/e2e/linux-sandbox/e2e_test.py$" "$E2E_INVOCATION_LOG"; then
    echo "ok - sec filter resolves the e2e script absolutely"
else
    fail "sec filter did not invoke the e2e script through its absolute path"
fi

# ── 2. full run: the shell suite's cd must not affect later suites ─────
: > "$E2E_INVOCATION_LOG"
(cd "$TEST_TMP" && bash tests/run-all-tests.sh >/dev/null 2>&1)
if grep -q "^$TEST_TMP/src/agent-sec-core/tests/e2e/linux-sandbox/e2e_test.py$" "$E2E_INVOCATION_LOG"; then
    echo "ok - full run reaches the e2e script after the shell suite"
else
    fail "full run lost the e2e script after the shell suite changed directory"
fi

# ── 3. component suites still execute inside their component directory
# (the subshell isolation must not change where the tools run).
: > "$NPM_CWD_LOG"
(cd "$TEST_TMP" && bash tests/run-all-tests.sh --filter shell >/dev/null 2>&1)
if grep -q "^npm cwd=$TEST_TMP/deprecated/copilot-shell$" "$NPM_CWD_LOG"; then
    echo "ok - npm still runs inside the copilot-shell directory"
else
    fail "npm did not run from the copilot-shell directory"
fi

if [ "$fail_count" -ne 0 ]; then
    echo "# $fail_count failure(s)"
    exit 1
fi
echo "# all path handling checks passed"
