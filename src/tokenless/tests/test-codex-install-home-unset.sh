#!/usr/bin/env bash
# The Codex installer must skip registration cleanly when the host omits HOME.
# Regression test for #8771. No external dependencies: bash + coreutils only.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="$ROOT/adapters/tokenless/codex/scripts/install.sh"

sb="$(mktemp -d)"; trap 'rm -rf "$sb"' EXIT
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"

fail() { echo "FAIL: $1" >&2; exit 1; }

# 1. Unresolvable CODEX_BIN + no HOME -> skip registration, exit 0 (#8771).
rc=0
out="$(env -i PATH=/usr/local/bin:/usr/bin:/bin \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/no-such-codex" \
    bash "$INSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "expected exit 0, got $rc:
$out"
case "$out" in
    *"codex CLI not found"*) ;;
    *) fail "expected the skip message, got:
$out" ;;
esac

# 2. No HOME and no XDG_DATA_HOME must never produce a rooted or privileged path.
case "$out" in
    */root/*|*"mkdir: cannot create directory"*) fail "fell back to a sentinel path:
$out" ;;
esac

# 3. A usable XDG_DATA_HOME still registers normally (stub codex accepts everything).
printf '#!/bin/sh\nexit 0\n' > "$sb/codex"; chmod 755 "$sb/codex"
rc=0
env -i PATH=/usr/local/bin:/usr/bin:/bin \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    XDG_DATA_HOME="$sb/xdg" \
    CODEX_BIN="$sb/codex" \
    bash "$INSTALL" --non-interactive >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "XDG_DATA_HOME path regressed, exit $rc"
[ -f "$sb/xdg/anolisa/codex-marketplace/.agents/plugins/marketplace.json" ] \
    || fail "marketplace.json not written under XDG_DATA_HOME"

echo "PASS: codex install tolerates an unset HOME"
