#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Score every arm of every repeat with the SWE-bench test harness, which is what
# turns "the agent produced a patch" into "the patch fixes the bug". Runs on the
# host because it needs the instance images and the local dataset cache; no model
# traffic and no provider key are involved.
#
# Usage:  L4_EVAL_REPEATS="1 2 3 4 5" bash evaluate_all.sh
set -uo pipefail

W=${L4_REMOTE_WORK:-/root/l4}
RUNNER=$W/venv-swe/bin/swe-runner
REPEATS=${L4_EVAL_REPEATS:-"1 2 3 4 5"}
ARMS=${L4_EVAL_ARMS:-"a b c d"}
WORKERS=${L4_EVAL_WORKERS:-4}
# The dataset is fetched through the mirror; the harness also needs it for the
# gold test lists, not just for images.
export HF_ENDPOINT=${HF_ENDPOINT:-https://hf-mirror.com}
export PATH=/usr/local/bin:/opt/node/bin:$PATH

fails=0
for rep in $REPEATS; do
    for arm in $ARMS; do
        preds=$W/out/repeat-$rep/arm-$arm/run/preds.json
        out=$W/eval/repeat-$rep-arm-$arm
        if [ ! -f "$preds" ]; then
            echo "[eval] skip repeat-$rep arm-$arm: no preds.json"
            continue
        fi
        # Idempotent on a completion marker, not on the mere presence of an output
        # json: an interrupted run can leave a half-written report behind, and
        # keying off file existence would treat that broken eval as finished and
        # never rescore it. The marker is written only after a clean rc=0.
        if [ -f "$out/.eval-complete" ]; then
            echo "[eval] done already repeat-$rep arm-$arm"
            continue
        fi
        mkdir -p "$out"
        echo "[eval] $(date -u +%FT%TZ) repeat-$rep arm-$arm"
        "$RUNNER" evaluate \
            --predictions "$preds" \
            --subset lite --split test \
            --output "$out" \
            --workers "$WORKERS" --timeout 1800 \
            --run-id "l4r${rep}${arm}" > "$out/evaluate.log" 2>&1
        rc=$?
        line=$(grep -E "Instances (resolved|unresolved|completed|submitted):" "$out/evaluate.log" | tr '\n' ' ')
        echo "[eval]   rc=$rc $line"
        if [ "$rc" -eq 0 ]; then
            touch "$out/.eval-complete"
        else
            fails=$((fails + 1))
        fi
    done
done
echo "[eval] all done $(date -u +%FT%TZ); failed_arms=$fails"
# A harness whose design principle is "failures must be visible" must not always
# exit 0: propagate a non-zero status so a caller (or CI) can tell an arm failed
# to score, rather than reading the final line as success.
[ "$fails" -eq 0 ] || exit 1
