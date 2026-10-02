#!/usr/bin/env bash
# uninstall.sh — Disable and remove tokenless plugin from Hermes Agent.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-hermes}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
export PATH="$HOME/.local/bin:${HERMES_HOME%/}/bin:/usr/local/bin:$PATH"

PLUGIN_DST="${HERMES_HOME%/}/plugins/tokenless"
HERMES_CONFIG="${HERMES_HOME%/}/config.yaml"

# Whether tokenless is still registered, read structurally from config.yaml.
# Three answers, because "not enabled" and "cannot tell" are different states and
# only the first is a successful deregistration:
#   enabled  listed under plugins.enabled — a live registration
#   absent   not listed. This includes the normal result of a successful
#            `hermes plugins disable`, which moves the name into plugins.disabled:
#            that is Hermes' documented contract for a disabled plugin, not a
#            leftover registration.
#   unknown  the config could not be parsed, or there is no config and no CLI that
#            can answer, so nothing can be concluded in either direction.
#
# A coarse text test ("does the file mention tokenless?") made a *successful*
# uninstall fail permanently: re-running the disable/remove that its own error
# message suggests cannot clear a normal plugins.disabled entry, so the caller kept
# the adapter tree and the receipt forever. A shape-specific regex has the
# mirror-image flaw — it misses a legal flow sequence (`enabled: [tokenless]`).
# Neither is used here, and neither is a hand-rolled YAML subset parser. That was
# tried and removed, because legal YAML it did not model — sequence entries level
# with `enabled:`, a quoted `"enabled"` key, an unrelated deeper `enabled` nested
# under `plugins.settings` — was answered as "absent", and "absent" is followed by
# irreversible deletion of the plugin files, the shared adapter resources and the
# receipt. The config is read with a real YAML parser. When none is available, or
# it cannot parse the file, or the structure is not one this script positively
# understands, the answer is "unknown" — which is never reported as success.
#
# $1 is "cli" when an executable HERMES_BIN was found, empty otherwise.
hermes_registration_state() {
    local have_cli="${1:-}"
    if [ ! -f "$HERMES_CONFIG" ]; then
        # No config file means there is no plugins.enabled list to appear in — but
        # a CLI that was found and cannot even answer leaves no witness at all,
        # which is "cannot confirm" rather than "confirmed absent". With no CLI at
        # all there is also nothing that could have registered the plugin here.
        if [ -n "$have_cli" ] && ! hermes_cli_usable; then
            printf 'unknown\n'
        else
            printf 'absent\n'
        fi
        return 0
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        printf 'unknown\n'
        return 0
    fi
    python3 - "$HERMES_CONFIG" <<'PYEOF'
import sys

path = sys.argv[1]
NAME = "tokenless"

# A real YAML parser is the only reader used here. A hand-written subset parser
# was tried and removed: legal YAML it did not model was reported as "absent",
# and "absent" is followed by irreversible deletion of the plugin files, the
# shared adapter resources and the receipt. Three ordinary shapes broke it —
# sequence entries at the same indentation as `enabled:` (which is legal YAML), a
# quoted `"enabled"` key, and an unrelated deeper `enabled` nested under
# `plugins.settings`. None of those is a reason to add another syntax special
# case; guessing is simply not safe in the "absent" direction. So when there is
# no parser to ask, the answer is "unknown" and the caller keeps everything.
try:
    import yaml
except ImportError:
    print("unknown")
    sys.exit(0)

try:
    with open(path, encoding="utf-8") as fh:
        doc = yaml.safe_load(fh)
except Exception:
    print("unknown")
    sys.exit(0)


def clean(value):
    if isinstance(value, str):
        return value.strip().strip("'\"")
    return value


# Returns None for anything that cannot be placed confidently; the caller reports
# that as "unknown". Only a structure that positively shows the plugin is not
# enabled may answer "absent".
def classify(document):
    if document is None:
        return "absent"            # an empty config registers nothing
    if not isinstance(document, dict):
        return None
    plugins = document.get("plugins")
    if plugins is None:
        return "absent"            # no plugins block at all
    if not isinstance(plugins, dict):
        return None
    if "enabled" not in plugins:
        return "absent"            # the registration lives in plugins.enabled
    enabled = plugins["enabled"]
    if enabled is None:
        return "absent"
    if isinstance(enabled, list):
        items = [clean(i) for i in enabled]
    elif isinstance(enabled, dict):
        items = [clean(k) for k in enabled.keys()]
    else:
        return None                # a scalar or other shape we will not guess at
    return "enabled" if NAME in items else "absent"


answer = classify(doc)
print(answer if answer is not None else "unknown")
PYEOF
}

# Whether the CLI can answer at all. Without config.yaml there is no second
# witness, so a CLI that cannot even report its version leaves nothing to confirm
# the removal with — and "cannot confirm" must not be reported as "removed".
hermes_cli_usable() {
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" --version >/dev/null 2>&1 && return 0
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" version >/dev/null 2>&1 && return 0
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" --help >/dev/null 2>&1 && return 0
    return 1
}

echo "[${COMPONENT}] Uninstalling ${AGENT} plugin..."

if [ -z "$HERMES_BIN" ]; then
    HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
fi

if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
        echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} $HERMES_BIN plugins disable tokenless"
        echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} $HERMES_BIN plugins remove tokenless"
    else
        echo "DRY-RUN: hermes CLI not found; skip CLI disable/remove"
    fi
    echo "DRY-RUN: rm -f $PLUGIN_DST/__init__.py $PLUGIN_DST/plugin.yaml"
    echo "DRY-RUN: rmdir $PLUGIN_DST || rm -rf $PLUGIN_DST"
    exit 0
fi

# Deregister via the hermes CLI when there is one, then verify by reading the
# config rather than by trusting exit statuses: "was not enabled" and "refused"
# both come back non-zero, so a CLI that ran and changed nothing is
# indistinguishable from one that did its job. The caller deletes the shared
# adapter resources the moment this script returns 0, so only a structurally read
# "not enabled" may be reported as success.
if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" plugins disable tokenless || true
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" plugins remove tokenless || true
    STATE="$(hermes_registration_state cli)"
    if [ "$STATE" = "enabled" ]; then
        echo "[${COMPONENT}] ERROR: ${HERMES_CONFIG} still lists tokenless under plugins.enabled." >&2
        echo "[${COMPONENT}] Plugin files were left in place so this can be retried:" >&2
        echo "[${COMPONENT}]   HERMES_HOME=${HERMES_HOME%/} hermes plugins disable tokenless" >&2
        echo "[${COMPONENT}]   HERMES_HOME=${HERMES_HOME%/} hermes plugins remove tokenless" >&2
        echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; re-run this script afterwards." >&2
        exit 1
    fi
    if [ "$STATE" = "unknown" ]; then
        echo "[${COMPONENT}] ERROR: ${HERMES_BIN} ran but the registration state could not be" >&2
        echo "[${COMPONENT}] ERROR: determined from ${HERMES_CONFIG}. Reading it needs a YAML" >&2
        echo "[${COMPONENT}] ERROR: parser for python3 (e.g. python3 -m pip install pyyaml); without" >&2
        echo "[${COMPONENT}] ERROR: one this script will not guess, and the CLI cannot answer" >&2
        echo "[${COMPONENT}] ERROR: --version either, so" >&2
        echo "[${COMPONENT}] ERROR: nothing confirms the plugin was disabled. Refusing to report success." >&2
        echo "[${COMPONENT}] Plugin files were left in place so this can be retried." >&2
        exit 1
    fi
    # STATE=absent. Note this includes plugins.disabled still naming tokenless:
    # that is what a successful `plugins disable` writes, and it is not a
    # registration.
else
    STATE="$(hermes_registration_state "")"
    if [ "$STATE" = "enabled" ]; then
        # No CLI to ask, so nothing can deregister it — and the entry is not inert.
        # The CLI being unresolvable right now is not proof Hermes is gone: it may
        # simply be off PATH in this shell. Reporting success would leave
        # plugins.enabled pointing at a path the caller is about to delete, and
        # take the receipt — the only way back — with it.
        echo "[${COMPONENT}] ERROR: hermes CLI not found and ${HERMES_CONFIG} still lists tokenless" >&2
        echo "[${COMPONENT}] ERROR: under plugins.enabled, so the registration cannot be confirmed removed." >&2
        echo "[${COMPONENT}] Plugin files were left in place so this can be retried once the CLI is back:" >&2
        echo "[${COMPONENT}]   HERMES_HOME=${HERMES_HOME%/} hermes plugins disable tokenless" >&2
        echo "[${COMPONENT}]   HERMES_HOME=${HERMES_HOME%/} hermes plugins remove tokenless" >&2
        exit 1
    fi
    if [ "$STATE" = "unknown" ]; then
        echo "[${COMPONENT}] ERROR: hermes CLI not found and ${HERMES_CONFIG} could not be parsed," >&2
        echo "[${COMPONENT}] ERROR: so it cannot be confirmed that tokenless is not enabled." >&2
        echo "[${COMPONENT}] Plugin files were left in place so this can be retried." >&2
        exit 1
    fi
fi

# Always clean up filesystem artifacts (the CLI may leave the symlink behind
# when the plugin wasn't fully registered, e.g. partial install).
if [ -d "$PLUGIN_DST" ] || [ -L "$PLUGIN_DST" ]; then
    rm -f "$PLUGIN_DST/__init__.py" "$PLUGIN_DST/plugin.yaml" 2>/dev/null || true
    rmdir "$PLUGIN_DST" 2>/dev/null || rm -rf "$PLUGIN_DST" 2>/dev/null || true
fi

echo "[${COMPONENT}] ${AGENT} plugin uninstalled."
