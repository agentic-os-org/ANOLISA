#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Pre-flight verification for the L4 end-to-end comparison. Run AFTER
# remote_setup.sh and BEFORE any billed agent run.
#
# Required environment:
#   L4_SSH_HOST / L4_SSH_PASS   (L4_SSH_USER defaults to root)
# Optional:
#   L4_REMOTE_WORK              (default: /root/l4)
#   L4_INSTANCE_LIST            remote path to the instance id list
#                               (default: $L4_REMOTE_WORK/l4_instances.txt)
#   L4_PROMPTS_DIR              per-instance prompt corpus
#                               (default: $L4_REMOTE_WORK/prompts)
#
# Every check here answers a question that, if answered wrongly, invalidates the
# whole run rather than degrading it. They are cheap; a full run is not. Exit
# code is non-zero if any hard check fails.
#
# Deliberately NOT checked here: whether compression actually happened. That is
# a per-instance property of the run itself and belongs to the run's own
# evidence path, not to a pre-flight probe.

set -euo pipefail

: "${L4_SSH_HOST:?L4_SSH_HOST is required (remote host or IP)}"
# Needed only to bootstrap a host that does not have the key yet; see below.
L4_SSH_PASS="${L4_SSH_PASS:-}"
L4_SSH_USER="${L4_SSH_USER:-root}"
L4_REMOTE_WORK="${L4_REMOTE_WORK:-/root/l4}"
L4_INSTANCE_LIST="${L4_INSTANCE_LIST:-$L4_REMOTE_WORK/l4_instances.txt}"
# The corpus lives beside the checkout, not inside the runner's packaged prompts/
# path, so that `rsync --delete` on the code cannot wipe it. remote_sync.sh and
# remote_run.sh use the same location; all three must agree or this check passes
# while the sweep silently falls back to the generic prompt.
L4_PROMPTS_DIR="${L4_PROMPTS_DIR:-$L4_REMOTE_WORK/prompts}"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)

# Key first, sshpass only to bootstrap a host that does not have the key yet.
# sshpass allocates a pty per connection and that setup races under concurrency,
# which appears as ssh failing with "Bad file descriptor".
L4_SSH_KEY=${L4_SSH_KEY:-$HOME/.ssh/l4_eval_ed25519}
if [ -f "$L4_SSH_KEY" ]; then
    SSH_OPTS+=(-i "$L4_SSH_KEY" -o IdentitiesOnly=yes)
    SSH_PW=()
else
    : "${L4_SSH_PASS:?L4_SSH_PASS is required when $L4_SSH_KEY does not exist}"
    SSH_PW=(sshpass -p "$L4_SSH_PASS")
fi

remote_ssh() {
    ${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" "$@"
}

echo "[verify] running pre-flight checks on $L4_SSH_HOST"
remote_ssh "L4_REMOTE_WORK=$L4_REMOTE_WORK L4_INSTANCE_LIST=$L4_INSTANCE_LIST L4_PROMPTS_DIR=$L4_PROMPTS_DIR bash -s" <<'CHECKS'
set -uo pipefail
export PATH=/usr/local/bin:/opt/node/bin:/root/.cargo/bin:$PATH
WORK="${L4_REMOTE_WORK:-/root/l4}"
LIST="${L4_INSTANCE_LIST:-$WORK/l4_instances.txt}"
PROMPTS="${L4_PROMPTS_DIR:-$WORK/prompts}"

# huggingface.co is blocked from this host; hf-mirror.com is not. swe-runner
# loads SWE-bench through the `datasets` library, which honours HF_ENDPOINT.
export HF_ENDPOINT=https://hf-mirror.com

rc=0
pass() { echo "  PASS  $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

echo "=== 1. per-profile plugin isolation ==="
# The failure this guards against is silent: if a scratch profile inherits the
# globally installed plugin registry, the baseline arm runs with a compressor
# attached and every delta measures nothing. Probe with a profile built exactly
# the way swe-runner builds one — a verbatim copy of the global config.
rm -rf "$WORK/probe-profile" /root/.openclaw-l4probe
mkdir -p "$WORK/probe-profile"
cp /root/.openclaw/openclaw.json "$WORK/probe-profile/openclaw.json" 2>/dev/null || {
    fail "global openclaw.json missing — run remote_setup.sh first"
    echo "PREFLIGHT_RC=$rc"; exit "$rc"
}
ln -s "$WORK/probe-profile" /root/.openclaw-l4probe
probe="$(openclaw --profile l4probe plugins list 2>&1 || true)"
if echo "$probe" | grep -qiE '^\s*(headroom|tokenless)\b.*enabled'; then
    fail "a scratch profile inherits an enabled plugin; the baseline arm would be contaminated"
    echo "$probe" | grep -iE 'headroom|tokenless'
else
    pass "scratch profile carries no enabled compressor plugin"
fi

echo "=== 2. both plugins present but globally disabled ==="
glist="$(openclaw plugins list 2>&1 || true)"
for p in tokenless headroom; do
    line="$(echo "$glist" | grep -i "$p" || true)"
    if [ -z "$line" ]; then
        fail "$p is not installed; its arm cannot run"
    elif echo "$line" | grep -qiE '\benabled\b'; then
        # A globally enabled plugin leaks into the baseline arm (the per-profile
        # symlink is supposed to be the only activation path), which voids every
        # delta. Installed-but-enabled is worse than absent, so assert disabled
        # rather than mere presence.
        fail "$p is installed but globally ENABLED; it would contaminate the baseline arm"
        echo "$line"
    else
        pass "$p is installed and not globally enabled"
    fi
done

echo "=== 3. dataset reachable and instance list fully covered ==="
# A missing instance id would otherwise surface as a silently smaller denominator.
"$WORK/venv-swe/bin/python" - "$LIST" <<'PY'
import sys
from datasets import load_dataset

list_path = sys.argv[1]
ids = set(load_dataset("princeton-nlp/SWE-bench_Lite", split="test")["instance_id"])
want = [line.strip() for line in open(list_path, encoding="utf-8") if line.strip()]
missing = [i for i in want if i not in ids]
print(f"  lite_test_size={len(ids)} wanted={len(want)} missing={len(missing)}")
if not want:
    # An empty list is not "all present": it is a preflight that would let a
    # zero-instance sweep past, so fail rather than vacuously pass.
    print("  FAIL  instance list is empty")
    sys.exit(1)
if missing:
    print("  FAIL  missing instance ids:", missing[:10])
    sys.exit(1)
print("  PASS  every wanted instance exists in the split")
PY
[ $? -eq 0 ] || fail "dataset coverage check failed"

echo "=== 4. headroom proxy CLI callable ==="
if "$WORK/venv-headroom/bin/headroom" --help >/dev/null 2>&1; then
    pass "headroom CLI responds"
else
    fail "headroom CLI not callable; the compression arms cannot run"
fi
# The CLI is pure Python and responds even when maturin left the native _core
# extension out of the tree; the proxy that arms c and d actually run does not.
# Checked separately so a missing _core fails here, in the free preflight, rather
# than as an unactivated proxy in the dry run.
if "$WORK/venv-headroom/bin/python" -c "import headroom._core" >/dev/null 2>&1; then
    pass "headroom native proxy extension (_core) importable"
else
    fail "headroom._core missing; the proxy arms cannot activate (build patchelf gap)"
fi

echo "=== 5. tree-sitter grammars resolve offline ==="
# --code-aware silently degrades without these, which would understate the
# compression arm rather than failing it.
if "$WORK/venv-headroom/bin/python" -c \
        "from tree_sitter_language_pack import get_parser; get_parser('python')" >/dev/null 2>&1; then
    pass "python grammar parses offline"
else
    fail "tree-sitter grammar unavailable; --code-aware would be a no-op"
fi

echo "=== 6. sampling controls are reachable from the CLI ==="
# The variance track depends on these; without them repeats are not samples.
help_out="$(cd "$WORK/runner" 2>/dev/null && \
    PYTHONPATH=src "$WORK/venv-swe/bin/python" -m swe_runner.cli run --help 2>&1 || true)"
for flag in --temperature --seed --base-config --headroom --tokenless --per-case-prompt --prompts-dir; do
    if echo "$help_out" | grep -q -- "$flag"; then
        pass "$flag present"
    else
        fail "$flag missing from the deployed runner"
    fi
done

echo "=== 7. docker and disk headroom ==="
if docker info >/dev/null 2>&1; then
    pass "docker daemon reachable"
else
    fail "docker daemon unreachable"
fi
avail_gb="$(df -BG --output=avail / | tail -1 | tr -dc '0-9')"
echo "  available=${avail_gb}G  images=$(docker images -q --filter reference='swebench/*' | wc -l)"
if [ "${avail_gb:-0}" -lt 30 ]; then
    fail "less than 30G free; instance images will exhaust the disk mid-run"
else
    pass "disk headroom sufficient to pull instance images"
fi

echo "=== 8. a per-instance prompt exists for every wanted instance ==="
# This is the run's most dangerous silent failure. build_openclaw_prompt() calls
# the lenient loader: a missing or empty prompt file logs one
# PER_CASE_PROMPT_SKIPPED line and the instance then runs UNGUIDED. The task
# still completes and still reports tokens, so the guided arm quietly becomes a
# second baseline and the prompt effect is diluted by an unknown amount.
# The corpus also lives outside this repository, so its presence cannot be
# assumed from a successful sync.
if [ ! -d "$PROMPTS" ]; then
    fail "prompt corpus missing at $PROMPTS; --per-case-prompt would degrade silently"
else
    missing_prompts=0
    empty_prompts=0
    wanted=0
    while read -r id; do
        [ -n "$id" ] || continue
        wanted=$((wanted + 1))
        if [ ! -f "$PROMPTS/$id" ]; then
            missing_prompts=$((missing_prompts + 1))
            [ "$missing_prompts" -le 5 ] && echo "    missing: $id"
        elif [ ! -s "$PROMPTS/$id" ]; then
            # An empty file passes an -f test but the loader treats it as absent.
            empty_prompts=$((empty_prompts + 1))
            [ "$empty_prompts" -le 5 ] && echo "    empty:   $id"
        fi
    done < "$LIST"
    echo "  wanted=$wanted missing=$missing_prompts empty=$empty_prompts" \
         "corpus_files=$(find "$PROMPTS" -type f | wc -l)"
    if [ "$wanted" -eq 0 ]; then
        # A zero-length list makes missing==empty==0 and would otherwise PASS,
        # certifying a sweep with no instances. The list is the run's
        # denominator, so an empty one is a hard failure here.
        fail "instance list is empty; nothing to run"
    elif [ "$missing_prompts" -eq 0 ] && [ "$empty_prompts" -eq 0 ]; then
        pass "every wanted instance has a non-empty prompt"
    else
        fail "$((missing_prompts + empty_prompts)) instances would run unguided"
    fi
fi

echo "PREFLIGHT_RC=$rc"
exit "$rc"
CHECKS
