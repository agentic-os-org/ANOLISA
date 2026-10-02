#!/bin/bash
# build-kernel.sh - Optimized kernel build script for Alinux4
# Supports both SRPM method (Alinux4 official) and Upstream method (kernel.org)
# Usage: ./build-kernel.sh [srpm|upstream] [options]

set -e

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Default options
BUILD_METHOD="${1:-upstream}"
KERNEL_VERSION="${2:-latest}"
PARALLEL_JOBS="${3:-$(nproc)}"
CONFIG_TYPE="${4:-defconfig}"
OUTPUT_DIR="/root/rpmbuild"
WORK_DIR="/root/upstream-kernel"
LOG_FILE="/tmp/kernel-build.log"

# Color output functions
info() { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[✓]${NC} $1"; }
warn() { echo -e "${YELLOW}[⚠]${NC} $1"; }
error() { echo -e "${RED}[✗]${NC} $1"; }
step() { echo -e "\n${CYAN}=== $1 ===${NC}"; }

# Check root privileges
check_root() {
    if [ "$EUID" -ne 0 ]; then
        warn "Root privileges recommended for full functionality"
        SUDO_CMD="sudo"
    else
        SUDO_CMD=""
    fi
}

# Detect system info
detect_system() {
    ARCH=$(uname -m)
    KERNEL_VER=$(uname -r)
    OS_ID=$(grep -i '^ID=' /etc/os-release | cut -d'=' -f2 | tr -d '"')
    
    info "Architecture: $ARCH"
    info "Current Kernel: $KERNEL_VER"
    info "OS: $OS_ID"
    
    case "$ARCH" in
        x86_64)
            KARCH="x86"
            IMAGE_TARGET="bzImage"
            IMAGE_PATH="arch/x86/boot/bzImage"
            ;;
        aarch64)
            KARCH="arm64"
            IMAGE_TARGET="Image"
            IMAGE_PATH="arch/arm64/boot/Image"
            ;;
        *)
            error "Unsupported architecture: $ARCH"
            exit 1
            ;;
    esac
    
    success "System detected: $ARCH ($OS_ID)"
}

# Install build dependencies
install_deps() {
    step "Installing Build Dependencies"
    
    local deps=(
        gcc gcc-c++ make binutils
        flex bison
        libelf-devel openssl-devel ncurses-devel
        pahole perl python3 python3-devel
        git ccache dwarves wget curl kmod
        rpm-build rpmdevtools yum-utils
    )
    
    info "Installing packages: ${deps[*]}"
    $SUDO_CMD yum install -y "${deps[@]}" > /dev/null 2>&1 || {
        warn "Some packages may have failed to install"
    }
    
    success "Dependencies installed"
}

# Get latest stable kernel version from kernel.org
get_latest_kernel() {
    local version
    version=$(curl -sL https://kernel.org/ 2>/dev/null | grep -o 'linux-[0-9.]*\.tar\.xz' | head -1 | sed 's/linux-//;s/\.tar\.xz//')
    
    if [ -z "$version" ]; then
        version="6.12.9"  # Fallback version
        warn "Could not fetch latest version, using fallback: $version"
    else
        success "Latest stable kernel: $version"
    fi
    
    echo "$version"
}

# Build using SRPM method (Alinux4 official)
build_srpm() {
    step "SRPM Method - Alinux4 Official Kernel"
    
    local kernel_ver="${KERNEL_VERSION:-$(uname -r)}"
    local spec_file="$OUTPUT_DIR/SPECS/kernel.spec"
    
    info "Building kernel: $kernel_ver"
    info "Output directory: $OUTPUT_DIR"
    info "Parallel jobs: $PARALLEL_JOBS"
    
    # Create RPM build directory structure
    mkdir -p "$OUTPUT_DIR"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    
    # Download SRPM if not exists
    if [ ! -f "$OUTPUT_DIR/SRPMS/kernel-${kernel_ver}.src.rpm" ]; then
        info "Downloading SRPM for $kernel_ver"
        cd "$OUTPUT_DIR/SRPMS"
        yumdownloader --source kernel --releasever=4 2>/dev/null || {
            error "Failed to download SRPM"
            exit 1
        }
    fi
    
    # Install SRPM to extract spec and sources
    rpm -ivh "$OUTPUT_DIR/SRPMS/"*.src.rpm 2>/dev/null || true
    
    # Prepare build
    info "Preparing build environment..."
    cd "$OUTPUT_DIR/SPECS"
    rpmbuild -bp kernel.spec --define "_topdir $OUTPUT_DIR" 2>&1 | tee -a "$LOG_FILE"
    
    # Build kernel
    info "Starting kernel compilation (this may take 1-3 hours)..."
    info "Build log: $LOG_FILE"
    
    rpmbuild -bc kernel.spec \
        --define "_topdir $OUTPUT_DIR" \
        --define "with_debug 0" \
        --define "with_doc 0" \
        --define "with_headers 1" \
        --define "with_perf 1" \
        2>&1 | tee -a "$LOG_FILE" &
    
    local build_pid=$!
    info "Build started in background (PID: $build_pid)"
    info "Monitor progress: tail -f $LOG_FILE"
    
    # Wait for completion
    wait $build_pid
    
    success "SRPM build completed"
    info "RPMs location: $OUTPUT_DIR/RPMS/x86_64/"
}

# Build using Upstream method (kernel.org)
build_upstream() {
    step "Upstream Method - Latest from kernel.org"
    
    local kernel_ver
    if [ "$KERNEL_VERSION" = "latest" ]; then
        kernel_ver=$(get_latest_kernel)
    else
        kernel_ver="$KERNEL_VERSION"
    fi
    
    local src_dir="$WORK_DIR/linux-$kernel_ver"
    local tarball="$WORK_DIR/linux-$kernel_ver.tar.xz"
    
    info "Building kernel: $kernel_ver"
    info "Work directory: $WORK_DIR"
    info "Parallel jobs: $PARALLEL_JOBS"
    
    # Create work directory
    mkdir -p "$WORK_DIR"
    cd "$WORK_DIR"
    
    # Download kernel source if not exists
    if [ ! -f "$tarball" ]; then
        info "Downloading kernel source..."
        wget -q "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$kernel_ver.tar.xz" || {
            error "Failed to download kernel $kernel_ver"
            exit 1
        }
        success "Downloaded: $tarball ($(ls -lh $tarball | awk '{print $5}'))"
    else
        success "Source tarball exists: $tarball"
    fi
    
    # Extract if not already extracted
    if [ ! -d "$src_dir" ]; then
        info "Extracting source..."
        tar -xf "$tarball"
        success "Extracted to: $src_dir"
    else
        success "Source directory exists: $src_dir"
    fi
    
    cd "$src_dir"
    
    # Configure ccache if available
    if command -v ccache &>/dev/null; then
        export CC="ccache gcc"
        export CXX="ccache g++"
        success "Ccache enabled for faster compilation"
    fi
    
    # Configure kernel
    info "Configuring kernel ($CONFIG_TYPE)..."
    case "$CONFIG_TYPE" in
        defconfig)
            make defconfig 2>&1 | tee -a "$LOG_FILE"
            ;;
        tinyconfig)
            make tinyconfig 2>&1 | tee -a "$LOG_FILE"
            ;;
        menuconfig)
            make menuconfig 2>&1 | tee -a "$LOG_FILE"
            ;;
        current)
            if [ -f "/boot/config-$KERNEL_VER" ]; then
                cp "/boot/config-$KERNEL_VER" .config
                make olddefconfig 2>&1 | tee -a "$LOG_FILE"
            else
                warn "Current config not found, falling back to defconfig"
                make defconfig 2>&1 | tee -a "$LOG_FILE"
            fi
            ;;
        *)
            warn "Unknown config type: $CONFIG_TYPE, using defconfig"
            make defconfig 2>&1 | tee -a "$LOG_FILE"
            ;;
    esac
    
    success "Kernel configured"
    
    # Build kernel
    info "Starting kernel compilation (estimated: 30-60 minutes)..."
    info "Build log: $LOG_FILE"
    
    make -j"$PARALLEL_JOBS" "$IMAGE_TARGET" 2>&1 | tee -a "$LOG_FILE" &
    local build_pid=$!
    info "Build started in background (PID: $build_pid)"
    info "Monitor progress: tail -f $LOG_FILE"
    
    # Wait for completion
    wait $build_pid
    
    # Build modules
    info "Building kernel modules..."
    make -j"$PARALLEL_JOBS" modules 2>&1 | tee -a "$LOG_FILE"
    
    success "Upstream build completed"
    info "Kernel image: $src_dir/$IMAGE_PATH"
    info "Modules: $src_dir/drivers/**/*.ko"
}

# Install compiled kernel
install_kernel() {
    step "Installing Kernel"
    
    local method="$1"
    
    if [ "$method" = "srpm" ]; then
        info "Installing RPM packages..."
        $SUDO_CMD rpm -ivh "$OUTPUT_DIR/RPMS/x86_64/"kernel-*.rpm 2>&1 | tee -a "$LOG_FILE"
        
        info "Updating GRUB2 configuration..."
        $SUDO_CMD grub2-mkconfig -o /boot/grub2/grub.cfg 2>&1 | tee -a "$LOG_FILE"
        
        success "SRPM kernel installed"
        
    elif [ "$method" = "upstream" ]; then
        local kernel_ver="${KERNEL_VERSION:-$(get_latest_kernel)}"
        local src_dir="$WORK_DIR/linux-$kernel_ver"
        
        cd "$src_dir"
        
        info "Installing kernel modules..."
        $SUDO_CMD make modules_install 2>&1 | tee -a "$LOG_FILE"
        
        info "Installing kernel..."
        $SUDO_CMD make install 2>&1 | tee -a "$LOG_FILE"
        
        info "Updating GRUB2 configuration..."
        $SUDO_CMD grub2-mkconfig -o /boot/grub2/grub.cfg 2>&1 | tee -a "$LOG_FILE"
        
        success "Upstream kernel installed"
    fi
    
    info "Reboot to use new kernel: sudo reboot"
}

# Show build status
show_status() {
    step "Build Status"
    
    info "SRPM Build Log:"
    if [ -f "/tmp/kernel-rpmbuild.log" ]; then
        tail -10 /tmp/kernel-rpmbuild.log
    else
        warn "SRPM build log not found"
    fi
    
    echo ""
    info "Upstream Build Log:"
    if [ -f "$LOG_FILE" ]; then
        tail -10 "$LOG_FILE"
    else
        warn "Upstream build log not found"
    fi
    
    echo ""
    info "Running Build Processes:"
    ps aux | grep -E "make.*kernel|rpmbuild" | grep -v grep | head -5 || warn "No active builds found"
}

# Print usage
usage() {
    cat << EOF
Usage: $0 [method] [version] [jobs] [config]

Methods:
  srpm      - Build from Alinux4 SRPM (official kernel)
  upstream  - Build from kernel.org source (latest stable)
  status    - Show current build status

Options:
  method   : srpm | upstream | status (default: upstream)
  version  : Kernel version (default: latest)
  jobs     : Parallel compilation jobs (default: CPU cores)
  config   : Config type: defconfig|tinyconfig|menuconfig|current (default: defconfig)

Examples:
  $0 upstream latest 8 defconfig          # Build latest upstream kernel (6.12.x+)
  $0 srpm                                 # Build current Alinux4 kernel (auto-detect)
  $0 srpm 6.6.102-5.2.alnx4.x86_64        # Build specific Alinux4 kernel version
  $0 upstream 6.12.9 4 current            # Build 6.12.9 with current config
  $0 status                               # Show build progress

Output:
  SRPM builds:    $OUTPUT_DIR/RPMS/x86_64/
  Upstream builds: $WORK_DIR/linux-*/arch/*/boot/
  Build logs:     $LOG_FILE

EOF
}

# Main execution
main() {
    echo "========================================"
    echo "  Alinux4 Kernel Build Automation"
    echo "========================================"
    echo ""
    
    check_root
    detect_system
    
    case "$BUILD_METHOD" in
        srpm)
            install_deps
            build_srpm
            ;;
        upstream)
            install_deps
            build_upstream
            ;;
        install)
            install_kernel "${2:-upstream}"
            ;;
        status)
            show_status
            ;;
        help|--help|-h)
            usage
            exit 0
            ;;
        *)
            error "Unknown method: $BUILD_METHOD"
            usage
            exit 1
            ;;
    esac
}

main "$@"
