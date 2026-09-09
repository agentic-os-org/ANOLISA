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
# Optional explicit subset. When unset, each repeat discovers every arm-* tree
# that the sweep actually produced, so e/f cannot run and then silently escape
# correctness scoring merely because an old default stopped at d.
ARMS_OVERRIDE=${L4_EVAL_ARMS:-}
WORKERS=${L4_EVAL_WORKERS:-4}
# The dataset is fetched through the mirror; the harness also needs it for the
# gold test lists, not just for images.
export HF_ENDPOINT=${HF_ENDPOINT:-https://hf-mirror.com}
export PATH=/usr/local/bin:/opt/node/bin:$PATH

fails=0
for rep in $REPEATS; do
    # "This repeat was never run on this host" and "this repeat ran but produced
    # no arm" are different outcomes. The default REPEATS spans 1-5 while the
    # layer's default is a single repeat (remote_run.sh L4_REPEAT defaults to 1;
    # repeats 2-5 are the opt-in reproducibility check), so a missing out/repeat-N
    # directory is not-applicable and must be skipped, not counted as a failure --
    # otherwise a healthy single-repeat host exits non-zero on the documented
    # default invocation.
    if [ ! -d "$W/out/repeat-$rep" ]; then
        echo "[eval] repeat-$rep skipped: this repeat was not run on this host"
        continue
    fi
    eval_arms=()
    if [ -n "$ARMS_OVERRIDE" ]; then
        read -ra eval_arms <<< "$ARMS_OVERRIDE"
    else
        for arm_dir in "$W/out/repeat-$rep"/arm-*; do
            [ -d "$arm_dir" ] || continue
            eval_arms+=("${arm_dir##*/arm-}")
        done
    fi
    if [ "${#eval_arms[@]}" -eq 0 ]; then
        # The run tree exists but holds no arm: a sweep that started (mkdir -p
        # ran) then died before any arm produced output. That is a real failure.
        echo "[eval] repeat-$rep FAILED: run tree exists but has no arm-* dirs"
        fails=$((fails + 1))
        continue
    fi
    for arm in "${eval_arms[@]}"; do
        preds=$W/out/repeat-$rep/arm-$arm/run/preds.json
        out=$W/eval/repeat-$rep-arm-$arm
        if [ ! -f "$preds" ]; then
            echo "[eval] repeat-$rep arm-$arm FAILED: arm ran but produced no preds.json"
            fails=$((fails + 1))
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
