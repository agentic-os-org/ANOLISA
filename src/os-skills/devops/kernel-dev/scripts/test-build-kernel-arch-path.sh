#!/bin/bash
# test-build-kernel-arch-path.sh - regression test for arch-aware RPM paths
#
# build-kernel.sh hardcoded $OUTPUT_DIR/RPMS/x86_64/ in the "RPMs location"
# status line and in install_kernel's `rpm -ivh` glob, although detect_system
# supports aarch64 and sets $ARCH. On aarch64 the glob does not expand, rpm is
# handed a literal nonexistent path, and the script still reports the kernel
# installed. Stub uname -m to aarch64 (pure path-logic test, no rpm build
# needed) and assert both sites use the detected architecture.
#
# Usage: ./test-build-kernel-arch-path.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_KERNEL="$SCRIPT_DIR/build-kernel.sh"

TMP="$(mktemp -d /tmp/test-build-kernel-arch.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

RPMBUILD="$TMP/rpmbuild"
mkdir -p "$RPMBUILD/SRPMS" "$RPMBUILD/RPMS/aarch64" "$TMP/bin"

printf 'ID="alinux"\nPRETTY_NAME="Alinux4 (test)"\n' > "$TMP/os-release"

# Path-redirected copy: only the hardcoded root path and /etc/os-release are
# rewritten so the test can run unprivileged on any host.
sed -e "s|/root/rpmbuild|$RPMBUILD|g" \
    -e "s|/tmp/kernel-build.log|$TMP/kernel-build.log|g" \
    -e "s|/etc/os-release|$TMP/os-release|g" \
    "$BUILD_KERNEL" > "$TMP/build-kernel.sh"
chmod +x "$TMP/build-kernel.sh"

# Stub toolchain on an aarch64 host. rpm logs its arguments and, like a real
# rpm, refuses paths that do not exist (the unexpanded x86_64 glob).
cat > "$TMP/bin/uname" <<'EOF'
#!/bin/bash
case "$1" in
    -m) echo aarch64 ;;
    -r) echo 6.6.102-5.2.alnx4.aarch64 ;;
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
cat > "$TMP/bin/yumdownloader" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/rpm" <<EOF
#!/bin/bash
for a in "\$@"; do
    echo "\$a" >> "$TMP/rpm-args.log"
done
for a in "\$@"; do
    case "\$a" in
        */RPMS/*) [ -e "\$a" ] || { echo "rpm: package \$a does not exist" >&2; exit 1; } ;;
    esac
done
exit 0
EOF
cat > "$TMP/bin/rpmbuild" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/grub2-mkconfig" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod +x "$TMP/bin"/*
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $1" >&2; exit 1; }

# Only the aarch64 flavor of the built kernel exists, as rpmbuild would emit.
: > "$RPMBUILD/RPMS/aarch64/kernel-6.6.102-1.aarch64.rpm"

# 1) srpm build status line must report the detected arch's RPM directory.
out="$TMP/out-build.txt"
if ! "$TMP/build-kernel.sh" srpm > "$out" 2>&1; then
    fail "srpm build failed under the stub toolchain"
fi
if ! grep -q "RPMs location: $RPMBUILD/RPMS/aarch64/" "$out"; then
    fail "srpm build did not report the aarch64 RPMs location"
fi
echo "ok: srpm build reports RPMS/aarch64 location"

# 2) install must pass an existing aarch64 rpm to rpm -ivh.
: > "$TMP/rpm-args.log"
out="$TMP/out-install.txt"
if ! "$TMP/build-kernel.sh" install srpm > "$out" 2>&1; then
    fail "srpm install failed under the stub toolchain"
fi
if ! grep -q "^$RPMBUILD/RPMS/aarch64/kernel-.*\.rpm$" "$TMP/rpm-args.log"; then
    fail "rpm -ivh was not given the built aarch64 kernel package: $(cat "$TMP/rpm-args.log")"
fi
if grep -q "x86_64" "$TMP/rpm-args.log"; then
    fail "rpm -ivh was given an x86_64 path on an aarch64 host"
fi
echo "ok: srpm install targets the built aarch64 package"

echo "PASS: test-build-kernel-arch-path"
