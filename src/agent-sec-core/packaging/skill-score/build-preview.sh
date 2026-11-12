#!/usr/bin/env bash
# Build standard raw components, then publish independent base/optional bundles.
set -euo pipefail
[ "$#" -eq 1 ] || { echo "usage: $0 OUTPUT_DIR" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "Linux x86_64 is required" >&2; exit 1; }
score_component="$(cd "$(dirname "$0")/../.." && pwd)"
score_repo="$(git -C "$score_component" rev-parse --show-toplevel)"
score_output="$(realpath -m "$1")"
[ ! -e "$score_output" ] || { echo "output directory must not exist" >&2; exit 2; }
[ -z "$(git -C "$score_repo" status --porcelain)" ] || { echo "preview requires a clean commit" >&2; exit 1; }
score_source="$(git -C "$score_repo" rev-parse HEAD)"
score_base="$(python3 "$score_component/packaging/raw/verify_release.py" "$score_component" "$score_component/packaging/anolisa/component.toml")"
score_version="$score_base-skillscore.${score_source:0:12}"
score_epoch="$(git -C "$score_repo" show -s --format=%ct "$score_source")"
mkdir -p "$score_output"
make -C "$score_component" package-raw RAW_PREVIEW_VERSION="$score_version" OUTPUT_DIR="$score_output"
python3 "$score_component/packaging/skill-score/package-optional.py" "$score_output" \
    --version "$score_version" --source-commit "$score_source" --epoch "$score_epoch"
cargo build --manifest-path "$score_repo/distribution/anolisa/Cargo.toml" --release --locked -p anolisa-cli
score_installer_version="$("$score_repo/distribution/anolisa/target/release/anolisa" --version | awk '{print $NF}')"
tar --owner=0 --group=0 --mtime="@$score_epoch" -czf \
    "$score_output/anolisa-cli-$score_installer_version-linux-x86_64.tar.gz" \
    -C "$score_repo/distribution/anolisa/target/release" anolisa
git -C "$score_repo" archive --format=tar.gz --output="$score_output/source.tar.gz" \
    "$score_source" LICENSE src/agent-sec-core distribution/anolisa
git -C "$score_repo" diff --exit-code
python3 "$score_component/packaging/skill-score/prepare-preview.py" "$score_output" \
    --source-commit "$score_source" --version "$score_version" \
    --components "$score_repo/distribution/anolisa/manifests/components-v2.toml"
python3 "$score_component/packaging/skill-score/bundle-preview.py" "$score_output" \
    "$score_output/bundles" --epoch "$score_epoch"
