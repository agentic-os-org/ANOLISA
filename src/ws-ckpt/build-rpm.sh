#!/usr/bin/env bash
# build-rpm.sh — Build ws-ckpt RPM package.
# Usage:
#   ./build-rpm.sh          # build SRPM + binary RPM (default)
#   ./build-rpm.sh --srpm   # build SRPM only (for Koji submission)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="${SCRIPT_DIR}/src"

# ── Extract version from Cargo.toml ──
VERSION=$(grep -m1 '^version' "${SRC_DIR}/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
NAME="ws-ckpt"
TARBALL="${NAME}-${VERSION}.tar.gz"
VENDOR_TARBALL="${NAME}-${VERSION}-vendor.tar.gz"

echo "==> Building ${NAME}-${VERSION}"

# ── Setup rpmbuild tree ──
RPMBUILD_DIR="${SCRIPT_DIR}/rpmbuild"
mkdir -p "${RPMBUILD_DIR}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# ── Generate spec from template ──
SPEC="${RPMBUILD_DIR}/SPECS/${NAME}.spec"
sed "s/@VERSION@/${VERSION}/g" "${SCRIPT_DIR}/ws-ckpt.spec.in" > "${SPEC}"
echo "    spec: ${SPEC}"

# ── Create source tarball ──
STAGING=$(mktemp -d)
STAGE_ROOT="${STAGING}/${NAME}-${VERSION}"
mkdir -p "${STAGE_ROOT}/src"

cp -a "${SRC_DIR}/Cargo.toml" "${SRC_DIR}/Cargo.lock" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/crates" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/config.toml.sample" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/systemd" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/skills" "${STAGE_ROOT}/src/"
cp -a "${SRC_DIR}/plugins" "${STAGE_ROOT}/src/"
# Drop musl-libc native node modules: package targets glibc systems only.
# Leaving them in would make RPM dep generator emit bogus libc.musl-x86_64.so.1 requires.
find "${STAGE_ROOT}/src/plugins" -type d -name '*-musl' -prune -exec rm -rf {} + 2>/dev/null || true
find "${STAGE_ROOT}/src/plugins" -type f -name '*.musl.node' -delete 2>/dev/null || true
cp -f "${SCRIPT_DIR}/LICENSE" "${STAGE_ROOT}/"
cp -f "${SCRIPT_DIR}/README.md" "${STAGE_ROOT}/"
cp -f "${SCRIPT_DIR}/component.toml" "${STAGE_ROOT}/"
cp -a "${SCRIPT_DIR}/scripts" "${STAGE_ROOT}/"

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
RPM=$(find "${RPMBUILD_DIR}/RPMS/" -name "${NAME}-${VERSION}-*.rpm" ! -name '*debuginfo*' 2>/dev/null | head -1)

if [[ -n "${SRPM:-}" ]]; then
    echo "SRPM: ${SRPM}"
    echo "  Koji submit:  koji build <target> ${SRPM}"
fi

if [[ -n "${RPM:-}" ]]; then
    echo ""
    echo "RPM:  ${RPM}"
    echo "  Install:      sudo yum install -y rsync btrfs-progs && sudo rpm -ivh ${RPM}"
    echo "  Upgrade:      sudo rpm -Uvh --replacepkgs ${RPM}"
fi
