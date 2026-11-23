#!/usr/bin/env bash
# Verify agentsight action input boundaries and target-specific SBOM generation.
set -euo pipefail
ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
COMMON_DIR="$ACTION_DIR/../prebuilt-rust-common"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT
VERSION="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["package"]["version"])' "$REPO_ROOT/src/agentsight/Cargo.toml")"

if AGENTSIGHT_HOST_CARGO=false "$ACTION_DIR/host-tools/cargo" build > "$TEMPORARY/host.log" 2>&1; then
    echo 'ERROR: host compilation fallback was accepted' >&2; exit 1
fi
grep -Fq 'refusing Cross fallback to host compilation' "$TEMPORARY/host.log"

for pair in linux-x86_64 linux-aarch64 macos-aarch64; do
    os="${pair%-*}"; arch="${pair#*-}"
    profile="gnu2.28-$arch"; triple="$arch-unknown-linux-gnu"
    sbom_args=(--project-dir "$REPO_ROOT/src/agentsight/crates/agentsight-enforcer" --features agentsight-enforcer/actplane)
    if [ "$os" = macos ]; then
        profile=darwin11-aarch64; triple=aarch64-apple-darwin
        sbom_args=(--no-default-features --features server)
    fi
    if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/$pair" --version "$VERSION" \
        --target-os "$os" --target-arch "$arch" --profile "$profile" \
        --tag invalid > "$TEMPORARY/invalid-tag.log" 2>&1; then
        echo 'ERROR: invalid source tag was accepted' >&2
        exit 1
    fi
    grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/invalid-tag.log"
    directory="$TEMPORARY/artifacts/agentsight-prebuilt-$VERSION-$os-$arch"
    archive="agentsight-$VERSION-$os-$arch.tar.gz"
    install -d "$directory" "$TEMPORARY/$pair"
    printf 'SBOM fixture for %s\n' "$arch" > "$TEMPORARY/$pair/payload"
    tar -C "$TEMPORARY/$pair" -czf "$directory/$archive" payload
    (cd "$directory" && sha256sum "$archive" > "$archive.sha256")
    python3 "$COMMON_DIR/generate-sbom.py" \
        --artifact "$directory/$archive" --component agentsight --version "$VERSION" \
        --os "$os" --arch "$arch" --target "$triple" "${sbom_args[@]}" \
        --project-dir "$REPO_ROOT/src/agentsight" --source-date-epoch 0 >/dev/null
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
install -d "$fixture/src/agentsight"
cp "$REPO_ROOT/src/agentsight/Cargo.toml" "$fixture/src/agentsight/"
cp -r "$REPO_ROOT/src/agentsight/packaging" "$fixture/src/agentsight/"
cp -r "$REPO_ROOT/src/agentsight/scripts" "$fixture/src/agentsight/"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Packaging test'
git -C "$fixture" config user.email 'packaging-test@localhost'
git -C "$fixture" config core.hooksPath /dev/null
git -C "$fixture" add .
git -C "$fixture" -c commit.gpgsign=false commit -qm 'Fixture'
git -C "$fixture" -c tag.gpgsign=false tag "sight/v$VERSION"
git -C "$fixture" -c commit.gpgsign=false commit --allow-empty -qm 'Next revision'
if "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/stale-tag" --version "$VERSION" \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    --tag "sight/v$VERSION" > "$TEMPORARY/stale-tag.log" 2>&1; then
    echo 'ERROR: release tag at a different commit was accepted' >&2
    exit 1
fi
grep -Fq 'does not point at checked-out commit' "$TEMPORARY/stale-tag.log"
if AGENTSIGHT_SOURCE_WORKTREE="$TEMPORARY/worktrees/agentsight" \
    "$ACTION_DIR/build.sh" --source-repo "$fixture" \
    --output-dir "$TEMPORARY/wrong-version" --version 99.0.0 \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    > "$TEMPORARY/version.log" 2>&1; then
    echo 'ERROR: requested version mismatch was accepted' >&2
    exit 1
fi
grep -Fq 'requested version 99.0.0 does not match agentsight' "$TEMPORARY/version.log"

python3 - "$TEMPORARY" "$VERSION" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
for os, arch in (("linux", "x86_64"), ("linux", "aarch64"), ("macos", "aarch64")):
    artifact = (root / "artifacts" / f"agentsight-prebuilt-{sys.argv[2]}-{os}-{arch}"
                / f"agentsight-{sys.argv[2]}-{os}-{arch}.tar.gz")
    document = json.loads(Path(f"{artifact}.cdx.json").read_text())
    component = document["metadata"]["component"]
    assert component["name"] == "agentsight" and component["version"] == sys.argv[2]
    assert component["hashes"][0]["content"] == hashlib.sha256(artifact.read_bytes()).hexdigest()
    properties = {item["name"]: item["value"] for item in component["properties"]}
    assert properties["anolisa:target:arch"] == arch
    names = {item["name"] for item in document["components"]}
    names.update(item["name"] for item in component.get("components", []))
    assert ("ebpf-ifc-engine" in names) == (os == "linux")
    assert ("agentsight-enforcer" in names) == (os == "linux")
    assert ("actplane-ifc-compiler" in names) == (os == "linux")
PY
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agentsight --version "$VERSION" --layout actions
archive="$TEMPORARY/artifacts/agentsight-prebuilt-$VERSION-linux-aarch64/agentsight-$VERSION-linux-aarch64.tar.gz"
printf 'tampered' >> "$archive"
if python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agentsight --version "$VERSION" --layout actions > "$TEMPORARY/tamper.log" 2>&1; then
    echo 'ERROR: modified archive was accepted' >&2
    exit 1
fi
grep -Fq 'archive checksum mismatch' "$TEMPORARY/tamper.log"
rm "$archive.sha256"
if python3 "$COMMON_DIR/verify-artifacts.py" --directory "$TEMPORARY/artifacts" \
    --component agentsight --version "$VERSION" --layout actions > "$TEMPORARY/missing.log" 2>&1; then
    echo 'ERROR: missing artifact was accepted' >&2
    exit 1
fi
echo 'agentsight prebuilt action tests passed'
