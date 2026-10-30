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

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -euo pipefail
    case "${1:-}" in
        binaries) shift; cosh_copy_binaries "$@" ;;
        skills) shift; cosh_copy_skills "$@" ;;
        *) printf 'ERROR: usage: %s {binaries|skills} ARGS...\n' "$0" >&2; exit 1 ;;
    esac
fi
