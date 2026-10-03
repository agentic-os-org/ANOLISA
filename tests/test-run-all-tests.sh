#!/bin/bash
set -euo pipefail

# Regression tests for tests/run-all-tests.sh. The runner is executed against
# a stub toolchain (fake npm/make/cargo/uv/python3 on PATH that record how
# they were called), so no real suite runs and the assertions can check the
# three properties that once broke together:
#   1. the sec-core e2e script is invoked at its absolute path and resolves;
#   2. every component's cd stays inside its suite, so suites are
#      order-independent even in one process;
#   3. a PATH-installed linux-sandbox enables the e2e suite (matching the
#      shutil.which() discovery of e2e_test.py itself).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="$ROOT_DIR/tests/run-all-tests.sh"
E2E_SCRIPT="$ROOT_DIR/src/agent-sec-core/tests/e2e/linux-sandbox/e2e_test.py"

WORK_DIR=$(mktemp -d)
STUB_BIN="$WORK_DIR/bin"
CALLS="$WORK_DIR/calls.log"
mkdir -p "$STUB_BIN"
: > "$CALLS"

trap 'rm -rf "$WORK_DIR"' EXIT

# Record "tool|cwd|args" per call. The python3 stub also asserts that the
# script path it was handed resolves in this checkout (property 1).
for tool in npm make cargo uv; do
    printf '#!/bin/bash\necho "%s|$(pwd)|$*" >> %q\nexit 0\n' "$tool" "$CALLS" > "$STUB_BIN/$tool"
    chmod +x "$STUB_BIN/$tool"
done
printf '#!/bin/bash\necho "python3|$(pwd)|$*" >> %q\n[ -f "$1" ] || { echo "python3 stub: $1 does not exist" >&2; exit 2; }\nexit 0\n' "$CALLS" > "$STUB_BIN/python3"
chmod +x "$STUB_BIN/python3"

# The sandbox stub exists ONLY on PATH (property 3): nothing is ever placed
# at /usr/local/bin/linux-sandbox, so a runner that gates on that single
# location skips the e2e suite and property 1's assertion fails.
printf '#!/bin/bash\nexit 0\n' > "$STUB_BIN/linux-sandbox"
chmod +x "$STUB_BIN/linux-sandbox"

# Report Linux regardless of the host so the uname-guarded agent-memory
# suite is exercised on every platform, including macOS.
printf '#!/bin/bash\n[ "$1" = "-s" ] && echo Linux && exit 0\nexit 1\n' > "$STUB_BIN/uname"
chmod +x "$STUB_BIN/uname"

assert_recorded() {
    if ! grep -qxF "$1" "$CALLS"; then
        echo "ERROR: expected runner call not recorded: $1" >&2
        echo "Recorded calls:" >&2
        cat "$CALLS" >&2
        exit 1
    fi
}

assert_not_recorded() {
    if grep -qF "$1" "$CALLS"; then
        echo "ERROR: unexpected runner call recorded: $1" >&2
        exit 1
    fi
}

# Case 1: full run. Started from a directory outside the repo to also prove
# the runner locates itself via ROOT_DIR. The recorded per-tool cwds pin
# property 2: with leaking cds, a later suite would record the previous
# component's directory instead of its own.
( cd "$WORK_DIR" && PATH="$STUB_BIN:$PATH" "$RUNNER" ) > "$WORK_DIR/run-full.log" 2>&1

assert_recorded "npm|$ROOT_DIR/deprecated/copilot-shell|test"
assert_recorded "make|$(cd "$WORK_DIR" && pwd)|-C $ROOT_DIR/src/agent-sec-core test-python"
assert_recorded "python3|$(cd "$WORK_DIR" && pwd)|$E2E_SCRIPT"
assert_recorded "cargo|$ROOT_DIR/src/agentsight|test"
assert_recorded "make|$ROOT_DIR/src/tokenless|test"
assert_recorded "make|$ROOT_DIR/src/agent-memory|test"

# Property 1, restated as a runnable assertion on the real checkout.
test -f "$E2E_SCRIPT"

# Case 2: --filter sec runs only the sec suite, reaching the e2e script from
# the repo root, where the old relative path resolved nowhere.
: > "$CALLS"
( cd "$ROOT_DIR" && PATH="$STUB_BIN:$PATH" "$RUNNER" --filter sec ) > "$WORK_DIR/run-sec.log" 2>&1
assert_recorded "make|$(cd "$ROOT_DIR" && pwd)|-C $ROOT_DIR/src/agent-sec-core test-python"
assert_recorded "python3|$(cd "$ROOT_DIR" && pwd)|$E2E_SCRIPT"
assert_not_recorded "npm|"

# Case 3: an unknown filter must fail the run instead of silently passing.
if ( PATH="$STUB_BIN:$PATH" "$RUNNER" --filter bogus ) > "$WORK_DIR/run-bogus.log" 2>&1; then
    echo "ERROR: unknown filter did not fail the run" >&2
    exit 1
fi

echo "Root test runner regression tests passed"
