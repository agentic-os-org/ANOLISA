#!/bin/bash
# test-build-kernel-pipefail.sh - regression test for the kernel build script
#
# build-kernel.sh pipes every make/rpmbuild step through tee and waits on the
# pipeline's PID, so without pipefail a failing compile step is masked and the
# script reports a successful build. Drive the upstream path with stub tools
# (no kernel toolchain or root needed) and assert failures propagate while a
# passing build still completes.
#
# Usage: ./test-build-kernel-pipefail.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_KERNEL="$SCRIPT_DIR/build-kernel.sh"

TMP="$(mktemp -d /tmp/test-build-kernel-pipefail.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

WORK="$TMP/work"
RPMBUILD="$TMP/rpmbuild"
mkdir -p "$WORK" "$RPMBUILD" "$TMP/bin"

printf 'ID="alinux"\nPRETTY_NAME="Alinux4 (test)"\n' > "$TMP/os-release"

# Path-redirected copy: only the hardcoded root paths and /etc/os-release are
# rewritten so the test can run unprivileged on any host.
sed -e "s|/root/rpmbuild|$RPMBUILD|g" \
    -e "s|/root/upstream-kernel|$WORK|g" \
    -e "s|/tmp/kernel-build.log|$TMP/kernel-build.log|g" \
    -e "s|/etc/os-release|$TMP/os-release|g" \
    "$BUILD_KERNEL" > "$TMP/build-kernel.sh"
chmod +x "$TMP/build-kernel.sh"

# Stub toolchain. make succeeds for config targets; MOCK_MAKE_FAIL=1 makes the
# compile steps fail like a real broken build.
cat > "$TMP/bin/uname" <<EOF
#!/bin/bash
case "\$1" in
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
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/bash
echo "linux-6.12.9.tar.xz"
EOF
cat > "$TMP/bin/wget" <<'EOF'
#!/bin/bash
url="${*: -1}"
: > "${url##*/}"
EOF
cat > "$TMP/bin/tar" <<'EOF'
#!/bin/bash
if [ "$1" = "-xf" ]; then
    mkdir -p "${2%.tar.xz}"
else
    /usr/bin/tar "$@"
fi
EOF
cat > "$TMP/bin/make" <<'EOF'
#!/bin/bash
case "$1" in
    defconfig|tinyconfig|olddefconfig) exit 0 ;;
    *)
        if [ "${MOCK_MAKE_FAIL:-0}" = 1 ]; then
            echo "make[$*]: *** Error 2"
            exit 2
        fi
        exit 0
        ;;
esac
EOF
chmod +x "$TMP/bin"/*
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $1" >&2; exit 1; }

# 1) A failing compile step must fail the script (was masked without pipefail).
out="$TMP/out-fail.txt"
if MOCK_MAKE_FAIL=1 "$TMP/build-kernel.sh" upstream 6.12.9 4 defconfig > "$out" 2>&1; then
    fail "script exited 0 although the kernel image build failed"
fi
if grep -q "Upstream build completed" "$out"; then
    fail "script reported success after a failed build"
fi
echo "ok: failing make fails the build"

# 2) Control: a passing build still succeeds end to end.
out="$TMP/out-ok.txt"
if ! "$TMP/build-kernel.sh" upstream 6.12.9 4 defconfig > "$out" 2>&1; then
    fail "script failed on the passing-build control run"
fi
if ! grep -q "Upstream build completed" "$out"; then
    fail "passing build did not report completion"
fi
echo "ok: passing make still completes"

echo "PASS: test-build-kernel-pipefail"
