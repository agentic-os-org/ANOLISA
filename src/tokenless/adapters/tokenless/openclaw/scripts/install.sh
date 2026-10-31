#!/usr/bin/env bash
# install.sh — Deploy the tokenless OpenClaw plugin via the openclaw CLI.
#
# Responsibility boundary (mirrors sec-core/openclaw-plugin/scripts/deploy.sh):
#   - This script ONLY deploys an already-built plugin.
#   - Compilation (index.ts -> dist/index.js) is the Makefile's job:
#       make -C src/tokenless build-openclaw-plugin
#     which `make install` runs automatically before `install-adapter-resources`
#     copies the result into $SHARE_DIR/openclaw.
#   - If dist/index.js is missing, exit with a clear error pointing at the
#     Makefile target. Do NOT compile here — adapters shouldn't invoke npm at
#     deploy time.
set -euo pipefail

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this installer with exit 1 -- the same status it otherwise reports for "not installed but installable",
# so the crash would reach the caller as a verdict instead of as an error.
# Resolve HOME once and derive every user-scope path from it; an empty value
# leaves the destination unresolved and the script refuses to act on a path
# under /.

HOME_DIR="${HOME:-}"

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

# Allow the orchestrator (or a packaging script) to inject a specific openclaw
# binary. Defaults to whatever `openclaw` resolves to on PATH.
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
# The OpenClaw home and state directory come from the explicit overrides
# first and from the home directory second. They stay empty when neither
# names a location, so the check below can tell "not configured" from
# "configured to /".
OPENCLAW_HOME="${OPENCLAW_HOME:-}"
if [ -z "$OPENCLAW_HOME" ] && [ -n "$HOME_DIR" ]; then
    OPENCLAW_HOME="$HOME_DIR/.openclaw"
fi
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
export PATH="${USER_LOCAL_BIN}${OPENCLAW_STATE_DIR:+${OPENCLAW_STATE_DIR%/}/bin:}/usr/local/bin:$PATH"

# The installer deploys the plugin into the OpenClaw state directory, so that
# directory has to be known before anything is written. A path built from a
# missing HOME would land under /, and an explicit OPENCLAW_HOME or
# OPENCLAW_STATE_DIR still names a real directory.
if [ -z "$OPENCLAW_STATE_DIR" ]; then
    echo "[${COMPONENT}] ERROR: cannot resolve the OpenClaw state directory: HOME is unset and no override names it." >&2
    echo "[${COMPONENT}] Set HOME, OPENCLAW_HOME or OPENCLAW_STATE_DIR to the directory to use, then retry." >&2
    exit 1
fi

PLUGIN_SRC="$ADAPTER_DIR/openclaw"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if [ ! -d "$PLUGIN_SRC" ]; then
    echo "[${COMPONENT}] Plugin source not found: $PLUGIN_SRC" >&2
    exit 1
fi

if [ ! -f "$PLUGIN_SRC/dist/index.js" ]; then
    echo "[${COMPONENT}] ERROR: $PLUGIN_SRC/dist/index.js is missing." >&2
    echo "[${COMPONENT}]        Build the plugin first:" >&2
    echo "[${COMPONENT}]            make -C src/tokenless build-openclaw-plugin" >&2
    echo "[${COMPONENT}]        (run by 'make install' automatically; only an issue when" >&2
    echo "[${COMPONENT}]         deploying a hand-assembled adapter directory)." >&2
    exit 1
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: require OpenClaw >= 2026.6.6 before installation"
    echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins install $PLUGIN_SRC --force"
    echo "DRY-RUN: add --dangerously-force-unsafe-install only while the advertised option still has effect (legacy hosts)"
    echo "DRY-RUN: add --accept-capabilities only if advertised by plugins install --help"
    exit 0
fi

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}) — skipping plugin installation."
    echo "[${COMPONENT}] Install OpenClaw first, then run this script again."
    exit 0
fi

if ! VERSION_OUTPUT="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" --version 2>&1)"; then
    echo "[${COMPONENT}] Cannot read OpenClaw version; require >= 2026.6.6." >&2
    exit 1
fi
VERSION_PATTERN='(^|[[:space:]])([0-9]{4})\.([0-9]{1,6})\.([0-9]{1,6})(-[[:alnum:].-]+)?(\+[[:alnum:].-]+)?([[:space:]]|$)'
if [[ ! "$VERSION_OUTPUT" =~ $VERSION_PATTERN ]]; then
    echo "[${COMPONENT}] Cannot identify OpenClaw version; require >= 2026.6.6." >&2
    exit 1
fi
VERSION_YEAR="${BASH_REMATCH[2]}"
VERSION_MINOR="${BASH_REMATCH[3]}"
VERSION_PATCH="${BASH_REMATCH[4]}"
VERSION_PRERELEASE="${BASH_REMATCH[5]}"
if (( 10#$VERSION_YEAR < 2026 \
    || (10#$VERSION_YEAR == 2026 && 10#$VERSION_MINOR < 6) \
    || (10#$VERSION_YEAR == 2026 && 10#$VERSION_MINOR == 6 && 10#$VERSION_PATCH < 6) )) \
    || { (( 10#$VERSION_YEAR == 2026 && 10#$VERSION_MINOR == 6 && 10#$VERSION_PATCH == 6 )) \
         && [ -n "$VERSION_PRERELEASE" ]; }; then
    echo "[${COMPONENT}] OpenClaw >= 2026.6.6 is required for live tool-result replacement." >&2
    exit 1
fi

INSTALL_ARGS=(plugins install "$PLUGIN_SRC" --force)
if ! INSTALL_HELP="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins install --help 2>&1)"; then
    printf '[%s] Cannot inspect OpenClaw installer options: %s\n' "$COMPONENT" "$INSTALL_HELP" >&2
    exit 1
fi
# Invoking this installer accepts declared capabilities. Older hosts must
# never receive an unsupported flag or a similarly named option.
if [[ "$INSTALL_HELP" =~ (^|[^[:alnum:]_.-])--accept-capabilities([^[:alnum:]_.-]|$) ]]; then
    INSTALL_ARGS+=(--accept-capabilities)
fi

# Legacy hosts gate installs on a safety scan of child_process usage whose only
# non-interactive bypass is --dangerously-force-unsafe-install. OpenClaw 2026.9.2
# keeps the token as a deprecated no-op: passing it there accomplishes nothing
# and breaks again once the token is removed outright, so keep it only while the
# advertised option still has effect (the scanner moved to
# security.installPolicy on no-op hosts). The npm package ships this script to
# macOS, where /bin/bash stays at 3.2 — no ${var,,} here.
UNSAFE_SUPPORT=absent
while IFS= read -r help_line; do
    if [[ "$help_line" =~ (^|[^[:alnum:]_.-])--dangerously-force-unsafe-install([^[:alnum:]_.-]|$) ]]; then
        help_line_lc="$(printf '%s' "$help_line" | tr '[:upper:]' '[:lower:]')"
        case "$help_line_lc" in
            *"no op"*|*"no-op"*) UNSAFE_SUPPORT=noop ;;
            *) UNSAFE_SUPPORT=effective; INSTALL_ARGS+=(--dangerously-force-unsafe-install) ;;
        esac
        break
    fi
done <<< "$INSTALL_HELP"

# Why the bypass is legitimate on the hosts that still need it: their scanner
# reads child_process imports as "dangerous code patterns", and the tokenless
# plugin does delegate to the tokenless and rtk system binaries via
# execFileSync/spawnSync — with fixed paths and timeouts. No shell injection
# vector exists: all subprocess arguments are hardcoded or come from
# resolveBinaryPath(), never from user input. Hosts that dropped install-time
# scanning get no note, because nothing was bypassed.
if [ "$UNSAFE_SUPPORT" = "effective" ]; then
    echo "[${COMPONENT}] Note: --dangerously-force-unsafe-install is required because"
    echo "[${COMPONENT}]       this plugin wraps tokenless/rtk system binaries via child_process."
    echo "[${COMPONENT}]       See https://github.com/alibaba/anolisa for source."
fi

env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" "${INSTALL_ARGS[@]}" || {
    echo "[${COMPONENT}] openclaw CLI install failed — require OpenClaw >= 2026.6.6." >&2
    if [ "$UNSAFE_SUPPORT" = "noop" ]; then
        echo "[${COMPONENT}]       this OpenClaw advertises --dangerously-force-unsafe-install as a" >&2
        echo "[${COMPONENT}]       deprecated no-op; the safety scan follows the operator-owned" >&2
        echo "[${COMPONENT}]       security.installPolicy instead." >&2
    fi
    exit 1
}

echo "[${COMPONENT}] ${AGENT} plugin installed via openclaw CLI."
echo "[${COMPONENT}] Run '${OPENCLAW_BIN} gateway restart' to activate."
