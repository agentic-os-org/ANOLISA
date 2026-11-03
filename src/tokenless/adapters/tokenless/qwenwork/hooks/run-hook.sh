#!/usr/bin/env bash
# Keep the desktop plugin self-contained; never fall back to unrelated FHS hooks.
set -euo pipefail

fail_open() { printf '{}\n'; exit 0; }

[ "$(uname -s 2>/dev/null)" = Darwin ] || fail_open
case "${1:-}" in
    rewrite_hook.py|compress_response_hook.py) SCRIPT="$1"; shift ;;
    *) fail_open ;;
esac
command -v python3 >/dev/null 2>&1 || fail_open
SCRIPT_DIR="$(CDPATH='' cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)" || fail_open
SHARED="${SCRIPT_DIR}/shared"
for resource in "$SCRIPT" hook_utils.py tool_categories.json; do
    [ -r "${SHARED}/${resource}" ] || fail_open
done
exec python3 -B "${SHARED}/${SCRIPT}" "$@"
