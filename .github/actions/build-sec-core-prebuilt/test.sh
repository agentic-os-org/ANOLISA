#!/usr/bin/env bash
# Exercise sec-core action input boundaries without a Cross build.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT
VERSION="$(python3 "$REPO_ROOT/src/agent-sec-core/packaging/raw/verify_release.py" \
    "$REPO_ROOT/src/agent-sec-core" \
    "$REPO_ROOT/src/agent-sec-core/.anolisa/component.toml")"

if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/profile" --version "$VERSION" \
    --target-os linux --target-arch aarch64 --profile gnu2.28-x86_64 \
    > "$TEMPORARY/profile.log" 2>&1; then
    echo 'ERROR: mismatched target profile was accepted' >&2; exit 1
fi
grep -Fq 'does not match target linux/aarch64' "$TEMPORARY/profile.log"

if "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/tag" --version "$VERSION" \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    --tag invalid > "$TEMPORARY/tag.log" 2>&1; then
    echo 'ERROR: invalid tag was accepted' >&2; exit 1
fi
grep -Fq 'does not match requested version' "$TEMPORARY/tag.log"

git -C "$TEMPORARY" init -q source
git -C "$TEMPORARY/source" config user.name 'Packaging test'
git -C "$TEMPORARY/source" config user.email 'packaging-test@localhost'
git -C "$TEMPORARY/source" config core.hooksPath /dev/null
git -C "$TEMPORARY/source" -c commit.gpgsign=false commit --allow-empty -qm 'Fixture'
git -C "$TEMPORARY/source" -c tag.gpgsign=false tag "sec-core/v$VERSION"
git -C "$TEMPORARY/source" -c commit.gpgsign=false commit --allow-empty -qm 'Next revision'
if "$ACTION_DIR/build.sh" --source-repo "$TEMPORARY/source" \
    --output-dir "$TEMPORARY/stale" --version "$VERSION" \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    --tag "sec-core/v$VERSION" > "$TEMPORARY/stale.log" 2>&1; then
    echo 'ERROR: stale tag was accepted' >&2; exit 1
fi
grep -Fq 'does not point at checked-out commit' "$TEMPORARY/stale.log"

if SEC_CORE_SOURCE_WORKTREE="$TEMPORARY/worktrees/sec-core" \
    "$ACTION_DIR/build.sh" --source-repo "$REPO_ROOT" \
    --output-dir "$TEMPORARY/version" --version 99.0.0 \
    --target-os linux --target-arch x86_64 --profile gnu2.28-x86_64 \
    > "$TEMPORARY/version.log" 2>&1; then
    echo 'ERROR: wrong component version was accepted' >&2; exit 1
fi
grep -Fq 'requested version 99.0.0 does not match sec-core' "$TEMPORARY/version.log"

python3 "$ACTION_DIR/test_artifacts.py"
python3 "$ACTION_DIR/test_python_sbom.py"
echo 'sec-core prebuilt action tests passed'
