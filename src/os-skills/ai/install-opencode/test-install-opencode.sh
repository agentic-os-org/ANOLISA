#!/bin/sh
# test-install-opencode.sh — Mocked-binary smoke tests for scripts/install.sh.
#
# Covers: happy path via the anolisa route (link created by the mocked driver +
# status verify), npm fallback via the adapter's own install.sh, --skip-tokenless,
# missing opencode prereq (exit 1), bogus argument (exit 2), no registration
# route (exit 1), and link-verification failure (exit 1).
#
# Usage: sh test-install-opencode.sh  (run from the install-opencode skill directory)

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL="$SCRIPT_DIR/scripts/install.sh"

failures=0
pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; failures=$((failures + 1)); }

# make_env — sandbox HOME with a mocked opencode CLI, a fake adapter tree
# (plugin.js + scripts/install.sh that creates the managed link), and a bin
# dir for the mocked anolisa CLI. OBIN holds the opencode mock alone, for
# runs that must not see the anolisa CLI.
make_env() {
    TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/install-opencode-test.XXXXXX")"
    BIN="$TEST_HOME/bin"
    OBIN="$TEST_HOME/obin"
    mkdir -p "$BIN" "$OBIN"
    ADAPTER_DIR="$TEST_HOME/.local/share/anolisa/adapters/tokenless/opencode"
    mkdir -p "$ADAPTER_DIR/scripts"

    cat >"$OBIN/opencode" <<'EOF'
#!/bin/sh
[ "$1" = "--version" ] && echo "opencode mock 1.0.0" && exit 0
exit 0
EOF
    cp "$OBIN/opencode" "$BIN/opencode"
    chmod +x "$OBIN/opencode" "$BIN/opencode"

    PLUGIN_SRC="$ADAPTER_DIR/plugin.js"
    printf 'export {}\n' >"$PLUGIN_SRC"
    cat >"$ADAPTER_DIR/scripts/install.sh" <<EOF
#!/bin/sh
# Mock of the adapter's install.sh: create the managed symlink.
mkdir -p "\$HOME/.config/opencode/plugins"
ln -s "$PLUGIN_SRC" "\$HOME/.config/opencode/plugins/tokenless.js"
EOF
    chmod +x "$ADAPTER_DIR/scripts/install.sh"

    cat >"$BIN/anolisa" <<'EOF'
#!/bin/sh
echo "$@" >>"${ANOLISA_CALLS:?}"
case " $* " in
    *" adapter enable tokenless opencode "*)
        mkdir -p "$HOME/.config/opencode/plugins"
        ln -s "$HOME/.local/share/anolisa/adapters/tokenless/opencode/plugin.js" \
            "$HOME/.config/opencode/plugins/tokenless.js" 2>/dev/null || true
        ;;
    *" adapter status "*) : ;;
    *) echo "unknown invocation: $*" >&2; exit 64 ;;
esac
exit 0
EOF
    chmod +x "$BIN/anolisa"
    ANOLISA_CALLS="$TEST_HOME/anolisa-calls"
    : >"$ANOLISA_CALLS"
}

# run_with_anolisa <args...> — run the wrapper with the mocked anolisa on PATH.
run_with_anolisa() {
    make_env
    PATH="$BIN:/usr/bin:/bin" HOME="$TEST_HOME" ANOLISA_CALLS="$ANOLISA_CALLS" \
        sh "$INSTALL" "$@" >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
}

# run_without_anolisa — run the wrapper on the npm-fallback route
# (only the opencode mock is visible).
run_without_anolisa() {
    make_env
    PATH="$OBIN:/usr/bin:/bin" HOME="$TEST_HOME" \
        sh "$INSTALL" >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
}

# --- 1. Happy path (anolisa route): enable + link + status --------------------
run_with_anolisa
rc=$?
LINK="$TEST_HOME/.config/opencode/plugins/tokenless.js"
if [ "$rc" -eq 0 ] && grep -q "adapter enable tokenless opencode" "$ANOLISA_CALLS" \
    && grep -q "adapter status tokenless" "$ANOLISA_CALLS" \
    && [ -L "$LINK" ] && [ -e "$LINK" ] \
    && grep -q "plugin link verified" "$TEST_HOME/stdout"; then
    pass "happy path: anolisa enable + link verified + status"
else
    fail "happy path (rc=$rc)"
fi

# --- 2. npm fallback route: adapter install.sh creates the link --------------
run_without_anolisa
rc=$?
LINK="$TEST_HOME/.config/opencode/plugins/tokenless.js"
if [ "$rc" -eq 0 ] && [ -L "$LINK" ] && [ -e "$LINK" ]; then
    pass "npm fallback: adapter install.sh ran and link verified"
else
    fail "npm fallback (rc=$rc)"
fi

# --- 3. --skip-tokenless: preflight only, no link, exit 0 --------------------
run_with_anolisa --skip-tokenless
rc=$?
if [ "$rc" -eq 0 ] && ! grep -q "adapter enable" "$ANOLISA_CALLS" \
    && [ ! -e "$TEST_HOME/.config/opencode/plugins/tokenless.js" ]; then
    pass "--skip-tokenless: no registration attempted"
else
    fail "--skip-tokenless (rc=$rc)"
fi

# --- 4. Missing opencode CLI: exit 1 -----------------------------------------
TEST_HOME2="$(mktemp -d "${TMPDIR:-/tmp}/install-opencode-test.XXXXXX")"
mkdir -p "$TEST_HOME2/bin"
printf '#!/bin/sh\nexit 0\n' >"$TEST_HOME2/bin/anolisa"
chmod +x "$TEST_HOME2/bin/anolisa"
PATH="$TEST_HOME2/bin:/usr/bin:/bin" HOME="$TEST_HOME2" sh "$INSTALL" \
    >"$TEST_HOME2/stdout" 2>"$TEST_HOME2/stderr"
rc=$?
if [ "$rc" -eq 1 ] && grep -q "opencode CLI not found" "$TEST_HOME2/stderr"; then
    pass "missing opencode prereq: exit 1 with guidance"
else
    fail "missing opencode prereq (rc=$rc)"
fi

# --- 5. Bogus argument: exit 2 -----------------------------------------------
run_with_anolisa --bogus
rc=$?
if [ "$rc" -eq 2 ] && grep -q "unknown argument" "$TEST_HOME/stderr"; then
    pass "bogus argument: exit 2 with usage"
else
    fail "bogus argument (rc=$rc)"
fi

# --- 6. No registration route (no anolisa, no adapter script): exit 1 --------
make_env
rm -rf "$TEST_HOME/.local/share/anolisa"
PATH="$OBIN:/usr/bin:/bin" HOME="$TEST_HOME" sh "$INSTALL" \
    >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
rc=$?
if [ "$rc" -eq 1 ] && grep -q "Tokenless is not installed" "$TEST_HOME/stderr"; then
    pass "no registration route: exit 1 with install pointer"
else
    fail "no registration route (rc=$rc)"
fi

# --- 7. Verification failure (enable runs but creates no link): exit 1 -------
make_env
cat >"$BIN/anolisa" <<'EOF'
#!/bin/sh
case " $* " in
    *" adapter enable "*) echo "enable ok (mock, no link)" ;;
    *" adapter status "*) exit 0 ;;
    *) exit 64 ;;
esac
exit 0
EOF
chmod +x "$BIN/anolisa"
PATH="$BIN:/usr/bin:/bin" HOME="$TEST_HOME" sh "$INSTALL" \
    >"$TEST_HOME/stdout" 2>"$TEST_HOME/stderr"
rc=$?
if [ "$rc" -eq 1 ] && grep -q "plugin link missing" "$TEST_HOME/stderr"; then
    pass "verification failure: exit 1 when the link is absent"
else
    fail "verification failure (rc=$rc)"
fi

rm -rf "${TMPDIR:-/tmp}"/install-opencode-test.*
if [ "$failures" -eq 0 ]; then
    echo "All install-opencode tests passed."
    exit 0
fi
echo "$failures test(s) failed." >&2
exit 1
