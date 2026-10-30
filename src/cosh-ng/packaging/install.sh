#!/usr/bin/env bash
# Install prebuilt files without registering shells, services, or extensions.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_ROOT="${COMPONENT_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

cosh_layout_error() {
    printf 'ERROR: %s\n' "$*" >&2
    return 1
}

cosh_resolve_layout() {
    if [ "${INSTALL_PROFILE+x}" != x ]; then
        if [ "$(id -u)" = 0 ]; then INSTALL_PROFILE=system; else INSTALL_PROFILE=user; fi
    fi
    case "$INSTALL_PROFILE" in
        system) ;;
        user) ;;
        *) cosh_layout_error 'INSTALL_PROFILE must be system or user'; return 1 ;;
    esac
    AWPRJ="${AWPRJ-}"
    case "$AWPRJ" in
        '' | [a-zA-Z0-9]*) ;;
        *) cosh_layout_error 'AWPRJ must be empty or start with a letter or digit'; return 1 ;;
    esac
    case "$AWPRJ" in
        *[!a-zA-Z0-9_.-]*) cosh_layout_error 'AWPRJ must be a single namespace segment'; return 1 ;;
    esac
    local default_prefix=/usr/local data_root user_home
    if [ "$INSTALL_PROFILE" = user ]; then
        user_home="${HOME:-}"
        case "$user_home" in
            ''|/) cosh_layout_error 'user installation requires a non-root absolute HOME'; return 1 ;;
            /*) ;;
            *) cosh_layout_error 'user installation requires an absolute HOME'; return 1 ;;
        esac
        case "/$user_home/" in
            */../*|*/./*) cosh_layout_error 'user HOME must not contain dot segments'; return 1 ;;
        esac
        [ -d "$user_home" ] && [ -O "$user_home" ] || { cosh_layout_error 'user HOME must be an existing directory owned by the invoking user'; return 1; }
        default_prefix="$user_home/.local"
        data_root="${XDG_DATA_HOME:-$user_home/.local/share}"
    fi
    PREFIX="${PREFIX-$default_prefix}"
    while [ "$PREFIX" != / ] && [ "${PREFIX%/}" != "$PREFIX" ]; do PREFIX="${PREFIX%/}"; done
    local root="${PREFIX%/}" namespace="${AWPRJ:+$AWPRJ/}"
    BINDIR="${BINDIR-$root/bin}"
    LIBDIR="${LIBDIR-$root/lib}"
    LIBEXECDIR="${LIBEXECDIR-$root/libexec/${namespace}cosh-ng}"
    if [ "$INSTALL_PROFILE" = system ]; then data_root="$root/share"; fi
    case "$data_root" in
        /*) ;;
        *) cosh_layout_error 'XDG_DATA_HOME must be absolute'; return 1 ;;
    esac
    case "/$data_root/" in
        */../*|*/./*) cosh_layout_error 'XDG_DATA_HOME must not contain dot segments'; return 1 ;;
    esac
    DATADIR="${DATADIR-$data_root/${namespace}cosh-ng}"
    local default_skilldir="$DATADIR/skills"
    [ -z "$AWPRJ" ] || default_skilldir="$data_root/$AWPRJ/skills"
    SKILLDIR="${SKILLDIR-$default_skilldir}"
    UNITDIR="${UNITDIR-$root/lib/systemd/system}"
    local directory
    for directory in "$PREFIX" "$BINDIR" "$LIBDIR" "$LIBEXECDIR" "$DATADIR" "$SKILLDIR" "$UNITDIR"; do
        case "$directory" in
            /*) ;;
            *) cosh_layout_error "layout directory must be absolute: $directory"; return 1 ;;
        esac
        case "/$directory/" in
            */../* | */./*) cosh_layout_error "layout directory contains a dot segment: $directory"; return 1 ;;
        esac
    done
}

cosh_resolve_layout
if [ "${1:-}" = --print-layout ]; then
    [ "$#" -eq 1 ] || { printf 'ERROR: --print-layout accepts no arguments\n' >&2; exit 1; }
    printf '%s\n' "INSTALL_PROFILE=$INSTALL_PROFILE" "AWPRJ=$AWPRJ" "PREFIX=$PREFIX" "BINDIR=$BINDIR" \
        "LIBDIR=$LIBDIR" "LIBEXECDIR=$LIBEXECDIR" "DATADIR=$DATADIR" \
        "SKILLDIR=$SKILLDIR" "UNITDIR=$UNITDIR"
    exit 0
fi

source "$SCRIPT_DIR/payload.sh"
case "${DESTDIR:-}" in
    '') ;;
    /) printf 'ERROR: use empty DESTDIR for direct installation\n' >&2; exit 1 ;;
    /*) ;;
    *) printf 'ERROR: DESTDIR must be absolute\n' >&2; exit 1 ;;
esac
case "${DESTDIR:-}" in
    '') ;;
    *[!/]*) ;;
    *) printf 'ERROR: use empty DESTDIR for direct installation\n' >&2; exit 1 ;;
esac
case "/${DESTDIR:-}/" in
    */../* | */./*) printf 'ERROR: DESTDIR cannot contain dot segments\n' >&2; exit 1 ;;
esac
root="${DESTDIR:-}"
while [ "${root%/}" != "$root" ]; do root="${root%/}"; done

check_destination_file() {
    local target="$1" current="$1"
    while [ "$current" != / ]; do
        [ ! -L "$current" ] || { printf 'ERROR: installation path is a symbolic link: %s\n' "$current" >&2; exit 1; }
        current="$(dirname "$current")"
    done
    if [ -d "$target" ]; then
        printf 'ERROR: installation file is a directory: %s\n' "$target" >&2
        exit 1
    fi
}

[ "$#" -eq 0 ] || { printf 'ERROR: usage: %s\n' "$0" >&2; exit 1; }
BIN_DIR="${BIN_DIR:-$COMPONENT_ROOT/target/release}"
for binary in cosh-cli $COSH_PRIVATE_BINARIES; do
    [ -x "$BIN_DIR/$binary" ] || { printf 'ERROR: missing prebuilt executable: %s\n' "$BIN_DIR/$binary" >&2; exit 1; }
done
for relative in LICENSE packaging/systemd/cosh-gateway@.service.in; do
    [ -f "$COMPONENT_ROOT/$relative" ] || { printf 'ERROR: missing install input: %s\n' "$COMPONENT_ROOT/$relative" >&2; exit 1; }
done
[ -f "$COMPONENT_ROOT/skills/manage-task-checkpoints/SKILL.md" ] || { printf 'ERROR: missing skill input\n' >&2; exit 1; }

# Validate every input and destination before copying any file.
check_destination_file "$root$BINDIR/cosh-cli"
for binary in $COSH_PRIVATE_BINARIES; do
    check_destination_file "$root$LIBEXECDIR/$binary"
done
check_destination_file "$root$DATADIR/systemd/cosh-gateway@.service.in"
check_destination_file "$root$DATADIR/LICENSE"
check_destination_file "$root$SKILLDIR/manage-task-checkpoints/SKILL.md"

cosh_copy_binaries "$BIN_DIR" "$root" "$BINDIR" "$LIBEXECDIR"
install -d -m 0755 "$root$DATADIR/systemd"
install -p -m 0644 "$COMPONENT_ROOT/packaging/systemd/cosh-gateway@.service.in" "$root$DATADIR/systemd/"
install -p -m 0644 "$COMPONENT_ROOT/LICENSE" "$root$DATADIR/LICENSE"
cosh_copy_skills "$COMPONENT_ROOT" "$root$SKILLDIR"
printf 'Installed prebuilt files; no launcher, shell registration or active unit was installed.\n' >&2
