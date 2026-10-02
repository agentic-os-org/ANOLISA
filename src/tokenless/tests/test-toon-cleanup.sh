#!/usr/bin/env bash
# Verify the stale-toon cleanup in `make install-helpers` / `make uninstall`
# removes only symlinks owned by older Tokenless releases and never touches
# an unrelated user-installed `toon` executable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-toon-cleanup-test.XXXXXX)"
RTK_DIR="$ROOT/third_party/rtk"
RTK_BIN="$RTK_DIR/target/release/rtk"
RTK_STUBBED=false
RTK_STUB_DIRS=()

cleanup() {
    if $RTK_STUBBED; then
        rm -f "$RTK_BIN"
        # Take the directories the stub created with it, deepest first. rmdir
        # only removes empty ones, so a real vendored checkout is never
        # touched. Leaving them behind is not cosmetic: scripts/setup-rtk.sh
        # refuses to fetch into an existing directory that has no Cargo.toml
        # ("RTK destination is incomplete"), so the residue broke every later
        # `just setup-rtk`, `make build` and `make test-rtk-integration` on a
        # fresh checkout.
        if [[ ${#RTK_STUB_DIRS[@]} -gt 0 ]]; then
            rmdir "${RTK_STUB_DIRS[@]}" 2>/dev/null || true
        fi
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

# install-helpers copies the rtk build artifact; provide a stub when the
# vendored rtk has not been built (same pattern as the packaging tests).
if [[ ! -f "$RTK_BIN" ]]; then
    # Remember exactly which directories the mkdir below will create -- walking
    # up until the first one that already exists -- so cleanup can remove those
    # and nothing else.
    stub_dir="$(dirname "$RTK_BIN")"
    while [[ ! -d "$stub_dir" ]]; do
        RTK_STUB_DIRS+=("$stub_dir")
        stub_dir="$(dirname "$stub_dir")"
    done
    mkdir -p "$(dirname "$RTK_BIN")"
    printf '#!/bin/sh\nexit 0\n' > "$RTK_BIN"
    chmod 0755 "$RTK_BIN"
    RTK_STUBBED=true
fi

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

# seed_destdir <root> <bin-toon-kind>
#   regular : unrelated user-installed executable
#   legacy  : symlink into an old Tokenless helper layout
#   foreign : symlink to an unrelated location
seed_destdir() {
    local root="$1"
    local kind="$2"
    mkdir -p "$root/bin" "$root/libexec/anolisa/tokenless"
    printf '#!/bin/sh\nexit 0\n' > "$root/libexec/anolisa/tokenless/toon"
    case "$kind" in
        regular)
            printf '#!/bin/sh\necho user-toon\n' > "$root/bin/toon"
            chmod 0755 "$root/bin/toon"
            ;;
        legacy)
            ln -s /usr/libexec/anolisa/tokenless/toon "$root/bin/toon"
            ;;
        foreign)
            ln -s /opt/toon/bin/toon "$root/bin/toon"
            ;;
    esac
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

for target in install-helpers uninstall; do
    # 1. An unrelated regular-file executable is preserved.
    dest="$TMP/$target-regular"
    seed_destdir "$dest" regular
    run_make "$target" "$dest"
    [[ -f "$dest/bin/toon" && ! -L "$dest/bin/toon" ]] \
        || fail "$target removed an unrelated regular-file toon executable"
    [[ ! -e "$dest/libexec/anolisa/tokenless/toon" ]] \
        || fail "$target kept the Tokenless-owned libexec toon helper"

    # 2. A legacy Tokenless-owned symlink is removed.
    dest="$TMP/$target-legacy"
    seed_destdir "$dest" legacy
    run_make "$target" "$dest"
    [[ ! -e "$dest/bin/toon" && ! -L "$dest/bin/toon" ]] \
        || fail "$target kept the legacy Tokenless toon symlink"
    [[ ! -e "$dest/libexec/anolisa/tokenless/toon" ]] \
        || fail "$target kept the Tokenless-owned libexec toon helper"

    # 3. A symlink pointing outside Tokenless layouts is preserved.
    dest="$TMP/$target-foreign"
    seed_destdir "$dest" foreign
    run_make "$target" "$dest"
    [[ -L "$dest/bin/toon" ]] \
        || fail "$target removed a symlink that does not belong to Tokenless"
done

echo "tokenless stale-toon cleanup tests passed"
