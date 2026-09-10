#!/usr/bin/env bash
# install.sh — Deploy the agent-memory OpenClaw plugin via the openclaw CLI.
#
# This script ONLY deploys an already-built plugin.
# Compilation is the Makefile's job:
#     make -C src/agent-memory build-openclaw-plugin
# If dist/index.js is missing, exit with a clear error.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-agent-memory}"
# ANOLISA_ADAPTER_DIR is injected by anolisa-adapter-ctl (FHS spec §2.4).
# Fall back to the directory containing manifest.json.
PLUGIN_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}/openclaw"

OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"

# Normalize the consent switch with the same systemd-style token table as
# the Rust side's env_bool() (src/config.rs): only leading/trailing
# whitespace is trimmed — deleting interior whitespace would normalize
# 't rue' into a grant — and matching is case-insensitive. Unset or empty
# accepts (the documented default); every other value, including
# whitespace-only, aborts with rc=2 before any CLI call — the operator
# must never believe they have withheld consent while the script accepts
# it. This runs before every early exit (missing CLI, missing dist) so a
# bad value is reported unconditionally.
ACCEPT_CAPABILITIES=1
_consent="${AGENT_MEMORY_ACCEPT_CAPABILITIES-}"
if [ -n "$_consent" ]; then
    _consent="${_consent#"${_consent%%[![:space:]]*}"}"
    _consent="${_consent%"${_consent##*[![:space:]]}"}"
    case "$(printf '%s' "$_consent" | tr '[:upper:]' '[:lower:]')" in
        1|true|yes|on) ACCEPT_CAPABILITIES=1 ;;
        0|false|no|off) ACCEPT_CAPABILITIES=0 ;;
        *)
            echo "[${COMPONENT}] ERROR: AGENT_MEMORY_ACCEPT_CAPABILITIES='${AGENT_MEMORY_ACCEPT_CAPABILITIES}' is not a boolean (use 1/true/yes/on or 0/false/no/off)." >&2
            exit 2 ;;
    esac
fi

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}) — skipping plugin installation."
    echo "[${COMPONENT}] Install OpenClaw first, then run this script again."
    exit 0
fi

if [ ! -f "$PLUGIN_DIR/dist/index.js" ]; then
    echo "[${COMPONENT}] ERROR: $PLUGIN_DIR/dist/index.js is missing." >&2
    echo "[${COMPONENT}]        Build the plugin first:" >&2
    echo "[${COMPONENT}]            cd $PLUGIN_DIR && npm run build" >&2
    exit 1
fi

# Base argv only. Both optional flags — capability consent and the legacy
# unsafe-install bypass — are negotiated from the installer help probed below,
# so nothing is appended before that probe has run.
INSTALL_ARGS=("--force")

# OpenClaw gates capability-declaring plugins behind install-time consent:
# a noninteractive `plugins install` is rejected unless --accept-capabilities
# is passed. Version boundaries here are unreliable — the flag shipped in
# 2026.8.1 but the CLI reference documents it only since 2026.9.1, and older
# releases abort on the unknown option outright — so probe the host's
# installer help instead of version-gating. Whole-token match (near-miss
# options such as --no-accept-capabilities stay out), mirroring tokenless's
# installer and anolisa-core's help_lists_flag().
PROBE_RC=0
INSTALL_HELP="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
    "$OPENCLAW_BIN" plugins install --help 2>&1)" || PROBE_RC=$?
if [ "$PROBE_RC" -ne 0 ]; then
    # Degrade instead of failing closed: the script must keep installing on
    # every host the manifest claims to support. Clearing INSTALL_HELP keeps
    # probe error text from being mistaken for an advertised option, and marks
    # the unsafe-install decision below as unclassified rather than absent.
    printf '[%s] WARNING: cannot inspect OpenClaw installer options (rc=%s): %s\n' \
        "$COMPONENT" "$PROBE_RC" "$INSTALL_HELP" >&2
    printf '[%s]          Installing without --accept-capabilities; the unsafe-install\n' "$COMPONENT" >&2
    printf '[%s]          bypass cannot be classified either (see its own note below).\n' "$COMPONENT" >&2
    INSTALL_HELP=""
fi
CONSENT_WITHHELD=0
if [[ "$INSTALL_HELP" =~ (^|[^[:alnum:]_.-])--accept-capabilities([^[:alnum:]_.-]|$) ]]; then
    if [ "$ACCEPT_CAPABILITIES" = "0" ]; then
        # Deliberate refusal, not a silent skip: policies that forbid
        # non-interactive consent grants must see the install fail loudly
        # on gating hosts instead of succeeding with an implicit grant.
        CONSENT_WITHHELD=1
        echo "[${COMPONENT}] AGENT_MEMORY_ACCEPT_CAPABILITIES=0: withholding --accept-capabilities — capability consent is not granted by this script." >&2
        echo "[${COMPONENT}]          Hosts that gate consent will reject this install until consent is granted interactively." >&2
    else
        INSTALL_ARGS+=("--accept-capabilities")
        echo "[${COMPONENT}] Passing --accept-capabilities: granting install-time consent to memory-anolisa's declared capabilities."
    fi
elif [ -n "$INSTALL_HELP" ]; then
    echo "[${COMPONENT}] OpenClaw did not advertise --accept-capabilities (pre-2026.8.1 host); installing with the base flags."
elif [ "$ACCEPT_CAPABILITIES" = "0" ]; then
    # Unreachable unless the probe failed: the host's gating status is
    # unknown, so surface that the opt-out is shaping the install.
    echo "[${COMPONENT}] AGENT_MEMORY_ACCEPT_CAPABILITIES=0 is active; if this host gates consent, the install will fail." >&2
fi

# Hosts whose installer still runs the install-time safety scan need
# --dangerously-force-unsafe-install as their only non-interactive bypass, and
# this plugin trips that scan because it spawns the agent-memory MCP server as a
# stdio subprocess. Later hosts dropped install-time dangerous-code blocking and
# keep the token as a deprecated no-op, so passing it there accomplishes nothing
# and fails outright once the token is removed. The boundary is not the
# capability-consent one above: in the published npm packages 2026.6.1 still
# advertises "Bypass built-in dangerous-code install blocking", while
# 2026.6.2-beta.1 and every later release (stable from 2026.6.5) advertise
# "Deprecated no-op; security.installPolicy may still block". Classify the option
# from the help already captured above rather than from a version — the same
# line-scoped, whole-token read anolisa-core's unsafe_install_support() performs
# — and pass the flag only while it still has effect. `tr` rather than ${var,,}:
# bash 3.2 has no case-conversion expansion.
UNSAFE_SUPPORT=absent
[ "$PROBE_RC" -eq 0 ] || UNSAFE_SUPPORT=unknown
# grep narrows the capture to candidate lines before bash reads them: scanning
# a very large help one loop iteration at a time costs seconds, while grep
# reads the whole capture (no -q/-m early exit, so nothing SIGPIPEs the
# producer under pipefail) in milliseconds. The decision itself is unchanged —
# whole-token match on the option's own line, then the no-op read below.
UNSAFE_HELP_LINES="$(printf '%s\n' "$INSTALL_HELP" \
    | grep -F -- '--dangerously-force-unsafe-install' || true)"
while IFS= read -r help_line; do
    if [[ "$help_line" =~ (^|[^[:alnum:]_.-])--dangerously-force-unsafe-install([^[:alnum:]_.-]|$) ]]; then
        help_line_lc="$(printf '%s' "$help_line" | tr '[:upper:]' '[:lower:]')"
        case "$help_line_lc" in
            *"no op"*|*"no-op"*) UNSAFE_SUPPORT=noop ;;
            *) UNSAFE_SUPPORT=effective ;;
        esac
        break
    fi
done <<< "$UNSAFE_HELP_LINES"

# AGENT_MEMORY_SAFE_INSTALL=1 declines the bypass. That is a real choice only
# where the bypass still does something; on a no-op host both paths are
# identical, so say so instead of silently ignoring the variable.
SAFE_INSTALL="${AGENT_MEMORY_SAFE_INSTALL:-0}"
UNSAFE_DECLINED=0
case "$UNSAFE_SUPPORT" in
    effective)
        if [ "$SAFE_INSTALL" = "1" ]; then
            UNSAFE_DECLINED=1
            echo "[${COMPONENT}] AGENT_MEMORY_SAFE_INSTALL=1: declining --dangerously-force-unsafe-install;" >&2
            echo "[${COMPONENT}]       this host still scans plugin sources at install time and may block" >&2
            echo "[${COMPONENT}]       the plugin for its child_process.spawn MCP transport." >&2
        else
            INSTALL_ARGS+=("--dangerously-force-unsafe-install")
            echo "[${COMPONENT}] Passing --dangerously-force-unsafe-install: this host still scans plugin"
            echo "[${COMPONENT}]       sources at install time, and that scan flags the child_process.spawn"
            echo "[${COMPONENT}]       this plugin uses to launch the agent-memory MCP server over stdio."
        fi
        ;;
    unknown)
        # Probe failed: the host cannot be classified, so keep the bypass that
        # every release still accepting the token needs, and name the opt-out.
        if [ "$SAFE_INSTALL" = "1" ]; then
            UNSAFE_DECLINED=1
            echo "[${COMPONENT}] AGENT_MEMORY_SAFE_INSTALL=1: declining --dangerously-force-unsafe-install." >&2
        else
            INSTALL_ARGS+=("--dangerously-force-unsafe-install")
            echo "[${COMPONENT}] WARNING: installer options are unclassified, so the legacy bypass is kept." >&2
            echo "[${COMPONENT}]          Set AGENT_MEMORY_SAFE_INSTALL=1 to decline it on hosts that no" >&2
            echo "[${COMPONENT}]          longer accept the option." >&2
        fi
        ;;
    noop)
        echo "[${COMPONENT}] Not passing --dangerously-force-unsafe-install: this OpenClaw advertises it"
        echo "[${COMPONENT}]       as a deprecated no-op, so install-time safety follows the operator-owned"
        echo "[${COMPONENT}]       security.installPolicy instead."
        if [ "$SAFE_INSTALL" = "1" ]; then
            echo "[${COMPONENT}]       AGENT_MEMORY_SAFE_INSTALL=1 changes nothing on this host: the flag is"
            echo "[${COMPONENT}]       omitted either way."
        fi
        ;;
    *)
        echo "[${COMPONENT}] Not passing --dangerously-force-unsafe-install: this OpenClaw does not"
        echo "[${COMPONENT}]       advertise the option."
        ;;
esac

# One install-failure cause the script can establish without any transcript.
# OpenClaw materialises the plugin under ${OPENCLAW_STATE_DIR}/extensions/ (the
# memory-anolisa directory uninstall.sh removes), so an unwritable state
# directory fails the install on its own, whatever the host's install policy
# says. Probing that path leaves the CLI's streams exactly as they were.
install_target_writable() {
    local probe="${OPENCLAW_STATE_DIR}/extensions/memory-anolisa"
    # Nearest existing ancestor — OpenClaw creates the rest of the path.
    while [ ! -e "$probe" ]; do
        probe="$(dirname -- "$probe")"
    done
    [ -w "$probe" ]
}

# The transcript (tee into a log file, then grep the file — never a
# short-circuit pipe, whose early match would SIGPIPE the producer under
# pipefail) exists only for the consent-withheld path, the sole consumer of
# the consent-rejection signal; every other install runs the CLI exactly as
# before — stderr stays stderr, stdout stays the operator's terminal, so
# TTY-gated colour/prompts survive on the SAFE_INSTALL blocking-scan path.
# The install-target check below is transcript-free for the same reason: it
# reports only what this script can verify for itself and otherwise defers to
# the CLI's own message, which the operator can already read on the terminal.
# The merged 2>&1 on the withheld path is deliberate for a non-interactive
# refusal transcript: the CLI's stderr surfaces on this script's stdout.
INSTALL_CMD=(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
    "$OPENCLAW_BIN" plugins install "$PLUGIN_DIR" "${INSTALL_ARGS[@]}")
INSTALL_RC=0
if [ "$CONSENT_WITHHELD" = "1" ]; then
    INSTALL_LOG="$(mktemp -t agent-memory-openclaw-install.XXXXXX)"
    trap 'rm -f "$INSTALL_LOG"' EXIT
    "${INSTALL_CMD[@]}" 2>&1 | tee "$INSTALL_LOG" || INSTALL_RC=${PIPESTATUS[0]}
else
    INSTALL_LOG=""
    "${INSTALL_CMD[@]}" || INSTALL_RC=$?
fi
if [ "$INSTALL_RC" -ne 0 ]; then
    # Attribute the failure to the withheld consent only when OpenClaw's
    # output carries the documented consent-rejection phrase ("Plugin "…"
    # requires capability consent. …"); permissions, a broken manifest, or
    # a security scan must keep the generic failure code so callers do not
    # mistake a real install failure for a policy refusal. If OpenClaw
    # rewords the message, this degrades to rc=1 with the opt-out note.
    if [ "$CONSENT_WITHHELD" = "1" ] && grep -qi 'requires capability consent' "$INSTALL_LOG"; then
        echo "[${COMPONENT}] install failed with consent withheld by AGENT_MEMORY_ACCEPT_CAPABILITIES=0 — grant consent interactively or unset the variable to let this script grant it." >&2
        echo "[${COMPONENT}]   openclaw plugins install <plugin-dir> --force --accept-capabilities" >&2
        echo "[${COMPONENT}]   plugin-dir: $PLUGIN_DIR" >&2
        exit 3
    fi
    if [ "$CONSENT_WITHHELD" = "1" ]; then
        echo "[${COMPONENT}] Note: AGENT_MEMORY_ACCEPT_CAPABILITIES=0 is active, but this failure does not look like a consent rejection." >&2
    fi
    echo "[${COMPONENT}] openclaw CLI install failed — check OpenClaw version >= 5.0.0" >&2
    # What the script itself did is knowable here; why OpenClaw rejected the
    # install is not. Neither note may assert a cause: 2388b370 attributes the
    # consent opt-out only when the captured output carries the documented
    # rejection phrase, and no equivalent phrase is documented for
    # security.installPolicy — so both stay conditional on the CLI output,
    # which this default path leaves on the operator's own terminal. One cause
    # is exempt because the script can verify it itself, so it ranks first: an
    # install target the current user cannot write fails the install whatever
    # else is true, and sending that operator off to reason about
    # security.installPolicy is the misattribution the review objected to.
    if ! install_target_writable; then
        echo "[${COMPONENT}]       the install target ${OPENCLAW_STATE_DIR}/extensions/memory-anolisa is not" >&2
        echo "[${COMPONENT}]       writable by the user running this script, which on its own fails the install with a" >&2
        echo "[${COMPONENT}]       filesystem-permission error (EACCES). Fix that directory's ownership/permissions — or" >&2
        echo "[${COMPONENT}]       point OPENCLAW_STATE_DIR at a writable state directory — and re-run before drawing any" >&2
        echo "[${COMPONENT}]       conclusion about security.installPolicy: an unwritable target is not a policy refusal," >&2
        echo "[${COMPONENT}]       so do not relax that policy for it." >&2
    elif [ "$UNSAFE_SUPPORT" = "noop" ]; then
        echo "[${COMPONENT}]       note: this OpenClaw advertises --dangerously-force-unsafe-install as a" >&2
        echo "[${COMPONENT}]       deprecated no-op, so the script sent no bypass and cannot shape install-time" >&2
        echo "[${COMPONENT}]       safety on this host. Read the CLI output above for the actual cause; only" >&2
        echo "[${COMPONENT}]       if it names security.installPolicy is that operator-owned policy what to" >&2
        echo "[${COMPONENT}]       relax — not this script." >&2
    elif [ "$UNSAFE_DECLINED" = "1" ]; then
        echo "[${COMPONENT}]       note: AGENT_MEMORY_SAFE_INSTALL=1 declined the unsafe-install bypass. If the" >&2
        echo "[${COMPONENT}]       CLI output above blames the install-time safety scan, unset the variable to" >&2
        echo "[${COMPONENT}]       let the script pass the bypass this host still honors." >&2
    fi
    exit 1
fi

# OpenClaw's bundled memory-core plugin owns the memory_get / memory_search
# tool names, and it stays loaded even after this plugin takes the memory slot:
# the loader exempts it from slot exclusivity so its dreaming (consolidation)
# lifecycle survives, which `openclaw plugins doctor` reports as "memory plugin
# not selected for the memory slot; skipping its indexing runtime and recall
# registration (consolidation lifecycle preserved)". OpenClaw's plugin tool
# registry is first-wins and drops a plugin tool whose normalized name is
# already taken, logging "plugin tool name conflict (memory-anolisa):
# memory_get" and never handing that tool to the agent. Calls to memory_get then
# bind to memory-core's workspace-file reader, which answers disabled:true for
# every path under ~/.anolisa/memory even though the file is on disk (#3218).
#
# Disabling memory-core releases both names, and the memory slot already routes
# memory traffic through this plugin. The marker records that *this script*
# caused that transition, so uninstall.sh restores exactly what install.sh took
# away and never re-enables a memory-core an operator disabled themselves. The
# disable is idempotent, so a re-install — or an operator who re-enabled
# memory-core in between — converges instead of being skipped.
MEMORY_CORE_ID="memory-core"
MEMORY_CORE_MARKER="${OPENCLAW_STATE_DIR}/.anolisa-memory-anolisa-disabled-${MEMORY_CORE_ID}"
MEMORY_CORE_ENABLED_KEY="plugins.entries.${MEMORY_CORE_ID}.enabled"

# `plugins disable` exits 0 whether or not memory-core was enabled, so its own
# status cannot distinguish a real transition from a no-op. Read the persisted
# enablement flag first — the same `plugins.entries.<id>.enabled` config key the
# OpenClaw driver writes — and only claim the disable when memory-core was on
# beforehand. Without that read, installing over a memory-core an operator had
# already turned off still writes the marker, and uninstall.sh then re-enables
# a plugin that operator meant to keep off.
#
# The bias on an unreadable answer is deliberate, because the two failure modes
# are not symmetric. An absent key means "bundled default", i.e. enabled, and so
# does a probe this host cannot answer (no `config get`, unreadable config);
# both fall through to recording the marker. Skipping it in that case would
# strand the host with no memory plugin at all after an uninstall, since
# `plugins uninstall` resets the memory slot to its memory-core default while
# config still says enabled=false — whereas recording it can only cost an
# operator one extra `plugins disable`. Only a positive "false" is honored as
# somebody else's choice.
MEMORY_CORE_WAS_ENABLED=1
_mc_prior="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
    "$OPENCLAW_BIN" config get "$MEMORY_CORE_ENABLED_KEY" 2>/dev/null)" || _mc_prior=""
# Hosts render the value differently (bare `false`, JSON `"false"`, or
# `key=value`), so reduce the last line to its trailing token and strip quoting
# before matching — the same whole-token, case-insensitive discipline the
# consent switch above uses.
_mc_prior="$(printf '%s' "$_mc_prior" | tail -n 1 | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/.*[=:]//' -e 's/[]["'\''`,;]//g' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
case "$_mc_prior" in
    false|0|no|off|disabled) MEMORY_CORE_WAS_ENABLED=0 ;;
esac

if env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" plugins disable "$MEMORY_CORE_ID"; then
    if [ "$MEMORY_CORE_WAS_ENABLED" = "0" ]; then
        # No transition to claim: the names this plugin needs are already free.
        echo "[${COMPONENT}] ${MEMORY_CORE_ID} was already disabled (${MEMORY_CORE_ENABLED_KEY}=${_mc_prior}), so"
        echo "[${COMPONENT}]       this install changed no plugin state and records no marker — the memory_get and"
        echo "[${COMPONENT}]       memory_search tool names are already free, and uninstall.sh leaves your choice alone."
    else
        echo "[${COMPONENT}] Disabled ${MEMORY_CORE_ID}: while it is loaded it holds the memory_get/memory_search"
        echo "[${COMPONENT}]       tool names, and OpenClaw's first-wins tool registry drops this plugin's own."
        # One note, not a WARNING: nothing failed, and on a host that never
        # wrote the key this is the ordinary bundled-default case. It exists
        # only so an operator whose memory-core *was* off learns that the
        # restore was claimed anyway, and how to opt out.
        if [ -z "$_mc_prior" ]; then
            echo "[${COMPONENT}]       note: '${OPENCLAW_BIN} config get ${MEMORY_CORE_ENABLED_KEY}' returned no value, so this"
            echo "[${COMPONENT}]       disable is recorded as this script's own — the alternative strands the host with no"
            echo "[${COMPONENT}]       memory plugin after uninstall. Had you disabled ${MEMORY_CORE_ID} yourself, delete"
            echo "[${COMPONENT}]       ${MEMORY_CORE_MARKER} to keep it off."
        fi
        # Written only after the disable succeeds: the marker is uninstall.sh's
        # sole evidence that restoring memory-core is this script's job. An
        # unwritable state directory earns a WARNING rather than a failed
        # install — the plugin works either way, and only the automatic restore
        # is lost.
        if ! touch "$MEMORY_CORE_MARKER" 2>/dev/null; then
            echo "[${COMPONENT}] WARNING: could not record ${MEMORY_CORE_MARKER}, so uninstall.sh cannot" >&2
            echo "[${COMPONENT}]          restore ${MEMORY_CORE_ID} automatically. Run '${OPENCLAW_BIN} plugins enable" >&2
            echo "[${COMPONENT}]          ${MEMORY_CORE_ID}' yourself after uninstalling this plugin." >&2
        fi
    fi
else
    echo "[${COMPONENT}] WARNING: '${OPENCLAW_BIN} plugins disable ${MEMORY_CORE_ID}' failed, so memory_get and" >&2
    echo "[${COMPONENT}]          memory_search keep binding to ${MEMORY_CORE_ID}: OpenClaw drops this plugin's" >&2
    echo "[${COMPONENT}]          same-named tools ('plugin tool name conflict') and ${MEMORY_CORE_ID} then" >&2
    echo "[${COMPONENT}]          answers disabled:true for every ~/.anolisa/memory path." >&2
fi

# OpenClaw 2026.6.11 requires non-bundled plugins to explicitly opt-in
# to conversation hooks (agent_end, before_prompt_build, etc.). Without
# this setting the hooks are silently blocked and auto-capture /
# auto-recall never fire (#1460).
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" config set \
    "plugins.entries.memory-anolisa.hooks.allowConversationAccess" true || {
    echo "[${COMPONENT}] WARNING: failed to set allowConversationAccess — auto-capture/auto-recall hooks may be blocked." >&2
}

echo "[${COMPONENT}] ${AGENT} plugin installed via openclaw CLI."
echo "[${COMPONENT}] Run '${OPENCLAW_BIN} gateway restart' to activate."
echo "[${COMPONENT}] NOTE: allowConversationAccess and plugin hooks only take effect after gateway restart."
echo "[${COMPONENT}]       Without restart, auto-capture and auto-recall hooks remain silently blocked."
echo "[${COMPONENT}] NOTE: the ${MEMORY_CORE_ID} disable above also takes effect only after gateway restart. Until"
echo "[${COMPONENT}]       then OpenClaw still serves memory_get/memory_search from ${MEMORY_CORE_ID} and drops this"
echo "[${COMPONENT}]       plugin's same-named tools, so memory_get keeps answering disabled:true."
