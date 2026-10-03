#!/usr/bin/env bash
# detect.sh — Inspect Claude Code presence and the tokenless plugin state.
# Read-only. Tri-state exit aligns with openclaw/hermes detect.sh:
#   0 = installed and ready
#   1 = not installed but installable (prereqs OK)
#   2 = missing prerequisites
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-claude-code}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

PLUGIN_ID="${COMPONENT}@anolisa-${COMPONENT}"
PLUGIN_SRC="$ADAPTER_DIR/claude-code"

CLAUDE_BIN="${CLAUDE_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

# First-run settling retries: right after provisioning, the claude binary or
# the plugin registry may be transiently invisible on the very first detect.sh
# execution (filesystem/PATH init timing race). settle() only retries checks
# that report a retryable failure (exit status 1); checks that succeed or
# report a definitive result return immediately, so steady-state runs stay
# fast.
DETECT_RETRIES="${TOKENLESS_DETECT_RETRIES:-3}"
DETECT_RETRY_DELAY="${TOKENLESS_DETECT_RETRY_DELAY:-1}"
# Separate budget for one result that only looks definitive: a `plugin list`
# that succeeds but omits the plugin. `claude plugin install` writes
# marketplace.json/plugin.json, yet the CLI's plugin registry index only picks
# them up on a later scan, so the very first list right after provisioning can
# succeed and still omit the just-installed plugin (GH #3082). Kept apart from
# DETECT_RETRIES so a genuinely absent plugin pays a bounded settle rather than
# the whole settling budget.
#
# That settle is bounded by BOTH a wall-clock window and an attempt count, and
# whichever runs out first ends the loop. Bounding it by count alone (GH #3085:
# 2 re-lists, ~2s) recurred as GH #3267 — registry-index lag is a wall-clock
# phenomenon, so on a host busy with a concurrent full build those same 2
# re-lists cover far less catch-up time and the false "not installed" verdict
# comes back. A window covers the lag no matter how slow each individual list
# call is; the count cap keeps a permanently broken registry from polling
# forever.
DETECT_PLUGIN_RELISTS="${TOKENLESS_DETECT_PLUGIN_RELISTS:-5}"
# Wall-clock seconds the plugin probe may spend settling, the initial
# `plugin list` included. That window is a deadline shared by the whole probe
# — the re-list loop and the settle retries nested inside every attempt — so
# no part of the probe outlives it (see PLUGIN_PROBE_DEADLINE below). Set this
# or DETECT_PLUGIN_RELISTS to 0 to opt out of re-listing entirely; the
# Makefile's informational pre-install probe does.
DETECT_PLUGIN_RELIST_WINDOW="${TOKENLESS_DETECT_PLUGIN_RELIST_WINDOW:-20}"
# Ceiling for a single backoff sleep, so a wide window polls the registry at a
# widening interval instead of hammering a CLI that is already contended.
DETECT_PLUGIN_RELIST_MAX_DELAY="${TOKENLESS_DETECT_PLUGIN_RELIST_MAX_DELAY:-4}"
# Deadline for that window, on the $SECONDS clock: the moment after which the
# plugin probe may start no further work. Empty while the probe is
# count-bounded only. settle_plugin_listed() sets it; the probe_* helpers next
# to it read it.
PLUGIN_PROBE_DEADLINE=""
# Seconds a sleep clamped to the deadline may wake up late and still get its
# attempt. $SECONDS counts whole seconds and probe_delay() clamps against it,
# so even an exact clamped sleep can cross the tick and report the deadline as
# one second gone; that much lateness is the clock's resolution, not the host
# being slow. Anything later is the host running the sleep long, the window
# really is over, and no new call may start.
DETECT_PROBE_DEADLINE_SLACK=1

# settle <cmd...> — run cmd once; if it reports a retryable failure (exit
# status 1), sleep DETECT_RETRY_DELAY and retry, up to DETECT_RETRIES retries
# (that is, at most 1 + DETECT_RETRIES attempts in total). Exit status 0 is
# success. Any other exit status is a definitive result and is returned
# immediately without further retries. Returns the final exit status.
# Callers must therefore reserve exit status 1 for conditions that a retry
# may still resolve.
settle() {
    local retry=0 rc=0
    "$@"; rc=$?
    while [ "$rc" -eq 1 ] && [ "$retry" -lt "$DETECT_RETRIES" ]; do
        retry=$((retry + 1))
        sleep "$DETECT_RETRY_DELAY"
        "$@"; rc=$?
    done
    return "$rc"
}

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

find_claude_bin() {
    CLAUDE_BIN="$(command -v claude 2>/dev/null || true)"
    [ -n "$CLAUDE_BIN" ]
}

if [ -z "$CLAUDE_BIN" ]; then
    # PATH/filesystem may still be settling right after install; re-check
    # briefly before declaring the CLI missing. A genuinely absent CLI cannot
    # be distinguished from that settling race, so it spends one retry budget
    # (set TOKENLESS_DETECT_RETRIES=0 to opt out).
    settle find_claude_bin || true
fi

line "${AGENT} detect"
if [ -n "$CLAUDE_BIN" ] && [ -x "$CLAUDE_BIN" ]; then
    CLAUDE_VER="$("$CLAUDE_BIN" --version 2>/dev/null | awk '{print $1}' || echo unknown)"
    field "claude CLI"        "present (${CLAUDE_BIN}, v${CLAUDE_VER})"
else
    field "claude CLI"        "missing"
    note_prereq_missing "claude CLI"
fi

# Informational only: claude creates ~/.claude on first run; absence is
# not a prerequisite failure. Check once, without settling retries: nothing in
# this script creates the directory before `claude plugin list` runs, and that
# call carries its own retry budget and initializes ~/.claude itself, so
# retrying this probe would only delay the report.
if [ -d "$HOME/.claude" ]; then
    field "claude config dir" "present ($HOME/.claude)"
else
    field "claude config dir" "missing (created on first claude run)"
fi

if [ -f "$PLUGIN_SRC/.claude-plugin/marketplace.json" ]; then
    field "marketplace.json"  "present"
else
    field "marketplace.json"  "missing"
    note_prereq_missing "marketplace.json"
fi

if [ -f "$PLUGIN_SRC/.claude-plugin/plugin.json" ]; then
    field "plugin.json"       "present"
else
    field "plugin.json"       "missing (run: make stamp-adapter-templates)"
fi

# plugin_manifests_staged — true when the local manifests that
# `claude plugin install` consumes (the single-plugin marketplace plus the
# stamped plugin manifest) are both on disk. Their presence is what makes an
# omitted plugin ambiguous: the installer had something to register, so the
# CLI's registry index may simply not have caught up with it yet.
plugin_manifests_staged() {
    [ -f "$PLUGIN_SRC/.claude-plugin/marketplace.json" ] \
        && [ -f "$PLUGIN_SRC/.claude-plugin/plugin.json" ]
}

# claude_plugin_listed — probe the plugin registry, using the settle()
# exit-status contract: 0 = plugin listed; 1 = `claude plugin list` itself
# failed (the CLI may still be initializing ~/.claude on first run, so a
# retry may still succeed); 2 = `plugin list` ran successfully but did not
# list the plugin.
claude_plugin_listed() {
    local listing
    if ! listing="$("$CLAUDE_BIN" plugin list 2>&1)"; then
        return 1
    fi
    if printf '%s\n' "$listing" | grep -qF "$PLUGIN_ID"; then
        return 0
    fi
    return 2
}

# relist_backoff <attempt> — how long to wait before re-list <attempt>
# (1-based): DETECT_RETRY_DELAY doubled per attempt and capped at
# DETECT_PLUGIN_RELIST_MAX_DELAY, so a wide window polls the registry at a
# widening interval instead of hammering a CLI that is already contended.
# probe_delay() below clamps that to the time left in the settling window.
# awk does the arithmetic because DETECT_RETRY_DELAY may be fractional, which
# bash cannot multiply.
relist_backoff() {
    awk -v base="$DETECT_RETRY_DELAY" -v attempt="$1" \
        -v cap="$DETECT_PLUGIN_RELIST_MAX_DELAY" 'BEGIN {
            delay = base * (2 ^ (attempt - 1))
            if (delay > cap) delay = cap
            printf "%.3f\n", delay
        }'
}

# The re-list loop and the settle retries nested inside each of its attempts
# share ONE deadline, so DETECT_PLUGIN_RELIST_WINDOW bounds the whole plugin
# probe rather than only the gaps between re-lists. Clamping the backoff is
# not enough on its own: a sleep clamped to the last of the window still hands
# over to a nested budget of DETECT_RETRIES x DETECT_RETRY_DELAY that never
# looks at the clock, so a CLI that starts failing part-way through a re-list
# runs the probe several times past the window it is documented to respect.
#
# The deadline bounds when the probe may start work, and every one of those
# decisions is made against the clock as it reads *after* sleeping, so a sleep
# the host ran long cannot buy a fresh attempt. A `plugin list` already in
# flight is a different matter and is never killed — aborting a slow but
# working call would manufacture the very false negative this settling exists
# to avoid — so one slow call can still finish after the window closes; nothing
# is started after it.

# probe_window_open — true while the probe still has settling time to spend,
# so another (clamped) sleep may be requested for it. Always true while
# re-listing is opted out and the probe is count-bounded only.
probe_window_open() {
    [ -z "$PLUGIN_PROBE_DEADLINE" ] || [ "$SECONDS" -lt "$PLUGIN_PROBE_DEADLINE" ]
}

# probe_attempt_allowed <requested> <granted> — true when a `plugin list` may
# still be started after a sleep that was asked to be <requested> seconds and
# was granted <granted>, judged on when that sleep actually woke up. Inside the
# window it is simply true. Past the deadline one narrow case still counts as
# the boundary attempt the clamp paid for: probe_delay() shortened this sleep
# to the time left AND it woke up no more than DETECT_PROBE_DEADLINE_SLACK
# seconds late, the lateness $SECONDS' whole-second resolution can produce on
# its own. A clamped sleep the host ran longer than that buys nothing, exactly
# like an unclamped one that overshot — the window closed while we were asleep,
# and the call this would start is a new one, not an invocation already in
# flight that we are letting finish. Being clamped is not a licence on its own:
# "granted < requested" says what we asked for, not how long we actually
# waited. Always true while re-listing is opted out.
probe_attempt_allowed() {
    [ -n "$PLUGIN_PROBE_DEADLINE" ] || return 0
    local left=$((PLUGIN_PROBE_DEADLINE - SECONDS))
    if [ "$left" -ge 0 ]; then
        return 0
    fi
    [ "$left" -ge $((0 - DETECT_PROBE_DEADLINE_SLACK)) ] \
        && awk -v want="$1" -v got="$2" 'BEGIN { exit !(got < want) }'
}

# probe_delay <requested-seconds> — the sleep to request before the next probe
# attempt: <requested-seconds> clamped to the whole seconds left in the
# settling window, so neither a re-list backoff nor a nested settle retry can
# sleep past the deadline both share. Passed through unchanged while
# re-listing is opted out. awk does the arithmetic because both sides may be
# fractional.
probe_delay() {
    awk -v want="$1" -v deadline="$PLUGIN_PROBE_DEADLINE" -v now="$SECONDS" 'BEGIN {
        if (deadline != "" && want > deadline - now) want = deadline - now
        if (want < 0) want = 0
        printf "%.3f\n", want
    }'
}

# settle_within_window <cmd...> — settle() for a probe that shares the plugin
# probe's deadline. The first attempt always runs: a probe that never called
# the CLI would only be guessing. Each retry then needs the window still open,
# sleeps no longer than the time left, and is skipped altogether when that
# sleep carried the probe past the deadline, so the nested budget cannot
# outlive the window that bounds it. Same exit-status contract as settle(), so
# callers must likewise use it in a condition context.
settle_within_window() {
    local retry=0 rc=0 want=0 got=0
    "$@"; rc=$?
    while [ "$rc" -eq 1 ] && [ "$retry" -lt "$DETECT_RETRIES" ] \
        && probe_window_open; do
        retry=$((retry + 1))
        want="$DETECT_RETRY_DELAY"
        got="$(probe_delay "$want")"
        sleep "$got"
        # Boundary after sleeping: a retry sleep a loaded host ran past the
        # deadline buys no further attempt.
        probe_attempt_allowed "$want" "$got" || break
        "$@"; rc=$?
    done
    return "$rc"
}

# settle_plugin_listed — settle() for the plugin probe, extended with a
# bounded re-list of the "list succeeded but omitted the plugin" case.
#
# With nothing staged on disk that omission is definitive — there was no
# plugin for the registry to index — so it is returned immediately (status
# 2) without spending any budget. With the manifests staged it is also the
# shape of the GH #3082 first-run race, where the registry index lags one
# scan behind the installer and the just-installed plugin is missing from an
# otherwise successful list. Re-list until the plugin shows up, the settling
# window elapses or the attempt cap is reached, backing off between attempts,
# so the loop covers that window on a contended host too (GH #3267) and not
# only on an idle one; a genuinely absent plugin still ends up reported as
# "not installed", only once the window has run out. The window is a deadline
# for the settle retries nested inside each attempt as well, not only for this
# loop, so the probe stops starting work when it expires.
#
# This deliberately does NOT degrade to "the CLI and ~/.claude are there, so
# call it installed": that reports a host whose plugin was never registered as
# ready, and the caller then skips install.sh. The plugin probe stays a hard
# negative — it is simply given enough wall-clock time to be right.
#
# Like settle(), this must be called in a condition context so that `set -e`
# stays suppressed while the probe reports a non-zero status.
settle_plugin_listed() {
    local relist=0 rc=0 want=0 got=0 started="$SECONDS"
    # One deadline for the whole probe — the initial list, every re-list and
    # the settle retries nested inside them — but only while the probe may
    # settle at all. With either budget at 0 it stays count-bounded: that is
    # the documented opt-out, and how the Makefile's informational
    # pre-install probe keeps its single instant list call.
    PLUGIN_PROBE_DEADLINE=""
    if [ "$DETECT_PLUGIN_RELISTS" -gt 0 ] \
        && [ "$DETECT_PLUGIN_RELIST_WINDOW" -gt 0 ]; then
        PLUGIN_PROBE_DEADLINE=$((started + DETECT_PLUGIN_RELIST_WINDOW))
    fi
    settle_within_window claude_plugin_listed; rc=$?
    # The elapsed-vs-window test below is the same bound the deadline encodes,
    # spelled out because it also has to hold when there is no deadline: a
    # zero window must still mean "no re-lists at all".
    while [ "$rc" -eq 2 ] && [ "$relist" -lt "$DETECT_PLUGIN_RELISTS" ] \
        && [ $((SECONDS - started)) -lt "$DETECT_PLUGIN_RELIST_WINDOW" ] \
        && plugin_manifests_staged; do
        relist=$((relist + 1))
        want="$(relist_backoff "$relist")"
        got="$(probe_delay "$want")"
        sleep "$got"
        # Boundary after sleeping: a clamped backoff bought this one last
        # attempt, an unclamped one that overshot the deadline bought none.
        probe_attempt_allowed "$want" "$got" || break
        settle_within_window claude_plugin_listed; rc=$?
    done
    return "$rc"
}

if [ -n "$CLAUDE_BIN" ] && [ -x "$CLAUDE_BIN" ]; then
    # First-run race: `claude plugin list` may transiently fail while the CLI
    # initializes ~/.claude (status 1), and may transiently omit a plugin
    # whose manifests were staged only moments ago (status 2). Both are
    # retried within their own bounded budgets, and "not installed" is
    # reported only once those budgets are exhausted.
    if settle_plugin_listed; then
        field "plugin install"    "installed ($PLUGIN_ID)"
    else
        field "plugin install"    "not installed"
        note_install_missing "$PLUGIN_ID"
    fi
fi

if [ -f "$PLUGIN_SRC/hooks/run-hook.sh" ]; then
    field "hook dispatcher"   "present"
else
    field "hook dispatcher"   "missing (hooks/run-hook.sh)"
    note_prereq_missing "hook dispatcher"
fi

if command -v python3 &>/dev/null; then
    field "python3"           "present ($(command -v python3))"
else
    field "python3"           "missing"
    note_prereq_missing "python3"
fi

# jq is required by tool_ready_hook.sh; absence disables that hook only
# (rewrite + compress-response still work). Treat as informational.
if command -v jq &>/dev/null; then
    field "jq"                "present ($(command -v jq))"
else
    field "jq"                "missing (tool-ready hook disabled)"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary"  "present (${runtime_bin})"
else
    field "tokenless binary"  "missing"
    note_prereq_missing "tokenless binary"
fi

rtk_bin="$(command -v rtk 2>/dev/null || true)"
if [ -n "$rtk_bin" ]; then
    field "rtk binary"        "present (${rtk_bin})"
else
    field "rtk binary"        "missing"
    note_prereq_missing "rtk binary"
fi

# Shared hook scripts live under FHS; warn when missing so user knows to run
# `make install` (or install the RPM) before adapter actually fires.
SHARED_HOOKS_DIR=""
for d in /usr/local/share/anolisa/adapters/tokenless/common/hooks \
         /usr/share/anolisa/adapters/tokenless/common/hooks \
         "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
    if [ -d "$d" ]; then SHARED_HOOKS_DIR="$d"; break; fi
done
if [ -n "$SHARED_HOOKS_DIR" ]; then
    field "shared hooks dir"  "present ($SHARED_HOOKS_DIR)"
else
    field "shared hooks dir"  "missing (run: make -C src/tokenless install)"
    note_prereq_missing "shared hooks dir"
fi

if [ ${#PREREQ_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: missing prerequisites (${PREREQ_MISSING[*]})"
    exit 2
fi
if [ ${#INSTALL_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: not installed (ready to install)"
    exit 1
fi
line "${AGENT}: ready"
exit 0
