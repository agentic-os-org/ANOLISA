#!/usr/bin/env bash
# detect.sh — Probe the QwenWorkCN sideload state through lifecycle.py.
#
# lifecycle.py returns 0 whether or not the plugin is sideloaded and 1 for every
# failure, so its exit code cannot carry the adapter state. It publishes
# {"status": ...} on stdout for `tokenless init` instead; the wrapper does the
# same when python3 itself is missing.
set -euo pipefail
SCRIPT_DIR="$(CDPATH='' cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)"
if ! command -v python3 >/dev/null 2>&1; then
    printf '[tokenless] ERROR: python3 is required for QwenWorkCN.\n' >&2
    printf '{"status": "missing_prereqs"}\n'
    exit 1
fi
exec python3 -B "${SCRIPT_DIR}/lifecycle.py" detect "$@"
