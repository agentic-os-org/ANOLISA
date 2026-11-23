#!/bin/bash
set -e

# Ensure we're running from the root of the repo
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    cat <<'EOF'
Usage: tests/run-all-tests.sh [--filter NAME] [--help]

Without arguments, run the aggregate suite for every component.
With --filter NAME, run only the named component: 'shell', 'sec',
'sight', 'tokenless', or 'memory' ('mem' is an accepted alias).
--help prints this message. Malformed invocations are rejected
without starting any component tests.
EOF
}

FILTER=""
if [ $# -gt 0 ]; then
    if [ "$1" == "--help" ]; then
        if [ $# -ne 1 ]; then
            echo "--help takes no other arguments: $*" >&2
            exit 1
        fi
        usage
        exit 0
    fi
    if [ "$1" != "--filter" ]; then
        echo "Unknown option: $1. Use --filter NAME or --help." >&2
        usage >&2
        exit 1
    fi
    if [ $# -lt 2 ]; then
        echo "--filter requires a component name. Use --help for usage." >&2
        exit 1
    fi
    if [ -z "$2" ]; then
        echo "--filter requires a non-empty component name. Use --help for usage." >&2
        exit 1
    fi
    if [ $# -gt 2 ]; then
        echo "Unexpected extra arguments: $*. Use --filter NAME or --help." >&2
        exit 1
    fi
    FILTER="$2"
fi

run_shell() {
    echo "==> Running copilot-shell tests"
    cd "$ROOT_DIR/deprecated/copilot-shell" || exit 1
    npm test
}

run_sec() {
    echo "==> Running agent-sec-core tests"
    if command -v uv >/dev/null 2>&1; then
        make -C "$ROOT_DIR/src/agent-sec-core" test-python
    else
        echo "uv not found, skipping agent-sec-core Python tests."
    fi

    echo "==> Running agent-sec-core e2e test scripts manually"
    if [ -x /usr/bin/linux-sandbox ] || command -v linux-sandbox >/dev/null 2>&1 || [ -x /usr/local/bin/linux-sandbox ]; then
        python3 "$ROOT_DIR/src/agent-sec-core/tests/e2e/linux-sandbox/e2e_test.py"
    else
        echo "linux-sandbox not found in /usr/bin, PATH or the legacy /usr/local/bin location, skipping e2e_test.py"
    fi
}

run_sight() {
    echo "==> Running agentsight tests"
    cd "$ROOT_DIR/src/agentsight" || exit 1
    if command -v cargo >/dev/null 2>&1; then
        cargo test
    else
        echo "cargo not found, skipping agentsight tests."
    fi
}

run_tokenless() {
    echo "==> Running tokenless tests"
    cd "$ROOT_DIR/src/tokenless" || exit 1
    if command -v make >/dev/null 2>&1; then
        make test
    elif command -v cargo >/dev/null 2>&1; then
        echo "make not found, using cargo directly"
        cargo test --workspace
    else
        echo "cargo not found, skipping tokenless tests."
    fi
}

run_agent_memory() {
    echo "==> Running agent-memory tests (Linux only)"
    cd "$ROOT_DIR/src/agent-memory" || exit 1
    if [ "$(uname -s)" != "Linux" ]; then
        echo "agent-memory is Linux-only; skipping on $(uname -s)."
        return 0
    fi
    # `make test` also runs the bash-only adapter install-script test, which
    # cargo cannot cover; fall back to cargo when make is unavailable.
    if command -v make >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
        make test
    elif command -v cargo >/dev/null 2>&1; then
        echo "make not found, using cargo directly"
        cargo test --locked
    else
        echo "cargo not found, skipping agent-memory tests."
    fi
}

if [ -z "$FILTER" ]; then
    run_shell
    run_sec
    run_sight
    run_tokenless
    run_agent_memory
elif [ "$FILTER" == "shell" ]; then
    run_shell
elif [ "$FILTER" == "sec" ]; then
    run_sec
elif [ "$FILTER" == "sight" ]; then
    run_sight
elif [ "$FILTER" == "tokenless" ]; then
    run_tokenless
elif [ "$FILTER" == "memory" ] || [ "$FILTER" == "mem" ]; then
    run_agent_memory
else
    echo "Unknown filter: $FILTER. Use 'shell', 'sec', 'sight', 'tokenless', or 'memory'."
    exit 1
fi

echo "==> All tests completed successfully!"
