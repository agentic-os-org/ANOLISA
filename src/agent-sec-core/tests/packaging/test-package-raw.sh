#!/usr/bin/env bash
# Fixture-driven regression tests for the V2 raw package.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

BUILD="$TMP/build"
VERSION="$(python3 "$ROOT/packaging/raw/verify_release.py" \
    "$ROOT" "$ROOT/.anolisa/component.toml")"

install -d -m 0755 \
    "$BUILD/v2/bin" \
    "$BUILD/python-runtime/bin" \
    "$BUILD/python-runtime/lib/Tix8.4.3" \
    "$BUILD/python-runtime/lib/python3.11" \
    "$BUILD/python-runtime/lib/tk8.6/demos" \
    "$BUILD/openclaw-plugin" \
    "$BUILD/hermes-plugin/src" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin" \
    "$BUILD/codex-plugin/hooks-plugin/hooks" \
    "$BUILD/qoder-plugin/.qoder-plugin" \
    "$BUILD/qoder-plugin/hooks" \
    "$BUILD/qwen-code-extension/hooks" \
    "$BUILD/cosh-extension/hooks"

for binary in agent-sec-cli agent-sec-daemon; do
    printf '#!/bin/sh\nexit 0\n' > "$BUILD/v2/bin/$binary"
    chmod 0755 "$BUILD/v2/bin/$binary"
done
printf '#!/bin/sh\nexit 0\n' > "$BUILD/linux-sandbox"
chmod 0755 "$BUILD/linux-sandbox"
printf '#!/bin/sh\nexit 0\n' > "$BUILD/python-runtime/bin/python3.11"
chmod 0755 "$BUILD/python-runtime/bin/python3.11"
printf 'license\n' > "$BUILD/python-runtime/lib/python3.11/LICENSE.txt"
printf 'license\n' > "$BUILD/python-runtime/lib/Tix8.4.3/license.terms"
printf 'license\n' > "$BUILD/python-runtime/lib/tk8.6/demos/license.terms"
cp "$ROOT/packaging/raw/assets/python-runtime/PROVENANCE.toml" \
    "$BUILD/python-runtime/PROVENANCE.toml"
(
    cd "$BUILD/python-runtime"
    sha256sum \
        lib/Tix8.4.3/license.terms \
        lib/python3.11/LICENSE.txt \
        lib/tk8.6/demos/license.terms > LICENSES.sha256
)

cp "$ROOT/openclaw-plugin/openclaw.plugin.json" "$BUILD/openclaw-plugin/"
cp "$ROOT/hermes-plugin/src/plugin.yaml" "$BUILD/hermes-plugin/src/"
cp "$ROOT/codex-plugin/hooks-plugin/.codex-plugin/plugin.json" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin/"
cp "$ROOT/codex-plugin/hooks-plugin/hooks/hooks.json" \
    "$BUILD/codex-plugin/hooks-plugin/hooks/"
cp "$ROOT/qoder-plugin/.qoder-plugin/plugin.json" "$BUILD/qoder-plugin/.qoder-plugin/"
cp "$ROOT/qoder-plugin/hooks/hooks.json" "$BUILD/qoder-plugin/hooks/"
cp "$ROOT/qwen-code-extension/qwen-extension.json" "$BUILD/qwen-code-extension/"
cp "$ROOT/cosh-extension/cosh-extension.json" "$BUILD/cosh-extension/"
make -C "$ROOT" stage-skills BUILD_DIR="$BUILD"

run_package() {
    local output="$1"

    BUILD_DIR="$BUILD" \
    OUTPUT_DIR="$output" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1783656696 \
        "$ROOT/packaging/raw/package.sh" package
}

OUT_ONE="$TMP/out-one"
OUT_TWO="$TMP/out-two"
run_package "$OUT_ONE"
run_package "$OUT_TWO"

ARTIFACT="sec-core-${VERSION}-linux-x86_64.tar.gz"
cmp "$OUT_ONE/$ARTIFACT" "$OUT_TWO/$ARTIFACT"

STAGE="$TMP/stage"
make -C "$ROOT" stage-raw \
    BUILD_DIR="$BUILD" \
    DESTDIR="$STAGE" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64

for binary in agent-sec-cli agent-sec-daemon agent-sec-python linux-sandbox; do
    test "$(stat -c '%a' "$STAGE/bin/$binary")" = "755"
done
test ! -e "$STAGE/lib/anolisa/sec-core/python3.11/site-packages"
cmp "$ROOT/.anolisa/component.toml" "$STAGE/.anolisa/component.toml"
grep -Fq 'WantedBy=multi-user.target' \
    "$STAGE/share/anolisa/sec-core/agent-sec-core.service.in"

tar -tzf "$OUT_ONE/$ARTIFACT" > "$TMP/tar-list.txt"
for expected in \
    "./bin/agent-sec-cli" \
    "./bin/agent-sec-daemon" \
    "./bin/agent-sec-python" \
    "./lib/anolisa/sec-core/python3.11/runtime/bin/python3.11"; do
    grep -Fxq "$expected" "$TMP/tar-list.txt"
done
if grep -Eq 'site-packages/agent_sec_cli|agent-sec-cli-wrapper|agent-sec-daemon-wrapper' \
    "$TMP/tar-list.txt"; then
    echo "ERROR: V2 raw archive contains V1 CLI/daemon payload" >&2
    exit 1
fi

echo "OK: agent-sec-core V2 raw package tests passed"
