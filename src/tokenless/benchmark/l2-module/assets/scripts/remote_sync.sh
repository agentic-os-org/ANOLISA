#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Sync the local anolisa and headroom source trees to the remote Linux host.
#
# Required environment:
#   L2_SSH_HOST   remote host or IP
#   L2_SSH_PASS   ssh password (never hard-coded here)
# Optional:
#   L2_SSH_USER   remote user            (default: root)
#   L2_REMOTE_WORK remote workspace root (default: /root/work)
#   HEADROOM_SRC  local headroom checkout (default: ~/git_repo/headroom)
#
# Heavy build artefacts (target/, .venv, node_modules) are excluded: the
# remote builds from source, and shipping macOS artefacts to Linux would only
# waste bandwidth and risk stale-binary confusion.

set -euo pipefail

: "${L2_SSH_HOST:?L2_SSH_HOST is required (remote host or IP)}"
: "${L2_SSH_PASS:?L2_SSH_PASS is required (ssh password)}"
L2_SSH_USER="${L2_SSH_USER:-root}"
# Remote workspace root. Defaults to /root/work for the usual throwaway root
# box; override it together with L2_SSH_USER when running as an ordinary
# account, e.g. L2_SSH_USER=ubuntu L2_REMOTE_WORK=/home/ubuntu/work.
L2_REMOTE_WORK="${L2_REMOTE_WORK:-/root/work}"
HEADROOM_SRC="${HEADROOM_SRC:-$HOME/git_repo/headroom}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Ask git for the checkout root rather than counting ".." hops: the hop count
# already broke once when the benchmark was split into per-layer workspaces,
# and an off-by-one here silently syncs the parent of the repository.
ANOLISA_SRC="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$ANOLISA_SRC" ]; then
    # Not a git checkout (e.g. an exported tarball): fall back to the literal
    # layout scripts/ -> assets/ -> l2-module/ -> benchmark/ -> tokenless/ ->
    # src/ -> repo root.
    ANOLISA_SRC="$(cd "$SCRIPT_DIR/../../../../../.." && pwd)"
fi
if [ ! -d "$ANOLISA_SRC/src/tokenless" ]; then
    echo "error: $ANOLISA_SRC does not look like the anolisa checkout" \
         "(no src/tokenless); set up the script inside the repository" >&2
    exit 1
fi

if [ ! -d "$HEADROOM_SRC" ]; then
    echo "error: headroom source not found at $HEADROOM_SRC (set HEADROOM_SRC)" >&2
    exit 1
fi

# Throwaway benchmark host: skip host-key pinning so reprovisioned machines
# do not break the pipeline.
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
# third_party/rtk is excluded on purpose: it is a gitignored pinned clone
# (just setup-rtk fetches v0.49.0 and applies the tokenless patches). Syncing a
# developer's local rtk could carry edits beyond the pinned upstream commit
# and attribute results from those sources to the ANOLISA SHA.
# Leaving it out forces the remote to build the
# pinned tree, whose revision the report then records.
RSYNC_EXCLUDES=(
    --exclude target
    --exclude .venv
    --exclude node_modules
    --exclude src/tokenless/third_party/rtk
)

# rsync only creates the final path component, not intermediate parents, so a
# fresh box without the workspace root would fail on the very first transfer.
# Create the destination root up front (idempotent).
echo "[sync] ensuring $L2_REMOTE_WORK exists on $L2_SSH_HOST"
SSHPASS="$L2_SSH_PASS" sshpass -e ssh "${SSH_OPTS[@]}" "$L2_SSH_USER@$L2_SSH_HOST" \
    "mkdir -p $L2_REMOTE_WORK"

echo "[sync] anolisa -> $L2_SSH_USER@$L2_SSH_HOST:$L2_REMOTE_WORK/anolisa"
SSHPASS="$L2_SSH_PASS" sshpass -e rsync -az --delete "${RSYNC_EXCLUDES[@]}" \
    -e "ssh ${SSH_OPTS[*]}" \
    "$ANOLISA_SRC/" "$L2_SSH_USER@$L2_SSH_HOST:$L2_REMOTE_WORK/anolisa/"

echo "[sync] headroom -> $L2_SSH_USER@$L2_SSH_HOST:$L2_REMOTE_WORK/headroom"
SSHPASS="$L2_SSH_PASS" sshpass -e rsync -az --delete "${RSYNC_EXCLUDES[@]}" \
    -e "ssh ${SSH_OPTS[*]}" \
    "$HEADROOM_SRC/" "$L2_SSH_USER@$L2_SSH_HOST:$L2_REMOTE_WORK/headroom/"

echo "[sync] done"
