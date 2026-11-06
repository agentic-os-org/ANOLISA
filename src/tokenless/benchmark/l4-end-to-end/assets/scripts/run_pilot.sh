#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# L4 pilot under a hard spending cap. Runs ON the host, detached:
#
#   ssh root@host 'DASHSCOPE_API_KEY=... setsid nohup bash \
#       /root/l4/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/run_pilot.sh \
#       > /root/l4/logs/pilot.log 2>&1 &'
#
# The sweep itself is remote_run.sh, started in its own process group. A watcher
# re-reads every instance's OpenClaw transcript store (the same per-turn usage
# records `swe-runner token-report` sums afterwards) every L4_COST_POLL_SECS,
# prices them with cached input billed at the full input price (an upper bound),
# and stops the whole sweep once that bound reaches the cap minus a margin that
# covers the turns the workers can still complete before the next poll.
#
# token-report itself is not usable here: it refuses a profile whose store does
# not exist yet, which is the normal state of an instance that has just started,
# and a refused report would read as zero spend for the whole arm.
# 新建且尚无 store 的 profile 可暂时跳过；已存在 store 无法读取或总量不一致时立即停止。
#
# The final ledger, including what the measured per-instance spend implies for
# the full instance set, is written to $SWEEP/pilot-cost.json.
#
# Optional environment:
#   L4_COST_CAP          cap in CNY                                   (default: 300)
#   L4_COST_MARGIN       stop this far below the cap, in CNY          (default: 20)
#   L4_COST_POLL_SECS    watcher interval                             (default: 30)
#   L4_PRICE_IN/OUT      CNY per million tokens                       (default: 12 / 36)
#   L4_PRICE_CACHED      CNY per million cached tokens, discounted view only (default: 1.5)
#   L4_ARMS              arms to run                                  (default: a,b,c; E and F
#                        duplicate C's default configuration, D needs a canary rollout)
#   L4_INSTANCE_LIST     pilot instance list                          (default: $W/pilot_instances.txt)
#   L4_PILOT_INSTANCES   comma-separated ids written to that list when it does not
#                        exist yet (default: the first instance of each of the four
#                        48-instance shards)
#   L4_FULL_INSTANCES    instance count for the estimate              (default: 48)
#   L4_FULL_ROUNDS       repeats for the estimate                     (default: 5)
set -uo pipefail

: "${DASHSCOPE_API_KEY:?set DASHSCOPE_API_KEY in the launch environment}"
W="${L4_REMOTE_WORK:-/root/l4}"
HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=l4_env.sh
. "$HERE/l4_env.sh" "$W"
export L4_ARMS=${L4_ARMS:-a,b,c}
export L4_INSTANCE_LIST=${L4_INSTANCE_LIST:-$W/pilot_instances.txt}
export L4_REPEAT=${L4_REPEAT:-1}
CAP=${L4_COST_CAP:-300}
MARGIN=${L4_COST_MARGIN:-20}
POLL=${L4_COST_POLL_SECS:-30}
SWEEP=$W/out/repeat-$L4_REPEAT
PY_SWE=$W/venv-swe/bin/python
mkdir -p "$SWEEP" "$W/logs"
if [ ! -s "$L4_INSTANCE_LIST" ]; then
    printf '%s\n' "${L4_PILOT_INSTANCES:-django__django-13158,django__django-12284,astropy__astropy-12907,astropy__astropy-14182}" \
        | tr ',' '\n' | grep . > "$L4_INSTANCE_LIST"
fi

# ledger [final]: prints the upper bound so far; exits 3 once it reaches CAP-MARGIN.
ledger() {
    "$PY_SWE" - "$SWEEP" "$CAP" "$MARGIN" "$L4_ARMS" "$@" <<'PY'
import json, math, os, pathlib, sys
from swe_runner.trace_extraction.openclaw_sqlite import SqliteUsageError, find_agent_stores, read_store_usage

sweep, cap, margin, arms = pathlib.Path(sys.argv[1]), float(sys.argv[2]), float(sys.argv[3]), sys.argv[4].split(",")
final = len(sys.argv) > 5 and sys.argv[5] == "final"
p_in = float(os.environ.get("L4_PRICE_IN", "12")); p_out = float(os.environ.get("L4_PRICE_OUT", "36"))
p_cached = float(os.environ.get("L4_PRICE_CACHED", "1.5"))
full_n = int(os.environ.get("L4_FULL_INSTANCES", "48")); full_rounds = int(os.environ.get("L4_FULL_ROUNDS", "5"))
if not all(math.isfinite(value) for value in (cap, margin, p_in, p_out, p_cached)) or not 0 <= margin < cap:
    sys.exit("invalid pilot cap, margin or prices")
if min(p_in, p_out) <= 0 or p_cached < 0 or full_n <= 0 or full_rounds <= 0:
    sys.exit("pilot prices and scale must be positive")
cache_path = sweep / ".pilot-ledger-cache.json"
try:
    last_good = json.loads(cache_path.read_text())
except (OSError, ValueError):
    last_good = {}

per_arm, upper, discounted, stale, errors = {}, 0.0, 0.0, [], []
for arm in arms:
    profiles = sweep / f"arm-{arm}" / "run" / "openclaw-profiles"
    if not profiles.is_dir():
        continue
    fresh = cached = out = requests = instances = 0
    for profile in sorted(p for p in profiles.iterdir() if p.is_dir()):
        for store in find_agent_stores(profile):
            key = str(store)
            try:
                usage = read_store_usage(store)
                if usage.aggregate_agrees is False:
                    raise SqliteUsageError(f"aggregate mismatch: {store}")
                reading = [usage.input_tokens, usage.cache_read_tokens, usage.output_tokens, usage.requests]
                last_good[key] = reading
            except SqliteUsageError as error:
                errors.append(str(error))
                stale.append(key)
                reading = last_good.get(key)
                if reading is None:
                    continue
            fresh += reading[0]; cached += reading[1]; out += reading[2]; requests += reading[3]
        instances += 1 if find_agent_stores(profile) else 0
    cost_upper = ((fresh + cached) * p_in + out * p_out) / 1e6
    cost_disc = (fresh * p_in + cached * p_cached + out * p_out) / 1e6
    per_arm[arm] = {"instances_started": instances, "requests": requests,
                    "input_tokens": fresh, "cache_read_tokens": cached, "output_tokens": out,
                    "cost_upper_bound": round(cost_upper, 4),
                    "cost_with_published_cache_price": round(cost_disc, 4),
                    "per_instance_upper_bound": round(cost_upper / max(1, instances), 4),
                    "per_instance_with_published_cache_price": round(cost_disc / max(1, instances), 4)}
    upper += cost_upper
    discounted += cost_disc
cache_path.write_text(json.dumps(last_good))

def scale(field: str) -> float | None:
    # Arms not reached (a cap stop) are estimated at the mean of the measured ones.
    measured = [a[field] for a in per_arm.values() if a["instances_started"]]
    return (sum(measured) / len(measured)) * len(arms) * full_n * full_rounds if measured and not errors else None

doc = {"cap": cap, "stop_threshold": cap - margin, "currency": "CNY",
       "prices_per_mtok": {"input": p_in, "output": p_out, "cached_input_discounted_view": p_cached},
       "cost_upper_bound": round(upper, 4), "cost_with_published_cache_price": round(discounted, 4),
       "billing_reliable": not errors, "billing_errors": errors,
       "stale_store_readings": stale, "arms": per_arm,
       "full_scale_estimate": {"instances": full_n, "rounds": full_rounds, "arms": arms,
                               "upper_bound": scale("per_instance_upper_bound"),
                               "with_published_cache_price": scale("per_instance_with_published_cache_price")}}
(sweep / "pilot-cost.json").write_text(json.dumps(doc, indent=2) + "\n")
print(f"{upper:.2f}")
if errors:
    sys.stderr.write("pilot billing is unreliable: " + "; ".join(errors) + "\n")
    sys.exit(2)
sys.exit(0 if final or upper < cap - margin else 3)
PY
}

case "$POLL" in ''|*[!0-9]*) echo "invalid L4_COST_POLL_SECS" >&2; exit 2 ;; esac
[ "$POLL" -gt 0 ] || exit 2
# 开跑前验证预算参数与已有读数，避免第一个轮询前已盲目计费。
ledger_status=0
ledger > "$SWEEP/.pilot-preflight-cost" || ledger_status=$?
if [ "$ledger_status" -ne 0 ]; then
    echo "[pilot] budget preflight refused: status=$ledger_status; see $SWEEP/pilot-cost.json" >&2
    exit "$ledger_status"
fi
echo "[pilot] $(date -u +%FT%TZ) arms=$L4_ARMS instances=$(grep -c . "$L4_INSTANCE_LIST") cap=$CAP margin=$MARGIN"
SWEEP_PID=
stop_sweep() {
    [ -n "$SWEEP_PID" ] || return 0
    kill -TERM -- "-$SWEEP_PID" 2>/dev/null || kill -TERM "$SWEEP_PID" 2>/dev/null || true
    if kill -0 -- "-$SWEEP_PID" 2>/dev/null; then
        sleep "${L4_STOP_GRACE_SECS:-10}"
        kill -KILL -- "-$SWEEP_PID" 2>/dev/null || true
    fi
}
pilot_cleanup() {
    local status=$?
    trap - TERM INT EXIT
    stop_sweep
    [ -z "$SWEEP_PID" ] || wait "$SWEEP_PID" 2>/dev/null || true
    exit "$status"
}
trap pilot_cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
setsid bash "$HERE/remote_run.sh" > "$W/logs/pilot-sweep.log" 2>&1 &
SWEEP_PID=$!
stop_reason=completed
while kill -0 "$SWEEP_PID" 2>/dev/null; do
    sleep "$POLL"
    spent=$(ledger)
    case $? in
        0) continue ;;
        3) stop_reason=cap ;;
        *) stop_reason=metering_failure
           echo "[pilot] $(date -u +%FT%TZ) ledger unreadable or inconsistent; stopping rather than spending blind"
           spent=unknown ;;
    esac
    echo "[pilot] $(date -u +%FT%TZ) stopping the sweep: reason=$stop_reason spend=$spent"
    stop_sweep
    break
done
wait "$SWEEP_PID" 2>/dev/null
rc=$?
spent=$(ledger final)
ledger_rc=$?
[ "$ledger_rc" -eq 0 ] || stop_reason=metering_failure
if [ "$stop_reason" = completed ] && [ "$rc" -ne 0 ]; then stop_reason=sweep_failure; fi
"$PY_SWE" - "$SWEEP/pilot-cost.json" "$stop_reason" "$rc" <<'PY'
import json, sys
path, reason, rc = sys.argv[1], sys.argv[2], int(sys.argv[3])
try:
    doc = json.load(open(path))
except (OSError, ValueError):
    doc = {"billing_reliable": False, "cost_upper_bound": None}
doc.update(stopped_by_cap=reason == "cap", stop_reason=reason, sweep_exit=rc)
open(path, "w").write(json.dumps(doc, indent=2) + "\n")
PY
[ "$?" -eq 0 ] || exit 2
echo "[pilot] $(date -u +%FT%TZ) done: sweep rc=$rc stop_reason=$stop_reason spend=$spent (see $SWEEP/pilot-cost.json)"
case "$stop_reason" in
    cap) exit 3 ;;
    metering_failure) exit 2 ;;
    *) exit "$rc" ;;
esac
