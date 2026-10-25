#!/usr/bin/env bash
# Cross-compile and exercise Windows release artifacts on the Linux runner.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
SOURCE="$(realpath "$1")"
OUTPUT="$(realpath "$2")"
WHEELS="$(realpath "$3")"
VERSION="$4"
IMAGE=anolisa/rust-release-builder:windows-msvc-x86_64
IMAGE_ID=sha256:7649bcb55d8c5632232c7f27e3c2191b71fb7c31149de87412c447fc7a997303
# The containerd store identifies the same image by its manifest digest.
MANIFEST_ID=sha256:1392a2684a4853a3da0219caef07c4b47f8833775b1559f18d54cd656abefb88
actual="$(docker image inspect "$IMAGE" --format '{{.Id}}')"
if [[ "$actual" != "$IMAGE_ID" && "$actual" != "$MANIFEST_ID" ]]; then
    printf 'ERROR: %s must match frozen config %s or manifest %s, found %s\n' \
        "$IMAGE" "$IMAGE_ID" "$MANIFEST_ID" "$actual" >&2
    exit 1
fi

CACHE="${TOKENLESS_WINDOWS_CACHE:-${HOME}/.cache/tokenless-windows/${IMAGE_ID#sha256:}}"
IMAGE_ID="$actual"
WORK="${OUTPUT}.build"
mkdir -p "$CACHE" "$WORK/out" "$WORK/wine"
CACHE="$(realpath "$CACHE")"
container=(docker run --rm --init --user "$(id -u):$(id -g)"
    --entrypoint /bin/bash
    --env "BUILDER_IMAGE_ID=$IMAGE_ID" --env "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH"
    --env HOME=/wine/home --env CARGO_BUILD_JOBS=8
    --env HTTP_PROXY --env HTTPS_PROXY --env ALL_PROXY --env NO_PROXY
    --env http_proxy --env https_proxy --env all_proxy --env no_proxy
    --volume "$SOURCE:/source"
    --volume "$ACTION_DIR:/action:ro" --volume "$COMMON_DIR:/common:ro"
    --volume "$CACHE:/cache" --volume "$WORK/out:/out" --volume "$WORK/wine:/wine"
    --volume "$OUTPUT:/artifacts" --volume "$WHEELS:/wheels")
"${container[@]}" --network host "$IMAGE_ID" /action/windows-container.sh build \
    2>&1 | tee "$WORK/out/build.log"

mkdir -p "$SOURCE/target/npm-prebuilt/win32-x64"
cp "$WORK/out/build/x86_64-pc-windows-msvc/release/"{tokenless,rtk}.exe \
    "$SOURCE/target/npm-prebuilt/win32-x64/"
node "$SOURCE/npm/scripts/package-npm.js" --target win32-x64
python3 "$ACTION_DIR/package-windows.py" "$SOURCE" "$OUTPUT" "$VERSION"
python3 "$ACTION_DIR/verify-wheels.py" --directory "$WHEELS" --version "$VERSION" \
    --layout flat --os windows --arch x86_64

# Smoke the extracted release ZIP and wheel, with no network or source mounts needed by the test.
"${container[@]}" --network none "$IMAGE_ID" /action/windows-container.sh smoke "$VERSION"
ARCHIVE="tokenless-$VERSION-windows-x86_64.zip"
(
    cd "$OUTPUT"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT/$ARCHIVE" --component tokenless --version "$VERSION" \
    --os windows --arch x86_64 --target x86_64-pc-windows-msvc \
    --project-dir "$SOURCE" --project-dir "$SOURCE/third_party/rtk" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$OUTPUT" \
    --component tokenless --version "$VERSION" --layout flat --os windows --arch x86_64
printf 'Windows ZIP (CLI, RTK, npm packages), ABI3 wheel and Wine smoke passed; image=%s\n' "$IMAGE_ID"
