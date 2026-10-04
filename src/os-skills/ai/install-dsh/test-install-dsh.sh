#!/bin/sh
# test-install-dsh.sh — Mocked-binary smoke tests for scripts/install.sh.
#
# Covers: happy path (multi-profile enable in ONE command + status verify),
# --skip-tokenless, missing prereqs (exit 1), bogus argument (exit 2),
# missing --profile (exit 2), empty --profile value (exit 2), Node < 22
# (exit 1), and enable failure propagation (exit 1).
#
# Usage: sh test-install-dsh.sh  (run from the install-dsh skill directory)

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL="$SCRIPT_DIR/scripts/install.sh"

failures=0
pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; failures=$((failures + 1)); }

# make_env <node_version> — create a sandbox with mocked dsh/anolisa/tokenless/node.
make_env() {
    node_version="${1:-v22.0.0}"
    TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/install-dsh-test.XXXXXX")"
    BIN="$TEST_HOME/bin"
    mkdir -p "$BIN"

    cat >"$BIN/dsh" <<EOF
#!/bin/sh
echo "dsh mock $*"
EOF
    cat >"$BIN/node" <<EOF
#!/bin/sh
[ "\$1" = "--version" ] && echo "$node_version" && exit 0
exit 1
EOF
    cat >"$BIN/tokenless" <<'EOF'
#!/bin/sh
echo "tokenless mock $*"
EOF
    cat >"$BIN/anolisa" <<'EOF'
#!/bin/sh
echo "$@" >>"${ANOLISA_CALLS:?}"
case " $* " in
    *" adapter enable tokenless dsh "*)
        echo "enabled (mock)" ;;
    *" adapter status "*) echo "status: ok" ;;
    *) echo "unknown invocation: $*" >&2; exit 64 ;;
esac
exit 0
EOF
    chmod +x "$BIN/dsh" "$BIN/node" "$BIN/tokenless" "$BIN/anolisa"
    ANOLISA_CALLS="$TEST_HOME/anolisa-calls"
    : >"$ANOLISA_CALLS"
}

run_case() {
    make_env "$1"
    shift
    PATH="$BIN:/usr/bin:/bin" HOME="$TEST_HOME" ANOLISA_CALLS="$ANOLISA_CALLS" \
        sh "$INSTALL" "$@" >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
}

# --- 1. Happy path: two profiles land in a single enable command -------------
run_case v22.11.0 --profile web --profile headless
rc=$?
if [ "$rc" -eq 0 ] && grep -q -- "--profile web --profile headless" "$ANOLISA_CALLS" \
    && grep -q "adapter enable tokenless dsh" "$ANOLISA_CALLS" \
    && grep -q "adapter status tokenless" "$ANOLISA_CALLS"; then
    pass "happy path: one enable with both profiles, then status"
else
    fail "happy path (rc=$rc, calls: $(cat "$ANOLISA_CALLS" 2>/dev/null | tr '\n' ';'))"
fi

# --- 2. --skip-tokenless: preflight only, no enable invocation ---------------
run_case v22.11.0 --profile web --skip-tokenless
rc=$?
if [ "$rc" -eq 0 ] && ! grep -q "adapter enable" "$ANOLISA_CALLS"; then
    pass "--skip-tokenless: no registration attempted"
else
    fail "--skip-tokenless (rc=$rc)"
fi

# --- 3. Missing prereq (no dsh on PATH): exit 1 ------------------------------
TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/install-dsh-test.XXXXXX")"
BIN="$TEST_HOME/bin"
mkdir -p "$BIN"
for cmd in anolisa tokenless node; do
    printf '#!/bin/sh\nexit 0\n' >"$BIN/$cmd"
    chmod +x "$BIN/$cmd"
done
PATH="$BIN:/usr/bin:/bin" HOME="$TEST_HOME" sh "$INSTALL" --profile web \
    >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
rc=$?
if [ "$rc" -eq 1 ] && grep -q "dsh is required" "$TEST_HOME/stderr"; then
    pass "missing dsh prereq: exit 1 with guidance"
else
    fail "missing dsh prereq (rc=$rc)"
fi

# --- 4. Bogus argument: exit 2 -----------------------------------------------
run_case v22.11.0 --profile web --bogus
rc=$?
if [ "$rc" -eq 2 ] && grep -q "unknown argument" "$TEST_HOME/stderr"; then
    pass "bogus argument: exit 2 with usage"
else
    fail "bogus argument (rc=$rc)"
fi

# --- 5. No --profile at all: exit 2 ------------------------------------------
run_case v22.11.0
rc=$?
if [ "$rc" -eq 2 ] && grep -q "at least one --profile" "$TEST_HOME/stderr"; then
    pass "missing --profile: exit 2"
else
    fail "missing --profile (rc=$rc)"
fi

# --- 6. Empty --profile value: exit 2 ----------------------------------------
run_case v22.11.0 --profile ""
rc=$?
if [ "$rc" -eq 2 ]; then
    pass "empty --profile value: exit 2"
else
    fail "empty --profile value (rc=$rc)"
fi

# --- 7. Node < 22: exit 1 ----------------------------------------------------
run_case v20.18.0 --profile web
rc=$?
if [ "$rc" -eq 1 ] && grep -q "Node.js >= 22" "$TEST_HOME/stderr"; then
    pass "old Node: exit 1"
else
    fail "old Node (rc=$rc)"
fi

# --- 8. enable failure propagates: exit 1 ------------------------------------
make_env v22.11.0
cat >"$BIN/anolisa" <<'EOF'
#!/bin/sh
case " $* " in
    *" adapter enable "*) echo "enable failed (mock)" >&2; exit 1 ;;
    *) exit 0 ;;
esac
EOF
chmod +x "$BIN/anolisa"
PATH="$BIN:/usr/bin:/bin" HOME="$TEST_HOME" sh "$INSTALL" --profile web \
    >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
rc=$?
if [ "$rc" -eq 1 ]; then
    pass "enable failure: exit 1 (set -e propagation)"
else
    fail "enable failure (rc=$rc)"
fi

rm -rf "${TMPDIR:-/tmp}"/install-dsh-test.*
if [ "$failures" -eq 0 ]; then
    echo "All install-dsh tests passed."
    exit 0
fi
echo "$failures test(s) failed." >&2
exit 1
