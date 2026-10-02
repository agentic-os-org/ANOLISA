#!/usr/bin/env bash
# Assemble prebuilt AgentSight binaries into the ANOLISA raw-package layout.
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
        *) die "unsupported AgentSight target OS: $1" ;;
    esac
}

normalize_arch() {
    case "$1" in
        x86_64 | amd64 | x64) printf 'x86_64\n' ;;
        aarch64 | arm64) printf 'aarch64\n' ;;
        *) die "unsupported AgentSight target architecture: $1" ;;
    esac
}

validate_target() {
    case "$TARGET_OS-$TARGET_ARCH" in
        linux-x86_64 | macos-aarch64) ;;
        *) die "unsupported AgentSight raw target: $TARGET_OS-$TARGET_ARCH" ;;
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

    if ! output="$("$BIN_DIR/agentsight" --version 2>&1)"; then
        die "agentsight --version failed: $output"
    fi
    reported="$(printf '%s\n' "$output" | awk 'NR == 1 { print $NF; exit }')"
    [ "$reported" = "$VERSION" ] || \
        die "agentsight reports $reported but contract says $VERSION"
}

normalize_modes() {
    local stage="$1"

    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    find "$stage/bin" -type f -exec chmod 0755 {} +
}

stage_payload() {
    local stage="$1"

    if [ -e "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "DESTDIR must be empty: $stage"
    fi

    install -d -m 0755 "$stage/.anolisa" "$stage/bin"
    install -p -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -p -m 0755 "$BIN_DIR/agentsight" "$stage/bin/agentsight"
    if [ "$TARGET_OS" = "linux" ]; then
        install -d -m 0755 "$stage/share/anolisa/agentsight"
        install -p -m 0755 "$BIN_DIR/agentsight-enforcer" \
            "$stage/bin/agentsight-enforcer"
        install -p -m 0755 "$SOURCE_ROOT/scripts/agentsight-start.sh" \
            "$stage/bin/agentsight-start"
        install -p -m 0644 \
            "$SOURCE_ROOT/scripts/agentsight.service" \
            "$SOURCE_ROOT/scripts/agentsight-enforcer.service" \
            "$stage/share/anolisa/agentsight/"
    fi
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
SOURCE_ROOT="${AGENTSIGHT_SOURCE_DIR:-$DEFAULT_ROOT}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/target/release}"
TARGET_OS="$(normalize_os "${TARGET_OS:-$(detect_os)}")"
TARGET_ARCH="$(normalize_arch "${TARGET_ARCH:-$(detect_arch)}")"
CONTRACT="${RAW_CONTRACT:-$(default_contract_path)}"
BUILD_METADATA="${AGENTSIGHT_BUILD_METADATA:-$BIN_DIR/agentsight-build.toml}"

validate_target
for input in \
    "$CONTRACT" \
    "$SOURCE_ROOT/Cargo.toml"; do
    require_file "$input"
done
[ -x "$BIN_DIR/agentsight" ] || die "missing executable: $BIN_DIR/agentsight"
BINARIES=("$BIN_DIR/agentsight")
if [ "$TARGET_OS" = "linux" ]; then
    for input in \
        "$SOURCE_ROOT/scripts/agentsight-start.sh" \
        "$SOURCE_ROOT/scripts/agentsight.service" \
        "$SOURCE_ROOT/scripts/agentsight-enforcer.service"; do
        require_file "$input"
    done
    [ -x "$BIN_DIR/agentsight-enforcer" ] || \
        die "missing executable: $BIN_DIR/agentsight-enforcer"
    BINARIES+=("$BIN_DIR/agentsight-enforcer")
fi

VERSION="$(
    python3 "$SCRIPT_DIR/verify-release.py" \
        "$SOURCE_ROOT" "$CONTRACT" \
        --os "$TARGET_OS" --arch "$TARGET_ARCH"
)"
if [ -n "${AGENTSIGHT_BUILD_METADATA:-}" ]; then
    require_file "$BUILD_METADATA"
fi
if [ -f "$BUILD_METADATA" ]; then
    python3 "$SCRIPT_DIR/verify-binaries.py" \
        --os "$TARGET_OS" \
        --arch "$TARGET_ARCH" \
        --metadata "$BUILD_METADATA" \
        --component-version "$VERSION" \
        "${BINARIES[@]}"
elif [ "$(normalize_os "$(detect_os)")-$(normalize_arch "$(detect_arch)")" = \
    "$TARGET_OS-$TARGET_ARCH" ]; then
    python3 "$SCRIPT_DIR/verify-binaries.py" \
        --os "$TARGET_OS" \
        --arch "$TARGET_ARCH" \
        "${BINARIES[@]}"
    verify_native_binary_version
else
    die "cross-target packaging requires build metadata: $BUILD_METADATA"
fi

if [ "$COMMAND" = "stage" ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage"
    stage_payload "$DESTDIR"
    printf 'Staged AgentSight %s for %s-%s at %s\n' \
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
ARTIFACT="agentsight-${VERSION}-${TARGET_OS}-${TARGET_ARCH}.tar.gz"
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
