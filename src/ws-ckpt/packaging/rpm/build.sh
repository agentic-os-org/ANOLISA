#!/usr/bin/env bash
# build-rpm.sh — Build ws-ckpt compatibility and role RPMs.
# Usage:
#   ./build-rpm.sh          # build SRPM + binary RPM (default)
#   ./build-rpm.sh --srpm   # build SRPM only (for Koji submission)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SRC_DIR="${COMPONENT_ROOT}/src"

# ── Extract version from Cargo.toml ──
VERSION=$(grep -m1 '^version' "${SRC_DIR}/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
NAME="ws-ckpt"
TARBALL="${NAME}-${VERSION}.tar.gz"
VENDOR_TARBALL="${NAME}-${VERSION}-vendor.tar.gz"

echo "==> Building ${NAME}-${VERSION}"

# ── Setup rpmbuild tree ──
RPMBUILD_DIR="${COMPONENT_ROOT}/rpmbuild"
mkdir -p "${RPMBUILD_DIR}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# ── Generate spec from template ──
SPEC="${RPMBUILD_DIR}/SPECS/${NAME}.spec"
sed "s/@VERSION@/${VERSION}/g" "${COMPONENT_ROOT}/packaging/rpm/ws-ckpt.spec.in" > "${SPEC}"
echo "    spec: ${SPEC}"

# ── Create source tarball ──
STAGING=$(mktemp -d)
STAGE_ROOT="${STAGING}/${NAME}-${VERSION}"
mkdir -p "${STAGE_ROOT}/src"

cp -a "${SRC_DIR}/Cargo.toml" "${SRC_DIR}/Cargo.lock" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/crates" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/config.toml.sample" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/skills" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/plugins" "${STAGE_ROOT}/src/"
# Drop musl-libc native node modules: package targets glibc systems only.
# Leaving them in would make RPM dep generator emit bogus libc.musl-x86_64.so.1 requires.
find "${STAGE_ROOT}/src/plugins" -type d -name '*-musl' -prune -exec rm -rf {} + 2>/dev/null || true
find "${STAGE_ROOT}/src/plugins" -type f -name '*.musl.node' -delete 2>/dev/null || true
cp -f "${COMPONENT_ROOT}/LICENSE" "${STAGE_ROOT}/"
cp -f "${COMPONENT_ROOT}/README.md" "${STAGE_ROOT}/"
cp -a "${COMPONENT_ROOT}/scripts" "${COMPONENT_ROOT}/packaging" "${COMPONENT_ROOT}/tests" "${STAGE_ROOT}/"
cp -f "${COMPONENT_ROOT}/Makefile" "${STAGE_ROOT}/"

tar -czf "${RPMBUILD_DIR}/SOURCES/${TARBALL}" -C "${STAGING}" "${NAME}-${VERSION}"
echo "    source: ${RPMBUILD_DIR}/SOURCES/${TARBALL}"

# ── Vendor dependencies (cargo vendor) ──
echo "==> Vendoring dependencies..."
(
  cd "${STAGE_ROOT}/src"
  cargo vendor --locked vendor > /dev/null 2>&1
  mkdir -p .cargo
  cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF
)

tar -czf "${RPMBUILD_DIR}/SOURCES/${VENDOR_TARBALL}" -C "${STAGE_ROOT}" src/vendor src/.cargo
echo "    vendor: ${RPMBUILD_DIR}/SOURCES/${VENDOR_TARBALL}"

rm -rf "${STAGING}"

# ── Build ──
if [[ "${1:-}" == "--srpm" ]]; then
    echo "==> Building SRPM only..."
    rpmbuild --define "_topdir ${RPMBUILD_DIR}" -bs "${SPEC}"
else
    echo "==> Building SRPM + RPM..."
    rpmbuild --define "_topdir ${RPMBUILD_DIR}" -ba "${SPEC}"
fi

echo ""
echo "==> Done!"
echo ""

SRPM=$(ls -1 "${RPMBUILD_DIR}/SRPMS/${NAME}-${VERSION}-"*.rpm 2>/dev/null | head -1)

if [[ -n "${SRPM:-}" ]]; then
    echo "SRPM: ${SRPM}"
    echo "  Koji submit:  koji build <target> ${SRPM}"
fi

if [[ "${1:-}" != "--srpm" ]]; then
    # Query this spec's exact NEVRAs, excluding artifacts from older builds.
    RPM_FILES=()
    while IFS= read -r relative; do
        artifact="${RPMBUILD_DIR}/RPMS/${relative}"
        test -f "$artifact" || { echo "ERROR: missing RPM output: $artifact" >&2; exit 1; }
        RPM_FILES+=("$artifact")
    done < <(rpmspec -q --rpms --qf '%{ARCH}/%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}.rpm\n' "$SPEC")
    test "${#RPM_FILES[@]}" -eq 4 || { echo "ERROR: expected ws-ckpt, core, plugins, and skills RPMs" >&2; exit 1; }
    printf '\nRPM: %s\n' "${RPM_FILES[@]}"
    printf '  Install:      sudo yum install -y rsync btrfs-progs && sudo rpm -ivh'
    printf ' %q' "${RPM_FILES[@]}"
    printf '\n  Upgrade:      sudo rpm -Uvh --replacepkgs'
    printf ' %q' "${RPM_FILES[@]}"
    printf '\n'
fi
