#!/bin/bash
# setup.sh - Automatic kernel development environment setup
# Usage: ./setup.sh

set -e

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo "========================================"
echo "  Kernel Development Environment Setup"
echo "========================================"
echo ""

# Detect architecture
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)

echo -e "${BLUE}Step 1: System Detection${NC}"
echo "  Architecture: $ARCH"
echo "  Kernel:       $KERNEL_VER"
echo ""

# Validate architecture
case "$ARCH" in
    x86_64)
        echo -e "  ${GREEN}✓${NC} x86_64 architecture supported"
        ;;
    aarch64)
        echo -e "  ${GREEN}✓${NC} aarch64 architecture supported"
        ;;
    *)
        echo -e "  ${RED}✗${NC} Unsupported architecture: $ARCH"
        echo "  Supported: x86_64, aarch64"
        exit 1
        ;;
esac
echo ""

# Check OS
echo -e "${BLUE}Step 2: OS Verification${NC}"
if grep -qi 'alinux\|alnx' /etc/os-release 2>/dev/null; then
    OS_NAME=$(grep -i 'PRETTY_NAME' /etc/os-release | cut -d'"' -f2)
    echo -e "  ${GREEN}✓${NC} $OS_NAME"
else
    echo -e "  ${YELLOW}⚠${NC} Warning: Not Alinux4 system"
    echo "  This script is optimized for Alinux4, but may work on other distributions."
    read -p "  Continue? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi
echo ""

# Check root privileges
echo -e "${BLUE}Step 3: Privilege Check${NC}"
if [ "$EUID" -ne 0 ]; then 
    echo -e "  ${YELLOW}⚠${NC} Root privileges required for package installation"
    echo "  Please enter your sudo password when prompted."
    SUDO_CMD="sudo"
else
    echo -e "  ${GREEN}✓${NC} Running as root"
    SUDO_CMD=""
fi
echo ""

# Install dependencies
echo -e "${BLUE}Step 4: Installing Dependencies${NC}"
echo "  Installing common packages..."
$SUDO_CMD yum install -y \
    gcc gcc-c++ make binutils \
    flex bison \
    libelf-devel openssl-devel ncurses-devel \
    pahole perl python3 python3-devel \
    git ccache dwarves wget curl kmod \
    > /dev/null 2>&1 || {
    echo -e "  ${YELLOW}⚠${NC} Some packages may have failed to install"
}
echo -e "  ${GREEN}✓${NC} Common packages installed"
echo ""

# Install kernel-devel
echo "  Installing kernel development packages..."
$SUDO_CMD yum install -y "kernel-devel-$KERNEL_VER" "kernel-headers-$KERNEL_VER" \
    > /dev/null 2>&1 || {
    echo -e "  ${RED}✗${NC} Failed to install kernel-devel packages"
    echo "  Please check if your kernel version is available in the repository."
    exit 1
}
echo -e "  ${GREEN}✓${NC} Kernel development packages installed"
echo ""

# Verify installation
echo -e "${BLUE}Step 5: Verification${NC}"
BUILD_DIR="/lib/modules/$KERNEL_VER/build"
if [ -L "$BUILD_DIR" ] || [ -d "$BUILD_DIR" ]; then
    REAL_PATH=$(readlink -f "$BUILD_DIR" 2>/dev/null || echo "$BUILD_DIR")
    echo -e "  ${GREEN}✓${NC} Kernel build directory: $REAL_PATH"
else
    echo -e "  ${RED}✗${NC} Kernel build directory not found"
    exit 1
fi

GCC_VERSION=$(gcc --version | head -1)
echo -e "  ${GREEN}✓${NC} Compiler: $GCC_VERSION"
echo ""

# Summary
echo "========================================"
echo -e "  ${GREEN}Setup Complete!${NC}"
echo "========================================"
echo ""
echo "Your kernel development environment is ready."
echo ""
echo "Quick Start:"
echo "  cd examples/hello_module"
echo "  make"
echo "  sudo make install"
echo "  make log"
echo "  sudo make unload"
echo ""
echo "For more examples, see: examples/README.md"
