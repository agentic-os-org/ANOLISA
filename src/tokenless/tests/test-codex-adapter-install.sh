#!/usr/bin/env bash
# Regression tests for the Codex adapter's ANOLISA_DRY_RUN previews.
#
# The adapter is the only one of the tokenless family whose install and
# uninstall did the full job whatever ANOLISA_DRY_RUN said. Its install has two
# write paths a preview must cover - the dev fallback that builds and installs
# the component binary when the host has none, and the marketplace tree plus the
# two `codex plugin` registrations - and its uninstall decides between two
# branches by whether a CLI can be resolved at all.
#
# The suite must not depend on the host: `PATH` is scrubbed, and the adapter's
# own absolute-path probes (/usr/local/bin/tokenless, /usr/bin/tokenless) are
# resolved the same way install.sh resolves them so a developer machine that has
# tokenless installed behaves like a clean CI runner.
set -uo pipefail

PASS=0
FAIL=0
SKIP=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }
skip() { echo "[SKIP] $1"; SKIP=$((SKIP + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-codex-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
PREFIX="$SANDBOX/prefix"
XDG="$SANDBOX/xdg"
CODEX_HOME_DIR="$SANDBOX/codex-home"
mkdir -p "$FAKE_HOME" "$PREFIX/bin" "$SANDBOX/bin" "$XDG" "$CODEX_HOME_DIR"

INSTALL_SH="$ADAPTER_DIR/codex/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/codex/scripts/uninstall.sh"
MARKETPLACE_DIR="$XDG/anolisa/codex-marketplace"
AGENTS_DIR="$MARKETPLACE_DIR/.agents/plugins"
PLUGIN_LINK="$MARKETPLACE_DIR/tokenless"
PLUGIN_SRC="$(cd "$ADAPTER_DIR/codex" && pwd)"
# install.sh resolves its source tree as `dirname(script)/../../../..`, which
# is the tokenless crate root in this checkout.
SRCDIR_EXPECTED="$(cd "$(dirname "$INSTALL_SH")/../../../.." && pwd)"
STUB_LOG="$FAKE_HOME/.codex-stub.log"
TOKENLESS_BIN="$PREFIX/bin/tokenless"
CONFIG_TOML="$CODEX_HOME_DIR/config.toml"

write_tokenless_stub() {
    cat > "$TOKENLESS_BIN" <<'STUBEOF'
#!/usr/bin/env bash
[ "${1:-}" = "--version" ] && echo "tokenless 0.0.0-test"
exit 0
STUBEOF
    chmod +x "$TOKENLESS_BIN"
}

# install.sh walks `command -v tokenless` (under the scrubbed PATH below), then
# `$BINDIR/tokenless` - the sandbox stub - and only then /usr/local/bin and
# /usr/bin. Ask the same question instead of recomputing that list, so the
# expectation cannot drift from the resolution the adapter performs.
EFFECTIVE_BIN="$(env -i PATH="$SANDBOX/bin:/usr/bin:/bin" bash -c 'command -v tokenless' || true)"
[ -n "$EFFECTIVE_BIN" ] || EFFECTIVE_BIN="$TOKENLESS_BIN"

# HOST_BIN only decides whether the no-binary scenario can run at all: that is
# the context where the stub has been moved away and an absolute fallback wins.
HOST_BIN=""
for candidate in /usr/local/bin/tokenless /usr/bin/tokenless; do
    if [ -x "$candidate" ]; then
        HOST_BIN="$candidate"
        break
    fi
done

write_tokenless_stub

# The codex CLI stand-in: it logs every call and answers both listings with an
# empty inventory, so only the file operations under test are observable.
CODEX_STUB="$SANDBOX/bin/codex"
cat > "$CODEX_STUB" <<'STUBEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/.codex-stub.log"
exit 0
STUBEOF
chmod +x "$CODEX_STUB"

# A `cargo` stand-in that records every invocation: "no build happened" is then
# proven by an empty log rather than by a file name nothing ever creates.
CARGO_LOG="$SANDBOX/cargo.log"
cat > "$SANDBOX/bin/cargo" <<STUBEOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$CARGO_LOG"
exit 0
STUBEOF
chmod +x "$SANDBOX/bin/cargo"

# `env -i` keeps the host out of the picture: the flag under test must be the
# only one in play, and `command -v tokenless` must not find a product installed
# on the developer machine.
env_for() {
    env -i PATH="$SANDBOX/bin:/usr/bin:/bin" HOME="$FAKE_HOME" \
        TOKENLESS_INSTALL_PREFIX="$PREFIX" XDG_DATA_HOME="$XDG" \
        CODEX_HOME="$CODEX_HOME_DIR" CODEX_BIN="$CODEX_STUB" \
        CARGO_LOG="$CARGO_LOG" "$@"
}
calls() {
    local n
    n="$(grep -Fc -- "$1" "$STUB_LOG" 2>/dev/null)"
    echo "${n:-0}"
}
snapshot_log() { cat "$STUB_LOG" 2>/dev/null || true; }
log_size() { wc -c < "$STUB_LOG" 2>/dev/null || echo 0; }
expect_line() {  # <label> <expected line> <output>
    case "$3" in
        *"$2"*) pass "$1" ;;
        *) fail "$1 (missing: $2)" ;;
    esac
}
expect_no_line() {  # <label> <unexpected line> <output>
    if printf '%s\n' "$3" | grep -Fq -- "$2"; then
        fail "$1 (unexpected: $2)"
    else
        pass "$1"
    fi
}

# ── A dry-run install with the binary in place previews and writes nothing ──
dry_install_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
expect_line "dry-run install keeps the resolved binary" \
    "DRY-RUN: keep the existing tokenless binary $EFFECTIVE_BIN" "$dry_install_out"
expect_line "dry-run install previews the marketplace tree" \
    "DRY-RUN: mkdir -p $AGENTS_DIR" "$dry_install_out"
expect_line "dry-run install previews the plugin symlink" \
    "DRY-RUN: ln -sfn $PLUGIN_SRC $PLUGIN_LINK" "$dry_install_out"
expect_line "dry-run install previews the manifest write" \
    "DRY-RUN: write $AGENTS_DIR/marketplace.json" "$dry_install_out"
expect_line "dry-run install previews the conditional marketplace removal" \
    "DRY-RUN: $CODEX_STUB plugin marketplace remove anolisa-tokenless (only when it is already registered)" "$dry_install_out"
expect_line "dry-run install previews the marketplace registration" \
    "DRY-RUN: $CODEX_STUB plugin marketplace add $MARKETPLACE_DIR" "$dry_install_out"
expect_line "dry-run install previews the plugin registration" \
    "DRY-RUN: $CODEX_STUB plugin add tokenless@anolisa-tokenless" "$dry_install_out"
if [ ! -e "$MARKETPLACE_DIR" ] && [ ! -e "$STUB_LOG" ]; then
    pass "dry-run install leaves the marketplace tree, the manifest and the CLI alone"
else
    fail "dry-run install wrote to the host"
fi

# ── A dry-run install without the binary previews the build it would run ────
if [ -n "$HOST_BIN" ]; then
    skip "host has $HOST_BIN, so the no-binary preview cannot be exercised here"
else
    mv "$TOKENLESS_BIN" "$SANDBOX/tokenless.saved"
    no_binary_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
    mv "$SANDBOX/tokenless.saved" "$TOKENLESS_BIN"
    expect_line "dry-run install previews the source build" \
        "DRY-RUN: cargo build --release -p tokenless-cli in $SRCDIR_EXPECTED" "$no_binary_out"
    expect_line "dry-run install previews the built binary install" \
        "DRY-RUN: install the built binary to $TOKENLESS_BIN (mode 755)" "$no_binary_out"
    if [ ! -e "$MARKETPLACE_DIR" ] && [ ! -e "$STUB_LOG" ] \
            && [ ! -s "$CARGO_LOG" ]; then
        pass "a host without the binary is not built on in a dry run"
    else
        fail "the dry run built or installed the component binary"
    fi

    # …and without a source tree the real run refuses at Phase 1, so the dry run
    # must refuse at the same point, with the same message and the same exit
    # code — not preview Phase 2 writes a real run never reaches.
    mv "$TOKENLESS_BIN" "$SANDBOX/tokenless.saved"
    empty_source="$SANDBOX/empty-source"
    mkdir -p "$empty_source"
    no_source_rc=0
    no_source_out="$(env_for env ANOLISA_DRY_RUN=1 TOKENLESS_SOURCE_DIR="$empty_source" bash "$INSTALL_SH" 2>&1)" || no_source_rc=$?
    mv "$SANDBOX/tokenless.saved" "$TOKENLESS_BIN"
    if [ "$no_source_rc" = 1 ]; then
        pass "a dry run without a source tree exits like the real run"
    else
        fail "a dry run without a source tree: expected exit 1, got $no_source_rc"
    fi
    expect_line "a dry run without a source tree reports the refusal" "ERROR: tokenless binary not found and no source tree at $empty_source" "$no_source_out"
    expect_no_line "a dry run without a source tree emits no preview" "DRY-RUN:" "$no_source_out"
    if [ ! -e "$MARKETPLACE_DIR" ]; then
        pass "a refused dry run creates no marketplace tree"
    else
        fail "a refused dry run created the marketplace tree"
    fi
fi

# ── A dry run without the codex CLI names no work it cannot reach ───────────
no_codex_out="$(env_for env ANOLISA_DRY_RUN=1 CODEX_BIN="$SANDBOX/bin/missing-codex" \
    bash "$INSTALL_SH" 2>&1)"
expect_line "a dry run without the codex CLI reports the skipped registration" \
    "DRY-RUN: no codex CLI found, so the registration and the marketplace directory are both skipped" "$no_codex_out"
expect_no_line "a dry run without the codex CLI does not preview the marketplace tree" \
    "mkdir -p $AGENTS_DIR" "$no_codex_out"

# ── The real install still builds the layout and drives the CLI ─────────────
if env_for bash "$INSTALL_SH" >/dev/null 2>&1; then
    pass "install succeeds"
else
    fail "install failed"
fi
if [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SRC" ] && \
        [ -f "$AGENTS_DIR/marketplace.json" ]; then
    pass "install links the plugin in and writes the marketplace manifest"
else
    fail "install did not create the marketplace layout"
fi
if [ "$(calls 'plugin marketplace add')" -ge 1 ] && \
        [ "$(calls 'plugin add tokenless@anolisa-tokenless')" -ge 1 ]; then
    pass "install registers the marketplace and the plugin through the CLI"
else
    fail "install did not register with the CLI"
fi
if [ "$(log_size)" -gt 0 ]; then
    pass "the CLI log is a usable snapshot"
else
    fail "the CLI log is empty, so the snapshots below prove nothing"
fi

# ── A dry-run uninstall previews and removes nothing ────────────────────────
calls_before="$(snapshot_log)"
dry_uninstall_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" --non-interactive 2>&1)"
expect_line "dry-run uninstall previews the plugin removal" \
    "DRY-RUN: $CODEX_STUB plugin remove tokenless@anolisa-tokenless (only when it is registered)" "$dry_uninstall_out"
expect_line "dry-run uninstall previews the marketplace removal" \
    "DRY-RUN: $CODEX_STUB plugin marketplace remove anolisa-tokenless (only when it is registered)" "$dry_uninstall_out"
expect_line "dry-run uninstall previews the marketplace directory removal" \
    "DRY-RUN: rm -rf $MARKETPLACE_DIR (only when it is there and the registration is confirmed gone)" "$dry_uninstall_out"
expect_line "dry-run uninstall previews the binary removal" \
    "DRY-RUN: rm -f $TOKENLESS_BIN (only when it is there; interactive mode asks first; without a terminal it is kept)" "$dry_uninstall_out"
if [ -L "$PLUGIN_LINK" ] && [ -f "$TOKENLESS_BIN" ] && \
        [ "$(snapshot_log)" = "$calls_before" ]; then
    pass "dry-run uninstall leaves the marketplace, the binary and the CLI alone"
else
    fail "dry-run uninstall removed host state"
fi

# ── A deregister-only dry run previews no binary removal ────────────────────
deregister_out="$(env_for env ANOLISA_DRY_RUN=1 TOKENLESS_DEREGISTER_ONLY=1 \
    bash "$UNINSTALL_SH" --non-interactive 2>&1)"
expect_no_line "a deregister-only dry run leaves the component binary out of the preview" \
    "rm -f $TOKENLESS_BIN" "$deregister_out"

# ── No CLI: the preview follows the config.toml state the real path reads ───
# A config.toml that still names tokenless keeps everything and exits 1.
cat > "$CONFIG_TOML" <<'TOMLEOF'
[plugins."tokenless@anolisa-tokenless"]
enabled = true
TOMLEOF
mentioned_out="$(env_for env ANOLISA_DRY_RUN=1 CODEX_BIN="$SANDBOX/bin/missing-codex" \
    bash "$UNINSTALL_SH" --non-interactive 2>&1)"
expect_line "a no-CLI dry run with config.toml naming tokenless keeps the tree" \
    "DRY-RUN: no codex CLI resolved and $CONFIG_TOML still names tokenless, so the real run keeps" "$mentioned_out"
# ...but the component binary is a separate decision in the real path, which
# removes it whenever TOKENLESS_DEREGISTER_ONLY is unset.
expect_line "that preview still announces the binary removal" \
    "DRY-RUN: rm -f $TOKENLESS_BIN (only when it is there; interactive mode asks first; without a terminal it is kept)" "$mentioned_out"
expect_no_line "that preview does not announce the directory removal" \
    "rm -rf $MARKETPLACE_DIR" "$mentioned_out"
if [ -L "$PLUGIN_LINK" ]; then
    pass "the dry run kept the marketplace tree it said it would keep"
else
    fail "the dry run removed the marketplace tree"
fi
# The real run in the same state: directory kept, binary gone, exit 1 - the
# contract tests/test-install-script.sh pins for this shape.
real_run_out="$(env_for env CODEX_BIN="$SANDBOX/bin/missing-codex" \
        bash "$UNINSTALL_SH" --non-interactive 2>&1)"
real_run_rc=$?
# The exit code is the predicate; the output is kept for the failure message.
if [ "$real_run_rc" -eq 0 ]; then
    fail "an unconfirmable registration must exit 1: ${real_run_out}"
elif [ -L "$PLUGIN_LINK" ] && [ ! -e "$TOKENLESS_BIN" ]; then
    pass "the real run keeps the marketplace directory and still removes the binary"
else
    fail "the real run did not mirror its preview (link=$([ -L "$PLUGIN_LINK" ] && echo present || echo gone), binary=$([ -e "$TOKENLESS_BIN" ] && echo present || echo gone)): ${real_run_out}"
fi
# Restore the stub binary for the scenarios below.
write_tokenless_stub

# Without a record the real path does remove the directory, so the preview must
# say so - that was the hidden `rm -rf` this branch used to omit.
rm -f "$CONFIG_TOML"
clean_out="$(env_for env ANOLISA_DRY_RUN=1 CODEX_BIN="$SANDBOX/bin/missing-codex" \
    bash "$UNINSTALL_SH" --non-interactive 2>&1)"
expect_line "a no-CLI dry run without a record announces the directory removal" \
    "DRY-RUN: rm -rf $MARKETPLACE_DIR (only when it is there), and the real run exits 0" "$clean_out"
expect_line "that preview announces the binary removal as well" \
    "DRY-RUN: rm -f $TOKENLESS_BIN (only when it is there; interactive mode asks first; without a terminal it is kept)" "$clean_out"
if [ -L "$PLUGIN_LINK" ] && [ -f "$TOKENLESS_BIN" ]; then
    pass "the announcing dry run still removed nothing"
else
    fail "the dry run removed host state it only previewed"
fi

# ── The real uninstall still removes both ──────────────────────────────────
if env_for bash "$UNINSTALL_SH" --non-interactive >/dev/null 2>&1; then
    pass "uninstall succeeds"
else
    fail "uninstall failed"
fi
if [ ! -e "$MARKETPLACE_DIR" ] && [ ! -e "$TOKENLESS_BIN" ]; then
    pass "uninstall removes the marketplace directory and the component binary"
else
    fail "uninstall left the marketplace directory or the binary behind"
fi

echo ""
echo "Codex adapter tests: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
