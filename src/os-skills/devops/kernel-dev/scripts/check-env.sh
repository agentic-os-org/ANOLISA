#!/bin/bash
# check-env.sh - Check kernel development environment dependencies
# Usage: ./check-env.sh

set -e

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "========================================"
echo "  Kernel Development Environment Check"
echo "========================================"
echo ""

# Detect architecture
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)

echo "System Information:"
echo "  Architecture: $ARCH"
echo "  Kernel:       $KERNEL_VER"
echo ""

# Check OS
echo "Operating System:"
if grep -qi 'alinux\|alnx' /etc/os-release 2>/dev/null; then
    OS_NAME=$(grep -i 'PRETTY_NAME' /etc/os-release | cut -d'"' -f2)
    echo -e "  ${GREEN}✓${NC} $OS_NAME"
else
    echo -e "  ${YELLOW}⚠${NC} Warning: Not Alinux4 system (may still work)"
fi
echo ""

# Check required packages
echo "Required Packages:"
PACKAGES=("gcc" "gcc-c++" "make" "binutils" "flex" "bison" "libelf-devel" "openssl-devel" "ncurses-devel" "pahole" "perl" "python3" "kmod")
MISSING=()

for pkg in "${PACKAGES[@]}"; do
    if rpm -q "$pkg" &>/dev/null; then
        VERSION=$(rpm -q "$pkg" --qf '%{VERSION}\n' 2>/dev/null | head -1)
        echo -e "  ${GREEN}✓${NC} $pkg ($VERSION)"
    else
        echo -e "  ${RED}✗${NC} $pkg (missing)"
        MISSING+=("$pkg")
    fi
done
echo ""

# Check kernel-devel
echo "Kernel Development Packages:"
DEVEL_PKG="kernel-devel-$KERNEL_VER"
HEADERS_PKG="kernel-headers-$KERNEL_VER"

if rpm -q "$DEVEL_PKG" &>/dev/null; then
    echo -e "  ${GREEN}✓${NC} $DEVEL_PKG"
else
    echo -e "  ${RED}✗${NC} $DEVEL_PKG (missing)"
    MISSING+=("$DEVEL_PKG")
fi

if rpm -q "$HEADERS_PKG" &>/dev/null; then
    echo -e "  ${GREEN}✓${NC} $HEADERS_PKG"
else
    echo -e "  ${RED}✗${NC} $HEADERS_PKG (missing)"
    MISSING+=("$HEADERS_PKG")
fi
echo ""

# Check kernel build directory
echo "Kernel Build Directory:"
BUILD_DIR="/lib/modules/$KERNEL_VER/build"
if [ -L "$BUILD_DIR" ] || [ -d "$BUILD_DIR" ]; then
    REAL_PATH=$(readlink -f "$BUILD_DIR" 2>/dev/null || echo "$BUILD_DIR")
    echo -e "  ${GREEN}✓${NC} $BUILD_DIR -> $REAL_PATH"
else
    echo -e "  ${RED}✗${NC} $BUILD_DIR (not found)"
    MISSING+=("kernel-build-dir")
fi
echo ""

# Check GCC version compatibility
echo "Compiler Information:"
if command -v gcc &>/dev/null; then
    GCC_VERSION=$(gcc --version | head -1)
    echo -e "  ${GREEN}✓${NC} $GCC_VERSION"
else
    echo -e "  ${RED}✗${NC} gcc not found"
fi
echo ""

# Summary
echo "========================================"
echo "  Summary"
echo "========================================"
if [ ${#MISSING[@]} -eq 0 ]; then
    echo -e "${GREEN}✓ All dependencies are installed!${NC}"
    echo ""
    echo "You can now compile kernel modules."
    exit 0
else
    echo -e "${RED}✗ Missing ${#MISSING[@]} package(s):${NC}"
    for pkg in "${MISSING[@]}"; do
        echo "    - $pkg"
    done
    echo ""
    echo -e "${YELLOW}To install missing packages, run:${NC}"
    echo ""
    if [ "$ARCH" = "x86_64" ] || [ "$ARCH" = "aarch64" ]; then
        echo "  sudo yum install -y ${MISSING[*]}"
    else
        echo "  Please install the missing packages manually."
    fi
    exit 1
fi
