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

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

# Allow the orchestrator (or a packaging script) to inject a specific openclaw
# binary. Defaults to whatever `openclaw` resolves to on PATH.
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
export PATH="$HOME/.local/bin:${OPENCLAW_STATE_DIR%/}/bin:/usr/local/bin:$PATH"

PLUGIN_SRC="$ADAPTER_DIR/openclaw"

# Capability consent, one policy across ANOLISA's OpenClaw installer scripts
# (docs/user-guide/en/token-saving/tokenless/framework-integration.md).
# ANOLISA_ACCEPT_CAPABILITIES is normalized with the same systemd-style token
# table the agent-memory installer uses for its own consent switch: only
# leading/trailing whitespace is trimmed — deleting interior whitespace would
# normalize 't rue' into a grant — and matching is case-insensitive. Unset or
# empty accepts, which is the documented default and this script's behavior
# before the switch existed; every other value, including whitespace-only,
# aborts with rc=2 before any CLI call and before the dry-run text, so an
# operator can never believe consent was withheld while the script grants it.
ACCEPT_CAPABILITIES=1
_consent="${ANOLISA_ACCEPT_CAPABILITIES-}"
if [ -n "$_consent" ]; then
    _consent="${_consent#"${_consent%%[![:space:]]*}"}"
    _consent="${_consent%"${_consent##*[![:space:]]}"}"
    case "$(printf '%s' "$_consent" | tr '[:upper:]' '[:lower:]')" in
        1|true|yes|on) ACCEPT_CAPABILITIES=1 ;;
        0|false|no|off) ACCEPT_CAPABILITIES=0 ;;
        *)
            echo "[${COMPONENT}] ERROR: ANOLISA_ACCEPT_CAPABILITIES='${ANOLISA_ACCEPT_CAPABILITIES}' is not a boolean (use 1/true/yes/on or 0/false/no/off)." >&2
            exit 2 ;;
    esac
fi

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
    echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins install $PLUGIN_SRC --force"
    echo "DRY-RUN: add --dangerously-force-unsafe-install only while the advertised option still has effect (legacy hosts)"
    echo "DRY-RUN: add --accept-capabilities only if advertised by plugins install --help"
    if [ "$ACCEPT_CAPABILITIES" = "0" ]; then
        echo "DRY-RUN: ANOLISA_ACCEPT_CAPABILITIES=0 withholds --accept-capabilities even where it is advertised"
    fi
    exit 0
fi

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}) — skipping plugin installation."
    echo "[${COMPONENT}] Install OpenClaw first, then run this script again."
    exit 0
fi

INSTALL_ARGS=(plugins install "$PLUGIN_SRC" --force)
if ! INSTALL_HELP="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins install --help 2>&1)"; then
    printf '[%s] Cannot inspect OpenClaw installer options: %s\n' "$COMPONENT" "$INSTALL_HELP" >&2
    exit 1
fi
# Invoking this installer accepts declared capabilities unless the operator
# withheld that grant with ANOLISA_ACCEPT_CAPABILITIES=0. Withholding is a
# visible refusal, not a silent skip: a policy that forbids non-interactive
# consent grants must see the install fail loudly on a gating host instead of
# succeeding with an implicit grant. Older hosts must never receive an
# unsupported flag or a similarly named option.
CONSENT_WITHHELD=0
if [[ "$INSTALL_HELP" =~ (^|[^[:alnum:]_.-])--accept-capabilities([^[:alnum:]_.-]|$) ]]; then
    if [ "$ACCEPT_CAPABILITIES" = "0" ]; then
        CONSENT_WITHHELD=1
        echo "[${COMPONENT}] ANOLISA_ACCEPT_CAPABILITIES=0: withholding --accept-capabilities — capability consent is not granted by this script." >&2
        echo "[${COMPONENT}]          Hosts that gate consent will reject this install until consent is granted interactively." >&2
    else
        INSTALL_ARGS+=(--accept-capabilities)
        echo "[${COMPONENT}] Passing --accept-capabilities: granting install-time consent to the tokenless plugin's declared capabilities."
    fi
elif [ "$ACCEPT_CAPABILITIES" = "0" ]; then
    # This host does not gate installs on consent, so the switch cannot shape
    # this install. Say so instead of letting the operator believe a refusal
    # took effect here.
    echo "[${COMPONENT}] ANOLISA_ACCEPT_CAPABILITIES=0 changes nothing on this host: --accept-capabilities is not advertised, so no consent grant is sent either way."
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

# The transcript (tee into a log file, then grep the file — never a
# short-circuit pipe, whose early match would SIGPIPE the producer under
# pipefail) exists only for the consent-withheld path, the sole consumer of the
# consent-rejection signal; every other install runs the CLI exactly as before,
# so stderr stays stderr and stdout stays the operator's terminal. The merged
# 2>&1 on the withheld path is deliberate for a non-interactive refusal
# transcript: the CLI's stderr surfaces on this script's stdout.
INSTALL_CMD=(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
    "$OPENCLAW_BIN" "${INSTALL_ARGS[@]}")
INSTALL_RC=0
if [ "$CONSENT_WITHHELD" = "1" ]; then
    INSTALL_LOG="$(mktemp -t tokenless-openclaw-install.XXXXXX)"
    trap 'rm -f "$INSTALL_LOG"' EXIT
    "${INSTALL_CMD[@]}" 2>&1 | tee "$INSTALL_LOG" || INSTALL_RC=${PIPESTATUS[0]}
else
    INSTALL_LOG=""
    "${INSTALL_CMD[@]}" || INSTALL_RC=$?
fi
if [ "$INSTALL_RC" -ne 0 ]; then
    # Attribute the failure to the withheld consent only when OpenClaw's output
    # carries the documented consent-rejection phrase ("Plugin "…" requires
    # capability consent. …"); permissions, a broken manifest, or a
    # security.installPolicy refusal must keep the generic failure code so
    # callers do not mistake a real install failure for a policy refusal. If
    # OpenClaw rewords the message, this degrades to rc=1 with the opt-out note.
    if [ "$CONSENT_WITHHELD" = "1" ] && grep -qi 'requires capability consent' "$INSTALL_LOG"; then
        echo "[${COMPONENT}] install failed with consent withheld by ANOLISA_ACCEPT_CAPABILITIES=0 — grant consent interactively or unset the variable to let this script grant it." >&2
        echo "[${COMPONENT}]   openclaw plugins install <plugin-dir> --force --accept-capabilities" >&2
        echo "[${COMPONENT}]   plugin-dir: $PLUGIN_SRC" >&2
        exit 3
    fi
    if [ "$CONSENT_WITHHELD" = "1" ]; then
        echo "[${COMPONENT}] Note: ANOLISA_ACCEPT_CAPABILITIES=0 is active, but this failure does not look like a consent rejection." >&2
    fi
    echo "[${COMPONENT}] openclaw CLI install failed — check OpenClaw version >= 5.0.0" >&2
    if [ "$UNSAFE_SUPPORT" = "noop" ]; then
        echo "[${COMPONENT}]       this OpenClaw advertises --dangerously-force-unsafe-install as a" >&2
        echo "[${COMPONENT}]       deprecated no-op; the safety scan follows the operator-owned" >&2
        echo "[${COMPONENT}]       security.installPolicy instead." >&2
    fi
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin installed via openclaw CLI."
echo "[${COMPONENT}] Run '${OPENCLAW_BIN} gateway restart' to activate."
