#!/usr/bin/env bash
# Verify the stale-helper cleanup in `make install-helpers` / `make uninstall`
# removes the standalone toon and rtk helpers of older Tokenless releases and
# only the $(BINDIR) symlinks they owned, never an unrelated user-installed
# executable of the same name.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-stale-helper-cleanup-test.XXXXXX)"
TMP="$(cd "$TMP" && pwd -P)"
trap 'rm -rf "$TMP"' EXIT

run_make() {
    local target="$1"
    local destdir="$2"
    make -C "$ROOT" "$target" \
        DESTDIR="$destdir" \
        BINDIR=/bin \
        LIBEXECDIR=/libexec/anolisa/tokenless \
        SHARE_DIR=/share/anolisa/adapters/tokenless \
        COSH_EXTENSION_DIR=/extensions/tokenless \
        >/dev/null
}

# seed_destdir <root> <helper> <bin-kind> <legacy-target>
#   regular : unrelated user-installed executable
#   legacy  : symlink into an old Tokenless helper layout
#   foreign : symlink to an unrelated location
seed_destdir() {
    local root="$1"
    local helper="$2"
    local kind="$3"
    local legacy="$4"
    mkdir -p "$root/bin" "$root/libexec/anolisa/tokenless"
    printf '#!/bin/sh\nexit 0\n' > "$root/libexec/anolisa/tokenless/$helper"
    case "$kind" in
        regular)
            printf '#!/bin/sh\necho user-%s\n' "$helper" > "$root/bin/$helper"
            chmod 0755 "$root/bin/$helper"
            ;;
        legacy)
            ln -s "$legacy" "$root/bin/$helper"
            ;;
        foreign)
            ln -s "/opt/$helper/bin/$helper" "$root/bin/$helper"
            ;;
    esac
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

for helper in toon rtk; do
    legacy="/usr/libexec/anolisa/tokenless/$helper"
    for target in install-helpers uninstall; do
        # 1. An unrelated regular-file executable is preserved.
        dest="$TMP/$helper-$target-regular"
        seed_destdir "$dest" "$helper" regular "$legacy"
        run_make "$target" "$dest"
        [[ -f "$dest/bin/$helper" && ! -L "$dest/bin/$helper" ]] \
            || fail "$target removed an unrelated regular-file $helper executable"
        [[ ! -e "$dest/libexec/anolisa/tokenless/$helper" ]] \
            || fail "$target kept the Tokenless-owned libexec $helper helper"

        # 2. A legacy Tokenless-owned symlink is removed.
        dest="$TMP/$helper-$target-legacy"
        seed_destdir "$dest" "$helper" legacy "$legacy"
        run_make "$target" "$dest"
        [[ ! -e "$dest/bin/$helper" && ! -L "$dest/bin/$helper" ]] \
            || fail "$target kept the legacy Tokenless $helper symlink"
        [[ ! -e "$dest/libexec/anolisa/tokenless/$helper" ]] \
            || fail "$target kept the Tokenless-owned libexec $helper helper"

        # 3. A symlink pointing outside Tokenless layouts is preserved.
        dest="$TMP/$helper-$target-foreign"
        seed_destdir "$dest" "$helper" foreign "$legacy"
        run_make "$target" "$dest"
        [[ -L "$dest/bin/$helper" ]] \
            || fail "$target removed a $helper symlink that does not belong to Tokenless"
    done
done

# 4. Releases installed with an empty AWPRJ used a non-namespaced helper layout;
#    its rtk link is Tokenless-owned as well.
for target in install-helpers uninstall; do
    dest="$TMP/rtk-$target-plain-legacy"
    seed_destdir "$dest" rtk legacy /usr/libexec/tokenless/rtk
    run_make "$target" "$dest"
    [[ ! -e "$dest/bin/rtk" && ! -L "$dest/bin/rtk" ]] \
        || fail "$target kept the non-namespaced legacy rtk symlink"
done

echo "tokenless stale-helper cleanup tests passed"
