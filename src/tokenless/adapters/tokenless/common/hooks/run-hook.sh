#!/usr/bin/env bash
# run-hook.sh — Locate and exec a shared tokenless hook script.
#
# Prefer hooks from the wrapper's own adapter tree so concurrent RPM, Makefile,
# and raw installations cannot cross-load another version. Hosts that copy the
# wrapper into a detached cache still use the historical FHS fallbacks.
#
# Usage:    run-hook.sh <hook-script-basename> [args...]
# Examples: run-hook.sh rewrite_hook.py
#           run-hook.sh compress_response_hook.py
#           run-hook.sh tool_ready_hook.sh
#
# Fail-open contract: any not-found / missing-interpreter condition emits
# an empty JSON object on stdout and exits 0, so the host never blocks
# on us.
#
# PreToolUse matcher overlap is by design: hooks.json registers both a
# Bash-specific entry (rewrite) and a catch-all entry (tool-ready). Bash
# tool calls therefore fire both — the host evaluates each matching
# matcher independently, so this is the documented way to attach a
# tool-specific hook alongside a global one. The timeout values are
# upper bounds; observed runtimes are well under them.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="${1:?usage: run-hook.sh <hook-script-basename> [args...]}"
shift

fail_open() { echo "{}"; exit 0; }

# Reject anything but a bare basename. Practical risk is low — hooks.json is
# RPM-installed root:root 0644 and unwritable at runtime — but a defense-in-
# depth check costs one line and stops any future caller from smuggling a
# traversal segment through this argument.
case "$SCRIPT" in
    */*|"") fail_open ;;
esac

CANDIDATES=(
    "${SCRIPT_DIR}/../../common/hooks/${SCRIPT}"
    "/usr/local/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}"
    "/usr/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}"
    "${HOME}/.local/share/anolisa/adapters/tokenless/common/hooks/${SCRIPT}"
)

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
