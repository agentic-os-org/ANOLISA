#!/usr/bin/env bash
# Verify skillfs action input boundaries and target-specific SBOM generation.
set -euo pipefail
ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
COMMON_DIR="$ACTION_DIR/../prebuilt-rust-common"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT
VERSION="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["workspace"]["package"]["version"])' "$REPO_ROOT/src/skillfs/Cargo.toml")"

for arch in x86_64 aarch64; do
    if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/$arch" --version "$VERSION" \
        --target-os linux --target-arch "$arch" --profile "gnu2.28-$arch" \
        --tag invalid > "$TEMPORARY/invalid-tag.log" 2>&1; then
        echo 'ERROR: invalid source tag was accepted' >&2
        exit 1
    fi
    grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/invalid-tag.log"
    directory="$TEMPORARY/artifacts/skillfs-prebuilt-$VERSION-linux-$arch"
    archive="skillfs-$VERSION-linux-$arch.tar.gz"
    install -d "$directory" "$TEMPORARY/$arch"
    printf 'SBOM fixture for %s\n' "$arch" > "$TEMPORARY/$arch/payload"
    tar -C "$TEMPORARY/$arch" -czf "$directory/$archive" payload
    (cd "$directory" && sha256sum "$archive" > "$archive.sha256")
    python3 "$COMMON_DIR/generate-sbom.py" \
        --artifact "$directory/$archive" --component skillfs --version "$VERSION" \
        --os linux --arch "$arch" --target "$arch-unknown-linux-gnu" \
        --project-dir "$REPO_ROOT/src/skillfs" --source-date-epoch 0 >/dev/null
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
install -d "$fixture/src/skillfs/docs" "$fixture/src/skillfs/packaging"
cp "$REPO_ROOT/src/skillfs/Cargo.toml" "$REPO_ROOT/src/skillfs/component.toml" "$fixture/src/skillfs/"
cp -r "$REPO_ROOT/src/skillfs/.anolisa" "$fixture/src/skillfs/"
cp -r "$REPO_ROOT/src/skillfs/docs/skills" "$fixture/src/skillfs/docs/"
cp -r "$REPO_ROOT/src/skillfs/packaging/raw" "$fixture/src/skillfs/packaging/"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Packaging test'
git -C "$fixture" config user.email 'packaging-test@localhost'
git -C "$fixture" config core.hooksPath /dev/null
git -C "$fixture" add .
git -C "$fixture" -c commit.gpgsign=false commit -qm 'Fixture'
git -C "$fixture" -c tag.gpgsign=false tag "skillfs/v$VERSION"
git -C "$fixture" -c commit.gpgsign=false commit --allow-empty -qm 'Next revision'
if "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/stale-tag" --version "$VERSION" \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    --tag "skillfs/v$VERSION" > "$TEMPORARY/stale-tag.log" 2>&1; then
    echo 'ERROR: release tag at a different commit was accepted' >&2
    exit 1
fi
grep -Fq 'does not point at checked-out commit' "$TEMPORARY/stale-tag.log"
if SKILLFS_SOURCE_WORKTREE="$TEMPORARY/worktrees/skillfs" \
    "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/wrong-version" --version 99.0.0 \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    > "$TEMPORARY/version.log" 2>&1; then
    echo 'ERROR: requested version mismatch was accepted' >&2
    exit 1
fi
grep -Fq 'requested version 99.0.0 does not match skillfs' "$TEMPORARY/version.log"

python3 - "$TEMPORARY" "$VERSION" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
for arch in ("x86_64", "aarch64"):
    artifact = (root / "artifacts" / f"skillfs-prebuilt-{sys.argv[2]}-linux-{arch}"
                / f"skillfs-{sys.argv[2]}-linux-{arch}.tar.gz")
    document = json.loads(Path(f"{artifact}.cdx.json").read_text())
    component = document["metadata"]["component"]
    assert component["name"] == "skillfs" and component["version"] == sys.argv[2]
    assert component["hashes"][0]["content"] == hashlib.sha256(artifact.read_bytes()).hexdigest()
    properties = {item["name"]: item["value"] for item in component["properties"]}
    assert properties["anolisa:target:arch"] == arch
    assert any(item["name"] == "fuser" for item in document["components"])
PY
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component skillfs --version "$VERSION" --layout actions
archive="$TEMPORARY/artifacts/skillfs-prebuilt-$VERSION-linux-aarch64/skillfs-$VERSION-linux-aarch64.tar.gz"
printf 'tampered' >> "$archive"
if python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component skillfs --version "$VERSION" --layout actions > "$TEMPORARY/tamper.log" 2>&1; then
    echo 'ERROR: modified archive was accepted' >&2
    exit 1
fi
grep -Fq 'archive checksum mismatch' "$TEMPORARY/tamper.log"
echo 'skillfs prebuilt action tests passed'
