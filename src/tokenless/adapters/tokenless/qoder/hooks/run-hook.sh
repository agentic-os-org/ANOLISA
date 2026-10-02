#!/usr/bin/env bash
# run-hook.sh — Locate and exec a shared Tokenless hook script.
#
# Prefer hooks from the wrapper's own adapter tree so source and staged
# installations stay version-aligned. A qodercli cache contains only the
# Qoder plugin, so packaged plugins use the ANOLISA FHS fallbacks.
#
# Fail open when the shared hook or its interpreter is unavailable so a
# Tokenless packaging problem never blocks a Qoder tool call.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="${1:?usage: run-hook.sh <hook-script-basename> [args...]}"
shift

fail_open() { echo "{}"; exit 0; }

case "$SCRIPT" in
    compress_response_hook.py|rewrite_hook.py|tool_ready_hook.sh) ;;
    *) fail_open ;;
esac

CANDIDATES=(
    "${SCRIPT_DIR}/../../common/hooks/${SCRIPT}"
    "/usr/local/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}"
    "/usr/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}"
)

# Minimal runners may omit HOME. Keep the user-local fallback conditional so
# an environment omission cannot bypass the fail-open behavior below.
if [ -n "${HOME:-}" ]; then
    CANDIDATES+=("${HOME}/.local/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}")
fi

for candidate in "${CANDIDATES[@]}"; do
    [ -f "$candidate" ] || continue
    case "$candidate" in
        *.py)
            command -v python3 >/dev/null 2>&1 || fail_open
            exec python3 "$candidate" "$@"
            ;;
        *.sh)
            exec bash "$candidate" "$@"
            ;;
        *)
            fail_open
            ;;
    esac
done

fail_open
