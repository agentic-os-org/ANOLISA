#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(CDPATH='' cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)"
if ! command -v python3 >/dev/null 2>&1; then
    printf '[tokenless] ERROR: python3 is required for QwenWorkCN.\n' >&2
    exit 1
fi
exec python3 -B "${SCRIPT_DIR}/lifecycle.py" uninstall "$@"
