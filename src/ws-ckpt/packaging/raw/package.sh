#!/usr/bin/env bash
# Assemble ws-ckpt runtime files without building or executing cross binaries.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="${WS_CKPT_SOURCE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/src/target/release}"
CONTRACT="${RAW_CONTRACT:-$SOURCE_ROOT/packaging/anolisa/raw/component.toml}"
TARGET_OS="${TARGET_OS:-linux}"
TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"
BUILD_METADATA="${WS_CKPT_BUILD_METADATA:-$BIN_DIR/ws-ckpt-build.toml}"
COMMAND="${1:-}"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

case "$COMMAND" in stage | package) ;; *) die "usage: $0 {stage|package}" ;; esac
# Raw manifests describe the legacy archive tree, not arbitrary install layouts.
if [ "${AWPRJ+x}" = x ] && [ "$AWPRJ" != anolisa ]; then
    die "raw payload requires AWPRJ=anolisa; use make install DESTDIR=<temporary-root> for independent layouts"
fi
case "$TARGET_OS/$TARGET_ARCH" in
    linux/x86_64 | linux/aarch64) ;;
    *) die "unsupported ws-ckpt target: $TARGET_OS/$TARGET_ARCH" ;;
esac
[ -x "$BIN_DIR/ws-ckpt" ] || die "missing executable: $BIN_DIR/ws-ckpt"
VERSION="$(python3 "$SCRIPT_DIR/verify-release.py" "$SOURCE_ROOT" "$CONTRACT" \
    --os "$TARGET_OS" --arch "$TARGET_ARCH")"

if [ -n "${WS_CKPT_BUILD_METADATA:-}" ] || [ -f "$BUILD_METADATA" ]; then
    python3 "$SCRIPT_DIR/verify-binaries.py" "$BIN_DIR/ws-ckpt" \
        --arch "$TARGET_ARCH" --metadata "$BUILD_METADATA" --component-version "$VERSION"
else
    [ "$(uname -s)/$(uname -m)" = "Linux/$TARGET_ARCH" ] \
        || die "cross-target packaging requires build metadata: $BUILD_METADATA"
    python3 "$SCRIPT_DIR/verify-binaries.py" "$BIN_DIR/ws-ckpt" \
        --arch "$TARGET_ARCH" --component-version "$VERSION"
    REPORTED="$("$BIN_DIR/ws-ckpt" --version)"
    [ "$REPORTED" = "ws-ckpt $VERSION" ] || die "binary reports $REPORTED, expected $VERSION"
fi

stage_payload() {
    local stage="$1"
    if [ -d "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "staging directory must be empty: $stage"
    fi
    local plugins="$stage/share/anolisa/plugins"
    install -d -m 0755 "$stage/.anolisa" "$stage/share/anolisa/hooks/ws-ckpt" "$stage/share/doc/ws-ckpt"
    install -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    make -s -C "$SOURCE_ROOT" install-core-files install-plugin-files install-skills-files \
        DESTDIR="$stage" BINARY="$BIN_DIR/ws-ckpt" BINDIR=/bin \
        SYSTEMD_SYSTEM_DIR=/share/anolisa/ws-ckpt CONFIG_DIR=/share/anolisa/ws-ckpt \
        SERVICE_BINDIR='{bindir}' INSTALL_LIVE_CONFIG=0 \
        RUNTIME_SKILLS_DIR=/share/anolisa/skills/ws-ckpt \
        COSH_SKILLS_DIR=/share/anolisa/skills/ws-ckpt \
        ADAPTER_DIR=/share/anolisa/adapters/ws-ckpt PLUGINS_DIR=/share/anolisa/plugins/ws-ckpt
    install -m 0755 "$SCRIPT_DIR/assets/hooks/pre-uninstall.sh" \
        "$stage/share/anolisa/hooks/ws-ckpt/"
    find "$plugins/ws-ckpt" \
        \( -type d \( -name __pycache__ -o -name __tests__ -o -name tests \) \
        -o -type f \( -name '*.pyc' -o -name '.coverage' \) \) -exec rm -rf {} +
    rm -f "$plugins/ws-ckpt/openclaw/dist/vitest.config."*
    # LICENSE is a source symlink in the monorepo; install copies its bytes.
    install -m 0644 "$SOURCE_ROOT/LICENSE" "$SOURCE_ROOT/README.md" "$stage/share/doc/ws-ckpt/"
    python3 - "$stage" "$VERSION" <<'PY'
import json
from pathlib import Path
import sys
stage = Path(sys.argv[1])
inventory = {
    "component": "ws-ckpt", "version": sys.argv[2],
    "targets": {
        framework: {
            "compatibleVersions": "*",
            "actions": {
                action: f"{framework}/{action}-{framework}.sh"
                for action in ("detect", "install", "uninstall")
            },
        }
        for framework in ("openclaw", "hermes")
    },
}
(stage / "share/anolisa/adapters/ws-ckpt/manifest.json").write_text(
    json.dumps(inventory, indent=2) + "\n"
)
PY
    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    chmod 0755 "$stage/bin/ws-ckpt" "$stage/share/anolisa/hooks/ws-ckpt/pre-uninstall.sh"
    find "$stage/share/anolisa/adapters/ws-ckpt" -type f \
        \( -name 'detect-*.sh' -o -name 'install-*.sh' -o -name 'uninstall-*.sh' \) \
        -exec chmod 0755 {} +
    [ -z "$(find "$stage" -type l -print -quit)" ] || die "raw payload contains a symlink"
}

if [ "$COMMAND" = stage ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage"
    stage_payload "$DESTDIR"
    exit 0
fi
OUTPUT_DIR="${OUTPUT_DIR:-$SOURCE_ROOT/src/target/raw}"
EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$SOURCE_ROOT" log -1 --format=%ct -- .)}"
case "$EPOCH" in '' | *[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;; esac
tar --version | grep -q 'GNU tar' || die "GNU tar is required for reproducible packages"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
stage_payload "$WORK/stage"
install -d -m 0755 "$OUTPUT_DIR"
ARTIFACT="ws-ckpt-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
LC_ALL=C tar --sort=name --mtime="@$EPOCH" --owner=0 --group=0 --numeric-owner \
    --hard-dereference --format=gnu -C "$WORK/stage" -cf - . | gzip -n -9 > "$WORK/$ARTIFACT"
mv "$WORK/$ARTIFACT" "$OUTPUT_DIR/$ARTIFACT"
printf '%s\n' "$OUTPUT_DIR/$ARTIFACT"
