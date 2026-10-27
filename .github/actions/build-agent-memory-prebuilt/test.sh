#!/usr/bin/env bash
# Verify agent-memory action input boundaries and target-specific SBOM generation.
set -euo pipefail
ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
COMMON_DIR="$ACTION_DIR/../prebuilt-rust-common"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT
VERSION="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["package"]["version"])' "$REPO_ROOT/src/agent-memory/Cargo.toml")"

if AGENT_MEMORY_HOST_CARGO=false "$ACTION_DIR/host-tools/cargo" build > "$TEMPORARY/host.log" 2>&1; then
    echo 'ERROR: host compilation fallback was accepted' >&2; exit 1
fi
grep -Fq 'refusing Cross fallback to host compilation' "$TEMPORARY/host.log"

python3 - "$TEMPORARY" "$VERSION" <<'PYFIXTURE'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1]) / "plugin"
(root / "dist").mkdir(parents=True)
(root / "package.json").write_text(json.dumps({"name": "memory-anolisa-openclaw-plugin", "version": sys.argv[2]}))
(root / "package-lock.json").write_text(json.dumps({"packages": {
    "node_modules/typebox": {"version": "1.1.38"},
    "node_modules/esbuild": {"version": "0.25.0"},
    "node_modules/openclaw": {"version": "2026.5.7"}}}))
(root / "dist/index.js").write_text("fixture")
(root / "dist/build-meta.json").write_text(json.dumps({"outputs": {"dist/index.js": {
    "bytes": 7, "inputs": {"node_modules/typebox/build/index.js": {"bytesInOutput": 7},
                         "src/index.ts": {"bytesInOutput": 0}}}}}))
PYFIXTURE
python3 "$ACTION_DIR/plugin-sbom.py" "$TEMPORARY/plugin" "$TEMPORARY/plugin-sbom.json"

for pair in linux-x86_64 linux-aarch64; do
    os="${pair%-*}"; arch="${pair#*-}"
    profile="gnu2.28-$arch"; triple="$arch-unknown-linux-gnu"
    if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/$pair" --version "$VERSION" \
        --target-os "$os" --target-arch "$arch" --profile "$profile" \
        --tag invalid > "$TEMPORARY/invalid-tag.log" 2>&1; then
        echo 'ERROR: invalid source tag was accepted' >&2
        exit 1
    fi
    grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/invalid-tag.log"
    directory="$TEMPORARY/artifacts/agent-memory-prebuilt-$VERSION-$os-$arch"
    archive="agent-memory-$VERSION-$os-$arch.tar.gz"
    install -d "$directory" "$TEMPORARY/$pair"
    printf 'SBOM fixture for %s\n' "$arch" > "$TEMPORARY/$pair/payload"
    tar -C "$TEMPORARY/$pair" -czf "$directory/$archive" payload
    (cd "$directory" && sha256sum "$archive" > "$archive.sha256")
    python3 "$COMMON_DIR/generate-sbom.py" \
        --artifact "$directory/$archive" --component agent-memory --version "$VERSION" \
        --os "$os" --arch "$arch" --target "$triple" --extra-sbom "$TEMPORARY/plugin-sbom.json" \
        --project-dir "$REPO_ROOT/src/agent-memory" --source-date-epoch 0 >/dev/null
done
if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/wrong-profile" --version "$VERSION" \
    --target-os linux --target-arch aarch64 --profile gnu2.28-x86_64 \
    --tag invalid > "$TEMPORARY/profile.log" 2>&1; then
    echo 'ERROR: mismatched target profile was accepted' >&2
    exit 1
fi
grep -Fq 'profile gnu2.28-x86_64 does not match target linux/aarch64' "$TEMPORARY/profile.log"

# Use an isolated repository so tag tests never modify the checkout's release tags.
fixture="$TEMPORARY/source"
install -d "$fixture/src/agent-memory"
cp "$REPO_ROOT/src/agent-memory/Cargo.toml" "$fixture/src/agent-memory/"
cp -r "$REPO_ROOT/src/agent-memory/packaging" "$fixture/src/agent-memory/"
tar -C "$REPO_ROOT/src/agent-memory" --exclude=node_modules --exclude=dist --exclude=tests \
    -cf - config adapters | tar -x -C "$fixture/src/agent-memory"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Packaging test'
git -C "$fixture" config user.email 'packaging-test@localhost'
git -C "$fixture" config core.hooksPath /dev/null
git -C "$fixture" add .
git -C "$fixture" -c commit.gpgsign=false commit -qm 'Fixture'
git -C "$fixture" -c tag.gpgsign=false tag "memory/v$VERSION"
git -C "$fixture" -c commit.gpgsign=false commit --allow-empty -qm 'Next revision'
if "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/stale-tag" --version "$VERSION" \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    --tag "memory/v$VERSION" > "$TEMPORARY/stale-tag.log" 2>&1; then
    echo 'ERROR: release tag at a different commit was accepted' >&2
    exit 1
fi
grep -Fq 'does not point at checked-out commit' "$TEMPORARY/stale-tag.log"
if AGENT_MEMORY_SOURCE_WORKTREE="$TEMPORARY/worktrees/agent-memory" \
    "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/wrong-version" --version 99.0.0 \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    > "$TEMPORARY/version.log" 2>&1; then
    echo 'ERROR: requested version mismatch was accepted' >&2
    exit 1
fi
grep -Fq 'requested version 99.0.0 does not match agent-memory' "$TEMPORARY/version.log"

python3 - "$TEMPORARY" "$VERSION" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
for os, arch in (("linux", "x86_64"), ("linux", "aarch64")):
    artifact = (root / "artifacts" / f"agent-memory-prebuilt-{sys.argv[2]}-{os}-{arch}"
                / f"agent-memory-{sys.argv[2]}-{os}-{arch}.tar.gz")
    document = json.loads(Path(f"{artifact}.cdx.json").read_text())
    component = document["metadata"]["component"]
    assert component["name"] == "agent-memory" and component["version"] == sys.argv[2]
    assert component["hashes"][0]["content"] == hashlib.sha256(artifact.read_bytes()).hexdigest()
    properties = {item["name"]: item["value"] for item in component["properties"]}
    assert properties["anolisa:target:arch"] == arch
    names = {item["name"] for item in document["components"]}
    names.update(item["name"] for item in component.get("components", []))
    assert "typebox" in names and "memory-anolisa-openclaw-plugin" in names
    assert "openclaw" not in names and "esbuild" not in names
    assert next(item["version"] for item in document["components"] if item["name"] == "typebox") == "1.1.38"
PY
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agent-memory --version "$VERSION" --layout actions
archive="$TEMPORARY/artifacts/agent-memory-prebuilt-$VERSION-linux-aarch64/agent-memory-$VERSION-linux-aarch64.tar.gz"
printf 'tampered' >> "$archive"
if python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agent-memory --version "$VERSION" --layout actions > "$TEMPORARY/tamper.log" 2>&1; then
    echo 'ERROR: modified archive was accepted' >&2
    exit 1
fi
grep -Fq 'archive checksum mismatch' "$TEMPORARY/tamper.log"
rm "$archive.sha256"
if python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agent-memory --version "$VERSION" --layout actions > "$TEMPORARY/missing.log" 2>&1; then
    echo 'ERROR: missing artifact was accepted' >&2
    exit 1
fi
echo 'agent-memory prebuilt action tests passed'

printf 'changed bundle' >> "$TEMPORARY/plugin/dist/index.js"
if python3 "$ACTION_DIR/plugin-sbom.py" "$TEMPORARY/plugin" "$TEMPORARY/bad.json" > "$TEMPORARY/stale.log" 2>&1; then
    echo 'ERROR: stale esbuild manifest was accepted' >&2; exit 1
fi
grep -Fq 'esbuild manifest does not describe the bundle' "$TEMPORARY/stale.log"
