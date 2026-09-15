#!/usr/bin/env bash
# Assemble prebuilt Tokenless binaries into the ANOLISA raw-package layout.
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
        *) die "unsupported target OS: $1" ;;
    esac
}

normalize_arch() {
    case "$1" in
        x86_64 | amd64 | x64) printf 'x86_64\n' ;;
        aarch64 | arm64) printf 'aarch64\n' ;;
        *) die "unsupported target architecture: $1" ;;
    esac
}

validate_target() {
    case "$TARGET_OS-$TARGET_ARCH" in
        linux-x86_64 | linux-aarch64 | macos-aarch64) ;;
        macos-x86_64) die "Tokenless raw packages do not support macOS x86_64" ;;
        *) die "unsupported Tokenless raw target: $TARGET_OS-$TARGET_ARCH" ;;
    esac
}

require_file() {
    [ -f "$1" ] || die "missing packaging input: $1"
}

normalize_modes() {
    local stage="$1"

    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    chmod 0755 \
        "$stage/bin/tokenless" \
        "$stage/libexec/anolisa/tokenless/rtk"
    find "$stage/adapters" "$stage/extensions/tokenless" \
        -type f \( -name '*.py' -o -name '*.sh' -o \
        -path '*/codex/scripts/*' \) -exec chmod 0755 {} +
}

materialize_hook() {
    local stage="$1"
    local relative="$2"
    local path="$stage/adapters/$relative"

    if [ -L "$path" ]; then
        install -p -m 0755 "$path" "$path.materialized"
        mv -f "$path.materialized" "$path"
    fi
    [ -f "$path" ] || die "missing staged adapter hook: $relative"
}

stage_payload() {
    local stage="$1"
    local adapters="$SOURCE_ROOT/adapters/tokenless"
    local relative

    if [ -e "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "DESTDIR must be empty: $stage"
    fi

    for relative in \
        manifest.json \
        openclaw/package.json \
        openclaw/openclaw.plugin.json \
        openclaw/dist/index.js \
        dsh/package.json \
        dsh/cordis.patch.yml \
        dsh/dist/index.js \
        hermes/plugin.yaml \
        qoder/.qoder-plugin/plugin.json \
        claude-code/.claude-plugin/marketplace.json \
        claude-code/.claude-plugin/plugin.json \
        claude-code/hooks/run-hook.sh \
        codex/.codex-plugin/plugin.json \
        qwencode/qwen-extension.json \
        qwencode/hooks/run-hook.sh \
        qwenpaw/plugin.json \
        qwenpaw/plugin.py \
        qwenpaw/requirements.txt \
        common/cosh-extension.json \
        common/tool-ready-spec.json \
        common/tokenless-env-fix.sh; do
        require_file "$adapters/$relative"
    done

    install -d -m 0755 \
        "$stage/.anolisa" \
        "$stage/bin" \
        "$stage/libexec/anolisa/tokenless" \
        "$stage/adapters" \
        "$stage/extensions/tokenless/hooks" \
        "$stage/extensions/tokenless/commands"
    install -p -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -p -m 0755 "$BIN_DIR/tokenless" "$stage/bin/tokenless"
    install -p -m 0755 "$BIN_DIR/rtk" \
        "$stage/libexec/anolisa/tokenless/"
    cp -a "$adapters"/. "$stage/adapters"/

    # A checkout upgraded from the former adapter layout can retain formerly
    # ignored AgentScope build artifacts. They are not part of this payload.
    rm -rf "$stage/adapters/agentscope"

    # ANOLISA intentionally ignores archive symlinks. The source tree shares
    # these dispatchers by symlink, so raw packages must carry regular files.
    materialize_hook "$stage" "claude-code/hooks/run-hook.sh"
    materialize_hook "$stage" "qwencode/hooks/run-hook.sh"
    materialize_hook "$stage" "workbuddy/hooks/run-hook.sh"

    install -p -m 0644 \
        "$adapters/common/cosh-extension.json" \
        "$adapters/common/tool-ready-spec.json" \
        "$stage/extensions/tokenless/"
    install -p -m 0755 "$adapters/common/tokenless-env-fix.sh" \
        "$stage/extensions/tokenless/"
    cp -a "$adapters/common/hooks"/. "$stage/extensions/tokenless/hooks"/
    cp -a "$adapters/common/commands"/. "$stage/extensions/tokenless/commands"/

    # Match the installed adapter payload and exclude build-only inputs.
    rm -rf "$stage/adapters/openclaw/node_modules"
    rm -f \
        "$stage/adapters/openclaw/package-lock.json" \
        "$stage/adapters/openclaw/index.ts" \
        "$stage/adapters/openclaw/tsconfig.json"
    find "$stage/adapters" "$stage/extensions/tokenless" \
        \( -name '*.in' -o -name '.gitignore' \) -delete
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
SOURCE_ROOT="${TOKENLESS_SOURCE_DIR:-$DEFAULT_ROOT}"
BIN_DIR="${BIN_DIR:-}"
CONTRACT="${RAW_CONTRACT:-$SOURCE_ROOT/.anolisa/component.toml}"
TARGET_OS="$(normalize_os "${TARGET_OS:-$(detect_os)}")"
TARGET_ARCH="$(normalize_arch "${TARGET_ARCH:-$(detect_arch)}")"

validate_target
[ -n "$BIN_DIR" ] || die "BIN_DIR must contain prebuilt tokenless and rtk binaries"
for binary in tokenless rtk; do
    [ -x "$BIN_DIR/$binary" ] || die "missing executable: $BIN_DIR/$binary"
done
require_file "$CONTRACT"

VERSION="$(python3 "$SCRIPT_DIR/verify-release.py" "$SOURCE_ROOT" "$CONTRACT")"
python3 "$SCRIPT_DIR/verify-binaries.py" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    "$BIN_DIR/tokenless" "$BIN_DIR/rtk"

if [ "$COMMAND" = "stage" ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage"
    stage_payload "$DESTDIR"
    printf 'Staged tokenless %s for %s-%s at %s\n' \
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
ARTIFACT="tokenless-${VERSION}-${TARGET_OS}-${TARGET_ARCH}.tar.gz"
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
