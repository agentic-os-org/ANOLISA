#!/bin/bash
# install-deps.sh - Install kernel development dependencies automatically
# Usage: sudo ./install-deps.sh

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "========================================"
echo "  Installing Kernel Development Dependencies"
echo "========================================"
echo ""

# Detect architecture and kernel version
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)

echo "System Information:"
echo "  Architecture: $ARCH"
echo "  Kernel:       $KERNEL_VER"
echo ""

# Check if running as root
if [ "$EUID" -ne 0 ]; then 
    echo -e "${YELLOW}Please run as root (use sudo)${NC}"
    exit 1
fi

# Check OS compatibility
if ! grep -qi 'alinux\|alnx' /etc/os-release 2>/dev/null; then
    echo -e "${YELLOW}Warning: Not an Alinux4 system. Proceeding anyway...${NC}"
fi

# Install common dependencies
echo "Installing common dependencies..."
yum install -y \
    gcc gcc-c++ make binutils \
    flex bison \
    libelf-devel openssl-devel ncurses-devel \
    pahole perl python3 python3-devel \
    git ccache dwarves wget curl kmod

echo -e "${GREEN}✓ Common dependencies installed${NC}"
echo ""

# Install kernel-devel and kernel-headers
echo "Installing kernel development packages..."
yum install -y "kernel-devel-$KERNEL_VER" "kernel-headers-$KERNEL_VER"

echo -e "${GREEN}✓ Kernel development packages installed${NC}"
echo ""

# Verify installation
echo "Verifying installation..."
BUILD_DIR="/lib/modules/$KERNEL_VER/build"
if [ -L "$BUILD_DIR" ] || [ -d "$BUILD_DIR" ]; then
    echo -e "${GREEN}✓ Kernel build directory verified${NC}"
else
    echo -e "${YELLOW}⚠ Kernel build directory not found${NC}"
fi

echo ""
echo "========================================"
echo "  Installation Complete!"
echo "========================================"
echo ""
echo "You can now compile kernel modules."
echo "Run './check-env.sh' to verify the environment."
