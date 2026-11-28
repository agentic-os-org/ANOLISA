#!/usr/bin/env bash
# A Codex uninstall without HOME must fail closed with an actionable message
# instead of aborting on an unbound variable. Regression test for #8807.
# Explicit TOKENLESS_INSTALL_PREFIX / XDG_DATA_HOME / CODEX_HOME overrides stay
# authoritative. No external dependencies: bash + coreutils only.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNINSTALL="$ROOT/adapters/tokenless/codex/scripts/uninstall.sh"

sb="$(mktemp -d -p .)"; trap 'rm -rf "$sb"' EXIT
fail() { echo "FAIL: $1" >&2; exit 1; }
run() { env -i PATH=/usr/local/bin:/usr/bin:/bin "$@"; }
has() {
    case "$1" in
        *"$2"*) ;;
        *) fail "$3: missing '$2' in:
$1" ;;
    esac
}

# 1. No HOME and no overrides: actionable failure, no unbound-variable dump.
rc=0
out="$(run bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 1 ] || fail "expected exit 1 without HOME/overrides, got $rc:
$out"
has "$out" "TOKENLESS_INSTALL_PREFIX or HOME must be set" \
    "no-root invocation must name the missing setting"
has "$out" "Nothing was removed" \
    "no-root invocation must state that no removal happened"
case "$out" in
    *"unbound variable"*) fail "exposed a set -u abort:
$out" ;;
esac

# 2. Explicit prefix: the binary is still found and removed without HOME.
prefix="$sb/prefix"
mkdir -p "$prefix/bin"
printf '#!/bin/sh\nexit 0\n' > "$prefix/bin/tokenless"; chmod 755 "$prefix/bin/tokenless"
codex_stub="$sb/codex"
printf '#!/bin/sh\nexit 0\n' > "$codex_stub"; chmod 755 "$codex_stub"
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$prefix" \
    CODEX_BIN="$codex_stub" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "explicit prefix without HOME should exit 0, got $rc:
$out"
[ ! -e "$prefix/bin/tokenless" ] || fail "explicit prefix did not remove the binary"
case "$out" in
    *"unbound variable"*) fail "explicit prefix hit a set -u abort:
$out" ;;
esac
# No XDG_DATA_HOME and no HOME: no marketplace path can be named, and a
# relative/root-level path must never be removed.
has "$out" "no marketplace directory was removed" \
    "missing marketplace root must be reported explicitly"

# 3. Explicit XDG_DATA_HOME: marketplace directory is still cleaned.
xdg="$sb/xdg"
market="$xdg/anolisa/codex-marketplace"
mkdir -p "$market"
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$prefix" \
    XDG_DATA_HOME="$xdg" \
    CODEX_BIN="$codex_stub" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "explicit XDG_DATA_HOME without HOME should exit 0, got $rc:
$out"
[ ! -d "$market" ] || fail "explicit XDG_DATA_HOME did not remove the marketplace directory"

# 4. Explicit CODEX_HOME: its config.toml still decides the no-CLI branch.
codex_home="$sb/codex-home"
mkdir -p "$codex_home"
printf 'tokenless@anolisa-tokenless\n' > "$codex_home/config.toml"
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$prefix" \
    CODEX_HOME="$codex_home" \
    CODEX_BIN="$sb/missing-codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 1 ] || fail "no CLI + surviving registration must exit 1, got $rc:
$out"
has "$out" "config.toml still mentions" \
    "explicit CODEX_HOME must be consulted for the registration check"

# 5. Dry run without a root reports the skip and removes nothing.
mkdir -p "$market"
rc=0
out="$(run ANOLISA_DRY_RUN=1 \
    TOKENLESS_INSTALL_PREFIX="$prefix" \
    CODEX_BIN="$codex_stub" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry run without HOME exited $rc:
$out"
has "$out" "no marketplace directory can be removed" \
    "dry run must preview the missing marketplace root"
[ -d "$market" ] || fail "dry run removed an existing marketplace directory"

echo "PASS: codex uninstall fails closed without HOME"
