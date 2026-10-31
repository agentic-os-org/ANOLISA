#!/usr/bin/env bash
# test-lifecycle-home-safe.sh — the adapter lifecycle scripts must not abort on
# a missing HOME, and must never act on a path under /.
#
# The adapter installers and uninstallers derive their user-scope paths from
# HOME while running under `set -euo pipefail`. A host that invokes one of them
# without HOME -- a minimal runner, a service manager unit without
# `Environment=HOME=`, a runner started with `env -i` -- therefore aborted on
# an unbound variable, with exit 1, which is also the status these scripts use
# for a real failure ("plugin source not found", "the agent CLI rejected the
# bundle"). The crash reached the caller dressed as a verdict, and it landed
# before the script had said whether anything had been written or removed.
#
# The second failure mode is the quieter one. An unset HOME aborts, but a HOME
# that is set to the empty string does not: `$HOME/.local` expands to
# `/.local`, and the script proceeds to write or remove paths directly under
# the filesystem root. A dry run shows where the real path would land:
#
#   $ env HOME= ANOLISA_DRY_RUN=1 bash .../qwenpaw/scripts/uninstall.sh
#   DRY-RUN: rm -rf /.qwenpaw/plugins/tokenless
#
# This suite pins the contract for the adapter lifecycle scripts:
#
#   1. An unset HOME and an empty HOME land on the same status, and neither
#      prints bash's unbound-variable diagnostic.
#   2. No path the scripts act on is built under /.
#   3. An explicit override keeps working when HOME is absent, so a host that
#      knows where its state lives is not forced to invent a HOME.
#   4. A host with a normal HOME sees unchanged behavior.
#   5. A script that cannot resolve its destination says so and stops before
#      it writes anything.
#
# It is plain bash over the checked-out adapter tree with every agent CLI
# pinned to a path that does not exist -- no cargo build output, node, jq,
# rtk, python3 or network, and no host agent is ever executed -- so it costs
# well under a second on CI.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
TOKENLESS_DIR="$(cd "$TESTS_DIR/.." && pwd)"
ADAPTERS="$TOKENLESS_DIR/adapters/tokenless"
# The sandbox is created inside the tree under test so that the path uses the
# same separator as the tree on every host: a temporary directory outside it
# can come back with a different separator, and the cleanup below then trips
# over it. The name carries the pid so that two suites running at the same
# time do not delete each other's sandbox.
SANDBOX="$TOKENLESS_DIR/.lifecycle-home-safe.$$.tmp"
rm -rf "$SANDBOX"
mkdir -p "$SANDBOX"
# FIXTURES_ARE_TEMP holds the files the suite creates below; only those are
# removed again, so a tree that already carries a build artifact keeps it.
FIXTURES_ARE_TEMP=""
cleanup() {
    local fixture
    for fixture in $FIXTURES_ARE_TEMP; do
        rm -f "$fixture"
    done
    rm -rf "$SANDBOX"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() {
    PASSED=$((PASSED + 1))
    printf '  ok   %s\n' "$1"
}

fail() {
    FAILED=$((FAILED + 1))
    printf '  FAIL %s\n' "$1"
    if [ $# -gt 1 ]; then
        shift
        printf '       %s\n' "$@"
    fi
}

# The adapters whose lifecycle scripts this suite covers. The detectors of
# claude-code, hermes, openclaw and qwencode are covered by
# test-detect-unset-home.sh, the opencode and qwenpaw detectors by
# test-detect-synthetic-home.sh, and the codex and opencode lifecycle scripts
# have their own pull requests.
ADAPTERS_UNDER_TEST=(qoder openclaw qwencode qwenpaw)

# The detectors that this suite does not own: openclaw's and qwencode's
# detectors are covered by test-detect-unset-home.sh, and qwenpaw's by
# test-detect-synthetic-home.sh. They are listed here so that the suite
# reports on them but does not fail on work that belongs to another pull
# request.
DETECTORS_OWNED_ELSEWHERE="openclaw/detect.sh qwencode/detect.sh qwenpaw/detect.sh"

detector_owned_elsewhere() {
    local candidate="$1" other
    for other in $DETECTORS_OWNED_ELSEWHERE; do
        [ "$candidate" = "$other" ] && return 0
    done
    return 1
}

# The agent CLIs are pinned to a path that does not exist. The lifecycle
# scripts must not reach a real agent, and a missing CLI is a state these
# scripts already handle, so pinning it keeps the suite hermetic.
NO_SUCH_CLI="$SANDBOX/no-such-cli"

# The home a run with a normal HOME must use: any path the scripts derive
# from it has to stay inside it.
FAKE_HOME="$SANDBOX/home"
mkdir -p "$FAKE_HOME"

# Some of the adapters check for a build artifact before they act. The
# fixtures are stubs: the suite is about the HOME handling, not about the
# artifacts themselves, and a stub is enough to get past the check.
if [ ! -f "$ADAPTERS/openclaw/dist/index.js" ]; then
    mkdir -p "$ADAPTERS/openclaw/dist"
    echo '// stub' > "$ADAPTERS/openclaw/dist/index.js"
    FIXTURES_ARE_TEMP="$FIXTURES_ARE_TEMP $ADAPTERS/openclaw/dist/index.js"
fi
for artifact in plugin.json plugin.py requirements.txt; do
    if [ -f "$ADAPTERS/qwenpaw/$artifact.in" ] && [ ! -f "$ADAPTERS/qwenpaw/$artifact" ]; then
        sed -e 's/@VERSION@/0.9.0/g' -e 's/@WHEEL_VERSION@/0.9.0/g' \
            "$ADAPTERS/qwenpaw/$artifact.in" > "$ADAPTERS/qwenpaw/$artifact"
        FIXTURES_ARE_TEMP="$FIXTURES_ARE_TEMP $ADAPTERS/qwenpaw/$artifact"
    fi
done

# run_script <script> <home-mode> [extra env assignments...]
#   home-mode: unset | empty | set
# The result lands in STATUS, STDOUT and STDERR.
run_script() {
    local script="$1" mode="$2"
    shift 2
    local status=0
    local -a env_args=(
        "ANOLISA_ADAPTER_DIR=$ADAPTERS"
        "ANOLISA_DRY_RUN=1"
        "CODEX_BIN=$NO_SUCH_CLI"
        "OPENCLAW_BIN=$NO_SUCH_CLI"
        "OPENCODE_BIN=$NO_SUCH_CLI"
        "QWEN_BIN=$NO_SUCH_CLI"
        "QWENPAW_BIN=$NO_SUCH_CLI"
    )
    case "$mode" in
        unset) env_args=("-u" "HOME" "${env_args[@]}") ;;
        empty) env_args=("HOME=" "${env_args[@]}") ;;
        set)   env_args=("HOME=$FAKE_HOME" "${env_args[@]}") ;;
    esac
    env "${env_args[@]}" "$@" bash "$script" \
        >"$SANDBOX/stdout" 2>"$SANDBOX/stderr" || status=$?
    STATUS=$status
    STDOUT="$(cat "$SANDBOX/stdout")"
    STDERR="$(cat "$SANDBOX/stderr")"
}

# A path the script would act on, sitting directly under the filesystem root.
# The adapters write into a dot-directory below the home, so anything of the
# form /.local, /.config, /.qwen, /.qoder, /.openclaw, /.qwenpaw or /.copaw
# is a root path. The dot is part of the name, so it has to be allowed
# between the separator and the name -- without it the pattern misses every
# directory these adapters actually use.
root_path_reported() {
    printf '%s\n%s\n' "$STDOUT" "$STDERR" \
        | grep -qE '(^|[^A-Za-z0-9_./-])/\.?(local|config|qwen|qoder|openclaw|qwenpaw|copaw)(/|$)'
}

echo "==> tokenless adapter lifecycle scripts without HOME"

for adapter in "${ADAPTERS_UNDER_TEST[@]}"; do
    for script in detect.sh install.sh uninstall.sh; do
        path="$ADAPTERS/$adapter/scripts/$script"
        [ -f "$path" ] || continue
        label="$adapter/$script"
        detector_owned_elsewhere "$label" && continue

        # 1. An unset HOME must not produce bash's unbound-variable
        #    diagnostic, and must land on the same status as an empty HOME.
        run_script "$path" unset
        unset_status=$STATUS
        unset_out="$STDOUT$STDERR"
        run_script "$path" empty
        empty_status=$STATUS

        case "$unset_out" in
            *"unbound variable"*)
                fail "$label aborted on an unbound HOME" "$unset_out"
                ;;
            *)
                pass "$label does not abort on an unbound HOME"
                ;;
        esac

        if [ "$unset_status" = "$empty_status" ]; then
            pass "$label lands on the same status without HOME and with an empty HOME"
        else
            fail "$label lands on different statuses without HOME ($unset_status) and with an empty HOME ($empty_status)"
        fi

        # 2. Neither run may name a path directly under the filesystem root.
        if root_path_reported; then
            fail "$label names a path under /" "$STDOUT" "$STDERR"
        else
            pass "$label names no path under /"
        fi
    done
done

# 3. An explicit override has to keep working when HOME is absent: a host
#    that knows where its state lives must not be forced to invent a HOME.
#
#    The check is that the script gets past the home guard, not that it
#    succeeds: a script whose agent CLI is missing reports that instead, and
#    that is a different condition from having no destination. So the
#    assertion is that the "cannot resolve" message is absent -- the script
#    found the destination the override named.
check_override() {
    local label="$1" script="$2" var="$3" value="$4"
    run_script "$script" unset "$var=$value"
    if printf '%s\n%s\n' "$STDOUT" "$STDERR" | grep -q "cannot resolve"; then
        fail "$label rejects an explicit $var without HOME" "$STDERR"
    else
        pass "$label accepts an explicit $var without HOME"
    fi
}

check_override "the Qoder installer" "$ADAPTERS/qoder/scripts/install.sh" \
    QODER_HOME "$SANDBOX/qoder-home"
check_override "the Qoder uninstaller" "$ADAPTERS/qoder/scripts/uninstall.sh" \
    QODER_HOME "$SANDBOX/qoder-home"
check_override "the OpenClaw installer" "$ADAPTERS/openclaw/scripts/install.sh" \
    OPENCLAW_STATE_DIR "$SANDBOX/openclaw-state"
check_override "the OpenClaw uninstaller" "$ADAPTERS/openclaw/scripts/uninstall.sh" \
    OPENCLAW_STATE_DIR "$SANDBOX/openclaw-state"
check_override "the Qwen Code uninstaller" "$ADAPTERS/qwencode/scripts/uninstall.sh" \
    QWEN_HOME "$SANDBOX/qwen-home"
check_override "the QwenPaw installer" "$ADAPTERS/qwenpaw/scripts/install.sh" \
    QWENPAW_WORKING_DIR "$SANDBOX/qwenpaw-working"
check_override "the QwenPaw uninstaller" "$ADAPTERS/qwenpaw/scripts/uninstall.sh" \
    QWENPAW_WORKING_DIR "$SANDBOX/qwenpaw-working"

# 4. A host with a normal HOME must not see a synthetic-path report: the
#    resolved paths have to stay inside that home.
for adapter in "${ADAPTERS_UNDER_TEST[@]}"; do
    for script in detect.sh install.sh uninstall.sh; do
        path="$ADAPTERS/$adapter/scripts/$script"
        [ -f "$path" ] || continue
        detector_owned_elsewhere "$adapter/$script" && continue
        run_script "$path" set
        case "$STDOUT$STDERR" in
            *"HOME is unset"*)
                fail "$adapter/$script reports a missing HOME when one is set" \
                     "$STDOUT" "$STDERR"
                ;;
            *)
                pass "$adapter/$script does not report a missing HOME when one is set"
                ;;
        esac
    done
done

# 5. A script that cannot resolve its destination has to say so, and it has to
#    stop before it acts. The uninstallers are the ones that remove things,
#    so they are the ones worth pinning.
for adapter in "${ADAPTERS_UNDER_TEST[@]}"; do
    path="$ADAPTERS/$adapter/scripts/uninstall.sh"
    [ -f "$path" ] || continue
    run_script "$path" unset
    if [ "$STATUS" -eq 0 ]; then
        fail "$adapter/uninstall.sh reports success without a resolvable destination" \
             "$STDOUT" "$STDERR"
    elif printf '%s\n%s\n' "$STDOUT" "$STDERR" | grep -qiE 'HOME|home'; then
        pass "$adapter/uninstall.sh says why it stopped without a resolvable destination"
    else
        fail "$adapter/uninstall.sh stops without explaining why" "$STDOUT" "$STDERR"
    fi
done

echo
echo "  $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
