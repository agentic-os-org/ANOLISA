#!/usr/bin/env bash
# Verify ws-ckpt action input boundaries and target-specific SBOM generation.
set -euo pipefail
ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
COMMON_DIR="$ACTION_DIR/../prebuilt-rust-common"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT
VERSION="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["workspace"]["package"]["version"])' "$REPO_ROOT/src/ws-ckpt/src/Cargo.toml")"

for arch in x86_64 aarch64; do
    if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/$arch" --version "$VERSION" \
        --target-os linux --target-arch "$arch" --profile "gnu2.28-$arch" \
        --tag invalid > "$TEMPORARY/invalid-tag.log" 2>&1; then
        echo 'ERROR: invalid source tag was accepted' >&2
        exit 1
    fi
    grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/invalid-tag.log"
    printf 'SBOM fixture for %s\n' "$arch" > "$TEMPORARY/$arch.tar.gz"
    (cd "$TEMPORARY" && sha256sum "$arch.tar.gz" > "$arch.tar.gz.sha256")
    python3 "$COMMON_DIR/generate-sbom.py" \
        --artifact "$TEMPORARY/$arch.tar.gz" --component ws-ckpt --version "$VERSION" \
        --os linux --arch "$arch" --target "$arch-unknown-linux-gnu" \
        --project-dir "$REPO_ROOT/src/ws-ckpt/src" --source-date-epoch 0 >/dev/null
done
if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/wrong-profile" --version "$VERSION" \
    --target-os linux --target-arch aarch64 --profile gnu2.28-x86_64 \
    --tag invalid > "$TEMPORARY/profile.log" 2>&1; then
    echo 'ERROR: mismatched target profile was accepted' >&2
    exit 1
fi
grep -Fq 'profile gnu2.28-x86_64 does not match target linux/aarch64' "$TEMPORARY/profile.log"

python3 - "$TEMPORARY" "$VERSION" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
for arch in ("x86_64", "aarch64"):
    artifact = root / f"{arch}.tar.gz"
    document = json.loads(Path(f"{artifact}.cdx.json").read_text())
    component = document["metadata"]["component"]
    assert component["name"] == "ws-ckpt" and component["version"] == sys.argv[2]
    assert component["hashes"][0]["content"] == hashlib.sha256(artifact.read_bytes()).hexdigest()
    properties = {item["name"]: item["value"] for item in component["properties"]}
    assert properties["anolisa:target:arch"] == arch
    assert any(item["name"] == "seccompiler" for item in document["components"])
PY
echo 'ws-ckpt prebuilt action tests passed'
