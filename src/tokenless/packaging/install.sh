#!/usr/bin/env bash
# Install prebuilt files without registering frameworks or starting services.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_ROOT="${COMPONENT_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

tokenless_layout_error() {
    printf 'ERROR: %s\n' "$*" >&2
    return 1
}

tokenless_resolve_layout() {
    if [ "${INSTALL_PROFILE+x}" != x ]; then
        if [ "$(id -u)" = 0 ]; then INSTALL_PROFILE=system; else INSTALL_PROFILE=user; fi
    fi
    case "$INSTALL_PROFILE" in
        system) ;;
        user) ;;
        *) tokenless_layout_error 'INSTALL_PROFILE must be system or user'; return 1 ;;
    esac
    AWPRJ="${AWPRJ-}"
    case "$AWPRJ" in
        '' | [a-zA-Z0-9]*) ;;
        *) tokenless_layout_error 'AWPRJ must be empty or start with a letter or digit'; return 1 ;;
    esac
    case "$AWPRJ" in
        *[!a-zA-Z0-9_.-]*) tokenless_layout_error 'AWPRJ must be a single namespace segment'; return 1 ;;
    esac
    local default_prefix=/usr/local data_root user_home
    if [ "$INSTALL_PROFILE" = user ]; then
        user_home="${HOME:-}"
        case "$user_home" in
            ''|/) tokenless_layout_error 'user installation requires a non-root absolute HOME'; return 1 ;;
            /*) ;;
            *) tokenless_layout_error 'user installation requires an absolute HOME'; return 1 ;;
        esac
        case "/$user_home/" in
            */../*|*/./*) tokenless_layout_error 'user HOME must not contain dot segments'; return 1 ;;
        esac
        [ -d "$user_home" ] && [ -O "$user_home" ] || { tokenless_layout_error 'user HOME must be an existing directory owned by the invoking user'; return 1; }
        default_prefix="$user_home/.local"
        data_root="${XDG_DATA_HOME:-$user_home/.local/share}"
    fi
    PREFIX="${PREFIX-$default_prefix}"
    while [ "$PREFIX" != / ] && [ "${PREFIX%/}" != "$PREFIX" ]; do PREFIX="${PREFIX%/}"; done
    local root="${PREFIX%/}" namespace="${AWPRJ:+$AWPRJ/}"
    BINDIR="${BINDIR-$root/bin}"
    LIBDIR="${LIBDIR-$root/lib}"
    LIBEXECDIR="${LIBEXECDIR-$root/libexec/${namespace}tokenless}"
    if [ "$INSTALL_PROFILE" = system ]; then data_root="$root/share"; fi
    case "$data_root" in
        /*) ;;
        *) tokenless_layout_error 'XDG_DATA_HOME must be absolute'; return 1 ;;
    esac
    case "/$data_root/" in
        */../*|*/./*) tokenless_layout_error 'XDG_DATA_HOME must not contain dot segments'; return 1 ;;
    esac
    local datadir_explicit="${DATADIR+x}"
    DATADIR="${DATADIR-$data_root/${namespace}tokenless}"
    local default_adapterdir="$DATADIR/adapters" default_extensiondir="$DATADIR/cosh-extension"
    if [ -n "$AWPRJ" ] && [ "$datadir_explicit" != x ]; then
        default_adapterdir="$data_root/$AWPRJ/adapters/tokenless"
        default_extensiondir="$data_root/$AWPRJ/extensions/tokenless"
    fi
    ADAPTERDIR="${ADAPTERDIR-$default_adapterdir}"
    EXTENSIONDIR="${EXTENSIONDIR-$default_extensiondir}"
    UNITDIR="${UNITDIR-$root/lib/systemd/system}"
    local directory
    for directory in "$PREFIX" "$BINDIR" "$LIBDIR" "$LIBEXECDIR" "$DATADIR" "$ADAPTERDIR" "$EXTENSIONDIR" "$UNITDIR"; do
        case "$directory" in
            /*) ;;
            *) tokenless_layout_error "layout directory must be absolute: $directory"; return 1 ;;
        esac
        case "/$directory/" in
            */../* | */./*) tokenless_layout_error "layout directory contains a dot segment: $directory"; return 1 ;;
        esac
    done
}

tokenless_resolve_layout
if [ "${1:-}" = --print-layout ]; then
    [ "$#" -eq 1 ] || { printf 'ERROR: --print-layout takes no arguments\n' >&2; exit 1; }
    printf '%s\n' "INSTALL_PROFILE=$INSTALL_PROFILE" "AWPRJ=$AWPRJ" "PREFIX=$PREFIX" "BINDIR=$BINDIR" \
        "LIBDIR=$LIBDIR" "LIBEXECDIR=$LIBEXECDIR" "DATADIR=$DATADIR" \
        "ADAPTERDIR=$ADAPTERDIR" "EXTENSIONDIR=$EXTENSIONDIR" "UNITDIR=$UNITDIR"
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

check_destination() {
    local target="$1" kind="$2" current="$1"
    # Reinstall the link we own without rejecting its expected private target.
    if [ "$kind" = rtk-link ] && [ -L "$target" ] && [ "$(readlink "$target")" = "$LIBEXECDIR/rtk" ]; then
        current="$(dirname "$target")"
    fi
    while [ "$current" != / ]; do
        [ ! -L "$current" ] || { printf 'ERROR: installation path is a symbolic link: %s\n' "$current" >&2; exit 1; }
        current="$(dirname "$current")"
    done
    if [ "$kind" != directory ] && [ -d "$target" ]; then
        printf 'ERROR: installation file is a directory: %s\n' "$target" >&2
        exit 1
    fi
    if [ "$kind" = directory ] && [ -e "$target" ] && [ ! -d "$target" ]; then
        printf 'ERROR: installation directory is a file: %s\n' "$target" >&2
        exit 1
    fi
}

check_tree_destinations() {
    local source="$1" target="$2" entry relative
    check_destination "$target" directory
    while IFS= read -r entry; do
        relative="${entry#"$source"/}"
        if [ -d "$entry" ]; then
            check_destination "$target/$relative" directory
        else
            check_destination "$target/$relative" file
        fi
    done < <(find "$source" -mindepth 1 -print)
}

group="${1:-all}"
[ "$#" -le 1 ] || { printf 'ERROR: usage: %s [all|core|plugins|uninstall]\n' "$0" >&2; exit 1; }
case "$group" in all|core|plugins|uninstall) ;; *) printf 'ERROR: unsupported install group: %s\n' "$group" >&2; exit 1;; esac
if [ "$group" = uninstall ]; then
    # Never recursively remove a shared prefix or a system directory.
    for directory in "$ADAPTERDIR" "$EXTENSIONDIR"; do
        case "$directory" in
            /|/usr|/usr/local|/etc|"$PREFIX"|"$BINDIR"|"$LIBEXECDIR"|"$DATADIR")
                printf 'ERROR: refusing to remove shared directory: %s\n' "$directory" >&2; exit 1 ;;
        esac
        check_destination "$root$directory" directory
    done
    check_destination "$root$BINDIR/tokenless" file
    check_destination "$root$BINDIR/rtk" rtk-link
    check_destination "$root$LIBEXECDIR/rtk" file
    check_destination "$root$DATADIR/LICENSE" file
    rm -f "$root$BINDIR/tokenless" "$root$LIBEXECDIR/rtk" "$root$LIBEXECDIR/toon" "$root$DATADIR/LICENSE"
    if [ -L "$root$BINDIR/rtk" ] && [ "$(readlink "$root$BINDIR/rtk")" = "$LIBEXECDIR/rtk" ]; then
        rm -f "$root$BINDIR/rtk"
    fi
    if [ -L "$root$BINDIR/toon" ]; then
        case "$(readlink "$root$BINDIR/toon")" in
            */anolisa/tokenless/toon|*/anolisa/libexec/tokenless/toon) rm -f "$root$BINDIR/toon" ;;
        esac
    fi
    rm -rf "$root$ADAPTERDIR" "$root$EXTENSIONDIR"
    exit 0
fi
BIN_DIR="${BIN_DIR:-$COMPONENT_ROOT/target/release}"
RTK_BIN="${RTK_BIN:-$COMPONENT_ROOT/third_party/rtk/target/release/rtk}"
if [ "$group" != plugins ]; then
    [ -x "$BIN_DIR/tokenless" ] || { printf 'ERROR: missing prebuilt executable: %s\n' "$BIN_DIR/tokenless" >&2; exit 1; }
    [ -x "$RTK_BIN" ] || { printf 'ERROR: missing prebuilt executable: %s\n' "$RTK_BIN" >&2; exit 1; }
    [ -f "$COMPONENT_ROOT/LICENSE" ] || { printf 'ERROR: missing component LICENSE\n' >&2; exit 1; }
fi
payload=""
trap '[ -z "$payload" ] || rm -rf "$payload"' EXIT
if [ "$group" != core ]; then
    adapters="$COMPONENT_ROOT/adapters/tokenless"
    for relative in manifest.json openclaw/dist/index.js dsh/dist/index.js common/cosh-extension.json common/tool-ready-spec.json common/tokenless-env-fix.sh; do
        [ -f "$adapters/$relative" ] || { printf 'ERROR: missing prebuilt plugin input: %s\n' "$adapters/$relative" >&2; exit 1; }
    done
    for relative in common/hooks common/commands; do
        [ -d "$adapters/$relative" ] || { printf 'ERROR: missing plugin resource directory: %s\n' "$adapters/$relative" >&2; exit 1; }
    done
    # Prepare only the selected runtime inventory, without compiling plugins.
    payload="$(mktemp -d "${TMPDIR:-/tmp}/tokenless-install.XXXXXX")"
    tokenless_copy_adapters "$adapters" "$payload/adapters" system
    tokenless_copy_extension "$adapters" "$payload/extension" system
fi
if [ "$group" != plugins ]; then
    check_destination "$root$BINDIR/tokenless" file
    check_destination "$root$BINDIR/rtk" rtk-link
    check_destination "$root$LIBEXECDIR/rtk" file
    check_destination "$root$DATADIR/LICENSE" file
fi
if [ "$group" != core ]; then
    check_tree_destinations "$payload/adapters" "$root$ADAPTERDIR"
    check_tree_destinations "$payload/extension" "$root$EXTENSIONDIR"
fi
if [ "$group" != plugins ]; then
    install -d -m 0755 "$root$BINDIR" "$root$LIBEXECDIR" "$root$DATADIR"
    install -p -m 0755 "$BIN_DIR/tokenless" "$root$BINDIR/tokenless"
    install -p -m 0755 "$RTK_BIN" "$root$LIBEXECDIR/rtk"
    install -p -m 0644 "$COMPONENT_ROOT/LICENSE" "$root$DATADIR/LICENSE"
    ln -sfn "$LIBEXECDIR/rtk" "$root$BINDIR/rtk"
fi
if [ "$group" != core ]; then
    install -d -m 0755 "$root$ADAPTERDIR" "$root$EXTENSIONDIR"
    cp -pR "$payload/adapters/." "$root$ADAPTERDIR/"
    cp -pR "$payload/extension/." "$root$EXTENSIONDIR/"
fi
printf 'Installed selected prebuilt files; framework registration and cosh discovery remain separate host operations.\n' >&2
