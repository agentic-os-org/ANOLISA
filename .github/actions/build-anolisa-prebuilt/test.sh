#!/usr/bin/env bash
# Exercise ANOLISA CLI action input binding and release metadata validation.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
REPO_ROOT="$(git -C "$ACTION_DIR" rev-parse --show-toplevel)"
COMPONENT_ROOT="$REPO_ROOT/distribution/anolisa"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

python3 - "$ACTION_DIR/action.yaml" <<'PY'
import sys
from pathlib import Path


action = Path(sys.argv[1]).read_text(encoding="utf-8")
expected_bindings = (
    "ANOLISA_VERSION: ${{ inputs.version }}",
    "ANOLISA_TARGET_OS: ${{ inputs.target-os }}",
    "ANOLISA_TARGET_ARCH: ${{ inputs.target-arch }}",
    "ANOLISA_PROFILE: ${{ inputs.profile }}",
    "ANOLISA_TAG: ${{ inputs.tag }}",
)
for binding in expected_bindings:
    if binding not in action:
        raise SystemExit(f"composite action is missing environment binding: {binding}")

marker = "      run: |\n"
if marker not in action:
    raise SystemExit("composite action is missing its Bash run block")
run_block = action.split(marker, 1)[1]
if "${{ inputs." in run_block:
    raise SystemExit("composite action interpolates an input directly into Bash")
PY

python3 - "$COMPONENT_ROOT" "$COMMON_DIR" <<'PY'
import json
import sys
from pathlib import Path


component_root = Path(sys.argv[1])
sys.path.insert(0, sys.argv[2])
from targets import TARGETS  # noqa: E402

identity = {
    ("linux", "x64"): ("linux", "x86_64"),
    ("linux", "arm64"): ("linux", "aarch64"),
    ("darwin", "arm64"): ("macos", "aarch64"),
    ("darwin", "x64"): ("macos", "x86_64"),
}
npm_targets = set()
for package_file in (component_root / "npm/platforms").glob("*/package.json"):
    package = json.loads(package_file.read_text(encoding="utf-8"))
    try:
        npm_targets.add(identity[(package["os"][0], package["cpu"][0])])
    except (KeyError, IndexError) as error:
        raise SystemExit(f"unsupported npm platform metadata: {package_file}") from error
matrix_targets = {
    (target["target-os"], target["target-arch"])
    for target in TARGETS["anolisa"]
}
if matrix_targets != npm_targets:
    raise SystemExit(
        f"ANOLISA prebuilt matrix does not match npm platforms: "
        f"matrix={sorted(matrix_targets)} npm={sorted(npm_targets)}"
    )
PY

VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/prebuilt/verify-release.py" \
        "$COMPONENT_ROOT" --os linux --arch x86_64
)"
for target in 'linux aarch64' 'macos aarch64' 'macos x86_64'; do
    read -r os_name arch <<<"$target"
    test "$(
        python3 "$COMPONENT_ROOT/packaging/prebuilt/verify-release.py" \
            "$COMPONENT_ROOT" --os "$os_name" --arch "$arch"
    )" = "$VERSION"
done

# Reject a mismatched cross profile before any build-side effects.
if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/unused" --version "$VERSION" \
    --target-os macos --target-arch x86_64 --profile darwin11-aarch64 \
    --tag invalid >"$TEMPORARY/target.log" 2>&1; then
    printf 'ERROR: mismatched Intel macOS profile accepted\n' >&2
    exit 1
fi
grep -Fq 'does not match target macos/x86_64' "$TEMPORARY/target.log"
if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/unused" --version "$VERSION" \
    --target-os macos --target-arch x86_64 --profile darwin11-x86_64 \
    --tag invalid >"$TEMPORARY/target.log" 2>&1; then
    printf 'ERROR: invalid tag accepted for Intel macOS\n' >&2
    exit 1
fi
grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/target.log"

SEMVER_REPO="$TEMPORARY/semver-repo"
install -d -m 0755 "$SEMVER_REPO/distribution/anolisa/packaging/prebuilt"
install -p -m 0644 "$COMPONENT_ROOT/Cargo.toml" "$SEMVER_REPO/distribution/anolisa/Cargo.toml"
cp -a "$COMPONENT_ROOT/npm" "$SEMVER_REPO/distribution/anolisa/npm"
install -p -m 0755 "$COMPONENT_ROOT/packaging/prebuilt/verify-release.py" \
    "$SEMVER_REPO/distribution/anolisa/packaging/prebuilt/verify-release.py"
git -C "$SEMVER_REPO" init -q
git -C "$SEMVER_REPO" add -- distribution/anolisa
git -C "$SEMVER_REPO" \
    -c user.name='ANOLISA CI' \
    -c user.email='ci@localhost' \
    commit -qm 'test fixture'
SEMVER_BIN="$TEMPORARY/semver-bin"
install -d -m 0755 "$SEMVER_BIN"
printf '#!/usr/bin/env sh\nexit 1\n' > "$SEMVER_BIN/cargo-sbom"
chmod 0755 "$SEMVER_BIN/cargo-sbom"

BUILD_VERSION="${VERSION}+build.1"
if PATH="$SEMVER_BIN:$PATH" \
    ANOLISA_SOURCE_WORKTREE="$TEMPORARY/build-version-worktree/anolisa" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$SEMVER_REPO" \
        --output-dir "$TEMPORARY/build-version-output" \
        --version "$BUILD_VERSION" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag '' >"$TEMPORARY/build-version.log" 2>&1; then
    printf 'ERROR: unsynchronized SemVer build metadata was accepted\n' >&2
    exit 1
fi
if ! grep -Fq \
    "requested version $BUILD_VERSION does not match ANOLISA CLI $VERSION" \
    "$TEMPORARY/build-version.log"; then
    cat "$TEMPORARY/build-version.log" >&2
    exit 1
fi

MARKER="$TEMPORARY/injected"
if ANOLISA_SOURCE_WORKTREE="$TEMPORARY/version-worktree/anolisa" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/version-output" \
        --version "${VERSION}\$(touch ${MARKER})" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag '' >"$TEMPORARY/version.log" 2>&1; then
    printf 'ERROR: malicious version input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious version input executed a command\n' >&2
    exit 1
}

if ANOLISA_SOURCE_WORKTREE="$TEMPORARY/tag-worktree/anolisa" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/tag-output" \
        --version "$VERSION" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag "anolisa/v${VERSION}\$(touch ${MARKER})" \
        >"$TEMPORARY/tag.log" 2>&1; then
    printf 'ERROR: malicious tag input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious tag input executed a command\n' >&2
    exit 1
}

printf 'ANOLISA CLI prebuilt action tests passed\n'
