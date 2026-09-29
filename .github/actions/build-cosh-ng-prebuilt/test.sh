#!/usr/bin/env bash
# Exercise action input binding, untrusted inputs, and target-specific SBOM filtering.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
REPO_ROOT="$(git -C "$ACTION_DIR" rev-parse --show-toplevel)"
COMPONENT_ROOT="$REPO_ROOT/src/cosh-ng"
python3 "$COMMON_DIR/test_macho.py"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

python3 - "$ACTION_DIR/action.yaml" <<'PY'
import sys
from pathlib import Path


action = Path(sys.argv[1]).read_text(encoding="utf-8")
expected_bindings = (
    "COSH_NG_VERSION: ${{ inputs.version }}",
    "COSH_NG_TARGET_OS: ${{ inputs.target-os }}",
    "COSH_NG_TARGET_ARCH: ${{ inputs.target-arch }}",
    "COSH_NG_PROFILE: ${{ inputs.profile }}",
    "COSH_NG_TAG: ${{ inputs.tag }}",
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

EMPTY_CARGO_HOME="$TEMPORARY/empty-cargo-home"
DELEGATED_MARKER="$TEMPORARY/delegated-command"
install -d -m 0755 "$EMPTY_CARGO_HOME"
CARGO_HOME="$EMPTY_CARGO_HOME" \
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" \
        --source-date-epoch 0 \
        -- sh -c "test \"\$SOURCE_DATE_EPOCH\" = 0; touch \"\$1\"" \
        sh "$DELEGATED_MARKER"
[ -f "$DELEGATED_MARKER" ] || {
    printf 'ERROR: empty Cargo home prevented delegated command execution\n' >&2
    exit 1
}

VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT" "$COMPONENT_ROOT/.anolisa/component.toml" \
        --os linux --arch x86_64
)"
MARKER="$TEMPORARY/injected"
MALICIOUS_VERSION="${VERSION}\$(touch ${MARKER})"
if COSH_NG_SOURCE_WORKTREE="$TEMPORARY/version-worktree/cosh-ng" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/version-output" \
        --version "$MALICIOUS_VERSION" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.28-x86_64 \
        --tag '' >"$TEMPORARY/version.log" 2>&1; then
    printf 'ERROR: malicious version input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious version input executed a command\n' >&2
    exit 1
}

MALICIOUS_TAG="cosh-ng/v${VERSION}\$(touch ${MARKER})"
if COSH_NG_SOURCE_WORKTREE="$TEMPORARY/tag-worktree/cosh-ng" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/tag-output" \
        --version "$VERSION" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.28-x86_64 \
        --tag "$MALICIOUS_TAG" >"$TEMPORARY/tag.log" 2>&1; then
    printf 'ERROR: malicious tag input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious tag input executed a command\n' >&2
    exit 1
}

# An invalid tag stops each valid target before worktree creation or compilation.
for target in linux/x86_64/gnu2.28-x86_64 linux/aarch64/gnu2.28-aarch64 \
    macos/aarch64/darwin11-aarch64 macos/x86_64/darwin11-x86_64; do
    IFS=/ read -r os arch profile <<< "$target"
    if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/unused" --version "$VERSION" \
        --target-os "$os" --target-arch "$arch" --profile "$profile" \
        --tag invalid >"$TEMPORARY/target.log" 2>&1; then
        printf 'ERROR: invalid tag accepted for %s\n' "$target" >&2
        exit 1
    fi
    grep -Fq 'release tag invalid does not match requested version' "$TEMPORARY/target.log"
done
if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/unused" --version "$VERSION" \
    --target-os macos --target-arch x86_64 --profile darwin11-aarch64 \
    --tag invalid >"$TEMPORARY/target.log" 2>&1; then
    printf 'ERROR: mismatched macOS profile accepted\n' >&2
    exit 1
fi
grep -Fq 'does not match target macos/x86_64' "$TEMPORARY/target.log"

for platform in linux macos macos-x86_64; do
    printf 'SBOM fixture for %s\n' "$platform" >"$TEMPORARY/$platform.tar.gz"
    (
        cd "$TEMPORARY"
        sha256sum "$platform.tar.gz" >"$platform.tar.gz.sha256"
    )
done

python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$TEMPORARY/linux.tar.gz" \
    --component cosh-ng \
    --version "$VERSION" \
    --os linux \
    --arch x86_64 \
    --target x86_64-unknown-linux-gnu \
    --project-dir "$COMPONENT_ROOT" \
    --source-date-epoch 0 >/dev/null
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$TEMPORARY/macos.tar.gz" \
    --component cosh-ng \
    --version "$VERSION" \
    --os macos \
    --arch aarch64 \
    --target aarch64-apple-darwin \
    --project-dir "$COMPONENT_ROOT" \
    --source-date-epoch 0 >/dev/null

python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$TEMPORARY/macos-x86_64.tar.gz" \
    --component cosh-ng \
    --version "$VERSION" \
    --os macos \
    --arch x86_64 \
    --target x86_64-apple-darwin \
    --project-dir "$COMPONENT_ROOT" \
    --source-date-epoch 0 >/dev/null

python3 - "$TEMPORARY/linux.tar.gz.cdx.json" \
    "$TEMPORARY/macos.tar.gz.cdx.json" "$TEMPORARY/macos-x86_64.tar.gz.cdx.json" <<'PY'
import json
import sys
from pathlib import Path


def names(path: str) -> set[str]:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    components = list(data["components"])
    components.extend(data["metadata"]["component"].get("components", []))
    return {component["name"] for component in components}


linux = names(sys.argv[1])
macos = names(sys.argv[2])
macos_x86 = names(sys.argv[3])
for platform, packages in (("linux", linux), ("macos", macos), ("macos-x86_64", macos_x86)):
    windows = sorted(name for name in packages if name.startswith("windows"))
    if windows:
        raise SystemExit(f"{platform} SBOM contains Windows-only packages: {windows}")
if "core-foundation" in linux:
    raise SystemExit("Linux SBOM contains a macOS-only package")
if "core-foundation" not in macos or "core-foundation" not in macos_x86:
    raise SystemExit("macOS SBOM is missing a target dependency")
PY

printf 'cosh-ng prebuilt action tests passed\n'
