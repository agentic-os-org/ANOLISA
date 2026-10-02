#!/bin/bash
# verify-env.sh - Comprehensive kernel development environment verification
# Usage: ./verify-env.sh

# Don't use set -e as grep may return non-zero

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PASS=0
FAIL=0
WARN=0

check_pass() {
    echo -e "  ${GREEN}✓${NC} $1"
    ((PASS++))
}

check_fail() {
    echo -e "  ${RED}✗${NC} $1"
    ((FAIL++))
}

check_warn() {
    echo -e "  ${YELLOW}⚠${NC} $1"
    ((WARN++))
}

echo "========================================"
echo "  Kernel Development Environment Verification"
echo "========================================"
echo ""

# Step 1: System Information
echo -e "${BLUE}Step 1: System Information${NC}"
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)
echo "  Architecture: $ARCH"
echo "  Kernel:       $KERNEL_VER"

case "$ARCH" in
    x86_64|aarch64) check_pass "Architecture supported ($ARCH)" ;;
    *) check_fail "Unsupported architecture ($ARCH)" ;;
esac

if grep -qi 'alinux\|alnx' /etc/os-release 2>/dev/null; then
    OS_NAME=$(grep -i 'PRETTY_NAME' /etc/os-release | cut -d'"' -f2)
    check_pass "OS: $OS_NAME"
else
    check_warn "Not Alinux4 system (may still work)"
fi
echo ""

# Step 2: Required Packages
echo -e "${BLUE}Step 2: Required Packages${NC}"
PACKAGES=("gcc" "gcc-c++" "make" "binutils" "flex" "bison" "libelf-devel" "openssl-devel" "ncurses-devel" "pahole" "perl" "python3" "kmod")

for pkg in "${PACKAGES[@]}"; do
    if rpm -q "$pkg" &>/dev/null; then
        VERSION=$(rpm -q "$pkg" --qf '%{VERSION}\n' 2>/dev/null | head -1)
        check_pass "$pkg ($VERSION)"
    else
        check_fail "$pkg (missing)"
    fi
done
echo ""

# Step 3: Kernel Development Packages
echo -e "${BLUE}Step 3: Kernel Development Packages${NC}"
DEVEL_PKG="kernel-devel-$KERNEL_VER"
HEADERS_PKG="kernel-headers-$KERNEL_VER"

if rpm -q "$DEVEL_PKG" &>/dev/null; then
    check_pass "$DEVEL_PKG"
else
    check_fail "$DEVEL_PKG"
fi

if rpm -q "$HEADERS_PKG" &>/dev/null; then
    check_pass "$HEADERS_PKG"
else
    check_fail "$HEADERS_PKG"
fi
echo ""

# Step 4: Kernel Build Directory
echo -e "${BLUE}Step 4: Kernel Build Directory${NC}"
BUILD_DIR="/lib/modules/$KERNEL_VER/build"
if [ -L "$BUILD_DIR" ] || [ -d "$BUILD_DIR" ]; then
    REAL_PATH=$(readlink -f "$BUILD_DIR" 2>/dev/null || echo "$BUILD_DIR")
    check_pass "Build directory exists: $REAL_PATH"
    
    # Check Makefile
    if [ -f "$BUILD_DIR/Makefile" ]; then
        check_pass "Kbuild Makefile exists"
    else
        check_fail "Kbuild Makefile missing"
    fi
    
    # Check Module.symvers
    if [ -f "$BUILD_DIR/Module.symvers" ]; then
        check_pass "Module.symvers exists"
    else
        check_warn "Module.symvers missing (may affect some modules)"
    fi
else
    check_fail "Build directory not found: $BUILD_DIR"
fi
echo ""

# Step 5: Compiler
echo -e "${BLUE}Step 5: Compiler${NC}"
if command -v gcc &>/dev/null; then
    GCC_VERSION=$(gcc --version | head -1)
    check_pass "GCC: $GCC_VERSION"
    
    # Check GCC plugin support (optional)
    if [ -d "$(gcc -print-file-name=plugin)" ]; then
        check_pass "GCC plugins available"
    else
        check_warn "GCC plugins not found (optional)"
    fi
else
    check_fail "GCC not found"
fi

if command -v cc &>/dev/null; then
    check_pass "CC linker available"
else
    check_warn "CC linker not found"
fi
echo ""

# Step 6: Build Tools
echo -e "${BLUE}Step 6: Build Tools${NC}"
if command -v make &>/dev/null; then
    MAKE_VERSION=$(make --version | head -1)
    check_pass "Make: $MAKE_VERSION"
else
    check_fail "Make not found"
fi

if command -v kmod &>/dev/null; then
    KMOD_VERSION=$(kmod --version 2>&1 | head -1)
    check_pass "Kmod: $KMOD_VERSION"
else
    check_fail "Kmod not found"
fi

if command -v git &>/dev/null; then
    GIT_VERSION=$(git --version)
    check_pass "$GIT_VERSION"
else
    check_warn "Git not found (optional)"
fi

if command -v ccache &>/dev/null; then
    CCACHE_VERSION=$(ccache --version | head -1)
    check_pass "$CCACHE_VERSION"
else
    check_warn "Ccache not found (optional, for faster builds)"
fi
echo ""

# Step 7: Kernel Headers
echo -e "${BLUE}Step 7: Kernel Headers${NC}"
HEADER_COUNT=$(ls /usr/include/linux/*.h 2>/dev/null | wc -l)
if [ "$HEADER_COUNT" -gt 0 ]; then
    check_pass "Kernel headers installed ($HEADER_COUNT files)"
else
    check_fail "Kernel headers not found"
fi

if [ -f "/usr/include/linux/version.h" ]; then
    check_pass "version.h exists"
else
    check_warn "version.h missing"
fi
echo ""

# Step 8: Test Compilation
echo -e "${BLUE}Step 8: Test Compilation${NC}"
TEST_DIR=$(mktemp -d)
cd "$TEST_DIR"

cat > test_module.c << 'EOF'
#include <linux/init.h>
#include <linux/module.h>
MODULE_LICENSE("GPL");
static int __init test_init(void) { return 0; }
static void __exit test_exit(void) {}
module_init(test_init);
module_exit(test_exit);
EOF

cat > Makefile << 'EOF'
obj-m += test_module.o
KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)
all:
	make -C $(KERNEL_DIR) M=$(PWD) modules 2>&1
clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean
EOF

if make -s > /dev/null 2>&1; then
    if [ -f "test_module.ko" ]; then
        check_pass "Test module compiled successfully"
        rm -f test_module.ko
    else
        check_fail "Test module compilation produced no output"
    fi
else
    check_fail "Test module compilation failed"
fi

cd - > /dev/null
rm -rf "$TEST_DIR"
echo ""

# Summary
echo "========================================"
echo "  Summary"
echo "========================================"
echo -e "  ${GREEN}Passed:${NC}   $PASS"
echo -e "  ${RED}Failed:${NC}   $FAIL"
echo -e "  ${YELLOW}Warnings:${NC} $WARN"
echo ""

if [ $FAIL -eq 0 ]; then
    echo -e "${GREEN}✓ Environment is ready for kernel development!${NC}"
    echo ""
    echo "Next steps:"
    echo "  cd examples/hello_module && make"
    exit 0
else
    echo -e "${RED}✗ Environment has issues. Please fix the failed checks.${NC}"
    echo ""
    echo "To fix automatically:"
    echo "  sudo ./scripts/install-deps.sh"
    exit 1
fi
