#!/usr/bin/env bash
# ws-ckpt end-to-end integration test
#
# Requires: root, btrfs-progs, rsync
# Usage:    sudo bash tests/integration/e2e.sh
#
# Creates a temporary loop-btrfs filesystem, starts the daemon, exercises every
# CLI subcommand, and tears everything down.  Exit code 0 = all passed.

set -euo pipefail

# ── paths ─────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CARGO_ROOT="$PROJECT_ROOT/src"

TMPBASE="$(mktemp -d /tmp/ws-ckpt-e2e.XXXXXX)"
IMG="$TMPBASE/btrfs.img"
MNT="$TMPBASE/mnt"
SOCKET="$TMPBASE/ws-ckpt.sock"
WORKSPACE="$TMPBASE/workspace"
DAEMON_PID=""
LOOP_DEV=""
STATE_DIR="/var/lib/ws-ckpt"
LOCKFILE="/run/ws-ckpt/ws-ckpt.lock"
STATE_DIR_BACKUP=""

PASS=0
FAIL=0

# ── helpers ───────────────────────────────────────────────────────────────────

cleanup() {
    echo ""
    echo "=== Cleanup ==="
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null && wait "$DAEMON_PID" 2>/dev/null || true
    umount "$MNT" 2>/dev/null || true
    [ -n "$LOOP_DEV" ] && losetup -d "$LOOP_DEV" 2>/dev/null || true
    # Clean test state from hardcoded state_dir
    rm -rf "$STATE_DIR"
    # Restore backed-up state_dir if it existed before test
    if [ -n "$STATE_DIR_BACKUP" ] && [ -d "$STATE_DIR_BACKUP" ]; then
        mv "$STATE_DIR_BACKUP" "$STATE_DIR"
        echo "Restored pre-existing $STATE_DIR"
    fi
    rm -rf "$TMPBASE"
    echo "Cleaned up $TMPBASE"
}

# Back up existing state_dir before cleanup can run, so early failures cannot
# remove real ws-ckpt state.
if [ -d "$STATE_DIR" ]; then
    STATE_DIR_BACKUP="$(mktemp -d /tmp/ws-ckpt-state-backup.XXXXXX)"
    mv "$STATE_DIR" "$STATE_DIR_BACKUP/state"
    STATE_DIR_BACKUP="$STATE_DIR_BACKUP/state"
    echo "Backed up existing $STATE_DIR to $STATE_DIR_BACKUP"
fi
trap cleanup EXIT

assert_ok() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (exit $?)"
        FAIL=$((FAIL + 1))
    fi
}

assert_fail() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "  FAIL  $desc (expected failure, got success)"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    fi
}

assert_output_contains() {
    local desc="$1"; shift
    local pattern="$1"; shift
    local output
    output=$("$@" 2>&1) || true
    if echo "$output" | grep -qi "$pattern"; then
        echo "  PASS  $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL  $desc (output missing '$pattern')"
        FAIL=$((FAIL + 1))
    fi
}

# ── build ─────────────────────────────────────────────────────────────────────

# WS_CKPT_E2E_BIN lets local reruns skip the cargo build by pointing at an
# already-built binary (it must contain the changes under test).
if [ -n "${WS_CKPT_E2E_BIN:-}" ]; then
    BIN="${WS_CKPT_E2E_BIN%/}"
    echo "=== Using prebuilt binary: $BIN ==="
else
    echo "=== Building ws-ckpt ==="
    cd "$CARGO_ROOT"
    cargo build --release --workspace
    BIN="$CARGO_ROOT/target/release/ws-ckpt"
fi
[ -x "$BIN" ] || { echo "FATAL: binary not found at $BIN"; exit 1; }

# ── setup btrfs loop ──────────────────────────────────────────────────────────

echo "=== Setting up btrfs loop device ==="
dd if=/dev/zero of="$IMG" bs=1M count=256 status=none
LOOP_DEV=$(losetup --find --show "$IMG")
mkfs.btrfs -f "$LOOP_DEV" >/dev/null 2>&1
mkdir -p "$MNT" "$WORKSPACE"
mount "$LOOP_DEV" "$MNT"
echo "Loop device $LOOP_DEV mounted on $MNT"

# ── guard against running daemon ───────────────────────────────────────────────

if [ -S "/run/ws-ckpt/ws-ckpt.sock" ]; then
    echo "FATAL: a ws-ckpt daemon is already running (socket exists at /run/ws-ckpt/ws-ckpt.sock)."
    echo "Stop it first: systemctl stop ws-ckpt"
    exit 1
fi

# ── start daemon ──────────────────────────────────────────────────────────────

echo "=== Starting daemon ==="
"$BIN" daemon --mount-path "$MNT" --socket "$SOCKET" >"$TMPBASE/daemon.log" 2>&1 &
DAEMON_PID=$!
# Wait for socket
for i in $(seq 1 30); do
    [ -S "$SOCKET" ] && break
    sleep 0.2
done
[ -S "$SOCKET" ] || { echo "FATAL: daemon socket not ready after 6s"; exit 1; }
echo "Daemon started (PID $DAEMON_PID)"

# ── set WS_CKPT env for CLI ──────────────────────────────────────────────────

export WS_CKPT_SOCKET="$SOCKET"

# ── populate workspace ────────────────────────────────────────────────────────

echo "hello world" > "$WORKSPACE/file1.txt"
mkdir -p "$WORKSPACE/subdir"
echo "nested" > "$WORKSPACE/subdir/file2.txt"

# ── tests ─────────────────────────────────────────────────────────────────────

echo ""
echo "=== CLI Tests ==="

# status
assert_ok "status" "$BIN" status

# init
assert_ok "init workspace" "$BIN" init -w "$WORKSPACE"

# checkpoint
assert_ok "create checkpoint snap1" "$BIN" checkpoint -w "$WORKSPACE" -i snap1 -m "first snapshot"

# list
assert_ok "list snapshots" "$BIN" list -w "$WORKSPACE"
assert_output_contains "list shows snap1" "snap1" "$BIN" list -w "$WORKSPACE"

# modify workspace and create second checkpoint
echo "modified" >> "$WORKSPACE/file1.txt"
assert_ok "create checkpoint snap2" "$BIN" checkpoint -w "$WORKSPACE" -i snap2 -m "after modification"

# diff
assert_ok "diff snap1 snap2" "$BIN" diff -w "$WORKSPACE" --from snap1 --to snap2

# rollback
assert_ok "rollback to snap1" "$BIN" rollback -w "$WORKSPACE" -s snap1
CONTENT=$(cat "$WORKSPACE/file1.txt")
if [ "$CONTENT" = "hello world" ]; then
    echo "  PASS  rollback restored file content"
    PASS=$((PASS + 1))
else
    echo "  FAIL  rollback content mismatch: got '$CONTENT'"
    FAIL=$((FAIL + 1))
fi

# delete
assert_ok "delete snap2" "$BIN" delete -s snap2 -w "$WORKSPACE" --force

# list after delete
assert_output_contains "list after delete shows snap1" "snap1" "$BIN" list -w "$WORKSPACE"

# config view
assert_ok "config view" "$BIN" config -w "$WORKSPACE"

# cleanup
assert_ok "cleanup" "$BIN" cleanup -w "$WORKSPACE"

# error paths
assert_fail "init nonexistent" "$BIN" init -w /nonexistent/path/should/fail
assert_fail "checkpoint without init" "$BIN" checkpoint -w /tmp -i bad

# ── detached registration (issue #3059 regression) ────────────────────────────
# The workspace symlink is externally deleted and the path recreated as a
# plain directory. Every path-addressed operation must fail loudly (pointing
# at recover) instead of snapshotting the stale subvolume, and the user's
# replacement directory must stay untouched.

DETACHED="$TMPBASE/detached-ws"
mkdir -p "$DETACHED"
echo "seed" > "$DETACHED/seed.txt"
assert_ok "detached: init workspace" "$BIN" init -w "$DETACHED"
assert_ok "detached: pre-detach checkpoint" "$BIN" checkpoint -w "$DETACHED" -i pre-detach

# External removal of the symlink + plain-directory replacement.
rm -rf "$DETACHED"
mkdir -p "$DETACHED"
echo "user-data" > "$DETACHED/f.txt"

assert_fail "detached: checkpoint refuses" "$BIN" checkpoint -w "$DETACHED" -i post-detach
assert_output_contains "detached: checkpoint error points at recover" "ws-ckpt recover" \
    "$BIN" checkpoint -w "$DETACHED" -i post-detach
assert_fail "detached: rollback refuses" "$BIN" rollback -w "$DETACHED" -s pre-detach
assert_fail "detached: list refuses" "$BIN" list -w "$DETACHED"

DETACHED_CONTENT=$(cat "$DETACHED/f.txt")
if [ "$DETACHED_CONTENT" = "user-data" ]; then
    echo "  PASS  detached: refused ops leave user directory untouched"
    PASS=$((PASS + 1))
else
    echo "  FAIL  detached: user directory changed: '$DETACHED_CONTENT'"
    FAIL=$((FAIL + 1))
fi

# ── recover -w on detached registration (issue #3059 review follow-up) ────────
# The CLI gathers confirm metadata from status before sending Recover; the
# single-workspace status form now refuses detached registrations, so using
# it here would abort the flow and make the suggested remediation
# ("ws-ckpt recover -w <path>") impossible to run. Recover must still repair
# a registration whose symlink was deleted without replacement.

MISSING_WS="$TMPBASE/missing-ws"
mkdir -p "$MISSING_WS"
echo "seed" > "$MISSING_WS/seed.txt"
assert_ok "recover -w: init workspace" "$BIN" init -w "$MISSING_WS"
rm "$MISSING_WS"  # symlink deleted externally, no replacement directory
assert_ok "recover -w repairs deleted-symlink registration" "$BIN" recover -w "$MISSING_WS" --force
MISSING_CONTENT=$(cat "$MISSING_WS/seed.txt" 2>/dev/null || true)
if [ "$MISSING_CONTENT" = "seed" ] && [ ! -L "$MISSING_WS" ]; then
    echo "  PASS  recover -w restored subvolume contents at the path"
    PASS=$((PASS + 1))
else
    echo "  FAIL  recover -w content mismatch: '$MISSING_CONTENT'"
    FAIL=$((FAIL + 1))
fi

# ── recover -w by workspace ID shows the real snapshot count ──────────────────
# The confirm prompt's snapshot count is filtered from the global status
# report; the filter must match the ws_id form too, not only the path form.
# A count of 0 would understate what the user is about to confirm deleting.

BYID_WS="$TMPBASE/byid-ws"
mkdir -p "$BYID_WS"
echo "seed" > "$BYID_WS/seed.txt"
assert_ok "recover by id: init workspace" "$BIN" init -w "$BYID_WS"
assert_ok "recover by id: checkpoint one" "$BIN" checkpoint -w "$BYID_WS" -i byid-snap1
assert_ok "recover by id: checkpoint two" "$BIN" checkpoint -w "$BYID_WS" -i byid-snap2
BYID_WS_ID=$("$BIN" status | grep "$BYID_WS" | awk '{print $1}')
BYID_OUTPUT=$(echo y | "$BIN" recover -w "$BYID_WS_ID" 2>&1)
if echo "$BYID_OUTPUT" | grep -q "2 snapshots"; then
    echo "  PASS  recover by id: confirm prompt shows real snapshot count"
    PASS=$((PASS + 1))
else
    echo "  FAIL  recover by id: prompt snapshot count wrong in: $BYID_OUTPUT"
    FAIL=$((FAIL + 1))
fi
BYID_CONTENT=$(cat "$BYID_WS/seed.txt" 2>/dev/null || true)
if [ "$BYID_CONTENT" = "seed" ] && [ ! -L "$BYID_WS" ]; then
    echo "  PASS  recover by id: workspace restored to plain directory"
    PASS=$((PASS + 1))
else
    echo "  FAIL  recover by id: workspace not recovered: '$BYID_CONTENT'"
    FAIL=$((FAIL + 1))
fi

# ── recover --all exit code (issue #3059 follow-up, links #3069) ──────────────
# One healthy workspace ($WORKSPACE) plus one detached registration
# ($DETACHED): the batch must exit non-zero and report the failure instead
# of printing "All workspaces recovered." while the detached workspace was
# never actually repaired.

# NOTE: order matters — the output assertion must run first: each invocation
# recovers the healthy workspace, so a second run would see 1/1 instead of 1/2.
assert_output_contains "recover --all reports failed count" "Recover failed for 1/2" \
    "$BIN" recover --all --force
assert_fail "recover --all nonzero when a workspace fails" "$BIN" recover --all --force
# The healthy workspace was still recovered by the batch: plain directory again.
if [ -d "$WORKSPACE" ] && [ ! -L "$WORKSPACE" ]; then
    echo "  PASS  recover --all still recovers healthy workspaces"
    PASS=$((PASS + 1))
else
    echo "  FAIL  recover --all left healthy workspace as symlink"
    FAIL=$((FAIL + 1))
fi


# ── summary ───────────────────────────────────────────────────────────────────

echo ""
echo "=== Results ==="
echo "  Passed: $PASS"
echo "  Failed: $FAIL"
echo "  Total:  $((PASS + FAIL))"

if [ "$FAIL" -ne 0 ]; then
    echo ""
    echo "=== Daemon log (last 50 lines) ==="
    tail -50 "$TMPBASE/daemon.log" 2>/dev/null || true
    exit 1
fi
