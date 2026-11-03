#!/usr/bin/env bash
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$ACTION_DIR/../../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export OS_SKILLS_SOURCE_WORKTREE="$WORK/source"
VERSION="$(python3 - "$ROOT/src/os-skills/component.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as stream:
    print(tomllib.load(stream)['component']['version'])
PY
)"
build() {
    "$ACTION_DIR/build.sh" --source-repo "$ROOT" --output-dir "$WORK/$1" \
        --version "$2" --target-os linux --target-arch "$3" --profile static-linux \
        --tag "${4:-}"
}
build x86 "$VERSION" x86_64
build arm "$VERSION" aarch64
python3 "$ACTION_DIR/../prebuilt-rust-common/verify-artifacts.py" \
    --directory "$WORK/x86" --component os-skills --version "$VERSION" \
    --layout flat --os linux --arch x86_64
python3 "$ACTION_DIR/../prebuilt-rust-common/verify-artifacts.py" \
    --directory "$WORK/arm" --component os-skills --version "$VERSION" \
    --layout flat --os linux --arch aarch64
if build bad-version 9.9.9 x86_64 >"$WORK/bad.log" 2>&1; then
    echo 'ERROR: accepted wrong release version' >&2; exit 1
fi
if build bad-tag "$VERSION" x86_64 "skill/v9.9.9" >"$WORK/bad.log" 2>&1; then
    echo 'ERROR: accepted wrong tag' >&2; exit 1
fi
if "$ACTION_DIR/build.sh" --source-repo "$ROOT" --output-dir "$WORK/bad-profile" \
    --version "$VERSION" --target-os macos --target-arch aarch64 \
    --profile static-linux >"$WORK/bad.log" 2>&1; then
    echo 'ERROR: accepted wrong platform' >&2; exit 1
fi
archive="$WORK/x86/os-skills-$VERSION-linux-x86_64.tar.gz"
printf 'corruption' >> "$archive"
if python3 "$ACTION_DIR/../prebuilt-rust-common/verify-artifacts.py" \
    --directory "$WORK/x86" --component os-skills --version "$VERSION" \
    --layout flat --os linux --arch x86_64 >"$WORK/bad.log" 2>&1; then
    echo 'ERROR: accepted archive checksum drift' >&2; exit 1
fi
printf 'OS Skills prebuilt regression passed\n'
