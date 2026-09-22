#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CATALOG="distribution/anolisa/manifests/components/tokenless/component.toml"
VERSION=$(grep '^version' src/tokenless/Cargo.toml | head -1 | sed 's/.*"\([^"]*\)".*/\1/')

WORK_DIR=$(mktemp -d)
SNAPSHOT="$WORK_DIR/catalog.snapshot"
BACKUP="$WORK_DIR/catalog.backup"
MISMATCH_LOG="$WORK_DIR/mismatch.log"

# On any exit, restore the catalog byte-for-byte from the snapshot taken
# before the first mutation, and only then remove the temporary files.
# `git checkout` is deliberately not used: it would silently discard an
# unrelated uncommitted catalog edit, and it fails in a source tree without
# Git (which would leave the drifted catalog in place).
trap 'if [ -f "${SNAPSHOT:-}" ]; then cp "$SNAPSHOT" "$CATALOG"; fi; rm -rf "$WORK_DIR"' EXIT
cp "$CATALOG" "$SNAPSHOT"

# Baseline: all metadata is currently synchronized.
python3 scripts/check-component-versions.py

# Bump only the catalog version field to a bogus value. The version travels
# through an environment variable and is re.escape()d, so SemVer build
# metadata (e.g. 0.7.4+build.1) cannot be misread as shell or regex syntax.
drift_version() {
    CATALOG_PATH="$1" CARGO_VERSION="$2" python3 - << 'PYEOF'
import os
import pathlib
import re

path = pathlib.Path(os.environ["CATALOG_PATH"])
version = os.environ["CARGO_VERSION"]
text = path.read_text()
pattern = re.compile(r'^(version = ")' + re.escape(version) + r'(")', re.M)
new_text, count = pattern.subn(r"\g<1>99.99.99\g<2>", text, count=1)
if count != 1:
    raise SystemExit(f"ERROR: catalog version line for {version!r} not found in {path}")
path.write_text(new_text)
PYEOF
}

expect_version_mismatch() {
    if python3 scripts/check-component-versions.py > "$MISMATCH_LOG" 2>&1; then
        echo "ERROR: check-component-versions.py did not fail on mismatched tokenless catalog version" >&2
        exit 1
    fi
    if ! grep -qF "$CATALOG" "$MISMATCH_LOG"; then
        echo "ERROR: mismatch output did not mention $CATALOG" >&2
        cat "$MISMATCH_LOG" >&2
        exit 1
    fi
}

restore_from_backup() {
    cp "$BACKUP" "$CATALOG"
    if ! cmp -s "$CATALOG" "$BACKUP"; then
        echo "ERROR: catalog was not restored byte-for-byte from its backup" >&2
        exit 1
    fi
}

# Case 1: a drifted catalog version is rejected and the fixture comes back
# byte-for-byte from its backup.
cp "$CATALOG" "$BACKUP"
drift_version "$CATALOG" "$VERSION"
expect_version_mismatch
restore_from_backup

# Case 2: a pre-existing uncommitted catalog edit that leaves the version
# synchronized survives the test byte-for-byte.
printf '\n# local wip: note unrelated to the version field\n' >> "$CATALOG"
cp "$CATALOG" "$BACKUP"
drift_version "$CATALOG" "$VERSION"
expect_version_mismatch
restore_from_backup
if ! grep -qF 'local wip' "$CATALOG"; then
    echo "ERROR: unrelated uncommitted catalog edit was lost during the test" >&2
    exit 1
fi
cp "$SNAPSHOT" "$CATALOG"

# Case 3: a version containing SemVer build metadata (+ is a regex quantifier
# when unescaped) is still substituted exactly once.
BUILD_METADATA_FIXTURE="$WORK_DIR/catalog-build-metadata.toml"
printf '[component]\nname = "tokenless"\nversion = "0.7.4+build.1"\n' > "$BUILD_METADATA_FIXTURE"
drift_version "$BUILD_METADATA_FIXTURE" "0.7.4+build.1"
if ! grep -q '^version = "99.99.99"$' "$BUILD_METADATA_FIXTURE"; then
    echo "ERROR: build-metadata version was not substituted (regex escaping broken?)" >&2
    cat "$BUILD_METADATA_FIXTURE" >&2
    exit 1
fi

# Case 4: byte-for-byte backup restoration also works outside any Git tree.
NOGIT_DIR="$WORK_DIR/no-git"
mkdir -p "$NOGIT_DIR"
cp "$CATALOG" "$NOGIT_DIR/component.toml"
NOGIT_BACKUP="$NOGIT_DIR/component.toml.backup"
cp "$NOGIT_DIR/component.toml" "$NOGIT_BACKUP"
drift_version "$NOGIT_DIR/component.toml" "$VERSION"
cp "$NOGIT_BACKUP" "$NOGIT_DIR/component.toml"
if ! cmp -s "$NOGIT_DIR/component.toml" "$NOGIT_BACKUP"; then
    echo "ERROR: catalog was not restored byte-for-byte outside a Git tree" >&2
    exit 1
fi

# The tree must be back in its pre-test state and synchronized.
if ! cmp -s "$CATALOG" "$SNAPSHOT"; then
    echo "ERROR: catalog differs from its pre-test state" >&2
    exit 1
fi
python3 scripts/check-component-versions.py

echo "Tokenless catalog version regression tests passed"
