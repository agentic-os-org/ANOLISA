#!/usr/bin/env bash
# Build the private assessment payload and separate offline images on Linux.
set -euo pipefail
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 OUTPUT_DIR [--allow-dirty]" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "Linux x86_64 is required" >&2; exit 1; }
score_component="$(cd "$(dirname "$0")/../.." && pwd)"
score_repo="$(git -C "$score_component" rev-parse --show-toplevel)"
score_output="$(realpath -m "$1")"
case "$score_output/" in "$score_component/"*) echo "output must be outside the component" >&2; exit 2;; esac
score_dirty="$(git -C "$score_repo" status --porcelain)"
if [ -n "$score_dirty" ] && [ "${2:-}" != --allow-dirty ]; then
    echo "release builds require a clean commit; --allow-dirty produces a development artifact" >&2
    exit 1
fi
[ -z "${2:-}" ] || [ "$2" = --allow-dirty ] || exit 2
mkdir -p "$score_output"
[ -z "$(ls -A "$score_output")" ] || { echo "output directory must be empty" >&2; exit 2; }
score_work="$(mktemp -d)"
trap 'rm -rf "$score_work"' EXIT
score_prefix="$score_work/payload/opt/agent-sec"
score_cli="$score_prefix/skill-score-cli"
score_runtime="$score_prefix/skill-score-runtime"
bash "$score_component/packaging/raw/prepare_python_runtime.sh" "$score_cli" "$score_work/python311.tgz"
score_python="$score_cli/bin/python3.11"
uv export --quiet --frozen --project "$score_component/agent-sec-cli" --extra skill-score \
    --no-dev --no-emit-project --format requirements-txt --output-file "$score_work/requirements.txt"
uv pip install --python "$score_python" --no-deps --require-hashes \
    --index-url https://mirrors.aliyun.com/pypi/simple/ -r "$score_work/requirements.txt"
uv run --project "$score_component/agent-sec-cli" --frozen --extra skill-score \
    maturin build --manifest-path "$score_component/agent-sec-cli/Cargo.toml" \
    --release --locked --interpreter "$score_python" --manylinux off --out "$score_work/wheels"
uv pip install --python "$score_python" --no-deps "$score_work"/wheels/*.whl
install -m 0755 "$score_component/packaging/skill-score/agent-sec-cli" "$score_cli/bin/agent-sec-cli"
rm -f "$score_cli/bin/agent-sec-daemon"
bash "$score_component/skill-score-runtime/prepare-runtime.sh" "$score_runtime"
PYTHONPATH="$score_component/skill-score-runtime/src" "$score_python" \
    "$score_component/skill-score-runtime/image/build.py" "$score_output" \
    --build-network "${ASC_IMAGE_BUILD_NETWORK:-default}"
gzip -n "$score_output/skill-score-images.tar"
install -m 0644 "$score_output/tool-inventory.json" \
    "$score_cli/lib/python3.11/site-packages/agent_sec_cli/skill_score/tool-inventory.json"
"$score_cli/bin/agent-sec-cli" skill-score plan --help >/dev/null
"$score_python" -I -c 'from agent_sec_cli import _native; import json; assert json.loads(_native.scan_prompt_json("A harmless test", mode="fast"))["degraded"] is False'
"$score_runtime/bin/python3.12" -I -c 'from agent_sec_skill_score_runtime.worker import main; import harbor'
tar -czf "$score_output/source.tar.gz" -C "$score_component" \
    --exclude=.venv --exclude=target --exclude=__pycache__ --exclude=.pytest_cache \
    agent-sec-cli skill-score-runtime packaging LICENSE
tar --owner=0 --group=0 -czf "$score_output/skill-score-payload.tar.gz" -C "$score_work/payload" \
    opt/agent-sec/skill-score-cli opt/agent-sec/skill-score-runtime
"$score_python" "$score_component/packaging/skill-score/verify-release.py" \
    --payload-only "$score_output/skill-score-payload.tar.gz"
