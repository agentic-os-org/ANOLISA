#!/usr/bin/env bash
# Check installer flag negotiation (capability consent and the legacy
# unsafe-install bypass) without installing a real plugin.
#
# Guards pinned here (shared with the agent-memory installer suite, whose
# stub-CLI harness has the same shape):
#   - probe form: `plugins install --help` is captured into a variable and
#     matched whole-token, never piped into `grep -q`. The `big` scenario
#     writes past any pipe buffer, so a piped probe SIGPIPEs the writer and
#     loses the match under `pipefail`.
#   - whole-token matching across alternate help layouts (`colon`, `paren`)
#     as well as near-miss options (`near_match`, `unsafe_near`).
#   - hermetic scenarios: ambient installer switches are unset per run, and
#     the suite self-invokes under hostile values at the end.
#   - diagnosability: counts come from awk (always exit 0) and every
#     assertion ends in `fail`, so a regression prints a FAIL line instead of
#     dying silently inside a failing assignment pipeline.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/../adapters/tokenless/openclaw/scripts/install.sh"
SANDBOX="$(mktemp -d -t tokenless-openclaw-install.XXXXXX)"
trap 'rm -r -- "$SANDBOX"' EXIT
# The space in "adapter root" pins argv quoting through ADAPTER_DIR.
mkdir -p "$SANDBOX/adapter root/openclaw/dist"
: > "$SANDBOX/adapter root/openclaw/dist/index.js"

cat > "$SANDBOX/openclaw" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[ -z "${OPENCLAW_HOME+x}" ]
[ "$OPENCLAW_STATE_DIR" = "$TEST_STATE_DIR" ]
printf '%s\n' "$*" >> "$TEST_ARGV_LOG"
if [ "$*" = "plugins install --help" ]; then
    echo "--force"
    case "${TEST_SUPPORT:?}" in
        modern|modern_unsafe) echo "--accept-capabilities" ;;
        near_match) echo "--accept-capabilities-only" ;;
        # Alternate layouts exercise the whole-token regex boundaries: a
        # prefix-anchored regex misses these, a substring regex over-matches
        # the near_match option above.
        colon) echo "--accept-capabilities: accept declared capabilities" ;;
        paren) echo "Options (--accept-capabilities):" ;;
        big)
            echo "--accept-capabilities"
            # Exceeds any pipe buffer: if the probe is ever reverted from
            # capture-into-variable to `cmd | grep -q`, grep's early exit
            # SIGPIPEs this writer under pipefail and the match is lost.
            # Same ~2.6 MB help volume as the agent-memory suite's `big`
            # scenario, laid out as fewer/longer lines: tokenless's install.sh
            # regex-scans every help line for the unsafe-install token (the
            # agent-memory installer matches the whole buffer once), so here
            # line count — not byte count — is what this scenario costs.
            big_line="filler$(printf '%.0s-' {1..512})"
            for _ in {1..5000}; do printf '%s\n' "$big_line"; done ;;
        failed) echo "cannot inspect --accept-capabilities" >&2; exit 3 ;;
    esac
    case "$TEST_SUPPORT" in
        unsafe_effective|modern_unsafe)
            echo "--dangerously-force-unsafe-install  Bypass the install safety scan" ;;
        unsafe_noop|noop_rejected)
            echo "--dangerously-force-unsafe-install  Deprecated no-op; governed by security.installPolicy" ;;
        unsafe_noop_upper)
            echo "--dangerously-force-unsafe-install  Deprecated NO-OP; governed by security.installPolicy" ;;
        unsafe_near) echo "--dangerously-force-unsafe-install-only" ;;
    esac
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
case "$TEST_SUPPORT" in
    modern|modern_unsafe|colon|paren|big)
        [ "$accepted" = 1 ] || { echo 'Plugin requires capability consent' >&2; exit 1; }
        ;;
    *)
        [ "$accepted" = 0 ] || { echo 'unknown option --accept-capabilities' >&2; exit 2; }
        ;;
esac
case "$TEST_SUPPORT" in
    unsafe_effective|modern_unsafe)
        [ "$unsafe" = 1 ] || { echo 'install safety scan blocks child_process plugins' >&2; exit 4; }
        ;;
    *)
        [ "$unsafe" = 0 ] || { echo 'unknown option --dangerously-force-unsafe-install' >&2; exit 5; }
        ;;
esac
[ "$TEST_SUPPORT" != noop_rejected ] || { echo 'install blocked by security.installPolicy' >&2; exit 6; }
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

fail() {
    echo "FAIL: $1" >&2
    sed 's/^/    /' "$SANDBOX/output" >&2
    exit 1
}

# run_installer [KEY=VAL ...] — invoke install.sh against a cleared argv log
# and no installed marker. The installer switches are unset first so ambient
# values in the caller's environment cannot falsify a baseline scenario;
# per-scenario KEY=VAL overrides are applied after the unsets.
run_installer() {
    : > "$TEST_ARGV_LOG"
    rm -f "$TEST_INSTALLED"
    env -u ANOLISA_DRY_RUN -u ANOLISA_TARGET -u ANOLISA_COMPONENT \
        "$@" bash "$INSTALL_SH" >"$SANDBOX/output" 2>&1
}

# Full expected install argv for the given flag combination:
# argv <accept-capabilities:yes|no> <unsafe-install:yes|no>
argv() {
    local a="plugins install $ANOLISA_ADAPTER_DIR/openclaw --force"
    if [ "$1" = yes ]; then a="$a --accept-capabilities"; fi
    if [ "$2" = yes ]; then a="$a --dangerously-force-unsafe-install"; fi
    printf '%s' "$a"
}

# Counters use awk (always exit 0) rather than `grep -c` or `grep -v | wc -l`:
# a zero-count grep or a failing pipeline in an assignment kills the whole
# suite under `set -euo pipefail` before the fail() below can print anything.
count_probes() { awk '/^plugins install --help$/ {n++} END {print n+0}' "$TEST_ARGV_LOG"; }
count_installs() { awk '/^plugins install / && !/--help$/ {n++} END {print n+0}' "$TEST_ARGV_LOG"; }

expect_one_probe() {
    local label="$1" probes
    probes="$(count_probes)"
    [ "$probes" = 1 ] || fail "$label: expected exactly one help probe, got $probes"
}

check_argv_log() {
    local label="$1" want_argv="$2" installs install_argv
    expect_one_probe "$label"
    installs="$(count_installs)"
    [ "$installs" = 1 ] || fail "$label: expected exactly one install invocation, got $installs"
    install_argv="$(awk '/^plugins install / && !/--help$/ {print}' "$TEST_ARGV_LOG")"
    [ "$install_argv" = "$want_argv" ] || fail "$label: install argv '$install_argv' != '$want_argv'"
}

expect_no_install() {
    local label="$1" installs
    installs="$(count_installs)"
    [ "$installs" = 0 ] || fail "$label: install ran despite failure ($installs invocation(s))"
}

expect_log() { grep -q -- "$2" "$SANDBOX/output" || fail "$1: expected log line: $2"; }
expect_no_log() { ! grep -q -- "$2" "$SANDBOX/output" || fail "$1: unexpected log line: $2"; }

for TEST_SUPPORT in modern legacy near_match colon paren big \
                    unsafe_effective unsafe_noop unsafe_noop_upper unsafe_near \
                    modern_unsafe; do
    export TEST_SUPPORT
    want_caps=no
    want_unsafe=no
    case "$TEST_SUPPORT" in
        modern|colon|paren|big) want_caps=yes ;;
        modern_unsafe) want_caps=yes; want_unsafe=yes ;;
        unsafe_effective) want_unsafe=yes ;;
    esac
    rc=0
    run_installer || rc=$?
    [ "$rc" = 0 ] || fail "$TEST_SUPPORT installer: rc=$rc, want 0"
    [ -f "$TEST_INSTALLED" ] || fail "$TEST_SUPPORT installer: install did not run"
    check_argv_log "$TEST_SUPPORT installer" "$(argv "$want_caps" "$want_unsafe")"
    case "$TEST_SUPPORT" in
        unsafe_effective|modern_unsafe)
            expect_log "$TEST_SUPPORT installer" '--dangerously-force-unsafe-install is required' ;;
        *)
            expect_no_log "$TEST_SUPPORT installer" '--dangerously-force-unsafe-install is required' ;;
    esac
    echo "PASS: $TEST_SUPPORT installer"
done

export TEST_SUPPORT=noop_rejected
rc=0
run_installer || rc=$?
[ "$rc" = 1 ] || fail "noop_rejected: policy-rejected install reported rc=$rc, want 1"
[ ! -f "$TEST_INSTALLED" ] || fail 'noop_rejected: install completed despite the policy refusal'
# The install IS attempted with base flags; the host then refuses it because
# the safety scan moved to security.installPolicy.
check_argv_log 'noop_rejected' "$(argv no no)"
expect_log 'noop_rejected' 'security.installPolicy'
expect_log 'noop_rejected' 'deprecated no-op'
echo 'PASS: noop host failure points at security.installPolicy'

export TEST_SUPPORT=failed
rc=0
run_installer || rc=$?
[ "$rc" = 1 ] || fail "failed probe: rc=$rc, want 1"
[ ! -f "$TEST_INSTALLED" ] || fail 'failed probe: install ran despite failure'
expect_one_probe 'failed probe'
expect_no_install 'failed probe'
expect_log 'failed probe' 'Cannot inspect OpenClaw installer options'
echo 'PASS: failed help probe stops installation'

# Dry-run must describe both gates without touching the CLI at all.
export TEST_SUPPORT=modern
rc=0
run_installer ANOLISA_DRY_RUN=1 || rc=$?
[ "$rc" = 0 ] || fail "dry-run: rc=$rc, want 0"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'dry-run: openclaw must not be invoked'
[ ! -f "$TEST_INSTALLED" ] || fail 'dry-run: install ran'
expect_log 'dry-run' '--accept-capabilities'
expect_log 'dry-run' '--dangerously-force-unsafe-install'
echo 'PASS: dry-run describes consent and bypass gating without invoking OpenClaw'

rc=0
run_installer OPENCLAW_BIN="$SANDBOX/missing-openclaw" || rc=$?
[ "$rc" = 0 ] || fail "missing CLI: rc=$rc, want 0"
[ ! -s "$TEST_ARGV_LOG" ] || fail 'missing CLI: openclaw must not be invoked'
[ ! -f "$TEST_INSTALLED" ] || fail 'missing CLI: install ran'
expect_log 'missing CLI' 'skipping plugin installation'
echo 'PASS: missing CLI preserves the existing skip behavior'

# Diagnosability pin: a regressed install.sh that probes but never invokes
# install must produce a FAIL diagnostic — not a silent set -e death from a
# failing pipeline inside an assignment. The check runs in a child bash -c:
# an OR-list subshell would suppress errexit inside it and hide exactly the
# silent death this pin guards against.
fake_install="$SANDBOX/fake-install.sh"
cat > "$fake_install" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
echo "[tokenless] Installing openclaw plugin..."
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins install --help >/dev/null 2>&1
# Regressed: exits successfully without invoking the install.
exit 0
FAKE
pin_body="set -euo pipefail
$(declare -f fail run_installer count_probes count_installs expect_one_probe check_argv_log)
run_installer
check_argv_log 'harness pin: install never invoked' 'plugins install never-ran'"
pin_rc=0
env SANDBOX="$SANDBOX" INSTALL_SH="$fake_install" bash -c "$pin_body" \
    >"$SANDBOX/pin-output" 2>&1 || pin_rc=$?
[ "$pin_rc" -ne 0 ] || fail 'harness pin: check should have failed'
grep -q 'expected exactly one install invocation' "$SANDBOX/pin-output" \
    || fail 'harness pin: no diagnosable FAIL line (silent death)'
echo 'PASS: missing install invocation fails with a diagnostic'

# The suite must be hermetic: hostile ambient installer switches must neither
# leak into scenarios nor break the suite itself.
if [ -z "${TEST_HOSTILE_AMBIENT:-}" ]; then
    rc=0
    env ANOLISA_DRY_RUN=1 ANOLISA_TARGET=hostile ANOLISA_COMPONENT=hostile \
        TEST_HOSTILE_AMBIENT=1 bash "$0" >"$SANDBOX/output" 2>&1 || rc=$?
    [ "$rc" = 0 ] || fail "hostile ambient environment broke the suite (rc=$rc)"
    echo 'PASS: hostile ambient environment does not leak into scenarios'
fi
