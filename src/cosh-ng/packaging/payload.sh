#!/usr/bin/env bash
# File-only inventory shared by independent, raw, and RPM delivery.
COSH_PRIVATE_BINARIES='cosh-core cosh-gateway cosh-shell'

cosh_copy_binaries() {
    local input="$1" stage="$2" bindir="$3" libexecdir="$4" binary
    install -d -m 0755 "$stage$bindir" "$stage$libexecdir"
    install -p -m 0755 "$input/cosh-cli" "$stage$bindir/cosh-cli"
    for binary in $COSH_PRIVATE_BINARIES; do
        install -p -m 0755 "$input/$binary" "$stage$libexecdir/$binary"
    done
}

cosh_copy_skills() {
    local source="$1" destination="$2"
    install -d -m 0755 "$destination/manage-task-checkpoints"
    install -p -m 0644 "$source/skills/manage-task-checkpoints/SKILL.md" "$destination/manage-task-checkpoints/"
}

cosh_write_resources() {
    local destination="$1" profile="$2" skilldir="$3" extensiondir="$4" value
    for value in "$profile" "$skilldir" "$extensiondir"; do
        case "$value" in
            *$'\r'*|*$'\n'*) printf 'ERROR: resource layout values must be single lines\n' >&2; return 1 ;;
        esac
    done
    {
        [ -z "$profile" ] || printf 'profile=%s\n' "$profile"
        printf 'skill_dir=%s\nextension_dir=%s\n' "$skilldir" "$extensiondir"
    } > "$destination"
    chmod 0644 "$destination"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -euo pipefail
    case "${1:-}" in
        binaries) shift; cosh_copy_binaries "$@" ;;
        skills) shift; cosh_copy_skills "$@" ;;
        resources) shift; cosh_write_resources "$@" ;;
        *) printf 'ERROR: usage: %s {binaries|skills|resources} ARGS...\n' "$0" >&2; exit 1 ;;
    esac
fi
