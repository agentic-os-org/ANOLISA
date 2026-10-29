#!/usr/bin/env bash
# The Hermes and Claude Code lifecycle scripts derive their user-scope paths
# from HOME while running under `set -u`. A host that invokes them without
# HOME -- a minimal runner, a service manager, `env -i` -- therefore aborted
# on an unbound variable before it could say why. That exit status was
# indistinguishable from a real failure, and for the installers and
# uninstallers it did not say whether anything had been written or removed.
#
# This suite pins the contract for the two lifecycle adapters that own a
# writable home:
#
#   - install and uninstall must fail *before* any write when no home is
#     resolvable, and the failure must name the remedy;
#   - an explicit override (HERMES_HOME, CLAUDE_HOME) must keep working when
#     HOME is absent, so a host that knows where its state lives is not
#     forced to invent a HOME;
#   - a host with a normal HOME must see unchanged behavior.
#
# Plain bash over a copy of the checked-out adapter tree with every agent CLI
# pinned to a path that does not exist -- no cargo build output, node, jq,
# rtk, python3 or network, and no host agent is ever executed -- so it costs
# well under a second.
set -uo pipefail

PASS=0
FAIL=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-lifecycle-home.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

# The scripts are exercised against a copy of the adapter tree, the way the
# other adapter suites do: the adapters resolve their own plugin sources
# relative to the script location, and a copy keeps the checked-out tree
# untouched.
ADAPTER_DIR="$SANDBOX/adapter"
mkdir -p "$ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

# hermes/plugin.yaml is generated from plugin.yaml.in at build time and is not
# checked in, but the installer requires it to be present. Stand in a minimal
# one so the suite runs against a plain checkout.
if [ ! -f "$ADAPTER_DIR/hermes/plugin.yaml" ]; then
    cp "$ADAPTER_DIR/hermes/plugin.yaml.in" "$ADAPTER_DIR/hermes/plugin.yaml"
fi

HERMES_INSTALL="$ADAPTER_DIR/hermes/scripts/install.sh"
HERMES_UNINSTALL="$ADAPTER_DIR/hermes/scripts/uninstall.sh"
CLAUDE_INSTALL="$ADAPTER_DIR/claude-code/scripts/install.sh"
CLAUDE_UNINSTALL="$ADAPTER_DIR/claude-code/scripts/uninstall.sh"

for f in "$HERMES_INSTALL" "$HERMES_UNINSTALL" "$CLAUDE_INSTALL" "$CLAUDE_UNINSTALL"; do
    [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done

# A home the scripts must not touch: HOME is unset in the runs below, so any
# path derived from a stray default would land here and be visible.
FAKE_HOME="$SANDBOX/home"
mkdir -p "$FAKE_HOME" "$SANDBOX/bin"

# The agent CLIs are pinned to a path that does not exist. The lifecycle
# scripts must not reach a real agent, and a missing CLI is a state these
# scripts already handle, so pinning it keeps the suite hermetic.
NO_SUCH_CLI="$SANDBOX/bin/missing-cli"

# A `claude` stand-in. The Claude Code installer stops early when the CLI is
# absent -- it reports "skipping plugin installation" and exits 0 -- so the
# dry-run preview is only reachable with a CLI present. The stub answers the
# version probe and nothing else: the suite is about the lifecycle scripts'
# own home handling, not about the CLI protocol.
# The stub records the environment it was invoked under, so a suite can check
# that an override actually reached the CLI. The CLI resolves its own
# configuration directory from the environment, so an override the script
# keeps to itself would leave the CLI writing somewhere else.
CLAUDE_STUB="$SANDBOX/bin/claude"
cat > "$CLAUDE_STUB" <<'STUBEOF'
#!/usr/bin/env bash
case "$*" in
    --version|version|--help)
        printf '%s\n' "${CLAUDE_CONFIG_DIR:-}" > "${CLAUDE_ENV_LOG:-/dev/null}"
        echo "1.0.0 (Claude Code)"
        exit 0 ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$CLAUDE_STUB"

# run_nohome <script> -- run a lifecycle script with HOME and the home
# overrides absent, and capture its combined output in $SANDBOX/last.out.
run_nohome() {
    local script="$1"
    env -u HOME -u HERMES_HOME -u CLAUDE_HOME \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        ANOLISA_DRY_RUN=0 \
        HERMES_BIN="$NO_SUCH_CLI" \
        CLAUDE_BIN="$NO_SUCH_CLI" \
        bash "$script" >"$SANDBOX/last.out" 2>&1
}

# expect_failure <label> <script> -- the script must fail, must not print
# bash's unbound-variable diagnostic, and must name the remedy.
expect_failure() {
    local label="$1" script="$2"
    if run_nohome "$script"; then
        fail "$label (the script succeeded without a resolvable home)"
        return
    fi
    if grep -Fq "unbound variable" "$SANDBOX/last.out"; then
        fail "$label (it aborted on an unbound variable instead of reporting why)"
        return
    fi
    pass "$label"
}

# expect_message <label> <expected substring>
expect_message() {
    local label="$1" needle="$2"
    if grep -Fq -- "$needle" "$SANDBOX/last.out"; then
        pass "$label"
    else
        fail "$label (missing: $needle)"
    fi
}

# expect_override <label> <expected substring>
# The paths the scripts echo are compared after normalizing the separators:
# a Windows host renders the same sandbox path with mixed separators, and
# the contract under test is that the override directory is the one the
# scripts act on, not how the host spells it.
expect_override() {
    local label="$1" needle="$2"
    local normalized_out normalized_needle
    normalized_out="$(sed -e 's#\\#/#g' -e 's#//#/#g' "$SANDBOX/last.out")"
    normalized_needle="$(printf '%s' "$needle" | sed -e 's#\\#/#g' -e 's#//#/#g')"
    if printf '%s' "$normalized_out" | grep -Fq -- "$normalized_needle"; then
        pass "$label"
    else
        fail "$label (missing: $needle)"
    fi
}

# ── A missing home is reported before anything is written ───────────────────
# The property under test is that a rejected run writes nothing at all, so
# the sandbox is snapshotted before the four rejected runs and compared
# afterwards. A snapshot catches a write anywhere under the sandbox, including
# into the adapter tree the scripts copy their plugin sources from, which a
# check on one guessed path cannot see.
SNAPSHOT_BEFORE="$SANDBOX/snapshot.before"
SNAPSHOT_AFTER="$SANDBOX/snapshot.after"
# The suite's own bookkeeping is not part of what the scripts may touch.
snapshot_list() {
    find "$SANDBOX" -mindepth 1 2>/dev/null \
        | grep -v -e "/last.out$" -e "/snapshot.before$" -e "/snapshot.after$" \
              -e "/snapshot.diff$" -e "/stderr$" -e "/trace.log$" \
        | sort
}
snapshot_list > "$SNAPSHOT_BEFORE"

expect_failure "the Hermes installer rejects an unresolvable home" \
    "$HERMES_INSTALL"
expect_message "the Hermes installer names the remedy" \
    "Set HOME or HERMES_HOME to the Hermes installation directory"

expect_failure "the Hermes uninstaller rejects an unresolvable home" \
    "$HERMES_UNINSTALL"
expect_message "the Hermes uninstaller names the remedy" \
    "Set HOME or HERMES_HOME to the Hermes installation directory"

expect_failure "the Claude Code installer rejects an unresolvable home" \
    "$CLAUDE_INSTALL"
expect_message "the Claude Code installer names the remedy" \
    "Set HOME or CLAUDE_HOME to the Claude Code configuration directory"

expect_failure "the Claude Code uninstaller rejects an unresolvable home" \
    "$CLAUDE_UNINSTALL"
expect_message "the Claude Code uninstaller names the remedy" \
    "Set HOME or CLAUDE_HOME to the Claude Code configuration directory"

# ── Nothing was written on the way out ─────────────────────────────────────
# The installers write the plugin tree and the uninstallers remove it, so a
# rejected run has to leave the sandbox exactly as it found it.
snapshot_list > "$SNAPSHOT_AFTER"
if diff "$SNAPSHOT_BEFORE" "$SNAPSHOT_AFTER" > "$SANDBOX/snapshot.diff" 2>&1; then
    pass "a rejected run wrote nothing under the sandbox"
else
    fail "a rejected run changed the sandbox:
$(cat "$SANDBOX/snapshot.diff")"
fi

# ── An explicit override still works without HOME ──────────────────────────
# A host that knows where its state lives must not be forced to invent a
# HOME: the override is the documented way to say so.
OVERRIDE_HOME="$SANDBOX/override-home"
mkdir -p "$OVERRIDE_HOME"

if env -u HOME HERMES_HOME="$OVERRIDE_HOME/.hermes" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        HERMES_BIN="$NO_SUCH_CLI" \
        bash "$HERMES_INSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Hermes installer accepts an explicit HERMES_HOME without HOME"
else
    fail "the Hermes installer rejects an explicit HERMES_HOME without HOME"
fi
expect_override "the Hermes installer previews the override path" \
    "DRY-RUN: mkdir -p $OVERRIDE_HOME/.hermes/plugins/tokenless"

if env -u HOME HERMES_HOME="$OVERRIDE_HOME/.hermes" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        HERMES_BIN="$NO_SUCH_CLI" \
        bash "$HERMES_UNINSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Hermes uninstaller accepts an explicit HERMES_HOME without HOME"
else
    fail "the Hermes uninstaller rejects an explicit HERMES_HOME without HOME"
fi
expect_override "the Hermes uninstaller previews the override path" \
    "DRY-RUN: rm -f $OVERRIDE_HOME/.hermes/plugins/tokenless/__init__.py"

# CLAUDE_HOME is the configuration directory -- $HOME/.claude -- so the
# override below stands for that directory, not for a home root.
CLAUDE_CONFIG_DIR_OVERRIDE="$OVERRIDE_HOME/.claude"

if env -u HOME CLAUDE_HOME="$CLAUDE_CONFIG_DIR_OVERRIDE" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        CLAUDE_BIN="$NO_SUCH_CLI" \
        bash "$CLAUDE_INSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Claude Code installer accepts an explicit CLAUDE_HOME without HOME"
else
    fail "the Claude Code installer rejects an explicit CLAUDE_HOME without HOME"
fi

if env -u HOME CLAUDE_HOME="$CLAUDE_CONFIG_DIR_OVERRIDE" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        CLAUDE_BIN="$NO_SUCH_CLI" \
        bash "$CLAUDE_UNINSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Claude Code uninstaller accepts an explicit CLAUDE_HOME without HOME"
else
    fail "the Claude Code uninstaller rejects an explicit CLAUDE_HOME without HOME"
fi
expect_override "the Claude Code uninstaller previews the override path" \
    "DRY-RUN: rm -rf $CLAUDE_CONFIG_DIR_OVERRIDE/plugins/cache/anolisa-tokenless"

# The override has to reach the CLI itself, not just the script: the CLI
# resolves its own configuration directory from the environment, so an
# override the script keeps to itself would leave the CLI writing somewhere
# else. The dry run stops before the CLI is driven, so the export is traced
# instead of observed at the call site.
if env -u HOME CLAUDE_HOME="$CLAUDE_CONFIG_DIR_OVERRIDE" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        CLAUDE_BIN="$CLAUDE_STUB" \
        bash -x "$CLAUDE_INSTALL" >"$SANDBOX/last.out" 2>"$SANDBOX/trace.log"; then
    pass "the Claude Code installer runs with an explicit CLAUDE_HOME"
else
    fail "the Claude Code installer failed with an explicit CLAUDE_HOME"
fi
if grep -Fq "export CLAUDE_CONFIG_DIR" "$SANDBOX/trace.log" &&
        grep -Fq "CLAUDE_CONFIG_DIR='$CLAUDE_CONFIG_DIR_OVERRIDE'" "$SANDBOX/trace.log"; then
    pass "the Claude Code CLI is given the override as its configuration directory"
else
    fail "the Claude Code CLI is not given the override as its configuration directory:
$(grep -i CLAUDE_CONFIG_DIR "$SANDBOX/trace.log" | head -4)"
fi

# ── A normal HOME keeps the previous behavior ──────────────────────────────
# The guard only fires when no home is resolvable. With a HOME set, the
# scripts must go on to their normal work -- here the dry-run preview, which
# needs no agent CLI and no writes.
if env HOME="$FAKE_HOME" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        HERMES_BIN="$NO_SUCH_CLI" \
        bash "$HERMES_INSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Hermes installer still works with a normal HOME"
else
    fail "the Hermes installer broke with a normal HOME"
fi
expect_message "the Hermes installer still previews the plugin link" \
    "DRY-RUN: ln -sfn"

if env HOME="$FAKE_HOME" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        HERMES_BIN="$NO_SUCH_CLI" \
        bash "$HERMES_UNINSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Hermes uninstaller still works with a normal HOME"
else
    fail "the Hermes uninstaller broke with a normal HOME"
fi
expect_message "the Hermes uninstaller still previews the removal" \
    "DRY-RUN: rm -f"

if env HOME="$FAKE_HOME" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        CLAUDE_BIN="$CLAUDE_STUB" \
        bash "$CLAUDE_INSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Claude Code installer still works with a normal HOME"
else
    fail "the Claude Code installer broke with a normal HOME"
fi
expect_message "the Claude Code installer still previews the registration" \
    "DRY-RUN:"

if env HOME="$FAKE_HOME" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_DRY_RUN=1 \
        CLAUDE_BIN="$CLAUDE_STUB" \
        bash "$CLAUDE_UNINSTALL" >"$SANDBOX/last.out" 2>&1; then
    pass "the Claude Code uninstaller still works with a normal HOME"
else
    fail "the Claude Code uninstaller broke with a normal HOME"
fi
expect_message "the Claude Code uninstaller still previews the removal" \
    "DRY-RUN:"

# ── The scripts stay syntactically valid ───────────────────────────────────
for f in "$HERMES_INSTALL" "$HERMES_UNINSTALL" "$CLAUDE_INSTALL" "$CLAUDE_UNINSTALL"; do
    if bash -n "$f" 2>/dev/null; then
        pass "$(basename "$(dirname "$(dirname "$f")")")/$(basename "$f") parses"
    else
        fail "$(basename "$f") does not parse"
    fi
done

echo ""
echo "Lifecycle HOME-unset tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
