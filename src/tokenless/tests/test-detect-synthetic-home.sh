#!/usr/bin/env bash
# The OpenCode and QwenPaw detectors must keep their own exit contract when
# the host omits HOME, and must say so when the path they report is synthetic.
#
# Both detectors run under `set -euo pipefail` and derive their user-scope
# paths from HOME: the OpenCode config root and the QwenPaw working
# directory. An unguarded expansion therefore aborts the detector with exit 1
# -- the very status these scripts document as "not installed but
# installable" -- so a host that runs detection without HOME (a minimal
# runner, a service manager, `env -i`) is handed a crash dressed as a verdict.
#
# Exit status alone cannot separate the crash from the verdict, so each
# detector is run three ways over one sandbox and the runs are compared:
#
#   1. HOME set to an empty directory -- the control verdict;
#   2. HOME unset -- must land on the same status as (1), because an absent
#      HOME carries exactly the same information as an empty one (no
#      user-scope path exists), and must not print bash's unbound-variable
#      diagnostic;
#   3. HOME set to a directory holding the agent's own state dir -- must still
#      report that directory present, which is what keeps the fix honest: the
#      user-scope probes have to follow HOME, not be switched off by it.
#
# On top of that, a detector without HOME has no real user-scope path to
# report. It must not present the synthetic path it falls back to as an
# ordinary location the operator could go and inspect: the report has to say
# the path is synthetic.
#
# Plain bash over the checked-out adapter tree: no cargo build output, node,
# jq, rtk, python3 or network, and no agent CLI has to be installed -- each
# detector's own CLI variable is pinned to a path that does not exist, so the
# suite never executes a host agent and never depends on what is installed.

set -uo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ADAPTERS="$SCRIPT_DIR/../adapters/tokenless"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

EMPTY_HOME="$TEST_DIR/empty-home"
PROVISIONED_HOME="$TEST_DIR/provisioned-home"
mkdir -p "$EMPTY_HOME" "$PROVISIONED_HOME" "$TEST_DIR/bin"

# The OpenCode detector treats a missing CLI as a missing prerequisite and
# exits before it probes the config root, so the suite needs a CLI that
# answers the version probe and nothing else: the detectors under test never
# drive a host agent, they only ask it which version it is. The stub also
# records the PATH it was invoked under, so a suite can check the entry list
# the detector assembled.
STUB_CLI="$TEST_DIR/bin/stub-agent"
cat > "$STUB_CLI" <<STUBEOF
#!$(command -v bash)
case "\$*" in
    --version|version)
        printf '%s\n' "\$PATH" > "\${STUB_PATH_LOG:-/dev/null}"
        echo "1.0.0 (stub agent)"
        exit 0 ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$STUB_CLI"

# The directory the shell itself lives in. The case below starts the detector
# with a PATH that holds only this directory, so the agent CLI can only be
# found through the entry the detector assembles from HOME -- the host may
# have its own copy of the agent elsewhere, and that would hide a broken
# PATH. The shell has to stay reachable, because the detector runs under it.
SHELL_DIR="$(dirname "$(command -v bash)")"

# The adapters whose detector resolves a user-scope path from HOME. The
# detectors of the other adapters are covered by the suite in #8692, which
# owns the same contract for them.
ADAPTERS_UNDER_TEST=(opencode qwenpaw)

# The user-scope directory each detector reports on, and the checklist label
# it prints for it.
user_scope_dir() {
    case "$1" in
        opencode) printf '.config/opencode' ;;
        qwenpaw)  printf '.qwenpaw' ;;
    esac
}

user_scope_field() {
    case "$1" in
        opencode) printf 'config' ;;
        qwenpaw)  printf 'working dir' ;;
    esac
}

fail() { echo "FAIL: $*" >&2; exit 1; }

# detect <adapter> [env(1) args...] -- sets status, stdout and stderr.
detect() {
    local adapter="$1"
    shift
    status=0
    stdout=$(env "$@" \
        ANOLISA_ADAPTER_DIR="$ADAPTERS" \
        OPENCODE_BIN="$STUB_CLI" \
        QWENPAW_BIN="$STUB_CLI" \
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
    [ -n "$stdout" ] || fail "$adapter with an empty HOME printed no report"

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

    # 3. A synthetic path must be reported as synthetic. A detector without
    #    HOME has no real user-scope path, and presenting the fallback it
    #    probes as an ordinary location would send an operator looking for a
    #    directory that does not exist. The report may carry the note on
    #    stdout or on stderr; either is actionable, but it must be there.
    case "$stdout$stderr" in
        *synthetic*) ;;
        *) fail "$adapter without HOME did not report the synthetic path as synthetic:
$stdout
$stderr" ;;
    esac

    # 4. An explicit override still names a real path, so the report must not
    #    call it synthetic -- and it must not call it synthetic even when the
    #    directory does not exist, because the detector did probe it. A
    #    directory that is not created on purpose: an override that names a
    #    path the host has not provisioned yet is exactly the case where
    #    "synthetic" would be a false claim.
    case "$adapter" in
        opencode) override_var=OPENCODE_CONFIG_DIR ;;
        qwenpaw)  override_var=QWENPAW_WORKING_DIR ;;
    esac
    override_dir="$TEST_DIR/override"
    detect "$adapter" -u HOME "$override_var=$override_dir"
    case "$stdout$stderr" in
        *synthetic*) fail "$adapter with $override_var set still reported a synthetic path:
$stdout
$stderr" ;;
    esac

    # 5. The user-scope probe must still follow a real HOME. The QwenPaw
    #    detector reports its working directory, so a HOME that holds one
    #    must be reported as present -- that is what keeps the fix honest: the
    #    user-scope probe has to follow HOME, not be switched off by it. The
    #    OpenCode detector reports the plugin link rather than the config
    #    directory, so there the same claim is checked the other way round: a
    #    real HOME must not produce a synthetic-path report.
    case "$adapter" in
        qwenpaw)
            scope_dir="$(user_scope_dir "$adapter")"
            mkdir -p "$PROVISIONED_HOME/$scope_dir"
            detect "$adapter" HOME="$PROVISIONED_HOME"
            printf '%s\n' "$stdout" | grep -q "$field.*present" ||
                fail "$adapter with HOME set did not report '$field' present ($PROVISIONED_HOME/$scope_dir exists)"
            rm -rf "$PROVISIONED_HOME/$scope_dir"
            ;;
    esac

    # 6. A real HOME must not produce a synthetic-path report either.
    detect "$adapter" HOME="$PROVISIONED_HOME"
    case "$stdout$stderr" in
        *synthetic*) fail "$adapter with a real HOME reported a synthetic path:
$stdout
$stderr" ;;
    esac

    # 7. The PATH the detector builds for itself must still hold the user's
    #    own bin directory as an entry of its own. The two user-scope entries
    #    have to be separated by a colon: a rewrite that moves the separator
    #    to the wrong end glues them into one entry, so the user's own bin
    #    directory silently drops off the PATH and an agent installed there
    #    stops being found.
    #
    #    The entry list is what matters, not a lookup: the host may have its
    #    own copy of the agent elsewhere, so a lookup can succeed from there
    #    and hide the broken PATH. The detector's own assembly line is
    #    therefore replayed here, taken from the detector rather than copied
    #    into the test, so the assertion follows the code it guards.
    if [ "$adapter" = "qwenpaw" ]; then
        scope_bin="$PROVISIONED_HOME/.local/bin"
        path_line="$(grep -m1 '^export PATH=' "$ADAPTERS/$adapter/scripts/detect.sh")"
        [ -n "$path_line" ] || fail "$adapter does not assemble a PATH"
        assembled_path="$(
            HOME_DIR="$PROVISIONED_HOME" QWENPAW_HOME="" \
            bash -c "$path_line"'; printf "%s" "$PATH"'
        )"
        case ":$assembled_path:" in
            *":$scope_bin:"*) ;;
            *) fail "$adapter did not put \$HOME/.local/bin on the PATH as its own entry:
$assembled_path" ;;
        esac
    fi

    echo "  $adapter: home-less exit $status matches the empty-HOME control, synthetic path reported, user scope still probed with HOME set"
done

echo "detect synthetic-HOME test passed"
