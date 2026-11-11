#!/usr/bin/env bash
# The Codex uninstaller must work when the host omits HOME.
# Regression test for the install fix (#8771) applied to uninstall.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNINSTALL="$ROOT/adapters/tokenless/codex/scripts/uninstall.sh"

sb="$(mktemp -d)"; trap 'rm -rf "$sb"' EXIT
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"
printf '#!/bin/sh\nexit 0\n' > "$sb/codex"; chmod 755 "$sb/codex"

fail() { echo "FAIL: $1" >&2; exit 1; }
run() { env -i PATH=/usr/local/bin:/usr/bin:/bin TOKENLESS_DEREGISTER_ONLY=1 "$@"; }

# 1. Neither HOME nor CODEX_HOME is usable, no resolvable codex: the config
#    cannot be located, so the registration cannot be confirmed gone. That is
#    a failure (exit 1), not a clean state.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/no-such-codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 1 ] || fail "expected fail-closed exit 1 without HOME, got $rc:
$out"
case "$out" in
    *"the Codex config cannot"*) ;;
    *) fail "expected the unlocatable-config error, got:
$out" ;;
esac
case "$out" in
    *"cannot be confirmed gone"*) ;;
    *) fail "expected the fail-closed message, got:
$out" ;;
esac
case "$out" in
    *"No marketplace directory is known"*) ;;
    *) fail "expected the marketplace skip message, got:
$out" ;;
esac
case "$out" in
    */root/*|*"mkdir: cannot create directory"*|*"rm -rf  "*) fail "fell back to a sentinel path:
$out" ;;
esac

# 2. No HOME and no XDG_DATA_HOME, usable codex: still no marketplace path.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "usable-codex path exited $rc:
$out"
case "$out" in
    *"no marketplace directory to remove"*) ;;
    *) fail "expected the marketplace skip message, got:
$out" ;;
esac

# 3. A usable XDG_DATA_HOME still removes the marketplace directory.
mkdir -p "$sb/xdg/anolisa/codex-marketplace/.agents/plugins"
: > "$sb/xdg/anolisa/codex-marketplace/.agents/plugins/marketplace.json"
rc=0
run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    XDG_DATA_HOME="$sb/xdg" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "XDG_DATA_HOME path regressed, exit $rc"
[ ! -d "$sb/xdg/anolisa/codex-marketplace" ] || fail "marketplace directory was not removed"

# 4. Dry run without a usable root reports the skip instead of a path.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run without HOME exited $rc:
$out"
case "$out" in
    *"DRY-RUN: neither HOME nor XDG_DATA_HOME"*) ;;
    *) fail "dry-run did not report the missing root:
$out" ;;
esac

# 5. Dry run with HOME previews the removal.
rc=0
out="$(run HOME="$sb/home" \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run exited $rc:
$out"
case "$out" in
    *"DRY-RUN: rm -rf"*) ;;
    *) fail "dry-run preview missing the removal plan:
$out" ;;
esac

# 6. The shared helper must not expand a bare $HOME either. resolve_codex
#    probes /usr/local/bin/codex and /usr/bin/codex by absolute path, so this
#    asserts on the helper itself instead of on what the host happens to have.
COMMON="$ROOT/adapters/tokenless/codex/scripts/_common.sh"
out="$(env -i PATH=/usr/local/bin:/usr/bin:/bin \
    bash -c 'set -u; . "$1"; resolve_codex >/dev/null; echo survived' _ "$COMMON" 2>&1)"
case "$out" in
    *survived*) ;;
    *) fail "the shared helper still expands a bare \$HOME:
$out" ;;
esac

# 7. A dry run with an unusable config location must not print a bare
#    "rm -rf" with an empty path.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/no-such-codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry run without HOME exited $rc:
$out"
case "$out" in
    *"the Codex config cannot be located"*) ;;
    *) fail "dry run did not report the unlocatable config:
$out" ;;
esac
case "$out" in
    *"rm -rf  "*) fail "dry run printed an empty removal path:
$out" ;;
esac

# 8. An explicit relative prefix is rejected instead of resolving into the
#    caller's working directory.
rc=0
out="$(cd "$sb" && run TOKENLESS_INSTALL_PREFIX=prefix-relative \
    CODEX_BIN="$sb/no-such-codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 1 ] || fail "expected exit 1 for a relative prefix, got $rc:
$out"
case "$out" in
    *"must be an absolute path"*) ;;
    *) fail "relative prefix was not rejected:
$out" ;;
esac

# 9. A relative XDG_DATA_HOME is ignored (XDG basedir), so a directory in the
#    caller's working directory is never removed.
mkdir -p "$sb/cwd-xdg/anolisa/codex-marketplace/marker"
rc=0
(cd "$sb" && run HOME="$sb/home" \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    XDG_DATA_HOME=cwd-xdg \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive >/dev/null 2>&1) || rc=$?
[ "$rc" = 0 ] || fail "relative XDG_DATA_HOME run exited $rc"
[ -d "$sb/cwd-xdg/anolisa/codex-marketplace" ] || fail "a relative XDG_DATA_HOME resolved into the working directory"

echo "PASS: codex uninstall tolerates an unset HOME"
