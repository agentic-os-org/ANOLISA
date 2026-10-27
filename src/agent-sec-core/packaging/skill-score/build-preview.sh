#!/usr/bin/env bash
# Build one standard raw preview plus separately versioned Docker resources.
set -euo pipefail
[ "$#" -eq 1 ] || { echo "usage: $0 OUTPUT_DIR" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "Linux x86_64 is required" >&2; exit 1; }
score_component="$(cd "$(dirname "$0")/../.." && pwd)"
score_repo="$(git -C "$score_component" rev-parse --show-toplevel)"
score_output="$(realpath -m "$1")"
[ ! -e "$score_output" ] || { echo "output directory must not exist" >&2; exit 2; }
[ -z "$(git -C "$score_repo" status --porcelain)" ] || { echo "preview requires a clean commit" >&2; exit 1; }
score_source="$(git -C "$score_repo" rev-parse HEAD)"
score_base="$(python3 "$score_component/packaging/raw/verify_release.py" "$score_component" "$score_component/.anolisa/component.toml")"
score_version="$score_base-skillscore.${score_source:0:12}"
score_image_args=()
if [ -n "${ASC_WORK_BASE_IMAGE:-}" ]; then
    [[ "$ASC_WORK_BASE_IMAGE" =~ ^([a-zA-Z0-9._:/-]+@)?sha256:[a-f0-9]{64}$ ]] || { echo "work base must use an immutable reference" >&2; exit 2; }
    score_image_args+=(--base-image "$ASC_WORK_BASE_IMAGE")
    score_image_id="$(docker image inspect --format '{{.Id}}' "$ASC_WORK_BASE_IMAGE")"
    [[ "$score_image_id" =~ ^sha256:[a-f0-9]{64}$ ]] || { echo "invalid work base image ID" >&2; exit 2; }
    score_version+=".base.${score_image_id:7:12}"
fi
if [ -n "${ASC_WORK_BASE_RECIPE:-}" ]; then
    [ -n "${ASC_WORK_BASE_IMAGE:-}" ] || { echo "ASC_WORK_BASE_RECIPE requires ASC_WORK_BASE_IMAGE" >&2; exit 2; }
    score_image_args+=(--base-image-recipe "$ASC_WORK_BASE_RECIPE")
fi
score_assets="${ASC_PREVIEW_ASSET_BASE:-https://github.com/${GITHUB_REPOSITORY:-1570005763/anolisa}/releases/download/sec-core-v$score_version}"
score_epoch="$(git -C "$score_repo" show -s --format=%ct "$score_source")"
mkdir -p "$score_output"
make -C "$score_component" package-raw RAW_PREVIEW_VERSION="$score_version" OUTPUT_DIR="$score_output"
python3 "$score_component/skill-score-runtime/image/build.py" "$score_output" \
    --version "$score_version" --build-network "${ASC_IMAGE_BUILD_NETWORK:-default}" "${score_image_args[@]}"
gzip -n "$score_output/skill-score-images.tar"
python3 "$score_component/packaging/skill-score/package-optional.py" "$score_output" \
    --version "$score_version" --source-commit "$score_source" --asset-base "$score_assets" --epoch "$score_epoch"
cargo build --manifest-path "$score_repo/distribution/anolisa/Cargo.toml" --release --locked -p anolisa-cli
tar --owner=0 --group=0 --mtime="@$score_epoch" -czf \
    "$score_output/anolisa-cli-0.3.20-linux-x86_64.tar.gz" \
    -C "$score_repo/distribution/anolisa/target/release" anolisa
git -C "$score_repo" archive --format=tar.gz --output="$score_output/source.tar.gz" \
    "$score_source" src/agent-sec-core distribution/anolisa
git -C "$score_repo" diff --exit-code
python3 "$score_component/packaging/skill-score/prepare-preview.py" "$score_output" \
    --source-commit "$score_source" --version "$score_version" \
    --components "$score_repo/distribution/anolisa/manifests/components-v2.toml"
