#!/usr/bin/env bash
# Stage and package agent-sec-core using its component-owned raw contract.
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

copy_tree() {
    local source="$1"
    local destination="$2"

    [ -d "$source" ] || die "missing build output $source"
    install -d -m 0755 "$destination"
    cp -a "$source"/. "$destination"/
}

copy_tree_dereferenced() {
    local source="$1"
    local destination="$2"

    [ -d "$source" ] || die "missing build output $source"
    install -d -m 0755 "$destination"
    cp -RLp "$source"/. "$destination"/
}

validate_bundled_python() {
    local runtime="$1"
    local required

    for required in \
        bin/python3.11 \
        lib/python3.11/LICENSE.txt \
        lib/Tix8.4.3/license.terms \
        lib/tk8.6/demos/license.terms \
        PROVENANCE.toml \
        LICENSES.sha256; do
        [ -f "$runtime/$required" ] || \
            die "bundled Python is missing $required"
    done
    [ -x "$runtime/bin/python3.11" ] || \
        die "bundled Python interpreter is not executable"
    cmp -s "$runtime/PROVENANCE.toml" \
        "$ROOT/packaging/raw/assets/python-runtime/PROVENANCE.toml" || \
        die "bundled Python provenance does not match the pinned runtime"
    (
        cd "$runtime"
        sha256sum --check --strict --status LICENSES.sha256
    ) || die "bundled Python license manifest is invalid"
}

normalize_modes() {
    local stage="$1"

    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    find "$stage/bin" -type f -exec chmod 0755 {} +
    find "$stage/lib/anolisa/sec-core/python3.11/runtime/bin" \
        -type f -exec chmod 0755 {} +
    find "$stage/adapters" "$stage/share/anolisa/skills" \
        -type f \( -name '*.sh' -o -name '*.py' \) -exec chmod 0755 {} +
}

stage_payload() {
    local stage="$1"

    if [ -e "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "DESTDIR must be empty: $stage"
    fi

    install -d -m 0755 \
        "$stage/.anolisa" \
        "$stage/bin" \
        "$stage/lib/anolisa/sec-core/python3.11/runtime" \
        "$stage/lib/anolisa/sec-core/python3.11/site-packages" \
        "$stage/adapters/sec-core/openclaw" \
        "$stage/adapters/sec-core/hermes" \
        "$stage/adapters/sec-core/codex" \
        "$stage/adapters/sec-core/qoder" \
        "$stage/adapters/sec-core/qwencode" \
        "$stage/adapters/sec-core/cosh" \
        "$stage/share/anolisa/skills" \
        "$stage/share/anolisa/sec-core" \
        "$stage/share/doc/sec-core"

    install -p -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -p -m 0755 "$BUILD_DIR/linux-sandbox" "$stage/bin/linux-sandbox"
    install -p -m 0755 "$ROOT/packaging/raw/assets/bin/agent-sec-cli" \
        "$stage/bin/agent-sec-cli"
    install -p -m 0755 "$ROOT/packaging/raw/assets/bin/agent-sec-daemon" \
        "$stage/bin/agent-sec-daemon"
    install -p -m 0755 "$ROOT/packaging/raw/assets/bin/agent-sec-python" \
        "$stage/bin/agent-sec-python"
    install -p -m 0644 \
        "$ROOT/packaging/systemd/agent-sec-core.service.in" \
        "$stage/share/anolisa/sec-core/agent-sec-core.service.in"
    install -p -m 0644 "$ROOT/LICENSE" "$stage/share/doc/sec-core/LICENSE"

    copy_tree "$BUILD_DIR/site-packages" \
        "$stage/lib/anolisa/sec-core/python3.11/site-packages"
    validate_bundled_python "$BUILD_DIR/python-runtime"
    copy_tree_dereferenced "$BUILD_DIR/python-runtime" \
        "$stage/lib/anolisa/sec-core/python3.11/runtime"
    copy_tree "$BUILD_DIR/openclaw-plugin" "$stage/adapters/sec-core/openclaw"
    copy_tree "$BUILD_DIR/hermes-plugin/src" "$stage/adapters/sec-core/hermes"
    if [ -d "$BUILD_DIR/hermes-plugin/scripts" ]; then
        copy_tree "$BUILD_DIR/hermes-plugin/scripts" \
            "$stage/adapters/sec-core/hermes/scripts"
    fi
    copy_tree "$BUILD_DIR/codex-plugin/hooks-plugin" "$stage/adapters/sec-core/codex"
    copy_tree "$BUILD_DIR/qoder-plugin" "$stage/adapters/sec-core/qoder"
    copy_tree "$BUILD_DIR/qwen-code-extension" "$stage/adapters/sec-core/qwencode"
    copy_tree "$BUILD_DIR/cosh-extension" "$stage/adapters/sec-core/cosh"
    copy_tree "$BUILD_DIR/skills" "$stage/share/anolisa/skills"

    python3 "$ROOT/packaging/raw/adapt_payload.py" "$stage"

    find "$stage/lib/anolisa/sec-core/python3.11" \
        -type d -name __pycache__ -prune -exec rm -rf {} +
    find "$stage/lib/anolisa/sec-core/python3.11" \
        -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete
    validate_bundled_python \
        "$stage/lib/anolisa/sec-core/python3.11/runtime"
    if [ -n "$(find "$stage/lib/anolisa/sec-core/python3.11/runtime" \
        -type l -print -quit)" ]; then
        die "bundled Python runtime contains a symbolic link"
    fi
    normalize_modes "$stage"
    python3 "$ROOT/packaging/raw/verify_release.py" \
        "$ROOT" "$CONTRACT" --payload-root "$stage" > /dev/null
}

resolve_epoch() {
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        printf '%s\n' "$SOURCE_DATE_EPOCH"
        return
    fi
    git -C "$ROOT" log -1 --format=%ct -- . 2>/dev/null || \
        die "SOURCE_DATE_EPOCH is unset and the source commit time is unavailable"
}

COMMAND="${1:-}"
[ "$COMMAND" = "stage" ] || [ "$COMMAND" = "package" ] || \
    die "usage: $0 {stage|package}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="${BUILD_DIR:-$ROOT/target}"
CONTRACT="${RAW_CONTRACT:-$ROOT/.anolisa/component.toml}"
TARGET_OS="${TARGET_OS:-linux}"
TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"

[ "$TARGET_OS" = "linux" ] || die "raw packages support Linux only"
[ "$TARGET_ARCH" = "x86_64" ] || die "raw packages currently support x86_64 only"
[ -f "$CONTRACT" ] || die "raw contract not found: $CONTRACT"
[ -x "$BUILD_DIR/linux-sandbox" ] || die "missing build output $BUILD_DIR/linux-sandbox"

VERSION="$(python3 "$ROOT/packaging/raw/verify_release.py" "$ROOT" "$CONTRACT")"

if [ "$COMMAND" = "stage" ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage-raw"
    stage_payload "$DESTDIR"
    printf 'Staged sec-core %s raw payload at %s\n' "$VERSION" "$DESTDIR"
    exit 0
fi

OUTPUT_DIR="${OUTPUT_DIR:-$BUILD_DIR/raw}"
EPOCH="$(resolve_epoch)"
case "$EPOCH" in
    ''|*[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;;
esac

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
ARTIFACT="sec-core-${VERSION}-${TARGET_OS}-${TARGET_ARCH}.tar.gz"
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
