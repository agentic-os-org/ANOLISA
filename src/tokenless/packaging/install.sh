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
    AWPRJ="${AWPRJ-anolisa}"
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
        managed_user_record="$data_root/anolisa/components/tokenless/component.toml"
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
    legacy_adapterdir= legacy_extensiondir=
    if [ -z "$AWPRJ" ] && [ "$datadir_explicit" != x ]; then
        [ "${ADAPTERDIR+x}" = x ] || legacy_adapterdir="$default_adapterdir"
        [ "${EXTENSIONDIR+x}" = x ] || legacy_extensiondir="$default_extensiondir"
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

# Resolve link targets in the installation's logical root before adding DESTDIR.
normalize_helper_directory() {
    local remaining="$1/" component directory=""
    while [ -n "$remaining" ]; do
        component="${remaining%%/*}"
        remaining="${remaining#*/}"
        case "$component" in
            ''|.) ;;
            ..)
                [ -n "$directory" ] || return 1
                directory="${directory%/*}" ;;
            *) directory="$directory/$component" ;;
        esac
    done
    printf '%s\n' "${directory:-/}"
}

has_managed_helper_record() {
    local directory user_directory
    directory="$(normalize_helper_directory "$1")" || return 1
    if [ "$INSTALL_PROFILE" = user ]; then
        user_directory="$(normalize_helper_directory "$HOME/.local/lib/anolisa/libexec/tokenless")"
        case "$directory" in
            "$user_directory") [ -e "$root$managed_user_record" ]; return ;;
            # A user-facing link or role override can point at a system install.
            */usr/libexec/anolisa/tokenless|*/usr/local/libexec/anolisa/tokenless) ;;
            *) return 1 ;;
        esac
    fi
    # System helper and record directories share a prefix, including overrides.
    case "$directory" in
        */libexec/anolisa/tokenless)
            [ -e "$root${directory%/libexec/anolisa/tokenless}/share/anolisa/components/tokenless/component.toml" ] ;;
        *) return 1 ;;
    esac
}

# Tokenless now embeds rtk and toon. Retire its old standalone helpers and links.
remove_stale_helpers() {
    local cleanup_libexec=1 helper link
    if has_managed_helper_record "$LIBEXECDIR"; then
        cleanup_libexec=0
    else
        rm -f "$root$LIBEXECDIR/rtk" "$root$LIBEXECDIR/toon"
    fi
    for helper in rtk toon; do
        [ -L "$root$BINDIR/$helper" ] || continue
        link="$(readlink "$root$BINDIR/$helper")" || continue
        case "$helper:$link" in
            "rtk:$LIBEXECDIR/rtk"|rtk:*/anolisa/tokenless/rtk|rtk:*/libexec/tokenless/rtk|\
            toon:*/anolisa/tokenless/toon|toon:*/anolisa/libexec/tokenless/toon) ;;
            *) continue ;;
        esac
        # A link can point to the managed layout even when LIBEXECDIR differs.
        [[ "$link" = /* ]] || link="$BINDIR/$link"
        if ! has_managed_helper_record "${link%/*}"; then
            rm -f "$root$BINDIR/$helper"
        fi
    done
    if [ "$cleanup_libexec" = 1 ]; then
        case "$LIBEXECDIR" in
            */tokenless) rmdir "$root$LIBEXECDIR" 2>/dev/null || true ;;
        esac
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

owns_resource_tree() {
    local target="$1"
    while [ "$target" != / ] && [ "${target%/}" != "$target" ]; do target="${target%/}"; done
    case "$target" in
        */adapters/tokenless|*/extensions/tokenless)
            [ ! -e "${target%/*/*}/components/tokenless/component.toml" ] || return 1 ;;
    esac
    if [ -e "$target/.tokenless-owner" ]; then
        [ "$(cat "$target/.tokenless-owner" 2>/dev/null)" = source-install:tokenless ]
        return
    fi
    # Older source installs had no marker in their exclusive default trees.
    if [ -n "$legacy_adapterdir" ] && [ "$target" = "$root$legacy_adapterdir" ]; then return 0; fi
    [ -n "$legacy_extensiondir" ] && [ "$target" = "$root$legacy_extensiondir" ]
}

check_resource_owner() {
    local target="$1"
    check_destination "$target/.tokenless-owner" file
    if [ -d "$target" ] && [ -n "$(find "$target" -mindepth 1 -print -quit)" ] && ! owns_resource_tree "$target"; then
        printf 'ERROR: refusing to replace resources without source-install ownership: %s\n' "$target" >&2
        printf 'Use the owning installer to remove them, or select separate ADAPTERDIR and EXTENSIONDIR paths.\n' >&2
        exit 1
    fi
}

group="${1:-all}"
[ "$#" -le 1 ] || { printf 'ERROR: usage: %s [all|core|core-with-cosh|plugins|uninstall]\n' "$0" >&2; exit 1; }
install_core=false
install_adapters=false
install_extension=false
case "$group" in
    all) install_core=true; install_adapters=true; install_extension=true ;;
    core) install_core=true ;;
    core-with-cosh) install_core=true; install_extension=true ;;
    plugins) install_adapters=true; install_extension=true ;;
    uninstall) ;;
    *) printf 'ERROR: unsupported install group: %s\n' "$group" >&2; exit 1 ;;
esac
if [ "$group" = uninstall ]; then
    # Never recursively remove a shared prefix or a system directory.
    for directory in "$ADAPTERDIR" "$EXTENSIONDIR"; do
        case "$directory" in
            /|/usr|/usr/local|/etc|"$PREFIX"|"$BINDIR"|"$LIBEXECDIR"|"$DATADIR")
                printf 'ERROR: refusing to remove shared directory: %s\n' "$directory" >&2; exit 1 ;;
        esac
        check_destination "$root$directory" directory
        check_destination "$root$directory/.tokenless-owner" file
    done
    check_destination "$root$BINDIR/tokenless" file
    check_destination "$root$LIBEXECDIR/rtk" file
    check_destination "$root$DATADIR/LICENSE" file
    rm -f "$root$BINDIR/tokenless" "$root$DATADIR/LICENSE"
    remove_stale_helpers
    for directory in "$ADAPTERDIR" "$EXTENSIONDIR"; do
        if owns_resource_tree "$root$directory"; then
            rm -rf "$root$directory"
        elif [ -d "$root$directory" ]; then
            printf 'WARNING: preserving resources without source-install ownership: %s; use the owning installer to remove them.\n' "$root$directory" >&2
        fi
    done
    exit 0
fi
BIN_DIR="${BIN_DIR:-$COMPONENT_ROOT/target/release}"
if $install_core; then
    [ -x "$BIN_DIR/tokenless" ] || { printf 'ERROR: missing prebuilt executable: %s\n' "$BIN_DIR/tokenless" >&2; exit 1; }
    [ -f "$COMPONENT_ROOT/LICENSE" ] || { printf 'ERROR: missing component LICENSE\n' >&2; exit 1; }
fi
payload=""
trap '[ -z "$payload" ] || rm -rf "$payload"' EXIT
if $install_extension; then
    adapters="$COMPONENT_ROOT/adapters/tokenless"
    required_files=(common/cosh-extension.json common/tool-ready-spec.json common/tokenless-env-fix.sh)
    if $install_adapters; then
        required_files+=(manifest.json openclaw/dist/index.js dsh/dist/index.js)
    fi
    for relative in "${required_files[@]}"; do
        [ -f "$adapters/$relative" ] || { printf 'ERROR: missing prebuilt plugin input: %s\n' "$adapters/$relative" >&2; exit 1; }
    done
    for relative in common/hooks common/commands; do
        [ -d "$adapters/$relative" ] || { printf 'ERROR: missing plugin resource directory: %s\n' "$adapters/$relative" >&2; exit 1; }
    done
    # Prepare only the selected runtime inventory, without compiling plugins.
    payload="$(mktemp -d "${TMPDIR:-/tmp}/tokenless-install.XXXXXX")"
    if $install_adapters; then
        tokenless_copy_adapters "$adapters" "$payload/adapters" system
    fi
    tokenless_copy_extension "$adapters" "$payload/extension" system
fi
if $install_core; then
    check_destination "$root$BINDIR/tokenless" file
    check_destination "$root$LIBEXECDIR/rtk" file
    check_destination "$root$DATADIR/LICENSE" file
fi
if $install_adapters; then
    check_tree_destinations "$payload/adapters" "$root$ADAPTERDIR"
    check_resource_owner "$root$ADAPTERDIR"
fi
if $install_extension; then
    check_tree_destinations "$payload/extension" "$root$EXTENSIONDIR"
    check_resource_owner "$root$EXTENSIONDIR"
fi
if $install_core; then
    install -d -m 0755 "$root$BINDIR" "$root$DATADIR"
    install -p -m 0755 "$BIN_DIR/tokenless" "$root$BINDIR/tokenless"
    install -p -m 0644 "$COMPONENT_ROOT/LICENSE" "$root$DATADIR/LICENSE"
    remove_stale_helpers
fi
if $install_adapters; then
    install -d -m 0755 "$root$ADAPTERDIR"
    cp -pR "$payload/adapters/." "$root$ADAPTERDIR/"
    printf '%s\n' source-install:tokenless > "$root$ADAPTERDIR/.tokenless-owner"
fi
if $install_extension; then
    install -d -m 0755 "$root$EXTENSIONDIR"
    cp -pR "$payload/extension/." "$root$EXTENSIONDIR/"
    printf '%s\n' source-install:tokenless > "$root$EXTENSIONDIR/.tokenless-owner"
fi
printf 'Installed selected prebuilt files; framework registration and cosh discovery remain separate host operations.\n' >&2
