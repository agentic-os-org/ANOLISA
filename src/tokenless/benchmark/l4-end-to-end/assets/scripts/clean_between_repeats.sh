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
#
# ~ is the workspace's private HOME ($L4_REMOTE_WORK/home, see remote_setup.sh
# ISOLATION), never the host user's. The container sweep matches every OpenClaw
# sandbox on the host by label, which on a shared machine includes sandboxes that
# are not the benchmark's; it therefore runs only with L4_CLEAN_CONTAINERS=1.
set -uo pipefail

W=${L4_REMOTE_WORK:-/root/l4}
# shellcheck source=l4_env.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/l4_env.sh" "$W"
LABEL="openclaw.sessionKey"

echo "[clean] $(date -u +%FT%TZ) purging SWE + openclaw run state under $HOME"

# 1. SWE testbed containers (label-scoped: never touches unrelated containers).
mkdir -p "$W/logs" || exit 1
printf '%s repeat=%s phase=%s outcome=started\n' "$(date -u +%FT%TZ)" \
    "${L4_REPEAT:-unknown}" "${L4_CLEAN_PHASE:-manual}" >> "$W/logs/cleanup.provenance" || exit 1
cids=$(docker ps -aq --filter "label=$LABEL" 2>/dev/null) || {
    echo "[clean] cannot inspect sandbox containers; cleanliness is unverified" >&2
    exit 1
}
mkdir -p "$W/logs" || exit 1
printf '%s containers_present=%s cleanup_enabled=%s\n' "$(date -u +%FT%TZ)" \
    "$([ -n "$cids" ] && echo true || echo false)" "${L4_CLEAN_CONTAINERS:-0}" >> "$W/logs/cleanup.provenance" || exit 1
if [ "${L4_CLEAN_CONTAINERS:-0}" != "1" ] && [ -n "$cids" ]; then
    echo "[clean] sandbox containers exist; explicit L4_CLEAN_CONTAINERS=1 is required on a dedicated host" >&2
    exit 1
fi
# Record the volumes those containers mount BEFORE removing them, so step 2 can
# stay scoped to this task rather than nuking every dangling volume on the host.
scoped_vols=""
if [ -n "$cids" ]; then
    scoped_vols=$(docker inspect -f '{{range .Mounts}}{{if .Name}}{{.Name}}{{"\n"}}{{end}}{{end}}' $cids 2>/dev/null | sort -u) || {
            echo "[clean] cannot inspect sandbox volumes; cleanliness is unverified" >&2
            exit 1
        }
    echo "[clean] removing $(echo "$cids" | wc -l | tr -d ' ') openclaw sandbox container(s)"
    docker rm -f $cids >/dev/null 2>&1 || { echo "[clean] container removal failed" >&2; exit 1; }
else
    echo "[clean] no openclaw sandbox containers present"
fi

# 2. Volumes those containers left behind. Scoped by default to the removed
# containers' now-dangling volumes; set L4_CLEAN_ALL_DANGLING_VOLUMES=1 to fall
# back to a host-wide `dangling=true` sweep. The default matters on a shared
# machine: an unconditional sweep would remove volumes belonging to unrelated
# tasks, contradicting step 1's label scoping.
dangling=$(docker volume ls -qf dangling=true 2>/dev/null) || {
    echo "[clean] cannot inspect dangling volumes; cleanliness is unverified" >&2
    exit 1
}
if [ "${L4_CLEAN_ALL_DANGLING_VOLUMES:-0}" = "1" ]; then
    vols=$dangling
else
    vols=$(printf '%s\n' "$dangling" | grep -Fxf <(printf '%s\n' "$scoped_vols") 2>/dev/null || true)
fi
if [ -n "$vols" ]; then
    docker volume rm $vols >/dev/null 2>&1 || { echo "[clean] volume removal failed" >&2; exit 1; }
fi

# 3. OpenClaw persistent agent-state DB (regenerated per run).
rm -f "$HOME/.openclaw/state/openclaw.sqlite" \
      "$HOME/.openclaw/state/openclaw.sqlite-wal" \
      "$HOME/.openclaw/state/openclaw.sqlite-shm" 2>/dev/null || exit 1

# 4. Stale run-local profile symlinks.
find "$HOME" -maxdepth 1 -name '.openclaw-*' -type l -delete 2>/dev/null || exit 1

# Report what survived, so a reader can confirm the plugins and images are intact.
echo "[clean] plugins kept: $(ls "$HOME/.openclaw/extensions" 2>/dev/null | tr '\n' ' ')"
echo "[clean] testbed images kept: $(docker images 2>/dev/null | grep -ciE 'sweb|swe-bench' || true)"
printf '%s repeat=%s phase=%s outcome=completed\n' "$(date -u +%FT%TZ)" \
    "${L4_REPEAT:-unknown}" "${L4_CLEAN_PHASE:-manual}" >> "$W/logs/cleanup.provenance" || exit 1
echo "[clean] done"
