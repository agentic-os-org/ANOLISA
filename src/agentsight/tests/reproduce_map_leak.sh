#!/bin/bash
# reproduce_map_leak.sh — Reproduce traced_processes BPF Map leak
#
# Root cause: trace_process_exit() gates cleanup of traced_processes on a
# child_pids lookup. User-space add_traced_pid() only writes traced_processes,
# so those entries are never cleaned up by the BPF exit handler.
#
# This script proves the leak by:
#   1. Starting agentsight trace
#   2. Directly inserting PIDs into traced_processes (simulating user-space add_traced_pid)
#      WITHOUT inserting into child_pids
#   3. Spawning and killing processes with those PIDs
#   4. Checking that traced_processes entries are NOT cleaned up (= leak confirmed)
#
# Usage: sudo bash tests/reproduce_map_leak.sh
# Requires: root, bpftool, agentsight binary

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

# Get BPF map ID by name (bpftool truncates names to 15 chars)
get_map_id() {
    local name="$1"
    bpftool map list 2>/dev/null | grep -E "hash.*name ${name}" | head -1 | awk -F: '{print $1}'
}

count_map_entries() {
    local map_id="$1"
    local count
    count=$(bpftool map dump id "$map_id" 2>/dev/null | grep -c '"key"') || count=0
    echo "$count"
}

echo "=== traced_processes Map Leak Reproducer ==="
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

# Resolve map IDs (bpftool truncates map names to 15 chars)
TRACED_MAP_ID=$(get_map_id "traced_processe")
CHILD_MAP_ID=$(get_map_id "child_pids")

if [ -z "$TRACED_MAP_ID" ] || [ -z "$CHILD_MAP_ID" ]; then
    echo -e "${RED}[error]${NC} could not find BPF maps (traced_processe=$TRACED_MAP_ID child_pids=$CHILD_MAP_ID)"
    exit 1
fi
echo -e "${GREEN}[step 1]${NC} map IDs: traced_processes=$TRACED_MAP_ID, child_pids=$CHILD_MAP_ID"

# --- Step 2: Record initial map state ---
INITIAL_TRACED=$(count_map_entries "$TRACED_MAP_ID")
INITIAL_CHILD=$(count_map_entries "$CHILD_MAP_ID")
echo ""
echo -e "${YELLOW}[step 2]${NC} initial map state:"
echo "  traced_processes: $INITIAL_TRACED entries"
echo "  child_pids:       $INITIAL_CHILD entries"

# --- Step 3: Simulate user-space add_traced_pid (write traced_processes only) ---
# We spawn N short-lived background processes, grab their PIDs, and insert
# each PID into traced_processes ONLY (not child_pids) via bpftool.
# Then we let the processes exit naturally.
# BPF trace_process_exit should clean up, but it won't because child_pids
# has no matching entry.

NUM_PROCS=50
echo ""
echo -e "${YELLOW}[step 3]${NC} spawning $NUM_PROCS processes and inserting into traced_processes only..."

# Snapshot the set of PIDs currently in the map (baseline, e.g. running agents)
BASELINE_PIDS=$(bpftool map dump id "$TRACED_MAP_ID" 2>/dev/null \
    | python3 -c "import json,sys; [print(e['key']) for e in json.load(sys.stdin)]" 2>/dev/null || true)

PIDS=()
for i in $(seq 1 $NUM_PROCS); do
    # Spawn a process that sleeps briefly
    sleep 3 &
    pid=$!
    PIDS+=("$pid")

    # Insert into traced_processes ONLY (simulating user-space add_traced_pid)
    key=$(printf '0x%02x 0x%02x 0x%02x 0x%02x' \
        $((pid & 0xFF)) $(((pid >> 8) & 0xFF)) $(((pid >> 16) & 0xFF)) $(((pid >> 24) & 0xFF)))
    bpftool map update id "$TRACED_MAP_ID" key $key value 0x01 0x00 0x00 0x00 any 2>/dev/null || true
done

MID_TRACED=$(count_map_entries "$TRACED_MAP_ID")
MID_CHILD=$(count_map_entries "$CHILD_MAP_ID")
echo -e "${GREEN}[step 3]${NC} after inserting $NUM_PROCS PIDs:"
echo "  traced_processes: $MID_TRACED entries (added ~$((MID_TRACED - INITIAL_TRACED)))"
echo "  child_pids:       $MID_CHILD entries"

# --- Step 4: Wait for all spawned processes to exit ---
echo ""
echo -e "${YELLOW}[step 4]${NC} waiting for all $NUM_PROCS processes to exit..."
for pid in "${PIDS[@]}"; do
    wait "$pid" 2>/dev/null || true
done
# Give BPF exit handler time to fire
sleep 5

# --- Step 5: Check map state after exit ---
# Count only the PIDs we inserted (exclude baseline agent PIDs)
LEAKED=0
for pid in "${PIDS[@]}"; do
    # Check if this specific PID is still in traced_processes
    key=$(printf '0x%02x 0x%02x 0x%02x 0x%02x' \
        $((pid & 0xFF)) $(((pid >> 8) & 0xFF)) $(((pid >> 16) & 0xFF)) $(((pid >> 24) & 0xFF)))
    if bpftool map lookup id "$TRACED_MAP_ID" key $key 2>/dev/null | grep -q "value"; then
        LEAKED=$((LEAKED + 1))
    fi
done

FINAL_TRACED=$(count_map_entries "$TRACED_MAP_ID")
FINAL_CHILD=$(count_map_entries "$CHILD_MAP_ID")
echo -e "${GREEN}[step 4]${NC} all processes exited"
echo ""
echo -e "${YELLOW}[step 5]${NC} final map state:"
echo "  traced_processes: $FINAL_TRACED entries (baseline was $INITIAL_TRACED)"
echo "  child_pids:       $FINAL_CHILD entries"

# --- Step 6: Analyze ---
echo ""
echo "=== Results ==="
echo "  Inserted: $NUM_PROCS entries into traced_processes (not child_pids)"
echo "  Leaked:   $LEAKED/$NUM_PROCS test PIDs still in map after exit"
echo ""

if [ "$LEAKED" -gt $((NUM_PROCS / 2)) ]; then
    echo -e "${RED}[BUG CONFIRMED]${NC} traced_processes leaked $LEAKED/$NUM_PROCS entries!"
    echo "  trace_process_exit() failed to clean up because child_pids had no matching entry."
    echo "  This will exhaust the 1024-entry map over time."
    exit 1
elif [ "$LEAKED" -gt 0 ]; then
    echo -e "${YELLOW}[PARTIAL LEAK]${NC} $LEAKED/$NUM_PROCS entries leaked"
    exit 1
else
    echo -e "${GREEN}[NO LEAK]${NC} all $NUM_PROCS test entries cleaned up on process exit"
    exit 0
fi
