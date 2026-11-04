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
#      exists), and must not print bash's unbound-variable diagnostic;
#   3. HOME set to a directory holding the agent's own state dir -- must still
#      report that directory present, which is what keeps the fix honest: the
#      user-scope probes have to follow HOME, not be switched off by it.
#
# Plain bash over the checked-out adapter tree: no cargo build output, node, jq,
# rtk, python3 or network, and no agent CLI has to be installed -- each
# detector's own CLI variable is pinned to a path that does not exist, so the
# suite never executes a host agent and never depends on what is installed.

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

# detect <adapter> [env(1) args...] -- sets status, stdout and stderr.
# claude-code's detector settles transient first-run misses by sleeping; that
# budget is not what this suite measures, so it is turned down to zero.
detect() {
    local adapter="$1"
    shift
    status=0
    stdout=$(env "$@" \
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

echo "detect unset-HOME test passed"
