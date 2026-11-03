#!/usr/bin/env bash
# File-only payload assembly; channel differences preserve existing artifacts.

tokenless_copy_adapters() {
    local source="$1" destination="$2" channel="$3"
    case "$channel" in legacy|system|raw) ;; *) printf 'ERROR: unknown payload channel: %s\n' "$channel" >&2; return 1 ;; esac
    install -d -m 0755 "$destination"
    if [ "$channel" = system ]; then
        cp -pRL "$source/." "$destination/"
    else
        cp -pr "$source/." "$destination/"
    fi
    rm -rf "$destination/agentscope" "$destination/openclaw/node_modules"
    rm -f "$destination/openclaw/package-lock.json"
    find "$destination" -type f -name '*.in' -delete
    if [ "$channel" != legacy ]; then
        rm -f "$destination/openclaw/index.ts" "$destination/openclaw/tsconfig.json"
        find "$destination" -type d -name __pycache__ -prune -exec rm -rf {} +
        find "$destination" -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete
    fi
    if [ "$channel" = system ]; then
        find "$destination" -type d -name node_modules -prune -exec rm -rf {} +
    fi
    if [ "$channel" = raw ]; then
        find "$destination" -type f -name .gitignore -delete
    fi
    find "$destination" -type f \( -name '*.py' -o -name '*.sh' \) -exec chmod 0755 {} +
}

tokenless_copy_extension() {
    local source="$1" destination="$2" channel="$3"
    case "$channel" in legacy|system|raw) ;; *) printf 'ERROR: unknown payload channel: %s\n' "$channel" >&2; return 1 ;; esac
    install -d -m 0755 "$destination/hooks" "$destination/commands"
    if [ "$channel" = system ]; then
        cp -pRL "$source/common/hooks/." "$destination/hooks/"
        cp -pRL "$source/common/commands/." "$destination/commands/"
    else
        cp -pr "$source/common/hooks/." "$destination/hooks/"
        cp -pr "$source/common/commands/." "$destination/commands/"
    fi
    install -p -m 0644 "$source/common/cosh-extension.json" "$destination/"
    if [ "$channel" != legacy ]; then
        install -p -m 0644 "$source/common/tool-ready-spec.json" "$destination/"
        install -p -m 0755 "$source/common/tokenless-env-fix.sh" "$destination/"
    fi
    if [ "$channel" != legacy ]; then
        find "$destination" -type d -name __pycache__ -prune -exec rm -rf {} +
        find "$destination" -type f \( -name '*.in' -o -name .gitignore -o -name '*.pyc' -o -name '*.pyo' \) -delete
    fi
    find "$destination" -type f \( -name '*.py' -o -name '*.sh' \) -exec chmod 0755 {} +
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -euo pipefail
    case "${1:-}" in
        adapters) shift; tokenless_copy_adapters "$@" ;;
        extension) shift; tokenless_copy_extension "$@" ;;
        *) printf 'ERROR: usage: %s {adapters|extension} SOURCE DESTINATION CHANNEL\n' "$0" >&2; exit 1 ;;
    esac
fi
