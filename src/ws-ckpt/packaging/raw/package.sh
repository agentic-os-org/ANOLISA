#!/usr/bin/env bash
# Assemble ws-ckpt runtime files without building or executing cross binaries.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="${WS_CKPT_SOURCE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/src/target/release}"
CONTRACT="${RAW_CONTRACT:-$SOURCE_ROOT/.anolisa/component.toml}"
TARGET_OS="${TARGET_OS:-linux}"
TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"
BUILD_METADATA="${WS_CKPT_BUILD_METADATA:-$BIN_DIR/ws-ckpt-build.toml}"
COMMAND="${1:-}"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

case "$COMMAND" in stage | package) ;; *) die "usage: $0 {stage|package}" ;; esac
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
    local runtime="$stage/share/anolisa/runtime"
    install -d -m 0755 "$stage/.anolisa" "$stage/bin" \
        "$runtime/skills" "$runtime/ws-ckpt/plugins/openclaw" \
        "$stage/share/anolisa/skills" "$stage/share/anolisa/ws-ckpt" \
        "$stage/share/anolisa/hooks/ws-ckpt" "$stage/share/doc/ws-ckpt"
    install -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -m 0755 "$BIN_DIR/ws-ckpt" "$stage/bin/"
    cp -pr "$SOURCE_ROOT/src/skills/ws-ckpt" "$runtime/skills/ws-ckpt"
    cp -pr "$SOURCE_ROOT/src/skills/ws-ckpt" "$stage/share/anolisa/skills/ws-ckpt"
    install -m 0644 "$SOURCE_ROOT/src/systemd/ws-ckpt.service" \
        "$SOURCE_ROOT/src/config.toml.sample" "$stage/share/anolisa/ws-ckpt/"
    install -m 0755 "$SCRIPT_DIR/assets/hooks/pre-uninstall.sh" \
        "$stage/share/anolisa/hooks/ws-ckpt/"
    for framework in openclaw hermes; do
        local adapter="$stage/share/anolisa/adapters/ws-ckpt/$framework"
        install -d -m 0755 "$adapter"
        install -m 0644 "$SOURCE_ROOT/scripts/$framework/lib-discover.sh" "$adapter/"
        for action in detect install uninstall; do
            install -m 0755 "$SOURCE_ROOT/scripts/$framework/$action-$framework.sh" "$adapter/"
        done
    done
    install -m 0644 "$SOURCE_ROOT/scripts/openclaw/lib-openclaw.sh" \
        "$stage/share/anolisa/adapters/ws-ckpt/openclaw/"
    install -m 0644 "$SOURCE_ROOT/src/plugins/openclaw/package.json" \
        "$SOURCE_ROOT/src/plugins/openclaw/openclaw.plugin.json" \
        "$runtime/ws-ckpt/plugins/openclaw/"
    cp -pr "$SOURCE_ROOT/src/plugins/openclaw/dist" "$runtime/ws-ckpt/plugins/openclaw/"
    cp -pr "$SOURCE_ROOT/src/plugins/hermes" "$runtime/ws-ckpt/plugins/hermes"
    find "$runtime/ws-ckpt/plugins" \
        \( -type d \( -name __pycache__ -o -name __tests__ -o -name tests \) \
        -o -type f \( -name '*.pyc' -o -name '.coverage' \) \) -exec rm -rf {} +
    rm -f "$runtime/ws-ckpt/plugins/openclaw/dist/vitest.config."*
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
