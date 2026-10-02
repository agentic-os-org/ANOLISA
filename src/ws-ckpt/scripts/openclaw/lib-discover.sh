#!/bin/bash
# lib-discover.sh — Shared resource discovery helpers for install scripts.
# Usage: source "$(dirname "$0")/lib-discover.sh"

# discover_dir REQUIRED_FILE... -- DIR...
#   Echoes the first directory containing every required file.
discover_dir() {
    local required_files=()
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
        required_files+=("$1")
        shift
    done
    [ "$#" -gt 0 ] || return 1
    shift

    local dir required_file
    for dir in "$@"; do
        [ -n "$dir" ] && [ -d "$dir" ] || continue
        for required_file in "${required_files[@]}"; do
            [ -f "$dir/$required_file" ] || continue 2
        done
        echo "$dir"
        return
    done
    return 1
}

# find_plugin_src COMPONENT
#   Searches plugin source paths in priority order for the given component
#   (e.g. "openclaw" or "hermes").
find_plugin_src() {
    local component="${1:?usage: find_plugin_src COMPONENT}"
    local candidates=()
    [ -n "${ANOLISA_TARGET_DIR:-}" ] && candidates+=("${ANOLISA_TARGET_DIR}/share/anolisa/runtime/ws-ckpt/plugins/${component}")
    [ -n "${ANOLISA_PROJECT_ROOT:-}" ] && candidates+=("${ANOLISA_PROJECT_ROOT}/src/ws-ckpt/src/plugins/${component}")
    candidates+=("/usr/share/anolisa/runtime/ws-ckpt/plugins/${component}")
    candidates+=("/usr/local/share/anolisa/runtime/ws-ckpt/plugins/${component}")
    candidates+=("${HOME}/.local/share/anolisa/runtime/ws-ckpt/plugins/${component}")
    case "$component" in
        openclaw)
            discover_dir package.json openclaw.plugin.json dist/src/index.js -- "${candidates[@]}"
            ;;
        hermes)
            discover_dir plugin.yaml __init__.py -- "${candidates[@]}"
            ;;
        *)
            return 1
            ;;
    esac
}

# find_skill_src
#   Searches skill source paths in priority order.
find_skill_src() {
    local candidates=()
    [ -n "${ANOLISA_TARGET_DIR:-}" ] && candidates+=("${ANOLISA_TARGET_DIR}/share/anolisa/runtime/skills/ws-ckpt")
    [ -n "${ANOLISA_PROJECT_ROOT:-}" ] && candidates+=("${ANOLISA_PROJECT_ROOT}/src/ws-ckpt/src/skills/ws-ckpt")
    candidates+=("/usr/share/anolisa/runtime/skills/ws-ckpt")
    candidates+=("/usr/local/share/anolisa/runtime/skills/ws-ckpt")
    candidates+=("${HOME}/.local/share/anolisa/runtime/skills/ws-ckpt")
    discover_dir SKILL.md -- "${candidates[@]}"
}

# print_search_error
#   Prints a standard error message listing all searched paths.
print_search_error() {
    echo "ERROR: no plugin or skill source found. Searched paths:"
    echo "  - \${ANOLISA_TARGET_DIR}/share/anolisa/runtime/... (ANOLISA_TARGET_DIR=${ANOLISA_TARGET_DIR:-<unset>})"
    echo "  - /usr/share/anolisa/runtime/..."
    echo "  - /usr/local/share/anolisa/runtime/..."
    echo "  - ~/.local/share/anolisa/runtime/..."
    echo "  - \${ANOLISA_PROJECT_ROOT}/src/ws-ckpt/src/... (ANOLISA_PROJECT_ROOT=${ANOLISA_PROJECT_ROOT:-<unset>})"
    echo "Please install ws-ckpt or set ANOLISA_TARGET_DIR to staged output."
}
