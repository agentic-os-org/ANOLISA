#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Purge the state that could let a temperature-0 repeat replay an earlier run
# instead of genuinely re-calling the model. Intended to run BETWEEN repeats.
#
# What it clears (all regenerated on the next run):
#   - OpenClaw sandbox containers (the per-instance /testbed), by session label,
#     plus dangling volumes -- so each instance is re-prepared fresh from its
#     image at base_commit rather than inheriting a prior repeat's edits.
#   - OpenClaw's persistent agent-state DB (~/.openclaw/state/openclaw.sqlite*),
#     which is keyed by a deterministic per-instance session id and could
#     otherwise resume a stored trajectory.
#   - Stale run-local profile symlinks (~/.openclaw-*).
#
# What it deliberately PRESERVES:
#   - ~/.openclaw/openclaw.json (global config) and ~/.openclaw/extensions/*
#     (the tokenless and headroom plugins under test -- deleting them would make
#     an arm silently degrade to baseline).
#   - The prebuilt SWE-bench testbed images (the clean-checkout source) and the
#     HuggingFace dataset cache (read-only gold data).
set -uo pipefail

LABEL="openclaw.sessionKey"

echo "[clean] $(date -u +%FT%TZ) purging SWE + openclaw run state"

# 1. SWE testbed containers (label-scoped: never touches unrelated containers).
cids=$(docker ps -aq --filter "label=$LABEL" 2>/dev/null)
# Record the volumes those containers mount BEFORE removing them, so step 2 can
# stay scoped to this task rather than nuking every dangling volume on the host.
scoped_vols=""
if [ -n "$cids" ]; then
    scoped_vols=$(docker inspect -f '{{range .Mounts}}{{if .Name}}{{.Name}}{{"\n"}}{{end}}{{end}}' $cids 2>/dev/null | sort -u)
    echo "[clean] removing $(echo "$cids" | wc -l | tr -d ' ') openclaw sandbox container(s)"
    docker rm -f $cids >/dev/null 2>&1 || true
else
    echo "[clean] no openclaw sandbox containers present"
fi

# 2. Volumes those containers left behind. Scoped by default to the removed
# containers' now-dangling volumes; set L4_CLEAN_ALL_DANGLING_VOLUMES=1 to fall
# back to a host-wide `dangling=true` sweep. The default matters on a shared
# machine: an unconditional sweep would remove volumes belonging to unrelated
# tasks, contradicting step 1's label scoping.
dangling=$(docker volume ls -qf dangling=true 2>/dev/null)
if [ "${L4_CLEAN_ALL_DANGLING_VOLUMES:-0}" = "1" ]; then
    vols=$dangling
else
    vols=$(printf '%s\n' "$dangling" | grep -Fxf <(printf '%s\n' "$scoped_vols") 2>/dev/null || true)
fi
[ -n "$vols" ] && docker volume rm $vols >/dev/null 2>&1 || true

# 3. OpenClaw persistent agent-state DB (regenerated per run).
rm -f /root/.openclaw/state/openclaw.sqlite \
      /root/.openclaw/state/openclaw.sqlite-wal \
      /root/.openclaw/state/openclaw.sqlite-shm 2>/dev/null || true

# 4. Stale run-local profile symlinks.
find /root -maxdepth 1 -name '.openclaw-*' -type l -delete 2>/dev/null || true

# Report what survived, so a reader can confirm the plugins and images are intact.
echo "[clean] plugins kept: $(ls /root/.openclaw/extensions 2>/dev/null | tr '\n' ' ')"
echo "[clean] testbed images kept: $(docker images 2>/dev/null | grep -ciE 'sweb|swe-bench' || true)"
echo "[clean] done"
