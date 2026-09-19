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
# Neither is used here. The state is read by structure: indentation, block versus
# flow sequences, comments stripped. Only a genuine parse failure says "unknown",
# and "unknown" is never reported as success.
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

try:
    with open(path, encoding="utf-8") as fh:
        raw = fh.read()
except OSError:
    print("unknown")
    sys.exit(0)


def clean(value):
    if isinstance(value, str):
        return value.strip().strip("'\"")
    return value


def from_document(doc):
    """Answer from an already-parsed mapping, or None if it is not understood."""
    if doc is None:
        return "absent"
    if not isinstance(doc, dict):
        return None
    plugins = doc.get("plugins")
    if plugins is None:
        return "absent"
    if not isinstance(plugins, dict):
        return None
    enabled = plugins.get("enabled")
    if enabled is None:
        return "absent"
    if isinstance(enabled, list):
        items = [clean(i) for i in enabled]
    elif isinstance(enabled, dict):
        items = [clean(k) for k in enabled.keys()]
    else:
        return None
    return "enabled" if NAME in items else "absent"


# A real YAML parser is authoritative about nesting, quoting and flow style, so
# prefer it when the environment happens to provide one.
try:
    import yaml
except ImportError:
    yaml = None

if yaml is not None:
    try:
        answer = from_document(yaml.safe_load(raw))
    except Exception:
        answer = None
    print(answer if answer is not None else "unknown")
    sys.exit(0)


# Fallback with no third-party YAML module: walk the indentation ourselves and read
# only plugins.enabled. Anything that cannot be placed confidently is "unknown";
# nothing is ever guessed in the "absent" direction.
def strip_comment(line):
    out, quote = [], None
    for ch in line:
        if quote:
            out.append(ch)
            if ch == quote:
                quote = None
            continue
        if ch in "'\"":
            quote = ch
            out.append(ch)
            continue
        if ch == "#":
            break
        out.append(ch)
    return "".join(out).rstrip()


def flow_items(body):
    body = body.strip()
    if not (body.startswith("[") and body.endswith("]")):
        return None
    inner = body[1:-1].strip()
    return [] if not inner else [clean(p) for p in inner.split(",")]


rows = []
for raw_line in raw.split("\n"):
    text = strip_comment(raw_line)
    if not text.strip():
        continue
    lead = text[: len(text) - len(text.lstrip())]
    if "\t" in lead:
        print("unknown")  # tabs are not valid YAML indentation
        sys.exit(0)
    rows.append((len(lead), text.strip()))

i, plugins_indent, items, saw_enabled = 0, None, None, False
while i < len(rows):
    indent, text = rows[i]
    if plugins_indent is None:
        if indent == 0 and text.startswith("plugins:"):
            if text[len("plugins:"):].strip():
                print("unknown")  # inline value we will not guess at
                sys.exit(0)
            plugins_indent = indent
        i += 1
        continue
    if indent <= plugins_indent:
        break
    key, _, value = text.partition(":")
    if key.strip() == "enabled":
        saw_enabled = True
        value = value.strip()
        if value:
            parsed = flow_items(value)
            if parsed is None:
                print("unknown")
                sys.exit(0)
            items = parsed
        else:
            items = []
            i += 1
            while i < len(rows):
                ind2, t2 = rows[i]
                if ind2 <= indent:
                    break
                if t2.startswith("- "):
                    items.append(clean(t2[2:]))
                elif t2 == "-":
                    print("unknown")
                    sys.exit(0)
                else:
                    items.append(clean(t2.partition(":")[0]))
                i += 1
            continue
    i += 1

if plugins_indent is None:
    # No readable top-level plugins: block. If the name appears anywhere outside a
    # comment, the config is shaped in a way this parser does not understand.
    print("unknown" if NAME in "\n".join(t for _, t in rows) else "absent")
    sys.exit(0)
if not saw_enabled:
    print("absent")
    sys.exit(0)
if items is None:
    print("unknown")
    sys.exit(0)
print("enabled" if NAME in items else "absent")
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
        echo "[${COMPONENT}] ERROR: determined from ${HERMES_CONFIG} (missing, unreadable, or not" >&2
        echo "[${COMPONENT}] ERROR: parsable), and the CLI cannot answer --version either, so" >&2
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
