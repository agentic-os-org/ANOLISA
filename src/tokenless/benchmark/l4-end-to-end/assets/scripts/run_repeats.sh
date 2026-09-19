#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Drive one or more L4 repeats at the runner's default temperature (temp=0),
# clearing SWE and OpenClaw run state before each so a repeat genuinely re-calls
# the model instead of replaying a cached trajectory. Runs detached on the host;
# the provider key is read from the launch environment and never written to disk.
#
# Usage (repeat indices default to 2 3 4 5):
#   ssh root@host 'DASHSCOPE_API_KEY=... setsid nohup bash \
#       /root/l4/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/run_repeats.sh \
#       2 > /root/l4/logs/repeats.log 2>&1 &'
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
: "${DASHSCOPE_API_KEY:?set DASHSCOPE_API_KEY in the launch environment}"

# temp-0 repeats are an intentional reproducibility check, opted in explicitly so
# remote_run.sh does not refuse them (see its guard).
export L4_ALLOW_GREEDY_REPEAT=1

REPEATS=("$@")
[ "${#REPEATS[@]}" -eq 0 ] && REPEATS=(2 3 4 5)

echo "RUN_REPEATS start $(date -u +%FT%TZ) repeats=${REPEATS[*]} host=$(hostname)"
for N in "${REPEATS[@]}"; do
    echo "===== $(date -u +%FT%TZ) REPEAT $N: clean ====="
    bash "$HERE/clean_between_repeats.sh"
    echo "===== $(date -u +%FT%TZ) REPEAT $N: run (temp=0, greedy reproducibility) ====="
    L4_REPEAT="$N" bash "$HERE/remote_run.sh"
    rc=$?
    echo "===== $(date -u +%FT%TZ) REPEAT $N: end rc=$rc ====="
    # Clear immediately after every repeat, including the last one. The next
    # repeat also pre-cleans defensively, but post-cleaning guarantees no live SWE
    # testbed or OpenClaw agent state survives the campaign at all.
    echo "===== $(date -u +%FT%TZ) REPEAT $N: post-clean ====="
    bash "$HERE/clean_between_repeats.sh"
done
echo "RUN_REPEATS done $(date -u +%FT%TZ)"
