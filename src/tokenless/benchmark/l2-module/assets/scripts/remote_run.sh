#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Full L2 run: sync sources, execute l2_compare remotely, pull reports back.
#
# Required environment:
#   L2_SSH_HOST         remote host or IP
#   L2_SSH_PASS         ssh password
# Optional:
#   L2_SSH_USER         remote user (default: root)
#   L2_REMOTE_WORK      remote workspace root (default: /root/work)
#   DASHSCOPE_API_KEY   enables the semantic probe when set
#
# All extra arguments are forwarded to l2_compare, e.g.:
#   ./remote_run.sh --categories json,code --n 10 --no-probe

set -euo pipefail

: "${L2_SSH_HOST:?L2_SSH_HOST is required (remote host or IP)}"
: "${L2_SSH_PASS:?L2_SSH_PASS is required (ssh password)}"
L2_SSH_USER="${L2_SSH_USER:-root}"
# Remote workspace root, kept in sync with remote_sync.sh / remote_setup.sh.
L2_REMOTE_WORK="${L2_REMOTE_WORK:-/root/work}"
export L2_REMOTE_WORK

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)

"$SCRIPT_DIR/remote_sync.sh"

# Rebuild after the sync. rsync excludes target/, so the previously built
# l2_compare, tokenless library and rtk binary all survive a source update:
# running them directly would benchmark stale artifacts while the report reads
# the freshly synced Git SHA, i.e. attribute numbers to code that never ran.
# remote_setup.sh is idempotent and rebuilds all three, so it is cheap when
# nothing changed and correct when something did.
echo "[run] rebuilding measured artifacts against the synced revision"
"$SCRIPT_DIR/remote_setup.sh"

# The API key travels only through the ssh session environment — never into
# a file or a script on either machine. It is %q-escaped like the CLI args:
# a key containing quotes or shell metacharacters must not be able to break
# (or extend) the remote command line.
SAFE_DASHSCOPE_API_KEY="$(printf '%q' "${DASHSCOPE_API_KEY:-}")"
#
# Arguments are %q-escaped one by one so each local argv element stays one
# remote argv element — the remote shell re-splits the command string, and
# an unquoted $* would break args with spaces (or interpret metacharacters).
REMOTE_ARGS=""
for arg in "$@"; do
    REMOTE_ARGS+=" $(printf '%q' "$arg")"
done

echo "[run] executing l2_compare on $L2_SSH_HOST"
SSHPASS="$L2_SSH_PASS" sshpass -e ssh "${SSH_OPTS[@]}" "$L2_SSH_USER@$L2_SSH_HOST" \
    "DASHSCOPE_API_KEY=$SAFE_DASHSCOPE_API_KEY \
     HEADROOM_PYTHON=$L2_REMOTE_WORK/headroom-venv/bin/python \
     RTK_BIN=$L2_REMOTE_WORK/anolisa/src/tokenless/third_party/rtk/target/release/rtk \
     $L2_REMOTE_WORK/anolisa/src/tokenless/benchmark/l2-module/target/release/l2_compare$REMOTE_ARGS"

# Reports live in the workspace root's reports/ directory, two levels above
# this script (assets/scripts/) — not under assets/.
LOCAL_REPORTS="$SCRIPT_DIR/../../reports"
mkdir -p "$LOCAL_REPORTS"
echo "[run] pulling reports back to $LOCAL_REPORTS"
SSHPASS="$L2_SSH_PASS" sshpass -e rsync -az -e "ssh ${SSH_OPTS[*]}" \
    "$L2_SSH_USER@$L2_SSH_HOST:$L2_REMOTE_WORK/anolisa/src/tokenless/benchmark/l2-module/reports/" \
    "$LOCAL_REPORTS/"

echo "[run] done — see $LOCAL_REPORTS/L2_MODULE_COMPARISON_REPORT.md"
