#!/usr/bin/env bash
# The Codex uninstaller must not abort under `set -u` when the host omits
# HOME, and must skip the home-derived removals with a diagnostic instead.
# Regression test for #9907. No external dependencies: bash + coreutils only.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNINSTALL="$ROOT/adapters/tokenless/codex/scripts/uninstall.sh"

sb="$(mktemp -d)"; trap 'rm -rf "$sb"' EXIT
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"
printf '#!/bin/sh\nexit 0\n' > "$sb/codex"; chmod 755 "$sb/codex"
# A marketplace tree that a HOME-ful run must remove and a HOME-less run must skip.
mk_marketplace() { install -D /dev/null "$1/anolisa/codex-marketplace/.agents/plugins/marketplace.json"; }

fail() { echo "FAIL: $1" >&2; exit 1; }
run() { env -i PATH=/usr/local/bin:/usr/bin:/bin "$@"; }

# 1. No HOME at all: the reported abort (issue #9907) must not happen — the
#    script deregisters, skips the unknown marketplace location with a
#    diagnostic, removes the binary, and exits 0.
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "expected exit 0 without HOME, got $rc:
$out"
case "$out" in
    *"unbound variable"*) fail "still trips over the unset HOME:
$out" ;;
esac
case "$out" in
    *"marketplace directory location unknown"*) ;;
    *) fail "expected the no-root marketplace skip message, got:
$out" ;;
esac
[ ! -e "$sb/prefix/bin/tokenless" ] || fail "binary not removed without HOME"
# With an empty PREFIX the script never builds a path at all — nothing
# root-relative exists to touch, including /bin (usr-merge with /usr/bin).
[ ! -e /bin/tokenless ] || fail "removed /bin/tokenless"

# 2. XDG_DATA_HOME alone is a usable root: the marketplace directory is removed.
mk_marketplace "$sb/xdg"
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    XDG_DATA_HOME="$sb/xdg" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "XDG_DATA_HOME path regressed, exit $rc:
$out"
[ ! -d "$sb/xdg/anolisa/codex-marketplace" ] || fail "marketplace directory not removed under XDG_DATA_HOME"

# 3. HOME alone is a usable root too.
mk_marketplace "$sb/home/.local/share"
rc=0
out="$(run HOME="$sb/home" \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "HOME path regressed, exit $rc:
$out"
[ ! -d "$sb/home/.local/share/anolisa/codex-marketplace" ] || fail "marketplace directory not removed under HOME"

# 4. Dry run without HOME: reports the nothing-registered decision and removes nothing.
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"
mk_marketplace "$sb/xdg"
rc=0
out="$(run TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run without HOME exited $rc:
$out"
case "$out" in
    *"neither XDG_DATA_HOME nor HOME is set"*) ;;
    *) fail "dry-run did not report the no-root decision:
$out" ;;
esac
case "$out" in
    *"/anolisa/codex-marketplace"*) fail "dry-run previewed a root-relative sentinel path:
$out" ;;
esac
[ -f "$sb/prefix/bin/tokenless" ] || fail "dry run removed the binary"
rm -rf "$sb/xdg"

# 5. Dry run with HOME previews the no-CLI config check against $HOME/.codex.
rc=0
out="$(run HOME="$sb/home" \
    TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/no-such-codex" \
    ANOLISA_DRY_RUN=1 \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "dry-run with HOME exited $rc:
$out"
case "$out" in
    *"records no tokenless registration"*) ;;
    *) fail "dry-run did not preview the config check:
$out" ;;
esac

# 6. Shared-helper path: USER_HOME resolution in _common.sh still finds
#    $HOME/.local/bin/codex when the CLI is not on PATH (no-CODEX_BIN run).
#    Hermeticity: resolve_codex probes /usr/local/bin/codex and /usr/bin/codex
#    ahead of the USER_HOME branch, so an ambient packaged CLI would win and
#    the scenario would deregister against the host instead of the sandbox.
#    Skip when such a CLI exists rather than pinning a fake over the probe
#    paths (which we cannot, and must not, touch).
if [ ! -x /usr/local/bin/codex ] && [ ! -x /usr/bin/codex ] && ! command -v codex >/dev/null 2>&1; then
    mkdir -p "$sb/home/.local/bin"
    printf '#!/bin/sh\nexit 0\n' > "$sb/home/.local/bin/codex"; chmod 755 "$sb/home/.local/bin/codex"
    rc=0
    out="$(run HOME="$sb/home" \
        TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
        bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
    [ "$rc" = 0 ] || fail "shared-helper HOME resolution regressed, exit $rc:
$out"
    case "$out" in
        *"Uninstall complete"*) ;;
        *) fail "expected the resolved-CLI deregistration path, got:
$out" ;;
    esac
else
    echo "SKIP: scenario 6 (ambient codex CLI at the absolute probe paths)"
fi

# 7. Empty HOME (`HOME=`) is not a usable root either: same skip-with-diagnostic
#    behavior as the unset case, and no removal anywhere.
install -D -m 755 /usr/bin/true "$sb/prefix/bin/tokenless"
rc=0
out="$(run HOME= TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "empty HOME regressed, exit $rc:
$out"
case "$out" in
    *"marketplace directory location unknown"*) ;;
    *) fail "empty HOME did not take the no-root skip, got:
$out" ;;
esac
[ ! -e "$sb/prefix/bin/tokenless" ] || fail "binary not removed with empty HOME"

# 8. A relative HOME is discarded by the `case /*)` normalization, exactly like
#    an unset one.
rc=0
out="$(run HOME=relative/path TOKENLESS_INSTALL_PREFIX="$sb/prefix" \
    CODEX_BIN="$sb/codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 0 ] || fail "relative HOME regressed, exit $rc:
$out"
case "$out" in
    *"marketplace directory location unknown"*) ;;
    *) fail "relative HOME did not take the no-root skip, got:
$out" ;;
esac
[ ! -e "relative/path/.local/share/anolisa" ] || fail "relative HOME was used as a root"

# 9. CODEX_HOME set while HOME is unset: the config check runs against
#    $CODEX_HOME and a tokenless-naming config keeps the marketplace.
install -D /dev/null "$sb/codex-home/config.toml"
printf '[plugins."tokenless@anolisa-tokenless"]\nenabled = true\n' > "$sb/codex-home/config.toml"
rc=0
out="$(run CODEX_HOME="$sb/codex-home" \
    CODEX_BIN="$sb/no-such-codex" \
    bash "$UNINSTALL" --non-interactive 2>&1)" || rc=$?
[ "$rc" = 1 ] || fail "CODEX_HOME with a live registration must fail closed, got $rc:
$out"
case "$out" in
    *"registration cannot be confirmed gone"*) ;;
    *) fail "CODEX_HOME path did not fail closed on a live registration, got:
$out" ;;
esac

echo "PASS: codex uninstall tolerates an unset HOME"
