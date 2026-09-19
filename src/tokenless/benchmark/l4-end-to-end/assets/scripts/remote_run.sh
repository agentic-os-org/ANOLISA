#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Execute one repeat of the L4 arm sweep. Unlike its siblings in this directory,
# this script runs ON the remote host: a sweep takes hours and an ssh-driven
# heredoc would die with the connection. remote_sync.sh already places it at
# $L4_REMOTE_WORK/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/.
#
# Invoke it detached, passing the key through the environment so it never lands
# on disk:
#
#   ssh root@host 'DASHSCOPE_API_KEY=... setsid nohup bash \
#       /root/l4/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/remote_run.sh \
#       > /root/l4/logs/sweep.log 2>&1 &'
#
# Required environment:
#   DASHSCOPE_API_KEY   provider key; referenced by the arm configs as ${DASHSCOPE_API_KEY}
# Optional:
#   L4_ARMS             comma-separated arm letters      (default: a,b,c,d)
#   L4_REPEAT           repeat index, recorded in paths  (default: 1)
#   L4_INSTANCE_LIST    instance id list                 (default: $W/l4_instances.txt)
#   L4_TEMPERATURE      sampling temperature; defaults to runner's greedy 0
#   L4_ALLOW_GREEDY_REPEAT=1  explicitly allow repeat >1 at temperature 0
#   L4_TIMEOUT          per-instance agent timeout       (default: 1200)
#   L4_WORKERS          concurrent instances per arm     (default: 4)
#   L4_PULL_REGISTRY    registry to pull eval images via (default: docker.m.daocloud.io)
#   L4_DRY_RUN=1        start every proxy, prove every arm's activation and check
#                       the config/port wiring, then stop short of the billed run
#
# Run remote_verify.sh first. Everything it checks is a precondition here and is
# not re-checked. Then run this once with L4_DRY_RUN=1: the activation evidence
# each arm depends on lives in its proxy's startup log, so it can be proven for
# free, and a flag the proxy silently ignores is far cheaper to discover now.

set -uo pipefail

# Honour the fleet's per-host workspace root. remote_fleet.sh deploys under
# $L4_REMOTE_WORK and the sibling scripts all read it; a hardcoded /root/l4 here
# would make the sweep read a different tree than the one that was provisioned --
# in the worst case an earlier revision left under /root/l4, producing numbers
# attributed to the wrong SHA.
W="${L4_REMOTE_WORK:-/root/l4}"
LOG=$W/logs
CFGDIR=$W/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/configs
RUNNER=$W/venv-swe/bin/swe-runner
HEADROOM=$W/venv-headroom/bin/headroom
# The corpus is synced to its own directory rather than into the runner's packaged
# prompts/ path, so `rsync --delete` on the checkout cannot wipe it and the report
# can name the corpus revision independently of the code revision. Kept in step
# with remote_sync.sh, which also writes $W/prompts.provenance.
PROMPTS=$W/prompts

L4_ARMS=${L4_ARMS:-a,b,c,d}
L4_REPEAT=${L4_REPEAT:-1}
L4_INSTANCE_LIST=${L4_INSTANCE_LIST:-$W/l4_instances.txt}
L4_TIMEOUT=${L4_TIMEOUT:-1200}
L4_WORKERS=${L4_WORKERS:-4}
# The runner pulls each instance's image unconditionally instead of trusting the
# prepull, so a sweep issues one registry request per instance per arm. Docker
# Hub's anonymous allowance is per-IP and shared with manifest queries, and one
# host has already exhausted it. Routing every host through the same mirror also
# keeps the arms identically configured, which the paired comparison requires.
L4_PULL_REGISTRY=${L4_PULL_REGISTRY:-docker.m.daocloud.io}
SWEEP=$W/out/repeat-$L4_REPEAT
SUMMARY=$SWEEP/sweep-summary.txt

export PATH=/usr/local/bin:/opt/node/bin:/root/.cargo/bin:$PATH
# huggingface.co is unreachable from this host; the mirror is. Without it every
# dataset load burns ~30s on retries before falling back to the local cache, and
# the headroom proxy stalls indefinitely fetching its tokenizer.
export HF_ENDPOINT=${HF_ENDPOINT:-https://hf-mirror.com}
# Arm D's only observable trace. ensure_tools() -- which fetches difft and scc --
# has exactly one call site in headroom, inside the `if intercept_tool_results:`
# branch of cli/proxy.py, and cache_dir() honours HEADROOM_BINARIES_CACHE. Giving
# each arm its own cache directory turns "did that branch run" into a filesystem
# fact: arm D's directory fills, its siblings' stay empty. Nothing else reports
# the flag -- not the banner, not /stats, not /health?include_config, and the
# transform that consumes it logs nothing on either path.
#
# This holds only because github is reachable from these hosts. The earlier host
# needed HEADROOM_BINARIES_OFFLINE=1 to keep the download from hanging the proxy;
# that is now gone, since it was a deviation from a stock headroom install and the
# skip message it produced was the weaker, order-dependent form of this same
# proof. Should the transfer stall again the proxy misses its readiness probe and
# the arm aborts -- which the dry run surfaces before any tokens are spent.
BINCACHE_ROOT=$W/headroom-bincache

mkdir -p "$LOG" "$SWEEP"

die() { echo "[sweep] FATAL: $*" >&2; exit 1; }
note() { echo "[sweep] $*" | tee -a "$SUMMARY"; }

[ -n "${DASHSCOPE_API_KEY:-}" ] || die "DASHSCOPE_API_KEY absent; the arm configs would expand it to an empty string"
[ -f "$L4_INSTANCE_LIST" ] || die "instance list missing at $L4_INSTANCE_LIST"
[ -x "$RUNNER" ] || die "runner not installed at $RUNNER"
[ -d "$PROMPTS" ] || die "prompt corpus missing at $PROMPTS"

# The runner takes ids on the command line, not a file; flatten the list here so
# the file stays the single place the sweep's denominator is defined.
INSTANCE_IDS=$(grep '[^[:space:]]' "$L4_INSTANCE_LIST" | tr -d '\r' | paste -sd,)
INSTANCE_COUNT=$(grep -c '[^[:space:]]' "$L4_INSTANCE_LIST")
[ "$INSTANCE_COUNT" -gt 0 ] || die "instance list is empty"

# A missing prompt is not an error to openclaw: build_openclaw_prompt calls the
# soft loader, logs PER_CASE_PROMPT_SKIPPED and falls back to the generic prompt.
# The sweep then reports success while that instance answered a different question
# from its counterpart in the other arms. The corpus covers only part of the
# dataset, and the loader resolves prompts_dir/<instance_id> with no extension, so
# check every selected id here: finding this in the log afterwards costs a sweep.
missing_prompts=()
while read -r iid; do
    [ -n "$iid" ] || continue
    [ -s "$PROMPTS/$iid" ] || missing_prompts+=("$iid")
done < <(grep '[^[:space:]]' "$L4_INSTANCE_LIST" | tr -d '\r')
if [ "${#missing_prompts[@]}" -gt 0 ]; then
    die "no prompt for ${#missing_prompts[@]} of $INSTANCE_COUNT instances" \
        "(${missing_prompts[*]}); those instances would run on the generic prompt"
fi

# --seed only takes effect above temperature 0, so repeats at temperature 0 are
# not a sampling study -- they re-measure the same greedy point and expose only
# provider-side nondeterminism. That is a valid reproducibility check but a
# useless variance study, so it must be opted into explicitly with
# L4_ALLOW_GREEDY_REPEAT=1 rather than reached by forgetting --temperature.
if [ "$L4_REPEAT" -gt 1 ]; then
    case "${L4_TEMPERATURE:-0}" in
        0|0.0|"")
            [ "${L4_ALLOW_GREEDY_REPEAT:-0}" = "1" ] || die \
                "repeat $L4_REPEAT at temperature 0 is a reproducibility check, not a" \
                "variance study; set L4_ALLOW_GREEDY_REPEAT=1 to run it, or L4_TEMPERATURE>0 to sample"
            note "repeat $L4_REPEAT at temperature 0: reproducibility check (greedy re-run), not a variance sample" ;;
    esac
fi

# ---------------------------------------------------------------------------
# Arm table. Each arm declares its config, its runner flags, its proxy port and
# proxy flags, and -- critically -- the log patterns that must and must not
# appear once that proxy is up.
#
# The expectations are not decoration. The headroom arms differ only in a proxy
# flag that the banner and /stats are free to stay silent about, and arm C already
# once reported a clean pass while every request bypassed the proxy. An arm whose
# activation cannot be shown afterwards is indistinguishable from the arm next to
# it, so each one carries a signal that no sibling can produce.
#
# Arms e and f stay in the table but are out of the default sweep: four arms at 48
# instances is the run that was commissioned.
# ---------------------------------------------------------------------------
arm_spec() {
    case "$1" in
        a) CFGNAME=arm-a-baseline.json;              RFLAGS=();            PORT=;     PFLAGS=() ;;
        b) CFGNAME=arm-b-tokenless.json;             RFLAGS=(--tokenless); PORT=;     PFLAGS=() ;;
        c) CFGNAME=arm-c-headroom-default.json;      RFLAGS=(--headroom);  PORT=8801; PFLAGS=() ;;
        d) CFGNAME=arm-d-headroom-tool-results.json; RFLAGS=(--headroom);  PORT=8802
           PFLAGS=(--intercept-tool-results) ;;
        e) CFGNAME=arm-e-headroom-code-aware.json;   RFLAGS=(--headroom);  PORT=8803; PFLAGS=(--code-aware) ;;
        f) CFGNAME=arm-f-headroom-cache.json;        RFLAGS=(--headroom);  PORT=8804; PFLAGS=(--mode cache) ;;
        *) die "unknown arm '$1'" ;;
    esac
}

# Patterns that prove the arm's distinguishing flag took effect.
#
# Arm D has no pattern of its own. Its flag is invisible to the banner, and the
# skip message that used to stand in for it only existed because tool downloads
# were disabled; arm D is now gated by arm_expect_tool_cache instead.
arm_expect_present() {
    case "$1" in
        c) printf '%s\n' 'Mode:[[:space:]]+token' 'Code-Aware:[[:space:]]+DISABLED' ;;
        d) printf '%s\n' 'Mode:[[:space:]]+token' ;;
        e) printf '%s\n' 'Code-Aware:[[:space:]]+ENABLED' ;;
        f) printf '%s\n' 'Mode:[[:space:]]+cache' ;;
    esac
}

# Patterns that would mean a sibling arm's flag leaked in.
arm_expect_absent() {
    case "$1" in
        c) printf '%s\n' 'Mode:[[:space:]]+cache' ;;
        d) printf '%s\n' 'Code-Aware:[[:space:]]+ENABLED' 'Mode:[[:space:]]+cache' ;;
        e) printf '%s\n' 'Mode:[[:space:]]+cache' ;;
        f) printf '%s\n' 'Code-Aware:[[:space:]]+ENABLED' ;;
    esac
}

# Whether ensure_tools() must have populated this arm's binary cache. Arm D is the
# only arm that reaches that call site, so an emptied-then-refilled directory is a
# per-start on/off signal, independent of arm order and of the repeat index.
arm_expect_tool_cache() {
    case "$1" in
        d) echo filled ;;
        c|e|f) echo empty ;;
    esac
}

# ---------------------------------------------------------------------------
# Drift sentinel. qwen3-coder-plus is a rolling alias, chosen so that prompt
# caching keeps working; the cost is that the weights behind it can change
# mid-sweep. Recording the alias's `created` timestamp on both sides of the run
# does not prevent that, but it makes an undetected swap impossible to mistake
# for an effect of the arms.
# ---------------------------------------------------------------------------
record_model_created() {
    # Two `local` assignments, not one: bash expands every word of a `local`
    # command before performing any of its assignments, so a second name cannot
    # reference the first.
    local when=$1
    local out=$SWEEP/model-created.$when.json
    curl -s --max-time 20 \
        -H "Authorization: Bearer $DASHSCOPE_API_KEY" \
        https://dashscope.aliyuncs.com/compatible-mode/v1/models > "$out" 2>&1
    local created
    created=$(python3 - "$out" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except Exception:  # noqa: BLE001
    sys.exit(1)
hit = [m for m in doc.get("data", []) if m.get("id") == "qwen3-coder-plus"]
if not hit or hit[0].get("created") is None:
    sys.exit(1)
print(hit[0]["created"])
PY
    ) || { note "  model-created($when): UNREADABLE -- drift cannot be ruled out"; return 1; }
    note "  model-created($when)=$created"
    return 0
}

# ---------------------------------------------------------------------------
# Proxy lifecycle
# ---------------------------------------------------------------------------
PROXY_PID=

start_proxy() {
    local arm=$1 port=$2; shift 2
    local plog=$LOG/proxy_$arm.repeat$L4_REPEAT.log
    local jsonl=$LOG/proxy_$arm.repeat$L4_REPEAT.requests.jsonl
    rm -f "$plog" "$jsonl"

    # Wiped rather than reused so the assertion below describes this proxy start
    # and not an earlier one. The refetch is a few MB and happens once per arm.
    local bincache=$BINCACHE_ROOT/repeat$L4_REPEAT-arm-$arm
    rm -rf "$bincache"
    mkdir -p "$bincache"
    export HEADROOM_BINARIES_CACHE=$bincache

    # Arm D's ensure_tools() fetches difft and scc from github before the server
    # binds (see the cache note above). From these hosts that transfer is slow
    # enough to overrun a tight readiness window, and a mid-transfer TLS reset
    # kills the process outright -- both observed as "proxy never became ready".
    # Neither is a headroom defect and neither may be papered over by pre-seeding
    # the cache, since a filled cache is arm D's only activation proof. So make
    # the harness patient instead: wait long enough for a genuinely-slow fetch,
    # and relaunch a process that died before binding. A fetch that never
    # completes still fails the readiness probe and aborts the arm, unchanged.
    local ready_secs=${L4_PROXY_READY_SECS:-360}
    local launch_tries=${L4_PROXY_LAUNCH_TRIES:-3}
    local up=no try _
    for try in $(seq 1 "$launch_tries"); do
        # --openai-api-url must be given explicitly: the default upstream is not
        # dashscope. --no-telemetry and --no-subscription-tracking are not tuning
        # either -- with them on, the first proxied request hangs.
        nohup "$HEADROOM" proxy \
            --port "$port" \
            "$@" \
            --openai-api-url https://dashscope.aliyuncs.com/compatible-mode \
            --no-telemetry \
            --no-subscription-tracking \
            --log-file "$jsonl" \
            >> "$plog" 2>&1 &
        PROXY_PID=$!

        # Match the consumer's real readiness contract. The OpenClaw plugin
        # probes both /health and /v1/retrieve/stats; /livez alone can turn green
        # while those endpoints are still unavailable, which makes the first
        # worker(s) fail before sending any model traffic. Probe with the same
        # fan-out the runner will use, so a single lucky response is not enough.
        local endpoints_ready=no
        for _ in $(seq 1 "$ready_secs"); do
            if seq 1 "$L4_WORKERS" | xargs -P "$L4_WORKERS" -I{} sh -c \
                "curl -sf -m 3 http://127.0.0.1:$port/health >/dev/null && \
                 curl -sf -m 3 http://127.0.0.1:$port/v1/retrieve/stats >/dev/null"; then
                endpoints_ready=yes
                break
            fi
            kill -0 "$PROXY_PID" 2>/dev/null || break
            sleep 1
        done

        # The first real completion initializes the upstream/tokenizer path and
        # can temporarily make the plugin's health probe fail even after all HTTP
        # endpoints are green. Warm that path serially before four workers race
        # into it. stats.before is captured later, and the JSONL is truncated
        # after success, so this one-token request is excluded from every metric.
        if [ "$endpoints_ready" = yes ]; then
            local warm_body
            warm_body='{"model":"qwen3-coder-plus","max_tokens":1,"temperature":0,'
            warm_body+='"messages":[{"role":"user","content":"reply ok"}]}'
            for _ in 1 2 3; do
                if curl -sf -o /dev/null -m 120 \
                    -X POST "http://127.0.0.1:$port/v1/chat/completions" \
                    -H "Authorization: Bearer $DASHSCOPE_API_KEY" \
                    -H 'Content-Type: application/json' \
                    -d "$warm_body" ; then
                    up=yes
                    : > "$jsonl"
                    note "  activation evidence present: proxy handled a real prewarm completion"
                    break
                fi
                sleep 3
            done
        fi
        [ "$up" = yes ] && break

        # Died or missed the window. Reap it, and if a try remains start clean --
        # the cache wipe keeps each attempt's proof describing only that attempt.
        kill "$PROXY_PID" 2>/dev/null
        wait "$PROXY_PID" 2>/dev/null
        note "  proxy for arm $arm not ready after ${ready_secs}s (attempt $try/$launch_tries)"
        if [ "$try" -lt "$launch_tries" ]; then
            rm -rf "$bincache"; mkdir -p "$bincache"
            sleep 3
        fi
    done
    if [ "$up" != yes ]; then
        note "  proxy for arm $arm never became ready; see $plog"
        tail -5 "$plog" | sed 's/^/    /'
        return 1
    fi

    local missing=0

    # ensure_tools() runs in the CLI before the server binds, so once /livez
    # answers the cache holds its final contents for this start.
    local want_cache found_tool
    want_cache=$(arm_expect_tool_cache "$arm")
    found_tool=$(find "$bincache" -type f \( -name 'difft*' -o -name 'scc*' \) 2>/dev/null | head -1)
    case "$want_cache" in
        filled)
            if [ -n "$found_tool" ]; then
                note "  activation evidence present: tool cache populated ($(basename "$found_tool"))"
            else
                note "  activation evidence MISSING: --intercept-tool-results left the tool cache empty"
                missing=1
            fi ;;
        empty)
            if [ -n "$found_tool" ]; then
                note "  foreign flag leaked into arm $arm: tool cache populated without --intercept-tool-results"
                missing=1
            fi ;;
    esac

    local pat
    while read -r pat; do
        [ -n "$pat" ] || continue
        if grep -qE "$pat" "$plog"; then
            note "  activation evidence present: /$pat/"
        else
            note "  activation evidence MISSING: /$pat/"
            missing=1
        fi
    done < <(arm_expect_present "$arm")
    while read -r pat; do
        [ -n "$pat" ] || continue
        if grep -qE "$pat" "$plog"; then
            note "  foreign flag leaked into arm $arm: /$pat/"
            missing=1
        fi
    done < <(arm_expect_absent "$arm")

    [ "$missing" -eq 0 ] || return 1
    return 0
}

stop_proxy() {
    [ -n "$PROXY_PID" ] || return 0
    kill "$PROXY_PID" 2>/dev/null
    wait "$PROXY_PID" 2>/dev/null
    PROXY_PID=
}

trap stop_proxy EXIT

stats_snapshot() { curl -s --max-time 5 "http://127.0.0.1:$1/stats"; }

# ---------------------------------------------------------------------------
# Per-instance activation evidence.
#
# The runner already writes one json per instance for both tokenless and headroom,
# each with a top-level `strong` plus the individual probes behind it. That is a
# strictly better gate than the arm-level proxy counters below, which can say that
# something bypassed the proxy but never which instance -- and an arm that was
# only half active is exactly the failure that would otherwise be averaged away.
#
# A `reasons` entry of null means the probe could not run, which the collectors
# distinguish from false on purpose (OpenClaw 2026.8.2 writes no trajectory file),
# so only explicit false is counted against the arm.
# ---------------------------------------------------------------------------
check_instance_evidence() {
    local kind=$1 dir=$2 rc
    python3 - "$kind" "$dir" "$INSTANCE_COUNT" <<'PY' | tee -a "$SUMMARY"
import json, pathlib, sys
from collections import Counter

kind, root, want = sys.argv[1], pathlib.Path(sys.argv[2]), int(sys.argv[3])
files = sorted(root.glob("*.json")) if root.is_dir() else []
weak, why = [], Counter()
for path in files:
    try:
        doc = json.loads(path.read_text())
    except Exception:  # noqa: BLE001
        weak.append(path.stem)
        why["unparseable"] += 1
        continue
    if doc.get("strong") is True:
        continue
    weak.append(doc.get("instance_id") or path.stem)
    for name, value in (doc.get("reasons") or {}).items():
        if value is False:
            why[name] += 1
print(f"  {kind}_evidence: files={len(files)}/{want} strong={len(files) - len(weak)} weak={len(weak)}")
if len(files) < want:
    print(f"  GUARD FAILED: {kind} evidence absent for {want - len(files)} instances")
if weak:
    print(f"  GUARD FAILED: {kind} activation not strong for {len(weak)} instances")
for name, count in why.most_common():
    print(f"    weak because {name} is false: {count}")
for iid in weak[:5]:
    print(f"    not strong: {iid}")
sys.exit(1 if weak or len(files) < want else 0)
PY
    rc=${PIPESTATUS[0]}
    return "$rc"
}

# ---------------------------------------------------------------------------
# One arm
# ---------------------------------------------------------------------------
run_arm() {
    local arm=$1
    arm_spec "$arm"
    local cfg=$CFGDIR/$CFGNAME
    local out=$SWEEP/arm-$arm
    local runlog=$LOG/arm_$arm.repeat$L4_REPEAT.log
    mkdir -p "$out"

    note "--- arm $arm  config=$CFGNAME flags=${RFLAGS[*]:-none} port=${PORT:-direct}"
    [ -f "$cfg" ] || { note "  config missing: $cfg"; return 1; }

    # A config pointing at another arm's port would route this arm's traffic
    # through the wrong compressor while every other check still passed.
    if [ -n "$PORT" ]; then
        grep -q "127.0.0.1:$PORT" "$cfg" || { note "  config does not reference port $PORT"; return 1; }
        start_proxy "$arm" "$PORT" ${PFLAGS[@]+"${PFLAGS[@]}"} \
            || { note "  arm $arm aborted: activation unproven"; return 1; }
        stats_snapshot "$PORT" > "$out/stats.before.json"
    fi

    if [ "${L4_DRY_RUN:-0}" = "1" ]; then
        note "  dry run: wiring and activation proven, billed run skipped"
        stop_proxy
        return 0
    fi

    local extra=()
    [ -n "${L4_TEMPERATURE:-}" ] && extra+=(--temperature "$L4_TEMPERATURE" --seed "$L4_REPEAT")

    local started ended
    started=$(date +%s)
    # --redo forces every selected instance to execute even if a prior attempt in
    # this output dir left a result: a repeat must actually re-call the model, not
    # resume a partial sweep and inherit an earlier run's trajectory.
    "$RUNNER" run \
        -a openclaw \
        -i "$INSTANCE_IDS" \
        --per-case-prompt \
        --prompts-dir "$PROMPTS" \
        --base-config "$cfg" \
        --docker-pull-registry "$L4_PULL_REGISTRY" \
        --redo \
        ${RFLAGS[@]+"${RFLAGS[@]}"} \
        ${extra[@]+"${extra[@]}"} \
        -o "$out" \
        --workers "$L4_WORKERS" \
        --timeout "$L4_TIMEOUT" \
        -v > "$runlog" 2>&1
    local rc=$?
    ended=$(date +%s)
    note "  runner rc=$rc elapsed=$((ended - started))s"

    [ -n "$PORT" ] && stats_snapshot "$PORT" > "$out/stats.after.json"

    # `swe-runner run` returns 0 even when every instance failed, so the exit
    # status is recorded but never trusted as evidence.
    local ok=1

    # A declared-but-unlinked plugin is only a warning to openclaw: the entry is
    # ignored and the arm silently becomes a second baseline.
    if grep -q "plugin not found" "$out/run/swe-runner.run.log" 2>/dev/null; then
        note "  plugin not found in the run log; the arm ran without its compressor"
        ok=0
    fi
    if grep -q "PER_CASE_PROMPT_SKIPPED" "$out/run/swe-runner.run.log" 2>/dev/null; then
        note "  PER_CASE_PROMPT_SKIPPED present: at least one instance ran unguided"
        ok=0
    fi
    # The absence of a negative marker is weaker than the presence of a positive
    # one, so count the instances the loader actually served. Deduplicated because
    # a retried instance loads its prompt more than once.
    local loaded
    loaded=$(grep -o 'CUSTOM_PROMPT_LOADED instance=[^[:space:]]*' "$out/run/swe-runner.run.log" 2>/dev/null \
        | sort -u | wc -l | tr -d ' ')
    if [ "${loaded:-0}" -ne "$INSTANCE_COUNT" ]; then
        note "  per-case prompts served for ${loaded:-0} of $INSTANCE_COUNT instances"
        ok=0
    fi

    case "$arm" in
        b) check_instance_evidence tokenless "$out/run/openclaw-tokenless-evidence" || ok=0 ;;
        c|d|e|f)
            check_instance_evidence headroom "$out/run/openclaw-headroom-evidence" || ok=0
            # Logged at error level for exactly the case this sweep cannot afford:
            # an arm configured correctly that never routed a request through its
            # proxy, i.e. a second baseline wearing a treatment's label.
            if grep -q "OPENCLAW_HEADROOM_NO_PROXY_TRAFFIC" "$out/run/swe-runner.run.log" 2>/dev/null; then
                note "  OPENCLAW_HEADROOM_NO_PROXY_TRAFFIC: an instance bypassed the proxy entirely"
                ok=0
            fi ;;
    esac

    python3 - "$out/run/results" <<'PY' | tee -a "$SUMMARY"
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
files = sorted(root.glob("*.json")) if root.is_dir() else []
bad = [json.loads(p.read_text()) for p in files]
failed = [d for d in bad if not d.get("success")]
print(f"  instances_completed={len(files) - len(failed)} harness_failures={len(failed)}")
for d in failed[:5]:
    print(f"    failed {d.get('instance_id')}: {d.get('error')}")
PY

    if [ -n "$PORT" ]; then
        python3 - "$out/stats.before.json" "$out/stats.after.json" "$INSTANCE_COUNT" <<'PY' | tee -a "$SUMMARY"
import json, sys


def load(path):
    try:
        return json.load(open(path))
    except Exception:  # noqa: BLE001
        return {}


def dig(doc, *keys):
    cur = doc
    for k in keys:
        cur = cur.get(k, {}) if isinstance(cur, dict) else {}
    return cur if isinstance(cur, int) else None


bef, aft, want = load(sys.argv[1]), load(sys.argv[2]), int(sys.argv[3])
verdict = []


def delta(*keys):
    a, b = dig(aft, *keys), dig(bef, *keys)
    return None if a is None or b is None else a - b


reqs = delta("requests", "total")
comp = delta("summary", "compression", "requests_compressed")
print(f"  proxy_requests_delta={reqs} requests_compressed_delta={comp} instances={want}")
if reqs is None or comp is None:
    verdict.append("stats unreadable: compression cannot be confirmed")
elif reqs == 0:
    verdict.append("zero proxy requests: the arm measured the baseline")
elif reqs < want:
    # Every instance must issue at least one model call, so fewer proxy requests
    # than instances proves some instances bypassed the proxy entirely. An
    # arm-level total cannot localise which, but it can refuse to pass.
    verdict.append(f"only {reqs} proxy requests for {want} instances: some bypassed the proxy")
elif comp == 0:
    verdict.append("traffic reached the proxy but nothing was compressed")
for v in verdict:
    print(f"  GUARD FAILED: {v}")
sys.exit(1 if verdict else 0)
PY
        [ "${PIPESTATUS[0]}" -eq 0 ] || ok=0
        stop_proxy
    fi

    # Token totals come from openclaw's own SQLite transcripts, not from the
    # proxy: the proxy sees what it forwarded, the store sees what the agent was
    # actually billed for, and the two answer different questions.
    local report_started
    report_started=$(date +%s)
    if ! "$RUNNER" token-report \
            --profiles-dir "$out/run/openclaw-profiles" \
            --arm "$arm" \
            --output "$out/report.json" >> "$SUMMARY" 2>&1; then
        # OpenClaw can retain a session aggregate while dropping individual turn
        # rows for a long trajectory. That invalidates that instance's per-turn
        # token/request pair, not every other instance in the arm. Keep the cell
        # eligible and make downstream analysis exclude the named instances. A
        # missing/unreadable report remains an arm-level failure.
        #
        # The output dir is reused across --redo re-runs, so a stale report.json
        # from an earlier attempt can still be on disk when THIS token-report
        # failed before writing one. Only follow the exclusion path when the file
        # was (re)written by this attempt; an older mtime means no valid report
        # exists now, which is an arm-level failure, not a per-instance exclusion.
        local report_mtime disagreeing=""
        report_mtime=$(stat -c %Y "$out/report.json" 2>/dev/null || echo 0)
        if [ "$report_mtime" -ge "$report_started" ]; then
            disagreeing=$(python3 - "$out/report.json" <<'PY'
import json, sys
try:
    values = json.load(open(sys.argv[1]))["totals"]["instances_with_disagreeing_aggregate"]
except Exception:
    values = []
print(" ".join(values))
PY
)
        fi
        if [ -n "$disagreeing" ]; then
            note "  token-report instance exclusions (per-turn vs aggregate mismatch): $disagreeing"
        else
            note "  token-report refused arm $arm; see $out/report.json"
            ok=0
        fi
    fi

    [ "$ok" -eq 1 ] || return 1
    return 0
}

# ---------------------------------------------------------------------------
note "sweep repeat=$L4_REPEAT arms=$L4_ARMS instances=$INSTANCE_COUNT temperature=${L4_TEMPERATURE:-unset}"
note "revision=$(git -C "$W/anolisa" rev-parse --short HEAD 2>/dev/null || echo unknown)"
[ "${L4_DRY_RUN:-0}" = "1" ] && note "DRY RUN: no instance will be executed and no tokens spent"
# The alias timestamp is not a nicety: without a reading on both sides of the
# sweep, a mid-run weight swap and a real arm effect look identical afterwards.
# Refuse to start rather than discover at the end that the guard never worked.
record_model_created before \
    || die "cannot read the model alias timestamp; the drift guard would be absent for the whole sweep"

failed_arms=()
IFS=, read -ra arms <<< "$L4_ARMS"
for arm in "${arms[@]}"; do
    run_arm "$arm" || failed_arms+=("$arm")
    stop_proxy
done

drift_ok=1
record_model_created after || drift_ok=0

if [ "$drift_ok" -eq 0 ]; then
    note "the closing alias timestamp is missing; model drift across this sweep is unruled-out"
fi
if [ "${#failed_arms[@]}" -gt 0 ]; then
    note "arms whose evidence did not hold: ${failed_arms[*]}"
    note "their numbers are on disk but must not be compared against the others"
    exit 1
fi
[ "$drift_ok" -eq 1 ] || exit 1
note "all arms produced evidence-backed results under $SWEEP"
