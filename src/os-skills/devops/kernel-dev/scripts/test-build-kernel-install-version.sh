#!/bin/bash
# test-build-kernel-install-version.sh - regression test for the install path
#
# install_kernel resolved its version with "${KERNEL_VERSION:-$(get_latest_kernel)}",
# but KERNEL_VERSION is always set (line 17 defaults it to the string "latest"),
# so the fallback never fired: `build-kernel.sh install` computed
# $WORK_DIR/linux-latest, failed at `cd`, and never ran make. The fix resolves
# the "latest" sentinel via get_latest_kernel, mirroring build_upstream. Drive
# the install path with stub tools (no kernel toolchain or root needed) and
# assert make modules_install / make install run under the resolved version.
#
# Usage: ./test-build-kernel-install-version.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_KERNEL="$SCRIPT_DIR/build-kernel.sh"
RESOLVED_VERSION="6.12.9"

TMP="$(mktemp -d /tmp/test-build-kernel-install.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

WORK="$TMP/work"
RPMBUILD="$TMP/rpmbuild"
mkdir -p "$WORK/linux-$RESOLVED_VERSION" "$RPMBUILD" "$TMP/bin"

printf 'ID="alinux"\nPRETTY_NAME="Alinux4 (test)"\n' > "$TMP/os-release"

# Path-redirected copy: only the hardcoded root paths and /etc/os-release are
# rewritten so the test can run unprivileged on any host.
sed -e "s|/root/rpmbuild|$RPMBUILD|g" \
    -e "s|/root/upstream-kernel|$WORK|g" \
    -e "s|/tmp/kernel-build.log|$TMP/kernel-build.log|g" \
    -e "s|/etc/os-release|$TMP/os-release|g" \
    "$BUILD_KERNEL" > "$TMP/build-kernel.sh"
chmod +x "$TMP/build-kernel.sh"

# Stub toolchain: get_latest_kernel's curl feed resolves to 6.12.9; make and
# grub2-mkconfig succeed and log their invocations.
cat > "$TMP/bin/uname" <<'EOF'
#!/bin/bash
case "$1" in
    -m) echo x86_64 ;;
    -r) echo 6.6.102-5.2.alnx4.x86_64 ;;
esac
EOF
cat > "$TMP/bin/nproc" <<'EOF'
#!/bin/bash
echo 4
EOF
cat > "$TMP/bin/yum" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/sudo" <<'EOF'
#!/bin/bash
exec "$@"
EOF
cat > "$TMP/bin/curl" <<EOF
#!/bin/bash
echo "linux-$RESOLVED_VERSION.tar.xz"
EOF
cat > "$TMP/bin/make" <<EOF
#!/bin/bash
echo "make \$*" >> "$TMP/make.log"
exit 0
EOF
cat > "$TMP/bin/grub2-mkconfig" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod +x "$TMP/bin"/*
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $1" >&2; exit 1; }

out="$TMP/out.txt"
if ! "$TMP/build-kernel.sh" install > "$out" 2>&1; then
    fail "install path failed instead of resolving the latest version"
fi
if ! grep -q "Latest stable kernel: $RESOLVED_VERSION" "$out"; then
    fail "install output does not reference the resolved version $RESOLVED_VERSION"
fi
for target in modules_install install; do
    if ! grep -q "^make $target\$" "$TMP/make.log"; then
        fail "make $target was not invoked under the resolved source directory"
    fi
done
echo "ok: install resolves 'latest' and installs from linux-$RESOLVED_VERSION"

echo "PASS: test-build-kernel-install-version"
