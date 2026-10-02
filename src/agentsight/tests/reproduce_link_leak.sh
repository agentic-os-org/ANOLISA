#!/bin/bash
# reproduce_link_leak.sh — Reproduce SslSniff _links Vec<Link> fd leak
#
# Root cause: SslSniff._links only grows (extend), never shrinks. When a
# process exits, detach_process() removes its inodes from traced_files
# (allowing re-attach), but the old Link objects stay in _links. When a
# new process loads the same SSL library, the same uprobe is attached
# again, creating duplicate Link objects and leaking fds.
#
# This script proves the leak by:
#   1. Starting agentsight trace
#   2. Recording the initial fd count of the agentsight process
#   3. Spawning an agent process (python3 with ssl) that gets attach
#   4. Letting it exit (triggers detach_process, removes inodes from traced_files)
#   5. Spawning another agent process with the same SSL library
#   6. Checking that fd count increased (= duplicate Links created)
#
# Usage: sudo bash tests/reproduce_link_leak.sh
# Requires: root, python3 with ssl module

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

cleanup() {
    echo -e "\n${YELLOW}[cleanup]${NC} stopping agentsight..."
    kill "$AGENTSIGHT_PID" 2>/dev/null || true
    wait "$AGENTSIGHT_PID" 2>/dev/null || true
    echo -e "${GREEN}[cleanup]${NC} done"
}

count_fds() {
    ls /proc/"$1"/fd 2>/dev/null | wc -l
}

count_uprobe_fds() {
    # Count fds that are perf_event (uprobe) links
    local pid=$1
    local count=0
    for fd in /proc/"$pid"/fdinfo/*; do
        if grep -q "perf_event" "$fd" 2>/dev/null; then
            count=$((count + 1))
        fi
    done
    echo "$count"
}

echo "=== SslSniff _links Vec<Link> fd Leak Reproducer ==="
echo ""

# --- Step 1: Start agentsight trace ---
echo -e "${YELLOW}[step 1]${NC} starting agentsight trace..."
agentsight trace &>/dev/null &
AGENTSIGHT_PID=$!
trap cleanup EXIT
sleep 2

if ! kill -0 "$AGENTSIGHT_PID" 2>/dev/null; then
    echo -e "${RED}[error]${NC} agentsight failed to start"
    exit 1
fi
echo -e "${GREEN}[step 1]${NC} agentsight running (pid=$AGENTSIGHT_PID)"

# --- Step 2: Wait for startup to stabilize, then record baseline ---
echo ""
echo -e "${YELLOW}[step 2]${NC} waiting 10s for startup scanning to stabilize..."
sleep 10
INITIAL_FDS=$(count_fds "$AGENTSIGHT_PID")
echo -e "${GREEN}[step 2]${NC} baseline fd count: $INITIAL_FDS"

# --- Step 3-6: Cycle claude processes and watch fd growth ---
# Config has: {"rule": ["claude*"], "agent_name": "Claude"}
# We create a script whose argv[0] matches "claude*" by using exec -a
NUM_CYCLES=5

echo ""
echo -e "${YELLOW}[step 3]${NC} running $NUM_CYCLES attach/detach cycles..."
echo "  Each cycle: spawn a process matching agent rule, loads libssl,"
echo "  agentsight attaches uprobe, process exits, repeat."

FD_COUNTS=("$INITIAL_FDS")

for i in $(seq 1 $NUM_CYCLES); do
    # Use exec -a to set argv[0] to "claude-fake" (matches "claude*" rule)
    # Then run python3 which loads libssl.so
    bash -c 'exec -a claude-fake python3 -c "import ssl; import time; time.sleep(3)"' &
    AGENT_PID=$!
    sleep 2  # Give agentsight time to detect via procmon and attach

    wait "$AGENT_PID" 2>/dev/null || true
    sleep 2  # Give agentsight time to process exit + detach

    FDS=$(count_fds "$AGENTSIGHT_PID")
    FD_COUNTS+=("$FDS")
    echo "  cycle $i: fd count = $FDS (delta from baseline: +$((FDS - INITIAL_FDS)))"
done

# --- Step 7: Analyze ---
FINAL_FDS=${FD_COUNTS[-1]}
FD_GROWTH=$((FINAL_FDS - INITIAL_FDS))

echo ""
echo "=== Results ==="
echo "  Initial fds:  $INITIAL_FDS"
echo "  Final fds:    $FINAL_FDS"
echo "  Growth:       +$FD_GROWTH over $NUM_CYCLES cycles"
echo "  Fd counts:    ${FD_COUNTS[*]}"
echo ""

# The first cycle may grow fds (initial attach of libssl uprobe).
# A leak means cycles 2+ KEEP growing beyond cycle 1's level.
CYCLE1_FDS=${FD_COUNTS[1]}
GROWTH_AFTER_FIRST=0
for i in $(seq 2 $NUM_CYCLES); do
    delta=$((${FD_COUNTS[$i]} - CYCLE1_FDS))
    if [ "$delta" -gt "$GROWTH_AFTER_FIRST" ]; then
        GROWTH_AFTER_FIRST=$delta
    fi
done

echo "  Cycle 1 fds:        $CYCLE1_FDS (first attach, +$((CYCLE1_FDS - INITIAL_FDS)) is expected)"
echo "  Growth after cycle1: +$GROWTH_AFTER_FIRST"
echo ""

if [ "$GROWTH_AFTER_FIRST" -gt 5 ]; then
    echo -e "${RED}[BUG CONFIRMED]${NC} fd count grew by $GROWTH_AFTER_FIRST AFTER the first attach!"
    echo "  _links Vec<Link> is only appended to, never cleaned up."
    echo "  Each cycle re-attaches the same SSL library, creating duplicate uprobe Links."
    exit 1
elif [ "$GROWTH_AFTER_FIRST" -gt 0 ]; then
    echo -e "${YELLOW}[POSSIBLE LEAK]${NC} fd count grew by $GROWTH_AFTER_FIRST after first cycle"
    exit 1
else
    echo -e "${GREEN}[NO LEAK]${NC} fd count stable after first attach across $NUM_CYCLES cycles"
    exit 0
fi
