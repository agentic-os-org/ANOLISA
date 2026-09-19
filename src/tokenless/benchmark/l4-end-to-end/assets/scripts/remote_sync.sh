#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Sync the source trees the L4 run needs to the remote Linux host, then verify
# the deployed runner imports and passes its unit suite.
#
# Required environment:
#   L4_SSH_HOST   remote host or IP
#   L4_SSH_PASS   ssh password (never hard-coded here)
# Optional:
#   L4_SSH_USER   remote user              (default: root)
#   L4_REMOTE_WORK remote workspace root   (default: /root/l4)
#   HEADROOM_SRC  local headroom checkout  (default: ~/git_repo/headroom)
#   PROMPTS_SRC   local prompt-corpus clone (default: ~/git_repo/swe-with-prompt)
#   L4_SKIP_TESTS set to 1 to skip the remote unit suite
#
# Run this before remote_setup.sh on a cold host, and again after any local code
# change. The unit suite at the end needs the venv that remote_setup.sh creates,
# so on the very first run it is skipped with a notice rather than failing.
#
# Heavy build artefacts (target/, .venv, node_modules) are excluded: the remote
# builds from source, and shipping macOS artefacts to Linux would only waste
# bandwidth and risk stale-binary confusion.

set -euo pipefail

: "${L4_SSH_HOST:?L4_SSH_HOST is required (remote host or IP)}"
# Needed only to bootstrap a host that does not have the key yet; see below.
L4_SSH_PASS="${L4_SSH_PASS:-}"
L4_SSH_USER="${L4_SSH_USER:-root}"
L4_REMOTE_WORK="${L4_REMOTE_WORK:-/root/l4}"
HEADROOM_SRC="${HEADROOM_SRC:-$HOME/git_repo/headroom}"
PROMPTS_SRC="${PROMPTS_SRC:-$HOME/git_repo/swe-with-prompt}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Ask git for the checkout root rather than counting ".." hops: the hop count
# already broke once when the benchmark was split into per-layer workspaces, and
# an off-by-one here silently syncs the parent of the repository.
ANOLISA_SRC="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$ANOLISA_SRC" ]; then
    # Not a git checkout (e.g. an exported tarball): fall back to the literal
    # layout scripts/ -> assets/ -> l4-end-to-end/ -> benchmark/ -> tokenless/ ->
    # src/ -> repo root.
    ANOLISA_SRC="$(cd "$SCRIPT_DIR/../../../../../.." && pwd)"
fi
if [ ! -d "$ANOLISA_SRC/src/tokenless" ]; then
    echo "error: $ANOLISA_SRC does not look like the anolisa checkout" \
         "(no src/tokenless); run this script from inside the repository" >&2
    exit 1
fi
if [ ! -d "$HEADROOM_SRC" ]; then
    echo "error: headroom source not found at $HEADROOM_SRC (set HEADROOM_SRC)" >&2
    exit 1
fi

# The per-instance prompt corpus lives in its own internal clone, outside both
# checkouts, because it is evaluation input rather than code under test.
PROMPTS_DIR="$PROMPTS_SRC/src/swe_runner/prompts"
if [ ! -d "$PROMPTS_DIR" ]; then
    echo "error: prompt corpus not found at $PROMPTS_DIR (set PROMPTS_SRC)" >&2
    exit 1
fi

# Throwaway benchmark host: skip host-key pinning so reprovisioned machines do
# not break the pipeline.
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)

# Key first, sshpass only to bootstrap a host that does not have the key yet.
# sshpass allocates a pty per connection and that setup races when several run
# concurrently in the background, which appears as ssh failing with "Bad file
# descriptor" -- indistinguishable from a network fault, and not one. Put in
# SSH_OPTS so rsync's `-e "ssh ${SSH_OPTS[*]}"` inherits it as well.
L4_SSH_KEY=${L4_SSH_KEY:-$HOME/.ssh/l4_eval_ed25519}
if [ -f "$L4_SSH_KEY" ]; then
    SSH_OPTS+=(-i "$L4_SSH_KEY" -o IdentitiesOnly=yes)
    SSH_PW=()
else
    : "${L4_SSH_PASS:?L4_SSH_PASS is required when $L4_SSH_KEY does not exist}"
    SSH_PW=(sshpass -p "$L4_SSH_PASS")
fi

# src/tokenless/third_party/rtk is excluded on purpose: it is a gitignored pinned
# clone. Syncing a developer's local rtk would let it bypass the pin and
# attribute results from an arbitrary rtk to the ANOLISA SHA.
#
# --no-owner/--no-group override the -a defaults. Preserving the macOS uid (501)
# on a root-only Linux box is meaningless, and it actively breaks provenance:
# git refuses to read a repository whose owner differs from the caller
# ("detected dubious ownership"), so `git rev-parse HEAD` fails and the report
# loses the one field that ties numbers to a revision.
#
# --partial because these hosts are also pulling multi-GB eval images over the
# same link, which starves the transfer down to tens of KB/s. Without it an
# interrupted sync discards the incomplete file and starts it over, and on a link
# this slow that is the difference between resuming and restarting.
RSYNC_EXCLUDES=(
    --exclude target
    --exclude .venv
    --exclude node_modules
    --exclude src/tokenless/third_party/rtk
    # dist/ is a gitignored build artefact in both the tokenless adapter and the
    # headroom plugin. Shipping the developer's local (macOS) build would let a
    # stale bundle stand in for the freshly synced source and be measured as the
    # component under test; remote_setup.sh rebuilds it from source on the host.
    --exclude dist
    --no-owner
    --no-group
    --partial
)

# rsync creates only the final path component, so a fresh box needs the root.
# rsync itself is not part of a minimal Alibaba Cloud Linux image, and its absence
# surfaces as "unexpected end of file" from the local rsync rather than as a
# missing-command error, so install it here rather than leaving the next reader to
# decode that. This is the only package this script installs; everything else is
# remote_setup.sh's job, but that script is itself delivered by this one.
echo "[sync] ensuring $L4_REMOTE_WORK and rsync exist on $L4_SSH_HOST"
${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" \
    "mkdir -p $L4_REMOTE_WORK && \
     { command -v rsync >/dev/null 2>&1 || \
       dnf install -y rsync >/dev/null 2>&1 || yum install -y rsync >/dev/null 2>&1; } && \
     command -v rsync >/dev/null 2>&1" \
    || { echo "error: rsync is unavailable on $L4_SSH_HOST and could not be installed" >&2; exit 1; }

# rsync prints nothing for minutes on a cold tree, which from the outside is
# indistinguishable from a hang -- that ambiguity already cost one aborted run.
# Each transfer therefore reports progress every 30s. What it reports is the
# remote tree's size, deliberately, and not a bare elapsed counter: a timer ticks
# just as happily on a stalled connection, whereas a total that stops growing is
# the signal actually worth acting on.
HEARTBEAT_PID=
# Every step here tolerates failure on purpose. Under `set -e` a kill aimed at an
# already-dead heartbeat returns 1 and takes the whole script down silently --
# which is exactly what happened once: a transfer that had in fact completed
# looked like an unexplained death, because the script died just before printing
# its own result. Progress reporting must never be able to fail what it reports on.
heartbeat_stop() {
    [ -n "$HEARTBEAT_PID" ] || return 0
    kill "$HEARTBEAT_PID" 2>/dev/null || true
    wait "$HEARTBEAT_PID" 2>/dev/null || true
    HEARTBEAT_PID=
}
trap heartbeat_stop EXIT

# ConnectTimeout is set here and not in SSH_OPTS because this probe must give up
# quickly: it runs while the host is busy and is only ever advisory.
remote_du() {
    ${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" -o ConnectTimeout=15 -o LogLevel=ERROR \
        "$L4_SSH_USER@$L4_SSH_HOST" "du -sh '$1' 2>/dev/null | cut -f1" 2>/dev/null | tr -d '\r'
}

# sync_tree <label> <remote-path> <rsync args...>
sync_tree() {
    local label=$1 dest=$2
    shift 2
    local started=$SECONDS rc=0
    echo "[sync] $label -> $dest"
    (
        while sleep 30; do
            # `|| true` on the probe too: these hosts refuse connections while
            # saturated, and without it the inherited `set -e` would end the
            # heartbeat at the first refusal -- silently, and exactly when it is
            # most wanted.
            sz=$(remote_du "$dest" || true)
            printf '[sync]   %s: %ds elapsed, remote %s\n' \
                "$label" "$((SECONDS - started))" "${sz:-?}"
        done
    ) &
    HEARTBEAT_PID=$!

    # Retried because rc=255 -- an ssh-layer failure, "Bad file descriptor" at
    # connect time -- appears sporadically on these links, but only when several
    # transfers run concurrently in the background. It survived the move from
    # sshpass to key auth, so the pty race was not the whole story and the residual
    # cause is not established; what is established is that it is transient and
    # host-random, and that a serial foreground sync never triggers it.
    #
    # Retrying is the correct response either way: --partial means an attempt
    # resumes rather than restarts, and a cross-continent link is entitled to drop
    # a connection. A real misconfiguration still fails all three attempts, so this
    # hides nothing -- each failed attempt is printed as it happens.
    local attempt
    for attempt in 1 2 3; do
        rc=0
        ${SSH_PW[@]+"${SSH_PW[@]}"} rsync "$@" || rc=$?
        [ "$rc" -ne 0 ] || break
        [ "$attempt" -lt 3 ] || break
        echo "[sync]   $label attempt $attempt failed (rc=$rc), retrying in $((attempt * 10))s"
        sleep $((attempt * 10))
    done

    heartbeat_stop
    [ "$rc" -eq 0 ] || { echo "error: $label transfer failed (rsync rc=$rc)" >&2; return "$rc"; }
    local sz
    sz=$(remote_du "$dest" || true)
    echo "[sync] $label complete in $((SECONDS - started))s, remote ${sz:-?}"
}

sync_tree anolisa "$L4_REMOTE_WORK/anolisa" \
    -az --delete "${RSYNC_EXCLUDES[@]}" \
    -e "ssh ${SSH_OPTS[*]}" \
    "$ANOLISA_SRC/" "$L4_SSH_USER@$L4_SSH_HOST:$L4_REMOTE_WORK/anolisa/"

sync_tree headroom "$L4_REMOTE_WORK/headroom" \
    -az --delete "${RSYNC_EXCLUDES[@]}" \
    -e "ssh ${SSH_OPTS[*]}" \
    "$HEADROOM_SRC/" "$L4_SSH_USER@$L4_SSH_HOST:$L4_REMOTE_WORK/headroom/"

# The corpus goes to its own directory, passed to the runner as --prompts-dir,
# rather than into the runner's packaged prompts/ path. Keeping it outside the
# synced checkout means `rsync --delete` on the code cannot wipe it, and the
# report can name the corpus revision independently of the code revision.
#
# A missing prompt file does NOT fail the run: openclaw's build_openclaw_prompt
# calls the soft loader, logs PER_CASE_PROMPT_SKIPPED and silently falls back to
# the generic prompt. That would leave one arm answering a different question
# from the other three while the run still reports success, so the corpus is
# fingerprinted here and remote_run.sh pre-checks every selected instance.
PROMPTS_REV="$(git -C "$PROMPTS_SRC" rev-parse HEAD 2>/dev/null || echo unknown)"
PROMPTS_BRANCH="$(git -C "$PROMPTS_SRC" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
PROMPTS_COUNT="$(find "$PROMPTS_DIR" -maxdepth 1 -type f | wc -l | tr -d ' ')"
sync_tree "prompt corpus" "$L4_REMOTE_WORK/prompts" \
    -az --delete --no-owner --no-group --partial \
    -e "ssh ${SSH_OPTS[*]}" \
    "$PROMPTS_DIR/" "$L4_SSH_USER@$L4_SSH_HOST:$L4_REMOTE_WORK/prompts/"
${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" \
    "printf 'corpus_rev=%s\ncorpus_branch=%s\ncorpus_files=%s\n' \
        '$PROMPTS_REV' '$PROMPTS_BRANCH' '$PROMPTS_COUNT' \
        > $L4_REMOTE_WORK/prompts.provenance"
echo "[sync] corpus $PROMPTS_BRANCH@${PROMPTS_REV:0:7}, $PROMPTS_COUNT files"

# remote_setup.sh builds tokenless from $L4_REMOTE_WORK/tokenless and installs
# the runner from $L4_REMOTE_WORK/runner. Point both at the synced checkout
# with symlinks rather than a second copy, so there is exactly one source of
# truth for the revision under test.
#
# The runner link is deliberately NOT called swe-runner: the per-instance prompt
# corpus lives in a separate internal clone that is also called swe-runner, and
# reusing the name here would either fail on an existing directory or shadow the
# corpus. That clone is left untouched by this script.
#
# A real directory at a link path means the host still carries an older
# tarball-based deployment. Refuse rather than replace: removing a directory that
# may hold the only copy of something is not this script's call to make.
echo "[sync] linking tokenless/ and runner/ into the workspace root"
${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" bash -s <<EOF || exit 1
set -eu
link_or_refuse() {
    target="\$1"; link="\$2"
    if [ -e "\$link" ] && [ ! -L "\$link" ]; then
        echo "error: \$link is a real directory, not a symlink." >&2
        echo "       It is most likely a leftover tarball deployment. Inspect it," >&2
        echo "       then remove or rename it by hand and re-run this script." >&2
        return 1
    fi
    ln -sfn "\$target" "\$link"
}
link_or_refuse $L4_REMOTE_WORK/anolisa/src/tokenless $L4_REMOTE_WORK/tokenless
link_or_refuse $L4_REMOTE_WORK/anolisa/src/benchmark/swe-runner $L4_REMOTE_WORK/runner
EOF

if [ "${L4_SKIP_TESTS:-0}" = "1" ]; then
    echo "[sync] done (unit suite skipped by request)"
    exit 0
fi

# On a cold host the venv does not exist yet, because remote_setup.sh has not
# run. Skipping loudly is right here: failing would block the very step that
# creates the missing venv.
if ! ${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" \
        "test -x $L4_REMOTE_WORK/venv-swe/bin/python"; then
    echo "[sync] done (no venv yet — run remote_setup.sh, then re-run this script" \
         "to exercise the unit suite)"
    exit 0
fi

# Run the runner's own unit suite on the host that will execute the benchmark.
# A harness that does not pass its tests here cannot be trusted to produce
# numbers here, and the failure is far cheaper to find now than mid-run.
echo "[sync] running the deployed runner's unit suite"
${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" \
    "cd $L4_REMOTE_WORK/runner && \
     PYTHONPATH=src $L4_REMOTE_WORK/venv-swe/bin/python -m pytest tests/unit -q \
        -p no:cacheprovider" \
    || { echo "[sync] remote unit suite FAILED — do not start a run" >&2; exit 1; }

echo "[sync] done"
