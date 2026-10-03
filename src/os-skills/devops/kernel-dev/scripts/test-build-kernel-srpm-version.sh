#!/bin/bash
# test-build-kernel-srpm-version.sh - regression test for the SRPM build path
#
# build_srpm resolved its version with "${KERNEL_VERSION:-$(uname -r)}", but
# KERNEL_VERSION is always set (the argument default on line 17 is the literal
# string "latest"), so the documented "auto-detect the running kernel" never
# happened: kernel_ver stayed "latest", the SRPM cache check at the SRPMS
# glob never hit, and every run re-invoked yumdownloader (which has no
# package named "latest" and fails). Resolve the sentinel and assert the
# cached SRPM for the running kernel is reused without any download.
#
# Usage: ./test-build-kernel-srpm-version.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_KERNEL="$SCRIPT_DIR/build-kernel.sh"
RUNNING_KERNEL="6.6.102-5.2.alnx4.x86_64"

TMP="$(mktemp -d /tmp/test-build-kernel-srpm.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

RPMBUILD="$TMP/rpmbuild"
mkdir -p "$RPMBUILD/SRPMS" "$TMP/bin"

printf 'ID="alinux"\nPRETTY_NAME="Alinux4 (test)"\n' > "$TMP/os-release"

# Path-redirected copy: only the hardcoded root path and /etc/os-release are
# rewritten so the test can run unprivileged on any host.
sed -e "s|/root/rpmbuild|$RPMBUILD|g" \
    -e "s|/tmp/kernel-build.log|$TMP/kernel-build.log|g" \
    -e "s|/etc/os-release|$TMP/os-release|g" \
    "$BUILD_KERNEL" > "$TMP/build-kernel.sh"
chmod +x "$TMP/build-kernel.sh"

# Stub toolchain. yumdownloader fails like a real repo would for a bogus
# version and logs the call; rpmbuild succeeds so the build completes.
cat > "$TMP/bin/uname" <<EOF
#!/bin/bash
case "\$1" in
    -m) echo x86_64 ;;
    -r) echo "$RUNNING_KERNEL" ;;
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
cat > "$TMP/bin/yumdownloader" <<EOF
#!/bin/bash
echo "yumdownloader \$*" >> "$TMP/yumdownloader.log"
echo "yumdownloader: no package matches (stub)" >&2
exit 1
EOF
cat > "$TMP/bin/rpm" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/rpmbuild" <<EOF
#!/bin/bash
echo "rpmbuild \$*" >> "$TMP/rpmbuild.log"
exit 0
EOF
chmod +x "$TMP/bin"/*
export PATH="$TMP/bin:$PATH"
: > "$TMP/yumdownloader.log"

# SRPM for the running kernel already cached: auto-detect must reuse it.
: > "$RPMBUILD/SRPMS/kernel-$RUNNING_KERNEL.src.rpm"

fail() { echo "FAIL: $1" >&2; exit 1; }

out="$TMP/out.txt"
if ! "$TMP/build-kernel.sh" srpm > "$out" 2>&1; then
    fail "srpm build failed instead of reusing the cached SRPM for the running kernel"
fi
if ! grep -q "Building kernel: $RUNNING_KERNEL" "$out"; then
    fail "srpm build did not auto-detect the running kernel version"
fi
if [ -s "$TMP/yumdownloader.log" ]; then
    fail "yumdownloader invoked although the SRPM was cached: $(cat "$TMP/yumdownloader.log")"
fi
if ! grep -q "SRPM build completed" "$out"; then
    fail "srpm build did not report completion"
fi
echo "ok: srpm auto-detects the running kernel and reuses the cached SRPM"

echo "PASS: test-build-kernel-srpm-version"
