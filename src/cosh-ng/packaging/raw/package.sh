#!/usr/bin/env bash
# Assemble prebuilt cosh-ng binaries into the ANOLISA raw-package layout.
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

detect_os() {
    case "$(uname -s)" in
        Linux) printf 'linux\n' ;;
        Darwin) printf 'macos\n' ;;
        *) die "unsupported host OS; set TARGET_OS explicitly" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64 | amd64) printf 'x86_64\n' ;;
        aarch64 | arm64) printf 'aarch64\n' ;;
        *) die "unsupported host architecture; set TARGET_ARCH explicitly" ;;
    esac
}

normalize_os() {
    case "$1" in
        linux) printf 'linux\n' ;;
        macos | darwin) printf 'macos\n' ;;
        *) die "unsupported cosh-ng target OS: $1" ;;
    esac
}

normalize_arch() {
    case "$1" in
        x86_64 | amd64 | x64) printf 'x86_64\n' ;;
        aarch64 | arm64) printf 'aarch64\n' ;;
        *) die "unsupported cosh-ng target architecture: $1" ;;
    esac
}

validate_target() {
    case "$TARGET_OS-$TARGET_ARCH" in
        linux-x86_64 | linux-aarch64 | macos-aarch64 | macos-x86_64) ;;
        *) die "unsupported cosh-ng raw target: $TARGET_OS-$TARGET_ARCH" ;;
    esac
}

require_file() {
    [ -f "$1" ] || die "missing packaging input: $1"
}

default_contract_path() {
    case "$TARGET_OS" in
        linux) printf '%s/.anolisa/component.toml\n' "$SOURCE_ROOT" ;;
        macos) printf '%s/.anolisa/component.macos.toml\n' "$SOURCE_ROOT" ;;
    esac
}

verify_native_binary_version() {
    local output reported

    if [ "$HAS_BUILD_METADATA" -eq 1 ]; then
        return
    fi
    if ! output="$("$BIN_DIR/cosh-cli" --version 2>&1)"; then
        die "cosh-cli --version failed: $output"
    fi
    reported="$(printf '%s\n' "$output" | awk 'NR == 1 { print $NF; exit }')"
    [ "$reported" = "$VERSION" ] || \
        die "cosh-cli reports $reported but contract says $VERSION"
}

normalize_modes() {
    local stage="$1"

    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    chmod 0755 \
        "$stage/bin/cosh-cli" \
        "$stage/bin/cosh" \
        "$stage/bin/cosh-switch" \
        "$stage/libexec/anolisa/cosh-ng/cosh-core" \
        "$stage/libexec/anolisa/cosh-ng/cosh-gateway" \
        "$stage/libexec/anolisa/cosh-ng/cosh-shell"
}

stage_payload() {
    local stage="$1"

    if [ -e "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "DESTDIR must be empty: $stage"
    fi

    install -d -m 0755 \
        "$stage/.anolisa" \
        "$stage/bin" \
        "$stage/libexec/anolisa/cosh-ng" \
        "$stage/share/doc/cosh-ng"
    install -p -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -p -m 0755 "$BIN_DIR/cosh-cli" "$stage/bin/cosh-cli"
    install -p -m 0755 "$BIN_DIR/cosh-core" "$BIN_DIR/cosh-gateway" \
        "$BIN_DIR/cosh-shell" \
        "$stage/libexec/anolisa/cosh-ng/"
    install -p -m 0755 \
        "$SCRIPT_DIR/assets/bin/cosh" \
        "$SCRIPT_DIR/assets/bin/cosh-switch" \
        "$stage/bin/"
    if [ "$TARGET_OS" = "linux" ]; then
        install -d -m 0755 \
            "$stage/share/anolisa/cosh-ng" \
            "$stage/share/anolisa/skills/manage-task-checkpoints"
        install -p -m 0644 \
            "$SOURCE_ROOT/packaging/systemd/cosh-gateway@.service.in" \
            "$stage/share/anolisa/cosh-ng/"
        install -p -m 0644 \
            "$SOURCE_ROOT/skills/manage-task-checkpoints/SKILL.md" \
            "$stage/share/anolisa/skills/manage-task-checkpoints/SKILL.md"
    fi
    install -p -m 0644 "$SOURCE_ROOT/LICENSE" "$stage/share/doc/cosh-ng/LICENSE"
    install -p -m 0644 "$SOURCE_ROOT/README.md" "$stage/share/doc/cosh-ng/README.md"
    normalize_modes "$stage"

    if [ -n "$(find "$stage" -type l -print -quit)" ]; then
        die "raw payload contains a symbolic link"
    fi
}

resolve_epoch() {
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        printf '%s\n' "$SOURCE_DATE_EPOCH"
        return
    fi
    git -C "$SOURCE_ROOT" log -1 --format=%ct -- . 2>/dev/null || \
        die "SOURCE_DATE_EPOCH is unset and the source commit time is unavailable"
}

COMMAND="${1:-}"
[ "$COMMAND" = "stage" ] || [ "$COMMAND" = "package" ] || \
    die "usage: $0 {stage|package}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SOURCE_ROOT="${COSH_NG_SOURCE_DIR:-$DEFAULT_ROOT}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/target/release}"
TARGET_OS="$(normalize_os "${TARGET_OS:-$(detect_os)}")"
TARGET_ARCH="$(normalize_arch "${TARGET_ARCH:-$(detect_arch)}")"
CONTRACT="${RAW_CONTRACT:-$(default_contract_path)}"
BUILD_METADATA="${COSH_NG_BUILD_METADATA:-$BIN_DIR/cosh-ng-build.toml}"

validate_target
for input in \
    "$CONTRACT" \
    "$SOURCE_ROOT/Cargo.toml" \
    "$SOURCE_ROOT/LICENSE" \
    "$SOURCE_ROOT/README.md" \
    "$SOURCE_ROOT/skills/manage-task-checkpoints/SKILL.md" \
    "$SOURCE_ROOT/packaging/systemd/cosh-gateway@.service.in" \
    "$SCRIPT_DIR/assets/bin/cosh" \
    "$SCRIPT_DIR/assets/bin/cosh-switch"; do
    require_file "$input"
done
for binary in cosh-cli cosh-core cosh-gateway cosh-shell; do
    [ -x "$BIN_DIR/$binary" ] || die "missing executable: $BIN_DIR/$binary"
done

VERSION="$(
    python3 "$SCRIPT_DIR/verify-release.py" \
        "$SOURCE_ROOT" "$CONTRACT" \
        --os "$TARGET_OS" --arch "$TARGET_ARCH"
)"
HAS_BUILD_METADATA=0
if [ -n "${COSH_NG_BUILD_METADATA:-}" ]; then
    require_file "$BUILD_METADATA"
fi
if [ -f "$BUILD_METADATA" ]; then
    HAS_BUILD_METADATA=1
    python3 "$SCRIPT_DIR/verify-binaries.py" \
        --os "$TARGET_OS" \
        --arch "$TARGET_ARCH" \
        --metadata "$BUILD_METADATA" \
        --component-version "$VERSION" \
        "$BIN_DIR/cosh-cli" "$BIN_DIR/cosh-core" "$BIN_DIR/cosh-gateway" \
        "$BIN_DIR/cosh-shell"
elif [ "$(detect_os)-$(detect_arch)" != "$TARGET_OS-$TARGET_ARCH" ]; then
    die "cross-target packaging requires build metadata: $BUILD_METADATA"
else
    python3 "$SCRIPT_DIR/verify-binaries.py" \
        --os "$TARGET_OS" \
        --arch "$TARGET_ARCH" \
        "$BIN_DIR/cosh-cli" "$BIN_DIR/cosh-core" "$BIN_DIR/cosh-gateway" \
        "$BIN_DIR/cosh-shell"
fi
verify_native_binary_version

if [ "$COMMAND" = "stage" ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage"
    stage_payload "$DESTDIR"
    printf 'Staged cosh-ng %s for %s-%s at %s\n' \
        "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$DESTDIR"
    exit 0
fi

OUTPUT_DIR="${OUTPUT_DIR:-$SOURCE_ROOT/target/raw}"
EPOCH="$(resolve_epoch)"
case "$EPOCH" in
    '' | *[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;;
esac
tar --version 2>/dev/null | grep -q 'GNU tar' || \
    die "GNU tar is required for reproducible raw packages"

WORK="$(mktemp -d)"
TEMP_ARTIFACT=""
cleanup() {
    rm -rf "$WORK"
    if [ -n "$TEMP_ARTIFACT" ]; then
        rm -f "$TEMP_ARTIFACT"
    fi
}
trap cleanup EXIT

STAGE="$WORK/stage"
stage_payload "$STAGE"
install -d -m 0755 "$OUTPUT_DIR"
ARTIFACT="cosh-ng-${VERSION}-${TARGET_OS}-${TARGET_ARCH}.tar.gz"
TEMP_ARTIFACT="$OUTPUT_DIR/.${ARTIFACT}.tmp.$$"
LC_ALL=C tar \
    --sort=name \
    --mtime="@$EPOCH" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --hard-dereference \
    --format=gnu \
    -C "$STAGE" \
    -cf - . | gzip -n -9 > "$TEMP_ARTIFACT"
mv -f "$TEMP_ARTIFACT" "$OUTPUT_DIR/$ARTIFACT"
TEMP_ARTIFACT=""
printf '%s\n' "$OUTPUT_DIR/$ARTIFACT"
