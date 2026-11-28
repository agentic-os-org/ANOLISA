#!/usr/bin/env bash
# Adapter detectors must keep their own exit contract when the host omits HOME.
#
# Every detect.sh runs under `set -euo pipefail` and derives its user-scope
# paths from HOME: the ~/.local/bin PATH prefix, the agent's own config or state
# directory, and the ~/.local/share hook fallback. An unguarded expansion
# therefore aborts the detector with exit 1 -- the very status these scripts
# document as "not installed but installable" -- so a host that runs detection
# without HOME (a minimal runner, a service manager, `env -i`) is handed a crash
# dressed as a verdict: the caller reports the agent installable, or acts on the
# missing user-scope state, instead of seeing an error.
#
# Exit status alone cannot separate the crash from the verdict, so each adapter
# is run three ways over one sandbox and the runs are compared:
#
#   1. HOME set to an empty directory -- the control verdict;
#   2. HOME unset -- must land on the same status as (1), because an absent HOME
#      carries exactly the same information as an empty one (no user-scope path
#      exists), must not print bash's unbound-variable diagnostic, and must say
#      "HOME is unset" in the report rather than leaving the caller to guess why
#      no user-scope path was probed;
#   3. HOME set to a directory holding the agent's own state dir -- must still
#      report that directory present, which is what keeps the fix honest: the
#      user-scope probes have to follow HOME, not be switched off by it.
#
# A fourth block covers the state dir a caller configures explicitly. That value
# is a decision of its own, not a HOME fallback, so it must be probed exactly as
# given: trailing slashes are trimmed and every other byte of the name reaches
# the probes untouched -- a bare "/" stays "/" instead of being mistaken for "no
# state dir at all", and a name ending in a newline keeps that newline.
#
# Every run is isolated from the adapter overrides a developer shell may already
# carry (HERMES_HOME, OPENCLAW_HOME, OPENCLAW_STATE_DIR). Cases 1-3 measure the
# documented default paths; an inherited override pointing at an existing
# directory makes the detector report that directory present -- correctly, but
# as a failure of the "no user scope exists" assertion, i.e. for a reason that
# has nothing to do with HOME.
#
# Plain bash over the checked-out adapter tree: no cargo build output, node, jq,
# rtk, python3 or network, and no agent CLI has to be installed -- each
# detector's own CLI variable is pinned to a path that does not exist, so the
# suite never executes a host agent and never depends on what is installed. The
# one executable the suite puts on PATH is a stub it writes itself, and that stub
# is only ever resolved with `command -v`, never run.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ADAPTERS="$SCRIPT_DIR/../adapters/tokenless"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

EMPTY_HOME="$TEST_DIR/empty-home"
PROVISIONED_HOME="$TEST_DIR/provisioned-home"
NO_CLI="$TEST_DIR/no-such-cli"
mkdir -p "$EMPTY_HOME" "$PROVISIONED_HOME"

# The adapters whose detector resolves user-scope paths from HOME and is not
# already exercised by an adapter suite of its own. qwenwork's detector is
# macOS-gated and never reads HOME.
ADAPTERS_UNDER_TEST=(claude-code hermes openclaw qwencode)

# The user-scope directory each detector reports on, and the checklist label it
# prints for it.
user_scope_dir() {
    case "$1" in
        claude-code) printf '.claude' ;;
        hermes)      printf '.hermes' ;;
        openclaw)    printf '.openclaw' ;;
        qwencode)    printf '.qwen' ;;
    esac
}

user_scope_field() {
    case "$1" in
        claude-code) printf 'claude config dir' ;;
        hermes)      printf 'hermes home' ;;
        openclaw)    printf 'openclaw home' ;;
        qwencode)    printf 'qwen config dir' ;;
    esac
}

fail() { echo "FAIL: $*" >&2; exit 1; }

# expect_report <report> <needle> <why> and refuse_report <report> <needle> <why>
# -- plain substring checks, so the assertions neither depend on the padding
# `printf '%-26s'` applies to a field label nor need a pipeline that grep -q
# could close early.
expect_report() {
    case "$1" in
        *"$2"*) return 0 ;;
    esac
    fail "$3 (report has no '$2')"
}

refuse_report() {
    case "$1" in
        *"$2"*) fail "$3 (report claims '$2')" ;;
    esac
    return 0
}

# detect <adapter> [env(1) args...] -- sets status, stdout and stderr.
# The three unsets isolate the run from adapter state-dir overrides inherited
# from the caller's shell (see the header). They come first because env(1)
# applies its arguments left to right, so a case that wants to pin one of them
# passes it in "$@" and still wins.
# claude-code's detector settles transient first-run misses by sleeping; that
# budget is not what this suite measures, so it is turned down to zero.
detect() {
    local adapter="$1"
    shift
    status=0
    stdout=$(env \
        -u HERMES_HOME \
        -u OPENCLAW_HOME \
        -u OPENCLAW_STATE_DIR \
        "$@" \
        ANOLISA_ADAPTER_DIR="$ADAPTERS" \
        CLAUDE_BIN="$NO_CLI" \
        HERMES_BIN="$NO_CLI" \
        OPENCLAW_BIN="$NO_CLI" \
        QWEN_BIN="$NO_CLI" \
        TOKENLESS_DETECT_RETRIES=0 \
        TOKENLESS_DETECT_RETRY_DELAY=0 \
        TOKENLESS_DETECT_PLUGIN_RELISTS=0 \
        TOKENLESS_DETECT_PLUGIN_RELIST_WINDOW=0 \
        bash "$ADAPTERS/$adapter/scripts/detect.sh" 2>"$TEST_DIR/stderr") || status=$?
    stderr=$(cat "$TEST_DIR/stderr")
}

for adapter in "${ADAPTERS_UNDER_TEST[@]}"; do
    detector="$ADAPTERS/$adapter/scripts/detect.sh"
    [ -f "$detector" ] || fail "$adapter has no detect.sh at $detector"

    # 1. Control: an empty HOME still yields one of the documented statuses.
    detect "$adapter" HOME="$EMPTY_HOME"
    control_status=$status
    case $control_status in
        0 | 1 | 2) ;;
        *) fail "$adapter with an empty HOME exited $control_status, expected 0, 1 or 2" ;;
    esac

    # 2. The same detector without HOME at all.
    detect "$adapter" -u HOME
    case $status in
        0 | 1 | 2) ;;
        *) fail "$adapter without HOME exited $status, expected 0, 1 or 2 (a crash under
     \`set -u\` exits 1 and is indistinguishable from a verdict, so see stderr)" ;;
    esac
    case $stderr in
        *"unbound variable"*)
            fail "$adapter aborted on an unbound variable without HOME: $stderr" ;;
    esac
    [ -n "$stdout" ] || fail "$adapter without HOME printed no report"
    expect_report "$stdout" "HOME is unset" \
        "$adapter without HOME did not say why no user-scope path was probed"
    [ "$status" = "$control_status" ] || fail "$adapter without HOME exited $status but exited $control_status with an empty HOME"
    field="$(user_scope_field "$adapter")"
    if printf '%s\n' "$stdout" | grep -q "$field.*present"; then
        fail "$adapter without HOME reported '$field' present, but no user scope exists"
    fi

    # 3. The user-scope probe must still follow a real HOME.
    scope_dir="$(user_scope_dir "$adapter")"
    mkdir -p "$PROVISIONED_HOME/$scope_dir"
    detect "$adapter" HOME="$PROVISIONED_HOME"
    printf '%s\n' "$stdout" | grep -q "$field.*present" ||
        fail "$adapter with HOME set did not report '$field' present ($PROVISIONED_HOME/$scope_dir exists)"
    rmdir "$PROVISIONED_HOME/$scope_dir"

    echo "  $adapter: home-less exit $status matches the empty-HOME control, user scope still probed with HOME set"
done

# 4. An explicitly configured state dir must be probed exactly as given. Only
# hermes and openclaw accept one; openclaw takes it from two variables
# (OPENCLAW_STATE_DIR, falling back to OPENCLAW_HOME).
EXPLICIT_STATE="$TEST_DIR/explicit-state"
mkdir -p "$EXPLICIT_STATE/bin" "$EXPLICIT_STATE/extensions/tokenless"
# `command -v tokenless` resolves this stub, and the detector never runs it: it
# shows that "<state dir>/bin" survives normalization and still precedes
# /usr/local/bin.
printf '#!/bin/sh\nexit 0\n' >"$EXPLICIT_STATE/bin/tokenless"
chmod +x "$EXPLICIT_STATE/bin/tokenless"

detect openclaw HOME="$EMPTY_HOME" OPENCLAW_STATE_DIR="$EXPLICIT_STATE/"
expect_report "$stdout" "present ($EXPLICIT_STATE)" \
    "openclaw did not trim the trailing slash of an explicit OPENCLAW_STATE_DIR"
expect_report "$stdout" "installed ($EXPLICIT_STATE/extensions/tokenless)" \
    "openclaw did not probe <state dir>/extensions/tokenless for an explicit OPENCLAW_STATE_DIR"
expect_report "$stdout" "present ($EXPLICIT_STATE/bin/tokenless)" \
    "openclaw did not put <state dir>/bin on PATH ahead of /usr/local/bin for an explicit OPENCLAW_STATE_DIR"
refuse_report "$stdout" "HOME is unset" \
    "openclaw fell back to the HOME story although OPENCLAW_STATE_DIR was set"

# The same state dir with a doubled separator, which a caller that concatenates
# "$dir/" onto an already terminated path produces: the trim has to repeat, or
# the report carries the untrimmed value and the field stops being comparable
# with the one every other probe prints.
detect openclaw HOME="$EMPTY_HOME" OPENCLAW_STATE_DIR="$EXPLICIT_STATE//"
expect_report "$stdout" "present ($EXPLICIT_STATE)" \
    "openclaw trimmed only one of the trailing slashes of an explicit OPENCLAW_STATE_DIR"

# Only the slashes may go. A name that ends in a newline is legal, and trimming
# it through a command substitution would delete that newline -- bash strips
# trailing newlines from $(...) -- so the detector would probe a directory that
# does not exist and report a configured state dir as missing.
NEWLINE_STATE="$TEST_DIR/newline-state"$'\n'
mkdir -p "$NEWLINE_STATE/extensions/tokenless"
detect openclaw HOME="$EMPTY_HOME" OPENCLAW_STATE_DIR="$NEWLINE_STATE/"
expect_report "$stdout" "present ($NEWLINE_STATE)" \
    "openclaw dropped the trailing newline of an explicit OPENCLAW_STATE_DIR"
expect_report "$stdout" "installed ($NEWLINE_STATE/extensions/tokenless)" \
    "openclaw probed the wrong directory for a state dir whose name ends in a newline"

# A bare "/" is a real, if unusual, state dir. `${dir%/}` collapses it to "", and
# an empty state dir is exactly how "HOME is unset" is reported, so an unguarded
# trim turns the explicit root into a missing one: "/bin" leaves PATH,
# "/extensions/tokenless" is never probed, and the report blames HOME -- which
# these three runs do have.
detect openclaw HOME="$EMPTY_HOME" OPENCLAW_STATE_DIR=/
expect_report "$stdout" "present (/)" \
    "openclaw treated an explicit OPENCLAW_STATE_DIR=/ as missing"
refuse_report "$stdout" "HOME is unset" \
    "openclaw blamed an unset HOME for an explicit OPENCLAW_STATE_DIR=/"

detect openclaw HOME="$EMPTY_HOME" OPENCLAW_HOME=/
expect_report "$stdout" "present (/)" \
    "openclaw treated an explicit OPENCLAW_HOME=/ as missing"
refuse_report "$stdout" "HOME is unset" \
    "openclaw blamed an unset HOME for an explicit OPENCLAW_HOME=/"

detect hermes HOME="$EMPTY_HOME" HERMES_HOME=/
expect_report "$stdout" "present (/)" \
    "hermes treated an explicit HERMES_HOME=/ as missing"
refuse_report "$stdout" "HOME is unset" \
    "hermes blamed an unset HOME for an explicit HERMES_HOME=/"

echo "  explicit state dir: trailing slashes trimmed, rest of the name kept, bare / still probed"

echo "detect unset-HOME test passed"
