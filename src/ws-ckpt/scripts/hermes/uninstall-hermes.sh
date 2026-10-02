#!/bin/bash

set -euo pipefail

HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
HERMES_SKILLS_DIR="${HERMES_SKILLS_DIR:-${HERMES_HOME%/}/skills}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
PLUGIN_DST="${HERMES_HOME%/}/plugins/ws-ckpt"
SKILL_DST="${HERMES_SKILLS_DIR%/}/ws-ckpt"

if [ -z "$HERMES_BIN" ]; then
    HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
fi

if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
        echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} $HERMES_BIN plugins disable ws-ckpt"
        echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} $HERMES_BIN plugins remove ws-ckpt"
    else
        echo "DRY-RUN: hermes CLI not found; skip CLI disable/remove"
    fi
    echo "DRY-RUN: rm -rf $PLUGIN_DST"
    echo "DRY-RUN: update ${HERMES_HOME%/}/config.yaml to remove ws-ckpt entries"
    echo "DRY-RUN: rm -rf $SKILL_DST"
    exit 0
fi

if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" plugins disable ws-ckpt 2>/dev/null || true
fi

# 1. Remove plugin symlink
if [ -L "$PLUGIN_DST" ] || [ -d "$PLUGIN_DST" ]; then
    rm -rf "$PLUGIN_DST"
    echo "plugin removed: $PLUGIN_DST"
fi

# 2. Remove ws-ckpt config from ~/.hermes/config.yaml
HERMES_CONFIG="${HERMES_CONFIG_PATH:-${HERMES_HOME%/}/config.yaml}"
if [ -f "$HERMES_CONFIG" ]; then
    python3 -c "
import sys, re

path = sys.argv[1]
with open(path) as f:
    lines = f.readlines()

out = []
in_plugins = False
plugins_indent = -1
skip_indent = -1

for line in lines:
    stripped = line.strip()
    indent = len(line) - len(line.lstrip()) if stripped else 0

    # Track whether we're inside the plugins: block
    if re.match(r'^plugins:\s*$', line):
        in_plugins = True
        plugins_indent = indent
        out.append(line)
        continue
    if in_plugins and stripped and indent <= plugins_indent:
        in_plugins = False

    # Still skipping children of ws-ckpt: block
    if skip_indent >= 0:
        if not stripped:
            out.append(line)
            continue
        if indent > skip_indent:
            continue
        skip_indent = -1

    # Only act inside plugins: block
    if in_plugins:
        if re.match(r'^\s*- ws-ckpt\s*$', line):
            continue
        m = re.match(r'^(\s*)ws-ckpt:\s*$', line)
        if m:
            skip_indent = len(m.group(1))
            continue

    out.append(line)

with open(path, 'w') as f:
    f.writelines(out)
" "$HERMES_CONFIG" && echo "ws-ckpt config removed from $HERMES_CONFIG"
fi

# 3. Remove skill if exists
if [ -d "$SKILL_DST" ]; then
    rm -rf "$SKILL_DST"
    echo "skill removed: $SKILL_DST"
fi
