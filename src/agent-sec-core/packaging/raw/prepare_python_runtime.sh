#!/usr/bin/env bash
# Prepare the pinned, redistributable Python runtime embedded in raw packages.
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

usage() {
    die "usage: $0 RUNTIME_DIR ARCHIVE_CACHE"
}

[ "$#" -eq 2 ] || usage

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$1"
ARCHIVE_CACHE="$2"
PYTHON_VERSION="3.11.6"
PYTHON_BUILD="20231002"
PYTHON_TARGET="x86_64-unknown-linux-gnu"
ARCHIVE_FLAVOR="install_only"
ARCHIVE_NAME="cpython-${PYTHON_VERSION}+${PYTHON_BUILD}-${PYTHON_TARGET}-${ARCHIVE_FLAVOR}.tar.gz"
SOURCE_URL="https://releases.astral.sh/github/python-build-standalone/releases/download/${PYTHON_BUILD}/cpython-${PYTHON_VERSION}%2B${PYTHON_BUILD}-${PYTHON_TARGET}-${ARCHIVE_FLAVOR}.tar.gz"
ARCHIVE_SHA256="ee37a7eae6e80148c7e3abc56e48a397c1664f044920463ad0df0fc706eacea8"
PROVENANCE_SOURCE="$SCRIPT_DIR/assets/python-runtime/PROVENANCE.toml"

command -v curl >/dev/null 2>&1 || die "curl is required to fetch bundled Python"
command -v sha256sum >/dev/null 2>&1 || \
    die "sha256sum is required to verify bundled Python"
[ -f "$PROVENANCE_SOURCE" ] || die "missing Python provenance metadata"
grep -Fqx "python_version = \"$PYTHON_VERSION\"" "$PROVENANCE_SOURCE" || \
    die "Python provenance version does not match $PYTHON_VERSION"
grep -Fqx "build = \"$PYTHON_BUILD\"" "$PROVENANCE_SOURCE" || \
    die "Python provenance build does not match $PYTHON_BUILD"
grep -Fqx "archive_filename = \"$ARCHIVE_NAME\"" "$PROVENANCE_SOURCE" || \
    die "Python provenance archive name does not match $ARCHIVE_NAME"
grep -Fqx "source_url = \"$SOURCE_URL\"" "$PROVENANCE_SOURCE" || \
    die "Python provenance URL does not match the pinned source"
grep -Fqx "archive_sha256 = \"$ARCHIVE_SHA256\"" "$PROVENANCE_SOURCE" || \
    die "Python provenance SHA-256 does not match the pinned archive"

archive_parent="$(dirname "$ARCHIVE_CACHE")"
runtime_parent="$(dirname "$RUNTIME_DIR")"
mkdir -p "$archive_parent" "$runtime_parent"

work="$(mktemp -d "$runtime_parent/.python-runtime.XXXXXX")"
download="$(mktemp "$archive_parent/.${ARCHIVE_NAME}.XXXXXX")"
runtime_work="$work/runtime"
extract="$work/extract"
cleanup() {
    rm -rf "$work"
    rm -f "$download"
}
trap cleanup EXIT

if [ ! -f "$ARCHIVE_CACHE" ] || \
    ! printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE_CACHE" | \
        sha256sum --check --status; then
    curl --fail --location --retry 2 --output "$download" "$SOURCE_URL"
    printf '%s  %s\n' "$ARCHIVE_SHA256" "$download" | \
        sha256sum --check --status || \
        die "bundled Python archive SHA-256 does not match $ARCHIVE_SHA256"
    mv -f "$download" "$ARCHIVE_CACHE"
fi

install -d -m 0755 "$extract" "$runtime_work/bin"
tar -xzf "$ARCHIVE_CACHE" -C "$extract"
[ -x "$extract/python/bin/python3.11" ] || \
    die "bundled Python archive has no python/bin/python3.11"
[ -f "$extract/python/lib/python3.11/LICENSE.txt" ] || \
    die "bundled Python archive has no CPython license"
[ -f "$extract/python/lib/Tix8.4.3/license.terms" ] || \
    die "bundled Python archive has no Tix license"
[ -f "$extract/python/lib/tk8.6/demos/license.terms" ] || \
    die "bundled Python archive has no Tk license"

install -p -m 0755 "$extract/python/bin/python3.11" \
    "$runtime_work/bin/python3.11"
cp -a "$extract/python/lib" "$runtime_work/lib"
install -p -m 0644 "$PROVENANCE_SOURCE" "$runtime_work/PROVENANCE.toml"

(
    cd "$runtime_work"
    while IFS= read -r -d '' license; do
        sha256sum "$license"
    done < <(
        find lib -type f \
            \( -iname 'license*' -o -iname 'copying*' -o \
            -iname 'notice*' -o -iname 'copyright*' \) \
            -print0 | LC_ALL=C sort -z
    ) > LICENSES.sha256
    [ -s LICENSES.sha256 ] || die "bundled Python has no license materials"
    sha256sum --check --strict --status LICENSES.sha256 || \
        die "bundled Python license manifest is invalid"
)

version="$(
    PYTHONDONTWRITEBYTECODE=1 "$runtime_work/bin/python3.11" \
        -c 'import platform; print(platform.python_version())'
)"
[ "$version" = "$PYTHON_VERSION" ] || \
    die "raw package requires Python $PYTHON_VERSION, found $version"

rm -rf "$RUNTIME_DIR"
mv "$runtime_work" "$RUNTIME_DIR"
