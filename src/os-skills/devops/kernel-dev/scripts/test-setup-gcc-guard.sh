#!/bin/bash
# test-setup-gcc-guard.sh - regression test for setup.sh's compiler check
#
# setup.sh read `GCC_VERSION=$(gcc --version | head -1)` unguarded: with gcc
# missing, the pipeline's status is head's 0, the version ends up empty, and
# setup printed "✓ Compiler: " plus "Setup Complete!" with exit 0 —
# certifying an environment that cannot compile a module. The sibling scripts
# (check-env.sh, verify-env.sh) guard the same probe with `command -v gcc`.
# Run a path-redirected copy of setup.sh under mock PATHs (no root or real
# packages needed) covering both the gcc-present and gcc-missing directions.
#
# Usage: ./test-setup-gcc-guard.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP="$SCRIPT_DIR/setup.sh"
KERNEL_VER="6.6.102-5.2.alnx4.x86_64"

TMP="$(mktemp -d /tmp/test-setup-gcc-guard.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# Path-redirected copy: only the hardcoded /etc/os-release and /lib/modules
# paths are rewritten so the test can run unprivileged on any host.
sed -e "s|/etc/os-release|$TMP/os-release|g" \
    -e "s|/lib/modules|$TMP/lib/modules|g" \
    "$SETUP" > "$TMP/setup.sh"

printf 'ID="alinux"\nPRETTY_NAME="Alinux4 (test)"\n' > "$TMP/os-release"
mkdir -p "$TMP/lib/modules/$KERNEL_VER/build"

# Minimal PATH holding exactly the commands setup.sh needs besides gcc.
mkdir -p "$TMP/bin"
for c in grep cut tr head readlink mkdir cat; do
    ln -sf "$(command -v "$c")" "$TMP/bin/$c"
done
cat > "$TMP/bin/yum" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/sudo" <<'EOF'
#!/bin/bash
exec "$@"
EOF
cat > "$TMP/bin/uname" <<EOF
#!/bin/bash
case "\$1" in
    -m) echo x86_64 ;;
    -r) echo "$KERNEL_VER" ;;
esac
EOF
chmod +x "$TMP/bin/yum" "$TMP/bin/sudo" "$TMP/bin/uname"

fail() { echo "FAIL: $1" >&2; exit 1; }

# 1) gcc missing: setup must fail clearly, not declare success.
out="$TMP/out-missing.txt"
if PATH="$TMP/bin" /bin/bash "$TMP/setup.sh" > "$out" 2>&1; then
    fail "setup exited 0 although gcc is missing"
fi
if ! grep -q "gcc not found" "$out"; then
    fail "setup did not report the missing compiler"
fi
if grep -q "Setup Complete" "$out"; then
    fail "setup declared success without a compiler"
fi
echo "ok: missing gcc fails setup with a clear message"

# 2) gcc present (stub): setup completes and reports the compiler line.
cat > "$TMP/bin/gcc" <<'EOF'
#!/bin/bash
echo "gcc (GCC) 11.4.0"
EOF
chmod +x "$TMP/bin/gcc"
out="$TMP/out-present.txt"
if ! PATH="$TMP/bin" /bin/bash "$TMP/setup.sh" > "$out" 2>&1; then
    fail "setup failed although gcc is present"
fi
if ! grep -q "Compiler: gcc (GCC) 11.4.0" "$out"; then
    fail "setup did not report the detected compiler version"
fi
if ! grep -q "Setup Complete" "$out"; then
    fail "setup did not declare success with gcc present"
fi
echo "ok: present gcc completes setup with the compiler version"

echo "PASS: test-setup-gcc-guard"
