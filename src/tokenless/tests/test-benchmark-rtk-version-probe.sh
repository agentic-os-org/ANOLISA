#!/usr/bin/env bash
# Regression test for the best-effort RTK version probe in
# benchmark/l1-compressor/run-benchmarks.sh (the `rtk_version` field of
# benchmark_identity.json).
#
# The full runner builds and benches the suite, so this test executes only its
# source-identity prelude: the script is truncated just before the
# "Source identity recorded" line, which keeps rtk discovery, the bounded
# --version probe and the identity writer intact while dropping cargo work.
#
# Covered:
#   1. a bare command name ($RTK_BIN=rtk) is resolved through PATH, matching
#      the Rust find_rtk_binary convention
#   2. an explicit path ($RTK_BIN=/abs/rtk) still resolves
#   3. missing / non-executable / unknown-on-PATH / silent rtk records
#      "unavailable"
#   4. the probe is bounded by RTK_VERSION_TIMEOUT_SECS and its child reaped —
#      both for a binary that dies on SIGTERM and for one that ignores it
#      (which needs the helper's unignorable follow-up deadline). A host with
#      neither timeout(1) nor gtimeout gets an UNBOUNDED probe by design, so
#      these groups report "skip" there instead of failing; everything else
#      (1-3, 6, 7, the exit-status half of 5, and 8) runs on every host
#   5. only a SUCCESSFUL probe records a version: output printed before an
#      expired deadline, and stdout from a non-zero exit, are dropped rather
#      than recorded, and the identity JSON stays valid
#   6. only the first --version line is recorded
#   7. resolution and the success-only rule also hold on hosts without a
#      timeout(1) helper
#
# The reaping checks go through pid_is_gone rather than a bare `kill -0`, and
# check 8 self-tests that helper — see the comments there for why a zombie has
# to count as gone.
#
# The deadline-and-reaping groups are gated on the host actually having a
# timeout(1) helper (HAVE_TIMEOUT_HELPER below): the runner treats "no helper"
# as supported and probes unbounded, so failing there would block
# test-rtk-integration on a host production explicitly permits.

# SC2016 (file scope): every rtk stub body below is deliberately single-quoted
# so it reaches the stub file verbatim and is expanded by the stub's own shell
# at probe time, not by make_stub.
# shellcheck disable=SC2016

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$SCRIPT_DIR/../benchmark/l1-compressor/run-benchmarks.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

L1_DIR="$WORK/benchmark/l1-compressor"
BIN_DIR="$WORK/bin"
mkdir -p "$L1_DIR" "$BIN_DIR"

# The prelude reads the workspace version from ../../Cargo.toml and hashes
# fixtures/*.json, so give it both (an empty fixture glob makes the prelude's
# `|| echo "unknown"` fallback append to the hash and emit invalid JSON).
printf '[workspace.package]\nversion = "0.7.4"\n' > "$WORK/Cargo.toml"
mkdir -p "$L1_DIR/fixtures"
printf '{"probe":"fixture"}\n' > "$L1_DIR/fixtures/sample.json"

awk '/^echo "==> Source identity recorded/{exit} {print}' "$RUNNER" > "$L1_DIR/prelude.sh"
# Same prelude with the timeout(1) helper disabled, to exercise the fallback.
sed 's|^    RTK_TIMEOUT_CMD="timeout"$|    RTK_TIMEOUT_CMD=""|' "$L1_DIR/prelude.sh" \
    > "$L1_DIR/prelude-no-helper.sh"

CURRENT_RUNNER="$L1_DIR/prelude.sh"
IDENTITY_FILE="$L1_DIR/benchmark_identity.json"
FAILED=0
SKIPPED=0

make_stub() { # make_stub <path> <body>
    printf '#!/usr/bin/env bash\n%s\n' "$2" > "$1"
    chmod +x "$1"
}

# probe [VAR=value ...] -> recorded rtk_version
probe() {
    rm -f "$IDENTITY_FILE"
    env "$@" bash "$CURRENT_RUNNER" > /dev/null 2>&1 || true
    sed -n 's/^[[:space:]]*"rtk_version": "\(.*\)",[[:space:]]*$/\1/p' "$IDENTITY_FILE"
}

check() { # check <label> <actual> <expected>
    if [ "$2" = "$3" ]; then
        echo "  ok   $1"
    else
        echo "  FAIL $1: expected '$3', got '$2'"
        FAILED=1
    fi
}

check_true() { # check_true <label> <0-or-1>
    if [ "$2" -eq 0 ]; then
        echo "  ok   $1"
    else
        echo "  FAIL $1"
        FAILED=1
    fi
}

# The deadline must be enforced by wall clock, not by the stub ending on its own.
check_elapsed() { # check_elapsed <label> <elapsed-secs> <deadline-secs> <max-secs>
    if [ "$2" -le "$4" ]; then
        echo "  ok   $1 (returned after ${2}s, deadline ${3}s)"
    else
        echo "  FAIL $1: took ${2}s for a ${3}s deadline"
        FAILED=1
    fi
}

# A probe we abandoned must not leave a LIVE child behind. A bare `kill -0`
# does not say that: it also succeeds for a zombie, i.e. a child that is dead
# but whose status nobody has collected yet — and timeout(1)'s SIGKILL path
# produces exactly that. GNU timeout moves itself and the monitored command
# into a fresh process group and, when the --kill-after deadline fires, signals
# the whole group (cleanup() in src/timeout.c ends with `send_sig (0, sig)`),
# so it dies by SIGKILL without reaping the child it just killed. That orphan
# is reparented to PID 1, and a PID 1 with no reaper (a plain container without
# --init, say) keeps it as a zombie indefinitely. A zombie holds no CPU and no
# descriptors, so "gone" here means: no such PID, or the PID is a zombie.
pid_is_gone() { # pid_is_gone <pid>
    local pid="$1" state=""
    kill -0 "$pid" 2>/dev/null || return 0
    if [ -r "/proc/$pid/stat" ]; then
        # The state char follows the parenthesised comm, which may itself hold
        # spaces and parens — cut at the last ')' rather than counting fields.
        state=$(sed 's/^.*(.*) //' "/proc/$pid/stat" 2>/dev/null | awk '{print $1}' || true)
    elif command -v ps > /dev/null 2>&1; then
        state=$(ps -o state= -p "$pid" 2>/dev/null | tr -d ' \n' || true)
    fi
    # With neither /proc nor ps the state stays unknown, so the answer falls
    # back to `kill -0` alone — the previous, stricter behaviour.
    # Linux prints a single state char; BSD/macOS `ps -o state=` prints a
    # sequence ("Z+", "SN"…), so match on the leading char, not the whole.
    case "$state" in
        Z*) return 0 ;;
    esac
    ! kill -0 "$pid" 2>/dev/null   # ...or it was collected between the checks
}

check_reaped() { # check_reaped <label> <pid-file>
    local rc=1
    [ -f "$2" ] && pid_is_gone "$(cat "$2")" && rc=0
    check_true "$1" "$rc"
}

# Mirror the runner's own discovery: with neither timeout(1) nor gtimeout it
# leaves RTK_TIMEOUT_PREFIX empty and the probe runs unbounded — documented,
# supported behaviour. Asserting a deadline on such a host would fail an
# environment production permits, and (since this suite now gates
# test-rtk-integration) block the RTK integration recipe with it.
HAVE_TIMEOUT_HELPER=1
if ! command -v timeout > /dev/null 2>&1 && ! command -v gtimeout > /dev/null 2>&1; then
    HAVE_TIMEOUT_HELPER=0
fi
NO_HELPER_REASON="no timeout(1) or gtimeout(1) helper on this host"

skip_checks() { # skip_checks <label>...
    local label
    for label in "$@"; do
        echo "  skip $label ($NO_HELPER_REASON)"
        SKIPPED=$((SKIPPED + 1))
    done
}

echo "benchmark rtk version probe:"

if cmp -s "$L1_DIR/prelude.sh" "$L1_DIR/prelude-no-helper.sh"; then
    echo "  FAIL could not derive the no-timeout-helper prelude (sed anchor drifted)"
    FAILED=1
fi

# 1. Bare command name on PATH, nothing named rtk in the working directory —
#    the case the Rust discovery still resolves and runs.
make_stub "$BIN_DIR/rtk" 'echo "rtk 0.1-test"'
check "bare command name resolved through PATH" \
    "$(probe RTK_BIN=rtk "PATH=$BIN_DIR:$PATH")" "rtk 0.1-test"

# 7a. Same binary, no timeout(1) helper available.
CURRENT_RUNNER="$L1_DIR/prelude-no-helper.sh"
check "PATH resolution without a timeout(1) helper" \
    "$(probe RTK_BIN=rtk "PATH=$BIN_DIR:$PATH")" "rtk 0.1-test"
CURRENT_RUNNER="$L1_DIR/prelude.sh"

# 2. Explicit absolute path.
make_stub "$BIN_DIR/rtk-abs" 'echo "rtk 0.2-abs"'
check "absolute RTK_BIN path" \
    "$(probe "RTK_BIN=$BIN_DIR/rtk-abs")" "rtk 0.2-abs"

# 3a. Default vendored location absent.
check "missing vendored rtk" "$(probe)" "unavailable"

# 3b. Present but not executable (bash's `command -v` also reports
#     non-executable PATH hits, so the resolved path must be re-checked).
mkdir -p "$WORK/third_party/rtk/target/release"
printf '#!/usr/bin/env bash\necho "rtk 0.3-noexec"\n' \
    > "$WORK/third_party/rtk/target/release/rtk"
chmod 0644 "$WORK/third_party/rtk/target/release/rtk"
check "non-executable vendored rtk" "$(probe)" "unavailable"
check "bare command name absent from PATH" \
    "$(probe RTK_BIN=rtk-not-installed "PATH=$BIN_DIR:$PATH")" "unavailable"

# 3c. Executable but silent on stdout.
make_stub "$BIN_DIR/rtk-silent" 'exit 0'
check "empty --version output" "$(probe "RTK_BIN=$BIN_DIR/rtk-silent")" "unavailable"

if [ "$HAVE_TIMEOUT_HELPER" -eq 1 ]; then
    # 4a. Hanging binary that dies on SIGTERM: bounded by the deadline, reaped.
    HANG_PID_FILE="$WORK/hang.pid"
    make_stub "$BIN_DIR/rtk-hang" 'echo $$ > "$RTK_HANG_PID_FILE"; exec sleep 30'
    START=$(date +%s)
    check "hanging rtk bounded by RTK_VERSION_TIMEOUT_SECS" \
        "$(probe "RTK_BIN=$BIN_DIR/rtk-hang" RTK_VERSION_TIMEOUT_SECS=1 "RTK_HANG_PID_FILE=$HANG_PID_FILE")" \
        "unavailable"
    check_elapsed "deadline honoured (SIGTERM-responsive rtk)" \
        "$(( $(date +%s) - START ))" 1 5
    check_reaped "timed-out probe child was reaped" "$HANG_PID_FILE"

    # 4b. Binary that IGNORES SIGTERM: the deadline still has to hold, which needs
    #     the helper's unignorable follow-up signal (--kill-after / -s KILL).
    IGN_PID_FILE="$WORK/ignore-term.pid"
    make_stub "$BIN_DIR/rtk-ignore-term" \
        'echo $$ > "$RTK_IGNORE_PID_FILE"; trap "" TERM; exec sleep 30'
    START=$(date +%s)
    check "SIGTERM-ignoring rtk still bounded" \
        "$(probe "RTK_BIN=$BIN_DIR/rtk-ignore-term" RTK_VERSION_TIMEOUT_SECS=1 "RTK_IGNORE_PID_FILE=$IGN_PID_FILE")" \
        "unavailable"
    check_elapsed "deadline honoured (SIGTERM-ignoring rtk)" \
        "$(( $(date +%s) - START ))" 1 5
    check_reaped "SIGTERM-ignoring probe child was force-killed and reaped" "$IGN_PID_FILE"

    # 5a. Output printed BEFORE the deadline must be discarded, not recorded: an
    #     abandoned probe cannot vouch for the version it started printing.
    PART_PID_FILE="$WORK/partial.pid"
    make_stub "$BIN_DIR/rtk-partial" \
        'echo $$ > "$RTK_PARTIAL_PID_FILE"; echo "rtk partial"; exec sleep 30'
    check "partial output dropped when the probe times out" \
        "$(probe "RTK_BIN=$BIN_DIR/rtk-partial" RTK_VERSION_TIMEOUT_SECS=1 "RTK_PARTIAL_PID_FILE=$PART_PID_FILE")" \
        "unavailable"
    check_reaped "partially-printing probe child was reaped" "$PART_PID_FILE"
else
    # Unbounded probe: there is no deadline to honour, no abandonment to
    # discard partial output for, and no child to reap early. The stubs
    # would each sit out their full `sleep 30` for nothing.
    skip_checks \
        "hanging rtk bounded by RTK_VERSION_TIMEOUT_SECS" \
        "deadline honoured (SIGTERM-responsive rtk)" \
        "timed-out probe child was reaped" \
        "SIGTERM-ignoring rtk still bounded" \
        "deadline honoured (SIGTERM-ignoring rtk)" \
        "SIGTERM-ignoring probe child was force-killed and reaped" \
        "partial output dropped when the probe times out" \
        "partially-printing probe child was reaped"
fi

# 6. Multi-line output from a successful probe: first line only.
make_stub "$BIN_DIR/rtk-multi" 'echo "rtk 0.4-multi"; echo "build abc123"'
check "only the first --version line recorded" \
    "$(probe "RTK_BIN=$BIN_DIR/rtk-multi")" "rtk 0.4-multi"

# 5b. Non-zero exit is a failed probe, so its stdout is not a trustworthy
#     version — recorded as "unavailable" (with and without a helper).
make_stub "$BIN_DIR/rtk-nonzero" 'echo "rtk 0.5-nonzero"; exit 3'
check "non-zero --version exit degrades to unavailable" \
    "$(probe "RTK_BIN=$BIN_DIR/rtk-nonzero")" "unavailable"
CURRENT_RUNNER="$L1_DIR/prelude-no-helper.sh"
check "non-zero exit degrades without a timeout(1) helper" \
    "$(probe "RTK_BIN=$BIN_DIR/rtk-nonzero")" "unavailable"
CURRENT_RUNNER="$L1_DIR/prelude.sh"
if command -v python3 > /dev/null 2>&1; then
    python3 -m json.tool "$IDENTITY_FILE" > /dev/null
    echo "  ok   benchmark_identity.json is valid JSON"
fi

# 8. Self-check of pid_is_gone, the tolerance every reaping check above relies
#    on: hold a dead child without collecting its status — what a PID 1 with no
#    reaper does to the child timeout(1) SIGKILLs — and require that the helper
#    reports it gone even though `kill -0` still says "present". python3 is the
#    portable way to get a parent that never wait()s; where it is missing this
#    reports a skip rather than a failure.
if command -v python3 > /dev/null 2>&1; then
    ZOMBIE_PID_FILE="$WORK/zombie.pid"
    python3 - "$ZOMBIE_PID_FILE" <<'PY' &
import os, sys, time
pid = os.fork()
if pid == 0:
    os._exit(7)
with open(sys.argv[1], "w") as fh:
    fh.write(str(pid))
time.sleep(30)  # never reaps, so the child above stays a zombie
PY
    ZOMBIE_HOLDER=$!
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if [ -s "$ZOMBIE_PID_FILE" ]; then break; fi
        sleep 0.1
    done
    ZOMBIE_PID="$(cat "$ZOMBIE_PID_FILE" 2>/dev/null || true)"
    if [ -n "$ZOMBIE_PID" ] && kill -0 "$ZOMBIE_PID" 2>/dev/null; then
        check_true "unreaped (zombie) child counts as gone" \
            "$(pid_is_gone "$ZOMBIE_PID"; echo $?)"
    else
        echo "  ok   zombie self-check skipped (host collected the child first)"
    fi
    kill "$ZOMBIE_HOLDER" 2>/dev/null || true
    wait "$ZOMBIE_HOLDER" 2>/dev/null || true
fi

if [ "$FAILED" -ne 0 ]; then
    echo "benchmark rtk version probe test FAILED"
    exit 1
fi
if [ "$SKIPPED" -gt 0 ]; then
    echo "benchmark rtk version probe test passed ($SKIPPED checks skipped: $NO_HELPER_REASON)"
else
    echo "benchmark rtk version probe test passed"
fi
