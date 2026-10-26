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
score_work="$(mktemp -d)"
trap 'rm -rf "$score_work"' EXIT
bash "$score_component/packaging/skill-score/build-payload.sh" "$score_work/payload"
make -C "$score_component" package-raw \
    RAW_SKILL_SCORE_PAYLOAD="$score_work/payload/skill-score-payload.tar.gz" \
    RAW_PREVIEW_VERSION="$score_version" OUTPUT_DIR="$score_output"
for score_file in skill-score-images.tar.gz images.json tool-inventory.json; do
    install -m 0644 "$score_work/payload/$score_file" "$score_output/$score_file"
done
git -C "$score_repo" archive --format=tar.gz --output="$score_output/source.tar.gz" \
    "$score_source" src/agent-sec-core distribution/anolisa
git -C "$score_repo" diff --exit-code
python3 "$score_component/packaging/skill-score/prepare-preview.py" "$score_output" \
    --source-commit "$score_source" --version "$score_version" \
    --components "$score_repo/distribution/anolisa/manifests/components-v2.toml"
