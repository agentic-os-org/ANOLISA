#!/usr/bin/env bash
# Pin the installer flag negotiation of scripts/install.sh against a stub
# openclaw CLI: probe form (help captured, not piped to grep), whole-token
# matching for both negotiated flags — capability consent and the legacy
# unsafe-install bypass — the consent opt-out, AGENT_MEMORY_SAFE_INSTALL, and
# the per-outcome log lines. No real plugin is installed.
#
# It also pins the memory_get / memory_search tool-name hand-off (#3218):
# install.sh must disable OpenClaw's bundled memory-core, record that it did,
# and uninstall.sh must restore it from that record alone.
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
# install.sh reads memory-core's persisted enablement flag before disabling it,
# because `plugins disable` is idempotent and its exit status cannot tell a real
# transition from a no-op. TEST_MEMORY_CORE_ENABLED models what a host answers
# for that key: `absent` (the default) is a bundled plugin nobody ever
# configured, so the probe fails and the flag reads as its enabled default;
# anything else is echoed verbatim, which is how the value-rendering variants
# get pinned.
if [ "$1" = config ] && [ "$2" = get ] \
        && [ "$3" = plugins.entries.memory-core.enabled ]; then
    case "${TEST_MEMORY_CORE_ENABLED:-absent}" in
        absent) echo "Config key not found: $3" >&2; exit 1 ;;
        *) printf '%s\n' "$TEST_MEMORY_CORE_ENABLED" ;;
    esac
    exit 0
fi
# uninstall.sh reads the memory slot's owner before it restores memory-core,
# because `plugins enable` re-runs OpenClaw's exclusive slot selection and would
# otherwise take the slot back from a backend the operator chose after
# install.sh ran (#3222). TEST_MEMORY_SLOT models the answer for that key the
# same way TEST_MEMORY_CORE_ENABLED models the enablement flag: `absent` (the
# default) fails the probe, anything else is echoed verbatim so the
# value-rendering variants get pinned.
if [ "$1" = config ] && [ "$2" = get ] \
        && [ "$3" = plugins.slots.memory ]; then
    case "${TEST_MEMORY_SLOT:-absent}" in
        absent) echo "Config key not found: $3" >&2; exit 1 ;;
        *) printf '%s\n' "$TEST_MEMORY_SLOT" ;;
    esac
    exit 0
fi
# install.sh disables OpenClaw's bundled memory backend so this plugin can own
# the memory_get / memory_search tool names (#3218). `plugins disable` is
# idempotent on a real host, so the stub is too; TEST_MEMORY_CORE_DISABLE_FAILS
# models a host that refuses it (unknown plugin, read-only config).
if [ "$1" = plugins ] && [ "$2" = uninstall ]; then
    exit 0
fi
if [ "$1" = plugins ] && [ "$2" = enable ] && [ "$3" = memory-core ]; then
    echo 'Enabled plugin "memory-core". Restart the gateway to apply.'
    exit 0
fi
if [ "$1" = plugins ] && [ "$2" = disable ] && [ "$3" = memory-core ]; then
    if [ "${TEST_MEMORY_CORE_DISABLE_FAILS:-0}" = 1 ]; then
        echo 'Plugin not found: memory-core.' >&2; exit 1
    fi
    echo 'Disabled plugin "memory-core". Restart the gateway to apply.'
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
# install.sh records that *it* disabled memory-core, so uninstall.sh restores
# exactly what install.sh changed and never re-enables one an operator disabled
# themselves. Same path the script derives from OPENCLAW_STATE_DIR.
# Exported because the harness-pin scenario re-runs scenario() in a child bash
# that inherits only the environment and `declare -f scenario fail`.
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
    local disable_calls
    disable_calls="$(awk '/^plugins disable memory-core$/ {n++} END {print n+0}' "$TEST_ARGV_LOG")"
    if [ "$want_rc" = 0 ]; then
        [ -f "$TEST_INSTALLED" ] || fail "$label: install did not run"
        [ "$(grep -c '^config set plugins.entries.memory-anolisa.hooks.allowConversationAccess true$' "$TEST_ARGV_LOG")" = 1 ] \
            || fail "$label: allowConversationAccess config-set missing"
        # The memory-core disable is what releases the memory_get /
        # memory_search tool names (#3218), so it is part of a successful
        # install's contract, not an optional extra.
        [ "$disable_calls" = 1 ] \
            || fail "$label: expected exactly one 'plugins disable memory-core', got $disable_calls"
        if [ "${TEST_MEMORY_CORE_DISABLE_FAILS:-0}" = 1 ]; then
            [ ! -f "$MEMORY_CORE_MARKER" ] \
                || fail "$label: marker written even though the disable failed"
        elif memory_core_pre_disabled; then
            # memory-core was already off, so `plugins disable` caused no
            # transition. Claiming one would make uninstall.sh re-enable a
            # plugin the operator deliberately turned off.
            [ ! -f "$MEMORY_CORE_MARKER" ] \
                || fail "$label: marker claims a memory-core disable this install did not cause"
        else
            [ -f "$MEMORY_CORE_MARKER" ] \
                || fail "$label: marker not written after a successful disable"
        fi
    else
        [ ! -f "$TEST_INSTALLED" ] || fail "$label: install ran despite failure"
        # Nothing may be changed on the host after a failed install: the script
        # exits before the memory-core step, so it must not have run either.
        [ "$disable_calls" = 0 ] \
            || fail "$label: memory-core disabled despite a failed install"
        [ ! -f "$MEMORY_CORE_MARKER" ] || fail "$label: marker written despite a failed install"
    fi
    echo "PASS: $label"
}

# Whether the value TEST_MEMORY_CORE_ENABLED hands back tells install.sh that
# memory-core was already disabled before this run — i.e. that the idempotent
# `plugins disable` caused no transition and no marker may be written. Mirrors
# the script's own false-ish token table so a scenario cannot drift from it.
memory_core_pre_disabled() {
    # Same reduction install.sh applies to the probe's answer: last line, lower
    # cased, trailing token after any `=`/`:`, quoting and punctuation stripped.
    # Duplicating it here rather than re-deriving a simpler match is the point —
    # a rendering the script classifies as "already off" must classify the same
    # way in the assertion, or the two silently disagree on one host shape.
    local prior
    prior="$(printf '%s' "${TEST_MEMORY_CORE_ENABLED:-absent}" | tail -n 1 \
        | tr '[:upper:]' '[:lower:]' \
        | sed -e 's/.*[=:]//' -e 's/[]["'\''`,;]//g' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    case "$prior" in
        false|0|no|off|disabled) return 0 ;;
        *) return 1 ;;
    esac
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

# --- memory-core tool-name release (#3218) -----------------------------------
# OpenClaw keeps its bundled memory-core loaded as the dreaming sidecar even
# after this plugin takes the memory slot, and memory-core owns the
# memory_get / memory_search names. OpenClaw's plugin tool registry is
# first-wins, so the plugin's same-named tools are dropped and agent calls bind
# to memory-core's workspace reader, which answers disabled:true for every
# ~/.anolisa/memory path. A successful install must therefore disable it.
export TEST_HELP=modern TEST_GATE=new
scenario 'successful install disables memory-core' 0 "$(argv no yes)"
expect_log 'Disabled memory-core'
expect_log 'first-wins tool registry'
expect_no_log "plugins disable memory-core' failed"

# A host that refuses the disable still has a working plugin (observe,
# auto-recall and the MCP transport are all unaffected), so the install must not
# fail — but it has to say why memory_get will keep answering disabled:true
# instead of leaving the operator with the silent breakage this fixes.
# Exported rather than passed as a KEY=VAL scenario argument: scenario() reads
# it too, to flip its own marker assertion, and a per-run env assignment would
# only reach the install.sh child.
export TEST_MEMORY_CORE_DISABLE_FAILS=1
scenario 'memory-core disable failure warns without failing the install' 0 "$(argv no yes)"
expect_log "plugins disable memory-core' failed"
expect_log 'disabled:true'
expect_no_log '] Disabled memory-core'
unset TEST_MEMORY_CORE_DISABLE_FAILS

# Re-running the installer (upgrade, or a retry after a warning) converges:
# `plugins disable` is idempotent and the marker survives every run, so
# uninstall.sh still knows the restore is its job. scenario() resets the marker
# before each run by design, so this pins the two-run sequence directly.
: > "$TEST_ARGV_LOG"
rm -f -- "$MEMORY_CORE_MARKER"
env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || fail 're-install pair: first install failed'
[ -f "$MEMORY_CORE_MARKER" ] || fail 're-install pair: marker missing after the first install'
env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || fail 're-install pair: second install failed'
[ -f "$MEMORY_CORE_MARKER" ] || fail 're-install pair: marker lost across a re-install'
[ "$(awk '/^plugins disable memory-core$/ {n++} END {print n+0}' "$TEST_ARGV_LOG")" = 2 ] \
    || fail 're-install pair: expected one memory-core disable per install run'
echo 'PASS: re-install keeps the memory-core disable and its marker'

# --- the marker records only a transition this install caused (#3218) --------
# `plugins disable` is idempotent, so an operator who had already turned
# memory-core off still gets exit 0 from it. Only the pre-read of
# plugins.entries.memory-core.enabled separates that no-op from a real
# transition, and the distinction is what keeps uninstall.sh from re-enabling a
# plugin the operator meant to keep off. Every rendering a host might use for
# the flag has to classify identically, or the operator's choice survives on
# some hosts and not others.
for prior in false FALSE '"false"' 0 off disabled \
        'plugins.entries.memory-core.enabled=false' 'enabled: false'; do
    export TEST_MEMORY_CORE_ENABLED="$prior"
    scenario "operator-disabled memory-core (${prior}) records no marker" 0 "$(argv no yes)"
    expect_log 'was already disabled'
    expect_no_log '] Disabled memory-core'
done
unset TEST_MEMORY_CORE_ENABLED

# A host that answers the probe positively keeps the restore: this disable is a
# real transition, so the marker stands and no uncertainty note is printed.
export TEST_MEMORY_CORE_ENABLED=true
scenario 'enabled memory-core still records the marker' 0 "$(argv no yes)"
expect_log '] Disabled memory-core'
expect_no_log 'returned no value'
unset TEST_MEMORY_CORE_ENABLED

# An unanswerable probe (no `config get`, unreadable config, or a bundled
# default nobody ever wrote) must not cost the restore. The two failure modes
# are not symmetric: skipping the marker strands the host with no memory plugin
# at all after uninstall, because `plugins uninstall` resets the memory slot to
# its memory-core default while config still says enabled=false, whereas
# recording it can only cost an operator one extra `plugins disable`.
export TEST_MEMORY_CORE_ENABLED=absent
scenario 'unanswerable prior-state probe keeps the restore' 0 "$(argv no yes)"
expect_log '] Disabled memory-core'
expect_log 'returned no value'
expect_log 'delete'
unset TEST_MEMORY_CORE_ENABLED

# --- uninstall restores exactly what install.sh changed (#3218) ---------------
# uninstall.sh resolves the CLI the same way install.sh does — OPENCLAW_BIN,
# defaulting to `openclaw` — so the stub reaches it through the exported
# override. PATH carries it too, which is what the no-override default needs.
run_uninstall() {
    : > "$TEST_ARGV_LOG"
    env PATH="$SANDBOX:$PATH" bash "$UNINSTALL_SH" >"$SANDBOX/output" 2>&1 || true
}

# Run uninstall.sh with a PATH that deliberately has no `openclaw` on it, so
# only an honored OPENCLAW_BIN can reach the CLI at all. The bin dir carries the
# externals the script and its shebang need — bash itself is resolved through
# `env`, so omitting it would fail the run for the wrong reason and prove
# nothing about the override.
run_uninstall_via_bin() {
    : > "$TEST_ARGV_LOG"
    env PATH="$SANDBOX/no-openclaw-bin" OPENCLAW_BIN="$1" \
        bash "$UNINSTALL_SH" >"$SANDBOX/output" 2>&1 || true
}

enable_calls() {
    awk '/^plugins enable memory-core$/ {n++} END {print n+0}' "$TEST_ARGV_LOG"
}

# With the marker: memory-anolisa is gone, so the memory slot falls back to its
# default — a memory-core still recorded as disabled would leave the host with
# no memory backend at all.
touch -- "$MEMORY_CORE_MARKER"
run_uninstall
[ "$(enable_calls)" = 1 ] || fail 'uninstall did not re-enable memory-core'
[ ! -f "$MEMORY_CORE_MARKER" ] || fail 'uninstall left the marker behind'
expect_log 'Re-enabled memory-core'
echo 'PASS: uninstall restores the memory-core install.sh disabled'

# Without the marker this script never disabled memory-core, so an operator's
# own choice to keep it off must survive the uninstall untouched.
rm -f -- "$MEMORY_CORE_MARKER"
run_uninstall
[ "$(enable_calls)" = 0 ] || fail 'uninstall re-enabled a memory-core it never disabled'
expect_no_log 'Re-enabled memory-core'
echo 'PASS: uninstall leaves an unmarked memory-core alone'

# End to end over the pair an operator actually hits: memory-core already off,
# install, uninstall. Neither step may touch the operator's choice.
export TEST_MEMORY_CORE_ENABLED=false
: > "$TEST_ARGV_LOG"
rm -f -- "$MEMORY_CORE_MARKER"
env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || fail 'pre-disabled pair: install failed'
[ ! -f "$MEMORY_CORE_MARKER" ] \
    || fail 'pre-disabled pair: install claimed a disable it did not cause'
run_uninstall
[ "$(enable_calls)" = 0 ] \
    || fail 'pre-disabled pair: uninstall re-enabled a memory-core the operator disabled'
echo 'PASS: an operator-disabled memory-core survives install and uninstall'
unset TEST_MEMORY_CORE_ENABLED

# --- uninstall must not take the memory slot back from a later choice (#3222) --
# The marker records that install.sh disabled memory-core; it does not record
# that this plugin still owns the memory slot. `plugins enable memory-core`
# re-runs OpenClaw's exclusive slot selection, so a restore driven by the marker
# alone drags the slot back from a backend the operator chose *after* that
# install, and the backend they picked then silently stops serving retrieval.
slot_probe_calls() {
    awk '/^config get plugins\.slots\.memory$/ {n++} END {print n+0}' "$TEST_ARGV_LOG"
}

OPENCLAW_CFG_UNDER_TEST="$OPENCLAW_STATE_DIR/openclaw.json"

# A host as the operator left it after switching backends: this plugin is still
# installed and allowed, memory-core is still off from install.sh, and the
# memory slot belongs to memory-lancedb.
write_lancedb_config() {
    cat > "$OPENCLAW_CFG_UNDER_TEST" <<'JSON'
{
  "plugins": {
    "allow": ["memory-anolisa", "memory-lancedb"],
    "entries": {
      "memory-anolisa": { "enabled": true },
      "memory-core": { "enabled": false },
      "memory-lancedb": { "enabled": true }
    },
    "slots": { "memory": "memory-lancedb" }
  }
}
JSON
}

memory_slot_in_config() {
    # grep, not jq/python3: this has to hold whichever of the two the script's
    # cleanup took, and must not depend on either being installed. Both render
    # the nested key identically, and leading whitespace is irrelevant here.
    grep -q '"memory": "memory-lancedb"' "$OPENCLAW_CFG_UNDER_TEST"
}

# The lifecycle end to end: install with memory-core enabled (so install.sh
# claims the disable and writes the marker), the operator moves the memory slot
# to memory-lancedb, uninstall. The slot must still read memory-lancedb
# afterwards — no `plugins enable memory-core` may run to re-select memory-core
# into it — while the cleanup still drops this plugin's own keys.
: > "$TEST_ARGV_LOG"
rm -f -- "$MEMORY_CORE_MARKER" "$OPENCLAW_CFG_UNDER_TEST"
env -u AGENT_MEMORY_SAFE_INSTALL -u AGENT_MEMORY_ACCEPT_CAPABILITIES \
    bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1 || fail 'slot hand-off: install failed'
[ -f "$MEMORY_CORE_MARKER" ] || fail 'slot hand-off: install wrote no marker'
write_lancedb_config
export TEST_MEMORY_SLOT=memory-lancedb
run_uninstall
[ "$(enable_calls)" = 0 ] \
    || fail 'slot hand-off: uninstall forced the memory slot back to memory-core'
[ "$(slot_probe_calls)" = 1 ] \
    || fail 'slot hand-off: uninstall never asked who owns the memory slot'
memory_slot_in_config \
    || fail 'slot hand-off: plugins.slots.memory no longer reads memory-lancedb'
[ -f "$MEMORY_CORE_MARKER" ] \
    || fail 'slot hand-off: uninstall dropped the record of a memory-core it left disabled'
expect_log 'Left memory-core disabled'
expect_log 'memory-lancedb'
expect_no_log 'Re-enabled memory-core'
if command -v jq &>/dev/null || command -v python3 &>/dev/null; then
    ! grep -q 'memory-anolisa' "$OPENCLAW_CFG_UNDER_TEST" \
        || fail 'slot hand-off: cleanup left this plugin behind in openclaw.json'
fi
echo 'PASS: install → slot switch → uninstall keeps the memory-lancedb slot'

# Every rendering a host might use for a third-party owner has to classify the
# same way, or the operator's later choice survives on some hosts and not others.
rm -f -- "$OPENCLAW_CFG_UNDER_TEST"
for owner in memory-lancedb '"memory-lancedb"' MEMORY-LANCEDB \
        'plugins.slots.memory=memory-lancedb' 'slots.memory: memory-lancedb'; do
    export TEST_MEMORY_SLOT="$owner"
    touch -- "$MEMORY_CORE_MARKER"
    run_uninstall
    [ "$(enable_calls)" = 0 ] \
        || fail "slot owner '${owner}' did not stop the memory-core restore"
    expect_log 'Left memory-core disabled'
done

# A slot this plugin or memory-core still owns — or one nobody owns, however the
# host renders that — keeps the restore. Skipping it there is exactly what
# strands the host with no memory backend at all, since `plugins uninstall`
# vacates the slot while config still says memory-core is disabled.
for owner in memory-anolisa '"memory-anolisa"' memory-core MEMORY-CORE \
        null none undefined '(empty)' absent ''; do
    export TEST_MEMORY_SLOT="$owner"
    touch -- "$MEMORY_CORE_MARKER"
    run_uninstall
    [ "$(enable_calls)" = 1 ] \
        || fail "slot owner '${owner}' blocked a restore it should have allowed"
    [ ! -f "$MEMORY_CORE_MARKER" ] \
        || fail "slot owner '${owner}' kept the marker after a restore"
    expect_log 'Re-enabled memory-core'
done
unset TEST_MEMORY_SLOT
rm -f -- "$MEMORY_CORE_MARKER"
echo 'PASS: only a positively identified third owner blocks the memory-core restore'

# The CLI is not the only source of truth: a host whose `config get` cannot
# answer still has the slot persisted in openclaw.json, which is the file the
# cleanup edits anyway. Reading it back is what stops the guard from silently
# degrading into the behaviour it exists to prevent.
if command -v jq &>/dev/null || command -v python3 &>/dev/null; then
    write_lancedb_config
    touch -- "$MEMORY_CORE_MARKER"
    run_uninstall
    [ "$(enable_calls)" = 0 ] \
        || fail 'a slot only openclaw.json knows about did not stop the restore'
    memory_slot_in_config \
        || fail 'the persisted memory slot no longer reads memory-lancedb'
    rm -f -- "$OPENCLAW_CFG_UNDER_TEST" "$MEMORY_CORE_MARKER"
    echo 'PASS: uninstall reads the slot owner from openclaw.json when the CLI cannot answer'
fi

# With no marker there is nothing to restore, so the slot is nobody's business
# and the probe must not run at all on that path.
rm -f -- "$MEMORY_CORE_MARKER"
run_uninstall
[ "$(slot_probe_calls)" = 0 ] \
    || fail 'uninstall probed the memory slot with no marker to act on'
echo 'PASS: an unmarked uninstall does not probe the memory slot'

# --- uninstall resolves the CLI through OPENCLAW_BIN (#3218) ------------------
# Install honors a supported OPENCLAW_BIN override — an absolute CLI path that
# is not on PATH as the literal `openclaw` — so it disables memory-core and
# writes the marker. If the restore ignores that same override it reports the
# CLI missing and leaves memory-core disabled on a host where the configured
# executable is available, which is the one outcome the marker exists to
# prevent.
mkdir -p "$SANDBOX/no-openclaw-bin"
for _b in bash env rm mv cat mkdir tail tr sed; do
    ln -sf "$(command -v "$_b")" "$SANDBOX/no-openclaw-bin/$_b"
done
[ ! -e "$SANDBOX/no-openclaw-bin/openclaw" ] || fail 'test bin dir shadows openclaw'

touch -- "$MEMORY_CORE_MARKER"
run_uninstall_via_bin "$OPENCLAW_BIN"
[ "$(enable_calls)" = 1 ] \
    || fail 'uninstall ignored OPENCLAW_BIN and left memory-core disabled'
[ ! -f "$MEMORY_CORE_MARKER" ] \
    || fail 'uninstall kept the marker after restoring through OPENCLAW_BIN'
expect_log 'Re-enabled memory-core'
expect_no_log 'CLI not found'
echo 'PASS: uninstall restores memory-core through the OPENCLAW_BIN override'

# An override that cannot be resolved must say which binary it tried and keep
# the marker, so the operator is pointed at the CLI they configured rather than
# at a bare `openclaw` that was never the install path.
touch -- "$MEMORY_CORE_MARKER"
run_uninstall_via_bin "$SANDBOX/absent-host/openclaw"
[ "$(enable_calls)" = 0 ] || fail 'uninstall enabled memory-core without a CLI'
[ -f "$MEMORY_CORE_MARKER" ] \
    || fail 'uninstall dropped a marker it could not honor'
expect_log 'OPENCLAW_BIN='"$SANDBOX"'/absent-host/openclaw'
expect_log 'still disabled by the'
echo 'PASS: an unresolvable OPENCLAW_BIN keeps the marker and names the override'
rm -f -- "$MEMORY_CORE_MARKER"

# The suite must be hermetic: hostile ambient switch values must neither
# leak into baseline scenarios nor break the suite itself.
if [ -z "${TEST_HOSTILE_AMBIENT:-}" ]; then
    rc=0
    env AGENT_MEMORY_ACCEPT_CAPABILITIES=0 AGENT_MEMORY_SAFE_INSTALL=1 TEST_HOSTILE_AMBIENT=1 \
        bash "$0" >"$SANDBOX/output" 2>&1 || rc=$?
    [ "$rc" = 0 ] || { sed 's/^/    /' "$SANDBOX/output" >&2; fail "hostile ambient environment broke the suite (rc=$rc)"; }
    echo 'PASS: hostile ambient environment does not leak into scenarios'
fi
