#!/usr/bin/env bash
# The Codex installer must skip registration cleanly when the host omits HOME.
# Regression test for #8771. No external dependencies: bash + coreutils only.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="$ROOT/adapters/tokenless/codex/scripts/install.sh"

sb="$(mktemp -d)"; trap 'rm -rf "$sb"' EXIT
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"
printf '#!/bin/sh\nexit 0\n' > "$sb/codex"; chmod 755 "$sb/codex"

fail() { echo "FAIL: $1" >&2; exit 1; }
run() { env -i PATH=/usr/local/bin:/usr/bin:/bin "$@"; }
no_marketplace() {
    if find "$sb" -name marketplace.json | grep -q .; then
        fail "$1: wrote a marketplace without a usable root"
    fi
}

# 1. Unresolvable CODEX_BIN + no HOME -> skip registration, exit 0 (#8771).
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/no-such-codex" \
    bash "$INSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "expected exit 0, got $rc:
$out"
case "$out" in
    *"codex CLI not found"*) ;;
    *) fail "expected the skip message, got:
$out" ;;
esac

# 2. No HOME and no XDG_DATA_HOME: the branch that replaced the /root sentinel.
#    A usable codex gets the script past Phase 1 and the CLI check, so this is
#    the block under test, not an earlier exit.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$INSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "expected exit 0 without HOME, got $rc:
$out"
case "$out" in
    *"neither HOME nor XDG_DATA_HOME"*) ;;
    *) fail "expected the no-root skip message, got:
$out" ;;
esac
case "$out" in
    */root/*|*"mkdir: cannot create directory"*) fail "fell back to a sentinel path:
$out" ;;
esac
no_marketplace "no-root path"

# 3. A usable XDG_DATA_HOME still registers normally (stub codex accepts everything).
rc=0
run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    XDG_DATA_HOME="$sb/xdg" \
    CODEX_BIN="$sb/codex" \
    bash "$INSTALL" --non-interactive >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "XDG_DATA_HOME path regressed, exit $rc"
[ -f "$sb/xdg/anolisa/codex-marketplace/.agents/plugins/marketplace.json" ] \
    || fail "marketplace.json not written under XDG_DATA_HOME"
rm -rf "$sb/xdg"

# 4. Dry run without any usable root reports the skip and writes nothing.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$INSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run without HOME exited $rc:
$out"
case "$out" in
    *"DRY-RUN: neither HOME nor XDG_DATA_HOME"*) ;;
    *) fail "dry-run did not report the missing root:
$out" ;;
esac
no_marketplace "dry-run without a root"

# 5. Dry run with HOME previews the writes and stops before making them.
rc=0
out="$(run HOME="$sb/home" \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$INSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run exited $rc:
$out"
case "$out" in
    *"DRY-RUN: mkdir -p"*) ;;
    *) fail "dry-run preview missing its write plan:
$out" ;;
esac
[ ! -e "$sb/home/.local/share/anolisa" ] || fail "dry-run created the marketplace tree"

echo "PASS: codex install tolerates an unset HOME"
