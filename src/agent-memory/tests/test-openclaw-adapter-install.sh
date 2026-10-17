#!/usr/bin/env bash
# Pin the installer flag negotiation of scripts/install.sh against a stub
# openclaw CLI: probe form (help captured, not piped to grep), whole-token
# matching for both negotiated flags — capability consent and the legacy
# unsafe-install bypass — the consent opt-out, AGENT_MEMORY_SAFE_INSTALL, and
# the per-outcome log lines. No real plugin is installed.
#
# Namespaced tools coexist with memory-core: neither script may enable or disable
# that plugin. Old installer markers remain available for manual recovery.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/../adapters/agent-memory/openclaw/scripts/install.sh"
UNINSTALL_SH="$SCRIPT_DIR/../adapters/agent-memory/openclaw/scripts/uninstall.sh"
SANDBOX="$(mktemp -d -t agent-memory-openclaw-install.XXXXXX)"
trap 'rm -r -- "$SANDBOX"' EXIT
# The space in "adapter root" pins argv quoting through PLUGIN_DIR.
mkdir -p "$SANDBOX/adapter root/openclaw/dist"
: > "$SANDBOX/adapter root/openclaw/dist/index.js"

cat > "$SANDBOX/openclaw" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[ -z "${OPENCLAW_HOME+x}" ]
[ "$OPENCLAW_STATE_DIR" = "$TEST_STATE_DIR" ]
printf '%s\n' "$*" >> "$TEST_ARGV_LOG"
if [ "$1" = plugins ] && [ "$2" = install ] && [ "$3" = --help ]; then
    case "${TEST_HELP:?}" in
        modern)
            echo 'Usage: openclaw plugins install [options] <path>'
            echo 'Options:'
            echo "  --accept-capabilities  Accept the plugin's declared capabilities (default: false)"
            echo '  --force  Overwrite an existing installed plugin (default: false)' ;;
        legacy)
            echo 'Options:'
            echo '  --dangerously-force-unsafe-install  Bypass built-in dangerous-code install blocking'
            echo '  --force  Overwrite an existing installed plugin' ;;
        near_match)
            echo 'Options:'
            echo '  --accept-capabilities-only          unrelated option (default: false)'
            echo '  --no-accept-capabilities             reverse switch (default: false)' ;;
        # Deprecated no-op hosts: the token is still advertised, so a
        # version-gated or presence-only read would keep passing it.
        noop)
            echo 'Usage: openclaw plugins install [options] <path>'
            echo 'Options:'
            echo "  --accept-capabilities  Accept the plugin's declared capabilities (default: false)"
            echo '  --dangerously-force-unsafe-install  Deprecated no-op; security.installPolicy may still block'
            echo '  --force  Overwrite an existing installed plugin (default: false)' ;;
        noop_upper)
            echo 'Usage: openclaw plugins install [options] <path>'
            echo 'Options:'
            echo "  --accept-capabilities  Accept the plugin's declared capabilities (default: false)"
            echo '  --dangerously-force-unsafe-install  Deprecated NO-OP; security.installPolicy may still block'
            echo '  --force  Overwrite an existing installed plugin (default: false)' ;;
        # Near-miss only: the unsafe whole-token match must not fire here.
        unsafe_near)
            echo 'Usage: openclaw plugins install [options] <path>'
            echo 'Options:'
            echo "  --accept-capabilities  Accept the plugin's declared capabilities (default: false)"
            echo '  --dangerously-force-unsafe-install-only  Unrelated near-match option'
            echo '  --force  Overwrite an existing installed plugin (default: false)' ;;
        # Verbatim captures from real hosts, commander's line wrapping included.
        real_2026_5_22) cat "$TEST_REAL_HELP_2026_5_22" ;;
        real_2026_8_1)  cat "$TEST_REAL_HELP_2026_8_1" ;;
        colon)
            echo '  --accept-capabilities: accept declared capabilities' ;;
        paren)
            echo 'Options (--accept-capabilities):'
            echo '  --force  overwrite' ;;
        big)
            echo "  --accept-capabilities  Accept the plugin's declared capabilities"
            # Exceeds any pipe buffer: if the probe is ever reverted from
            # capture-into-variable to `cmd | grep -q`, grep's early exit
            # SIGPIPEs this writer under pipefail and the match is lost.
            printf 'filler line %.0s\n' {1..200000} ;;
        failed)
            echo 'OpenClaw could not start: --accept-capabilities requires a TTY' >&2
            exit 3 ;;
    esac
    exit 0
fi
if [ "$1" = config ] && [ "$2" = set ]; then
    exit 0
fi
if [ "$1" = plugins ] && [ "$2" = uninstall ]; then
    exit 0
fi
[ "$1" = plugins ] && [ "$2" = install ]
[ "$3" = "$ANOLISA_ADAPTER_DIR/openclaw" ]
accepted=0
unsafe=0
for arg in "$@"; do
    [ "$arg" != --accept-capabilities ] || accepted=1
    [ "$arg" != --dangerously-force-unsafe-install ] || unsafe=1
done
if [ "${TEST_GATE:?}" = new ]; then
    [ "$accepted" = 1 ] || {
        echo 'Plugin "memory-anolisa" requires capability consent. Use --accept-capabilities, then retry.' >&2
        # Large trailing output: if the consent-signal check is ever
        # reverted to a short-circuit pipe (printf | grep -q), grep's early
        # exit SIGPIPEs this writer under pipefail and rc=3 degrades to 1.
        if [ "${TEST_INSTALL_BIG:-0}" = 1 ]; then
            printf 'filler line %.0s\n' {1..200000} >&2
        fi
        exit 1; }
elif [ "$TEST_GATE" = unrelated ]; then
    # Unconditional: an environment failure (e.g. EACCES) hits regardless
    # of whether the consent flag was passed, so the stream contract is
    # observable on the default path too.
    echo 'EACCES: permission denied, mkdir extensions/memory-anolisa' >&2; exit 1
else
    [ "$accepted" = 0 ] || { echo 'OpenClaw does not recognize option "--accept-capabilities".' >&2; exit 1; }
fi
# The unsafe-install gate is a third independent knob, like TEST_HELP and
# TEST_GATE: `required` models a host whose install-time scan still runs
# (omitting the bypass is fatal), `rejected` a host that has dropped the token
# (passing it is fatal). Unset models no enforcement, so the consent and
# help-layout scenarios above stay focused on the argv they pin.
case "${TEST_UNSAFE:-}" in
    '') ;;
    required)
        [ "$unsafe" = 1 ] || {
            echo 'install safety scan blocks child_process plugins' >&2; exit 4; } ;;
    rejected)
        [ "$unsafe" = 0 ] || {
            echo 'OpenClaw does not recognize option "--dangerously-force-unsafe-install".' >&2; exit 5; } ;;
    *) echo "stub: TEST_UNSAFE='${TEST_UNSAFE}' is neither required nor rejected" >&2; exit 9 ;;
esac
# security.installPolicy is operator-owned and orthogonal to both flags.
if [ "${TEST_POLICY_REJECT:-0}" = 1 ]; then
    echo 'install blocked by security.installPolicy' >&2; exit 6
fi
: > "$TEST_INSTALLED"
STUB
chmod +x "$SANDBOX/openclaw"

export ANOLISA_ADAPTER_DIR="$SANDBOX/adapter root"
export OPENCLAW_BIN="$SANDBOX/openclaw"
export OPENCLAW_STATE_DIR="$SANDBOX/state root"
export OPENCLAW_HOME="$SANDBOX/ignored home"
export TEST_STATE_DIR="$OPENCLAW_STATE_DIR"
export TEST_ARGV_LOG="$SANDBOX/argv"
export TEST_INSTALLED="$SANDBOX/installed"
# Legacy marker used to check that new installs do not claim other plugins.
export MEMORY_CORE_MARKER="$OPENCLAW_STATE_DIR/.anolisa-memory-anolisa-disabled-memory-core"
# `openclaw plugins install --help` captured verbatim from real releases, so the
# unsafe-install classifier is pinned against how commander actually renders and
# wraps the option descriptions rather than against hand-written help text.
#   2026.5.22 — bypass still effective, no capability-consent gate.
#   2026.8.1  — bypass advertised as "Deprecated no-op", consent gate present.
#               2026.9.2 renders the same help apart from its version banner
#               (verified), so one no-op fixture stands in for both.
export TEST_REAL_HELP_2026_5_22="$SCRIPT_DIR/fixtures/openclaw-2026.5.22-plugins-install-help.txt"
export TEST_REAL_HELP_2026_8_1="$SCRIPT_DIR/fixtures/openclaw-2026.8.1-plugins-install-help.txt"

fail() {
    echo "FAIL: $1" >&2
    sed 's/^/    /' "$SANDBOX/output" >&2
    exit 1
}

# Full expected install argv for the given flag combination, in the order
# install.sh appends them: capability consent is negotiated first, the legacy
# unsafe-install bypass second.
argv() {
    local a="plugins install $ANOLISA_ADAPTER_DIR/openclaw --force"
    if [ "$2" = yes ]; then a="$a --accept-capabilities"; fi
    if [ "$1" = yes ]; then a="$a --dangerously-force-unsafe-install"; fi
    printf '%s' "$a"
}

# scenario <label> <want-rc> <want-argv> [KEY=VAL ...] — run install.sh with
# the current TEST_HELP/TEST_GATE gating and assert rc, argv, and the
# config-set follow-up. The two installer switches are unset first so
# ambient values cannot leak into baseline scenarios; per-scenario KEY=VAL
# overrides are applied after the unsets.
scenario() {
    local label="$1" want_rc="$2" want_argv="$3"
    shift 3
    local rc=0
    : > "$TEST_ARGV_LOG"
    rm -f "$TEST_INSTALLED"
    # The state directory is the marker's home, and an earlier scenario may have
    # removed it (unlock_install_target). Recreate it, and start every scenario
    # from "install.sh has never disabled memory-core here".
    mkdir -p -- "$OPENCLAW_STATE_DIR"
    rm -f -- "$MEMORY_CORE_MARKER"
    env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
        -u ANOLISA_ACCEPT_CAPABILITIES \
        "$@" bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
    [ "$rc" = "$want_rc" ] || fail "$label: rc=$rc, want $want_rc"
    [ "$(grep -c '^plugins install --help$' "$TEST_ARGV_LOG")" = 1 ] \
        || fail "$label: expected exactly one help probe"
    # awk (always exit 0) instead of grep -v | wc -l: a failing pipeline in
    # an assignment kills the whole suite under `set -euo pipefail` before
    # the fail() below can print its diagnostic.
    local install_calls
    install_calls="$(awk '/^plugins install / && !/--help$/ {n++} END {print n+0}' "$TEST_ARGV_LOG")"
    [ "$install_calls" = 1 ] \
        || fail "$label: expected exactly one install invocation, got $install_calls"
    local install_argv
    install_argv="$(awk '/^plugins install / && !/--help$/ {print}' "$TEST_ARGV_LOG")"
    [ "$install_argv" = "$want_argv" ] \
        || fail "$label: install argv '$install_argv' != '$want_argv'"
    if [ "$want_rc" = 0 ]; then
        [ -f "$TEST_INSTALLED" ] || fail "$label: install did not run"
        [ "$(grep -c '^config set plugins.entries.memory-anolisa.hooks.allowConversationAccess true$' "$TEST_ARGV_LOG")" = 1 ] \
            || fail "$label: allowConversationAccess config-set missing"
    else
        [ ! -f "$TEST_INSTALLED" ] || fail "$label: install ran despite failure"
    fi
    ! grep -Eq '^plugins (enable|disable) memory-core$|^config .*plugins.entries.memory-core' "$TEST_ARGV_LOG" \
        || fail "$label: installer touched memory-core"
    [ ! -f "$MEMORY_CORE_MARKER" ] || fail "$label: installer created a displacement marker"
    echo "PASS: $label"
}

expect_log() { grep -q -- "$1" "$SANDBOX/output" || fail "expected log line: $1"; }
expect_no_log() { ! grep -q -- "$1" "$SANDBOX/output" || fail "unexpected log line: $1"; }

# channel_scenario <label> <want-rc> <want-streams: split|merged> <marker> [KEY=VAL ...]
# — like scenario() but with separated capture (stdout and stderr to
# distinct files), pinning the stream contract itself: on the default path
# the CLI's stderr must stay on stderr (base behavior), while the withheld
# path deliberately merges the transcript into stdout. <marker> is the
# CLI-authored text to track across the two channels.
channel_scenario() {
    local label="$1" want_rc="$2" want_streams="$3" marker="$4"
    shift 4
    local rc=0
    : > "$TEST_ARGV_LOG"
    rm -f "$TEST_INSTALLED"
    env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
        -u ANOLISA_ACCEPT_CAPABILITIES \
        "$@" bash "$INSTALL_SH" >"$SANDBOX/out" 2>"$SANDBOX/err" || rc=$?
    [ "$rc" = "$want_rc" ] || { echo "FAIL: $label: rc=$rc, want $want_rc" >&2; channel_dump; exit 1; }
    local install_calls
    install_calls="$(awk '/^plugins install / && !/--help$/ {n++} END {print n+0}' "$TEST_ARGV_LOG")"
    [ "$install_calls" = 1 ] \
        || { echo "FAIL: $label: expected exactly one install invocation, got $install_calls" >&2; exit 1; }
    if [ "$want_streams" = merged ]; then
        grep -qF -- "$marker" "$SANDBOX/out" \
            || { echo "FAIL: $label: CLI text '$marker' missing from stdout (merged transcript expected)" >&2; channel_dump; exit 1; }
    else
        ! grep -qF -- "$marker" "$SANDBOX/out" \
            || { echo "FAIL: $label: CLI text '$marker' leaked into stdout on the split-stream path" >&2; channel_dump; exit 1; }
        grep -qF -- "$marker" "$SANDBOX/err" \
            || { echo "FAIL: $label: CLI text '$marker' missing from stderr (split streams expected)" >&2; channel_dump; exit 1; }
    fi
    echo "PASS: $label"
}

channel_dump() {
    echo "    --- captured stdout ---" >&2
    sed 's/^/    /' "$SANDBOX/out" >&2
    echo "    --- captured stderr ---" >&2
    sed 's/^/    /' "$SANDBOX/err" >&2
}

# Make the plugin's install target unwritable, reproducing the EACCES host the
# failure attribution must recognise: OpenClaw materialises the plugin under
# ${OPENCLAW_STATE_DIR}/extensions/, so a read-only extensions/ directory is
# enough. chmod rather than a read-only mount, so the scenario needs no
# privileges and the sandbox cleanup still works.
lock_install_target() {
    mkdir -p -- "$OPENCLAW_STATE_DIR/extensions"
    chmod 500 -- "$OPENCLAW_STATE_DIR/extensions"
}

unlock_install_target() {
    chmod 700 -- "$OPENCLAW_STATE_DIR/extensions" 2>/dev/null || true
    rm -rf -- "$OPENCLAW_STATE_DIR" 2>/dev/null || true
}

# The "modern" help advertises no unsafe-install option at all, so every
# scenario below expects the bypass to be omitted — that is the negotiation
# this suite pins, not a side effect of the consent gate.
export TEST_HELP=modern TEST_GATE=new
scenario 'modern host, default' 0 "$(argv no yes)"
expect_log 'Passing --accept-capabilities'
expect_no_log '] Passing --dangerously-force-unsafe-install'
scenario 'modern host, safe install' 0 "$(argv no yes)" AGENT_MEMORY_SAFE_INSTALL=1
scenario 'modern host, explicit opt-in' 0 "$(argv no yes)" AGENT_MEMORY_ACCEPT_CAPABILITIES=1
# Opting out must be a visible refusal: no flag, no consent log, and the
# gated install fails with the refusal conclusion (rc=3) instead of
# silently succeeding.
scenario 'modern host, consent opt-out' 3 "$(argv no no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
expect_log 'AGENT_MEMORY_ACCEPT_CAPABILITIES=0'
expect_no_log 'Passing --accept-capabilities'
expect_log 'install failed with consent withheld'
# Pins the merged-stream decision: the CLI's own rejection text stays
# visible in the script's output (streamed live via tee).
expect_log 'requires capability consent'
# The token table accepts the full boolean vocabulary, not just literal 0.
scenario 'modern host, consent opt-out via false' 3 "$(argv no no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=false
scenario 'modern host, consent opt-in via TRUE' 0 "$(argv no yes)" AGENT_MEMORY_ACCEPT_CAPABILITIES=TRUE
# Leading/trailing whitespace is trimmed, matching Rust env_bool().
scenario 'modern host, consent opt-out with padding' 3 "$(argv no no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=' 0 '
# A consent rejection buried in large CLI output must still be attributed
# (rc=3): the signal check must never be a short-circuit pipe.
export TEST_INSTALL_BIG=1
scenario 'consent rejection with large install output' 3 "$(argv no no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
expect_log 'install failed with consent withheld'
unset TEST_INSTALL_BIG
# An install failure under opt-out that does not carry the consent-rejection
# phrase must stay a generic rc=1 failure with an opt-out note — never a
# misattributed policy refusal.
export TEST_GATE=unrelated
scenario 'opt-out with unrelated install error' 1 "$(argv no no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
expect_log 'does not look like a consent rejection'
expect_log 'EACCES'
expect_no_log 'install failed with consent withheld'
export TEST_GATE=new

# The same policy is reachable through ANOLISA_ACCEPT_CAPABILITIES, the switch
# shared with the other ANOLISA OpenClaw installer scripts, so one variable
# withholds consent on every script entry point.
scenario 'modern host, consent opt-out via shared switch' 3 "$(argv no no)" ANOLISA_ACCEPT_CAPABILITIES=0
expect_log 'ANOLISA_ACCEPT_CAPABILITIES=0: withholding --accept-capabilities'
expect_log 'install failed with consent withheld by ANOLISA_ACCEPT_CAPABILITIES=0'
expect_no_log 'Passing --accept-capabilities'
scenario 'modern host, shared opt-out via off' 3 "$(argv no no)" ANOLISA_ACCEPT_CAPABILITIES=OFF
scenario 'modern host, shared opt-out with padding' 3 "$(argv no no)" ANOLISA_ACCEPT_CAPABILITIES=' no '
scenario 'modern host, shared opt-in via Yes' 0 "$(argv no yes)" ANOLISA_ACCEPT_CAPABILITIES=Yes
expect_log 'Passing --accept-capabilities'

# The component-scoped name is the more specific one and wins in both
# directions, so an operator withholding consent everywhere can still decide
# for agent-memory alone. Every line must name the variable that decided,
# never the one that was ignored.
scenario 'component switch overrides shared opt-out' 0 "$(argv no yes)" \
    AGENT_MEMORY_ACCEPT_CAPABILITIES=1 ANOLISA_ACCEPT_CAPABILITIES=0
expect_log 'Passing --accept-capabilities'
scenario 'component switch overrides shared opt-in' 3 "$(argv no no)" \
    AGENT_MEMORY_ACCEPT_CAPABILITIES=0 ANOLISA_ACCEPT_CAPABILITIES=1
expect_log 'install failed with consent withheld by AGENT_MEMORY_ACCEPT_CAPABILITIES=0'
expect_no_log 'ANOLISA_ACCEPT_CAPABILITIES=0'

# Precedence keys on whether a variable is *set*, not on whether its value is
# non-empty. An explicitly empty component setting still decides, and empty is
# the documented accepting default, so it must not fall through to a shared
# ANOLISA_ACCEPT_CAPABILITIES=0 and withhold consent this component was told
# to leave alone. Regression: a `-n` test made set-but-empty mean unset.
scenario 'explicitly empty component switch beats a shared opt-out' 0 "$(argv no yes)" \
    AGENT_MEMORY_ACCEPT_CAPABILITIES= ANOLISA_ACCEPT_CAPABILITIES=0
expect_log 'Passing --accept-capabilities'
expect_no_log 'withholding --accept-capabilities'
expect_no_log 'ANOLISA_ACCEPT_CAPABILITIES=0'

# A component value that decides also shields the shared one from parsing: an
# unparseable shared setting must not abort an install the component already
# accepted.
scenario 'empty component switch shields an unparseable shared value' 0 "$(argv no yes)" \
    AGENT_MEMORY_ACCEPT_CAPABILITIES= ANOLISA_ACCEPT_CAPABILITIES=maybe
expect_log 'Passing --accept-capabilities'
expect_no_log 'is not a boolean'

# And the shared switch alone: set-but-empty is the accepting default too, so
# no refusal line may name it.
scenario 'explicitly empty shared switch accepts' 0 "$(argv no yes)" \
    ANOLISA_ACCEPT_CAPABILITIES=
expect_log 'Passing --accept-capabilities'
expect_no_log 'withholding --accept-capabilities'

# An unparseable shared value aborts like an unparseable component value, and
# the error names the variable that carried it.
: > "$TEST_ARGV_LOG"
rc=0
env ANOLISA_ACCEPT_CAPABILITIES=maybe bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid shared switch value: expected rc 2, got $rc"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'invalid shared switch value: openclaw must not be invoked'
grep -q "ANOLISA_ACCEPT_CAPABILITIES='maybe' is not a boolean" "$SANDBOX/output" \
    || fail 'invalid shared switch value: the error must name ANOLISA_ACCEPT_CAPABILITIES'
echo 'PASS: invalid shared switch value aborts before any OpenClaw call'

# A bad component value is not rescued by a good shared one: the more specific
# variable decided, so its value is the one that has to parse.
: > "$TEST_ARGV_LOG"
rc=0
env AGENT_MEMORY_ACCEPT_CAPABILITIES=maybe ANOLISA_ACCEPT_CAPABILITIES=0 \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid component value beside a valid shared value: expected rc 2, got $rc"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'invalid component value: openclaw must not be invoked'
grep -q "AGENT_MEMORY_ACCEPT_CAPABILITIES='maybe' is not a boolean" "$SANDBOX/output" \
    || fail 'invalid component value: the error must name AGENT_MEMORY_ACCEPT_CAPABILITIES'
echo 'PASS: an invalid component value is not masked by a valid shared value'

export TEST_HELP=legacy TEST_GATE=old
scenario 'legacy host, default' 0 "$(argv yes no)"
expect_log 'did not advertise --accept-capabilities'
expect_log '] Passing --dangerously-force-unsafe-install'
# Declining the bypass on a host whose install-time scan still runs is a real
# choice with a real consequence: the scan blocks the plugin, and the failure
# conclusion names the switch that caused it.
scenario 'legacy host, safe install' 1 "$(argv no no)" \
    AGENT_MEMORY_SAFE_INSTALL=1 TEST_UNSAFE=required
expect_log 'declining --dangerously-force-unsafe-install'
expect_log 'AGENT_MEMORY_SAFE_INSTALL=1 declined the unsafe-install bypass'
# Nothing to refuse when the host does not gate consent.
scenario 'legacy host, consent opt-out' 0 "$(argv yes no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
scenario 'legacy host, shared-switch consent opt-out' 0 "$(argv yes no)" ANOLISA_ACCEPT_CAPABILITIES=0
expect_log 'did not advertise --accept-capabilities'

export TEST_HELP=near_match
scenario 'near-miss options only' 0 "$(argv no no)"
expect_log 'did not advertise --accept-capabilities'

# Alternate help layouts pin the whole-token match: a prefix-only regex
# misses these, a substring regex over-matches near-miss options.
export TEST_HELP=colon TEST_GATE=new
scenario 'colon help style' 0 "$(argv no yes)"
export TEST_HELP=paren
scenario 'paren help style' 0 "$(argv no yes)"
export TEST_HELP=big
scenario 'large help output' 0 "$(argv no yes)"

# --- Unsafe-install negotiation ----------------------------------------------
# A host that advertises the token as a deprecated no-op must not receive it,
# and the log must say why instead of staying silent about the omission.
export TEST_HELP=noop TEST_GATE=new
scenario 'no-op host omits the deprecated bypass' 0 "$(argv no yes)" TEST_UNSAFE=rejected
expect_log 'Not passing --dangerously-force-unsafe-install'
expect_log 'deprecated no-op'
expect_log 'security.installPolicy'
# The marker is matched case-insensitively: commander's rendering is not
# something this repo controls.
export TEST_HELP=noop_upper
scenario 'uppercase NO-OP marker is classified too' 0 "$(argv no yes)" TEST_UNSAFE=rejected
expect_log 'deprecated no-op'
# Whole-token match on the unsafe side as well: a near-miss option must not be
# read as the bypass.
export TEST_HELP=unsafe_near
scenario 'unsafe near-miss option only' 0 "$(argv no yes)" TEST_UNSAFE=rejected
expect_log 'does not'
expect_log 'advertise the option'

# Real captures pin the classifier against commander's actual rendering — the
# 2026.8.1 help wraps "may still block" onto a continuation line — and pin the
# resulting argv byte for byte, so 2026.5.22 keeps exactly what it got before
# the negotiation landed while 2026.8.1 drops the no-op token.
export TEST_HELP=real_2026_5_22 TEST_GATE=old
scenario 'real OpenClaw 2026.5.22 keeps the legacy argv' 0 \
    "plugins install $ANOLISA_ADAPTER_DIR/openclaw --force --dangerously-force-unsafe-install" \
    TEST_UNSAFE=required
expect_log '] Passing --dangerously-force-unsafe-install'
expect_log 'did not advertise --accept-capabilities'
export TEST_HELP=real_2026_8_1 TEST_GATE=new
scenario 'real OpenClaw 2026.8.1 drops the no-op bypass' 0 \
    "plugins install $ANOLISA_ADAPTER_DIR/openclaw --force --accept-capabilities" \
    TEST_UNSAFE=rejected
expect_log 'Not passing --dangerously-force-unsafe-install'
expect_log 'deprecated no-op'
expect_log 'Passing --accept-capabilities'

# On a no-op host the bypass is omitted either way, so AGENT_MEMORY_SAFE_INSTALL
# is not a choice there — the script says so rather than implying two paths.
export TEST_HELP=noop
scenario 'no-op host, safe install changes nothing' 0 "$(argv no yes)" \
    AGENT_MEMORY_SAFE_INSTALL=1 TEST_UNSAFE=rejected
expect_log 'changes nothing on this host'
# A no-op host that rejects on policy points the operator at the policy they
# own — conditionally, because the script cannot see why OpenClaw refused.
scenario 'no-op host blocked by install policy' 1 "$(argv no yes)" \
    TEST_UNSAFE=rejected TEST_POLICY_REJECT=1
expect_log 'security.installPolicy'
expect_log 'deprecated no-op'
expect_log 'if it names security.installPolicy'
# The reviewer's repro: a no-op host whose install dies on an unrelated error is
# not a policy rejection. Asserting that security.installPolicy caused it sends
# the operator to weaken a policy that had nothing to do with the failure, so the
# note must stay conditional and point at the CLI output they can already read.
export TEST_GATE=unrelated
scenario 'no-op host, unrelated install error' 1 "$(argv no yes)" TEST_UNSAFE=rejected
expect_log 'EACCES'
expect_log 'deprecated no-op'
expect_log 'Read the CLI output above for the actual cause'
expect_no_log 'the rejection comes from'
expect_no_log 'relax that policy, not this script'
# The permission verdict must not be invented either: the script reports an
# unwritable install target only after verifying one, so a writable sandbox
# keeps the conditional note above.
expect_no_log 'writable by the user running this script'
# With the target genuinely unwritable the script can verify the cause itself
# and must report the filesystem failure it is — explicitly not a policy
# refusal to relax. Root is exempt: W_OK is granted to it regardless of mode.
if [ "$(id -u)" = 0 ]; then
    echo 'SKIP: unwritable install target (running as root; W_OK is always granted)'
else
    lock_install_target
    scenario 'no-op host, EACCES with unwritable target' 1 "$(argv no yes)" TEST_UNSAFE=rejected
    expect_log 'EACCES'
    expect_log 'writable by the user running this script'
    expect_log 'an unwritable target is not a policy refusal'
    expect_no_log 'Read the CLI output above for the actual cause'
    # The verified verdict outranks the policy note even when OpenClaw itself
    # reports a policy block: an unwritable target fails the install either way,
    # so fixing it is the step that can actually make progress.
    scenario 'unwritable target outranks the policy note' 1 "$(argv no yes)" \
        TEST_UNSAFE=rejected TEST_POLICY_REJECT=1
    expect_log 'writable by the user running this script'
    expect_no_log 'Read the CLI output above for the actual cause'
    # ... and outranks the declined-bypass note, so no operator is sent to unset
    # AGENT_MEMORY_SAFE_INSTALL for what is a permission failure.
    export TEST_HELP=legacy TEST_GATE=old
    scenario 'unwritable target outranks the safe-install note' 1 "$(argv no no)" \
        AGENT_MEMORY_SAFE_INSTALL=1 TEST_UNSAFE=required
    expect_log 'writable by the user running this script'
    expect_no_log 'declined the unsafe-install bypass'
    unlock_install_target
fi
export TEST_GATE=new

# A failing probe whose error text names the flag must not be mistaken for an
# advertised option; the install degrades to base flags with a WARNING.
export TEST_HELP=failed TEST_GATE=old
scenario 'failed help probe' 0 "$(argv yes no)"
expect_log 'WARNING: cannot inspect OpenClaw installer options (rc=3)'
expect_log 'legacy bypass is kept'
expect_no_log 'did not advertise --accept-capabilities'
# The same probe failure with the opt-out active must surface the switch —
# the advertised branch (and its refusal line) never runs. On a gating host
# the install then fails generically (rc=1, not 3): the host's gating
# status was never confirmed, so the refusal is not attributed.
scenario 'failed probe with opt-out' 0 "$(argv yes no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
expect_log 'WARNING: cannot inspect OpenClaw installer options (rc=3)'
expect_log 'AGENT_MEMORY_ACCEPT_CAPABILITIES=0 is active'
export TEST_GATE=new
scenario 'failed probe with opt-out, gating host' 1 "$(argv yes no)" AGENT_MEMORY_ACCEPT_CAPABILITIES=0
expect_log 'AGENT_MEMORY_ACCEPT_CAPABILITIES=0 is active'
expect_no_log 'install failed with consent withheld'
export TEST_GATE=old
# An unclassified host still honours AGENT_MEMORY_SAFE_INSTALL=1: declining a
# bypass whose effect is unknown is exactly what the opt-out is for.
scenario 'failed probe, safe install' 1 "$(argv no no)" \
    AGENT_MEMORY_SAFE_INSTALL=1 TEST_UNSAFE=required
expect_log 'WARNING: cannot inspect OpenClaw installer options (rc=3)'
expect_log 'declining --dangerously-force-unsafe-install'
expect_log 'AGENT_MEMORY_SAFE_INSTALL=1 declined the unsafe-install bypass'

# Stream contract under separated capture: a default (consent-granted)
# install keeps the CLI's own stderr text on stderr — identical to base —
# while the withheld path deliberately merges the refusal transcript into
# stdout. The default-path case uses the unrelated-failure gate so the CLI
# actually writes stderr text to track.
export TEST_HELP=modern TEST_GATE=unrelated
channel_scenario 'default path keeps CLI stderr on stderr' 1 split 'EACCES'
export TEST_GATE=new
channel_scenario 'withheld path merges transcript into stdout' 3 merged 'requires capability consent' AGENT_MEMORY_ACCEPT_CAPABILITIES=0
# The install-target diagnosis captures no transcript either: a no-op host's
# failure keeps the CLI's streams apart exactly like the base behavior. The
# marker is CLI-authored text — the script's own stdout note names
# security.installPolicy too, so that string cannot tell the channels apart.
export TEST_HELP=noop TEST_GATE=new
channel_scenario 'no-op host failure keeps CLI stderr on stderr' 1 split \
    'install blocked by security.installPolicy' TEST_UNSAFE=rejected TEST_POLICY_REJECT=1

# An unparseable switch value aborts before any OpenClaw invocation —
# including on hosts without the CLI at all (the validation precedes the
# missing-CLI early exit).
: > "$TEST_ARGV_LOG"
rc=0
env AGENT_MEMORY_ACCEPT_CAPABILITIES=maybe bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid switch value: expected rc 2, got $rc"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'invalid switch value: openclaw must not be invoked'
grep -q 'is not a boolean' "$SANDBOX/output" || fail 'invalid switch value: expected the boolean error'
echo 'PASS: invalid switch value aborts before any OpenClaw call'

# Interior whitespace must never be normalized into a valid token ('t rue'
# must not become a grant), and a whitespace-only value is "another value"
# per the documented contract — both abort before any CLI call.
for bad in 't rue' 'tr ue' '   '; do
    : > "$TEST_ARGV_LOG"
    rc=0
    env AGENT_MEMORY_ACCEPT_CAPABILITIES="$bad" bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "switch value '$bad': expected rc 2, got $rc"
    [ ! -s "$TEST_ARGV_LOG" ] || fail "switch value '$bad': openclaw must not be invoked"
    grep -q 'is not a boolean' "$SANDBOX/output" || fail "switch value '$bad': expected the boolean error"
done
echo 'PASS: interior-whitespace and blank values abort before any OpenClaw call'

rc=0
env OPENCLAW_BIN="$SANDBOX/missing-openclaw" AGENT_MEMORY_ACCEPT_CAPABILITIES=maybe \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid switch value without CLI: expected rc 2, got $rc"
grep -q 'is not a boolean' "$SANDBOX/output" \
    || fail 'invalid switch value without CLI: expected the boolean error'
echo 'PASS: invalid switch value aborts even when the CLI is missing'

: > "$TEST_ARGV_LOG"
rc=0
OPENCLAW_BIN="$SANDBOX/missing-openclaw" bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "missing CLI: expected rc 0, got $rc"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'missing CLI: openclaw must not be invoked'
grep -q 'skipping plugin installation' "$SANDBOX/output" \
    || fail 'missing CLI: expected skip behavior'
echo 'PASS: missing CLI preserves the existing skip behavior'

# Diagnosability pin: a regressed install.sh that probes but never invokes
# install must produce a FAIL diagnostic — not a silent set -e death from
# a failing pipeline inside an assignment. The scenario runs in a child
# bash -c: an OR-list subshell would suppress errexit inside it and hide
# exactly the silent death this pin guards against.
export TEST_HELP=modern TEST_GATE=new
fake_install="$SANDBOX/fake-install.sh"
cat > "$fake_install" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
echo "[agent-memory] Installing openclaw plugin..."
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins install --help >/dev/null 2>&1
# Regressed: exits successfully without invoking the install.
exit 0
FAKE
pin_rc=0
env SANDBOX="$SANDBOX" INSTALL_SH="$fake_install" \
    bash -c "set -euo pipefail; $(declare -f scenario fail); scenario 'harness pin: install never invoked' 0 'plugins install never-ran'" \
    >"$SANDBOX/pin-output" 2>&1 || pin_rc=$?
[ "$pin_rc" -ne 0 ] || fail 'harness pin: scenario should have failed'
grep -q 'expected exactly one install invocation' "$SANDBOX/pin-output" \
    || fail 'harness pin: no diagnosable FAIL line (silent death)'
echo 'PASS: missing install invocation fails with a diagnostic'

# remote-test is the only test path for macOS/Windows contributors; pin
# that its ssh command includes this suite (dry-run — nothing executes).
if command -v make >/dev/null 2>&1; then
    make -C "$SCRIPT_DIR/.." -n remote-test 2>/dev/null \
        | grep -q 'tests/test-openclaw-adapter-install.sh' \
        || fail 'remote-test does not run the installer test'
    echo 'PASS: remote-test includes the installer test'
fi

# Check fresh installs and upgrades without changing another plugin's state.
export TEST_HELP=modern TEST_GATE=new
scenario 'namespaced tools need no memory-core disable' 0 "$(argv no yes)"
for legacy in no yes; do
    for owner in memory-anolisa memory-lancedb none; do
        for enabled in true false; do
            mkdir -p "$OPENCLAW_STATE_DIR"
            rm -f "$MEMORY_CORE_MARKER"
            if [ "$legacy" = yes ]; then printf 'legacy recovery record\n' > "$MEMORY_CORE_MARKER"; fi
            cat > "$OPENCLAW_STATE_DIR/openclaw.json" <<EOF
{"plugins":{"allow":["memory-anolisa","memory-lancedb"],"slots":{"memory":"$owner"},"entries":{"memory-anolisa":{"enabled":true},"memory-core":{"enabled":$enabled},"memory-lancedb":{"enabled":true}}}}
EOF
            : > "$TEST_ARGV_LOG"
            bash "$INSTALL_SH" > "$SANDBOX/output" 2>&1 || fail 're-install failed'
            if [ "$legacy" = yes ]; then expect_log 'previous installer recorded'; fi
            bash "$UNINSTALL_SH" > "$SANDBOX/output" 2>&1 || fail 'uninstall failed'
            ! grep -Eq '^plugins (enable|disable) memory-core$|^config .*plugins.entries.memory-core' "$TEST_ARGV_LOG" \
                || fail 'install/uninstall mutated memory-core'
            python3 - "$OPENCLAW_STATE_DIR/openclaw.json" "$owner" "$enabled" <<'PYTEST'
import json, sys
plugins = json.load(open(sys.argv[1]))['plugins']
assert plugins['entries']['memory-core']['enabled'] == (sys.argv[3] == 'true')
assert 'memory-anolisa' not in plugins['entries']
assert 'memory-anolisa' not in plugins['allow']
assert plugins['entries']['memory-lancedb']['enabled'] is True
assert plugins['slots'].get('memory') == (None if sys.argv[2] == 'memory-anolisa' else sys.argv[2])
PYTEST
            if [ "$legacy" = yes ]; then
                expect_log 'previous installer recorded'
                [ "$(cat "$MEMORY_CORE_MARKER")" = 'legacy recovery record' ] || fail 'legacy marker lost'
            else
                [ ! -f "$MEMORY_CORE_MARKER" ] || fail 'new marker created'
                expect_no_log 'previous installer recorded'
            fi
            echo "PASS: install/uninstall preserves memory-core=$enabled, slot=$owner, legacy=$legacy"
        done
    done
done
