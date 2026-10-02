#!/usr/bin/env bash
# check-deps.sh — Verify build dependencies for agentsight
#
# Usage: ./scripts/check-deps.sh
#
# Supports: Anolis OS / CentOS / RHEL / Fedora (rpm-based) and Ubuntu / Debian (dpkg-based)
# Exit code 0 = all dependencies present, non-zero = missing dependencies.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

MISSING=0

# Detect package manager and distribution
detect_system() {
    if command -v rpm &>/dev/null && command -v yum &>/dev/null; then
        PM="rpm"
        PKG_MGR="yum"
    elif command -v dnf &>/dev/null; then
        PM="rpm"
        PKG_MGR="dnf"
    elif command -v dpkg &>/dev/null && command -v apt &>/dev/null; then
        PM="dpkg"
        PKG_MGR="apt"
    else
        PM="unknown"
        PKG_MGR="unknown"
    fi
    return 0
}

detect_system

# Package name lookup: rpm-name => dpkg-name
pkg_dpkg_name() {
    local rpm_pkg="$1"
    case "$rpm_pkg" in
        openssl-devel)       echo "libssl-dev" ;;
        elfutils-libelf-devel) echo "libelf-dev" ;;
        perl-IPC-Cmd)        echo "perl" ;;
        libbpf-devel)        echo "libbpf-dev" ;;
        *)                   echo "$rpm_pkg" ;;
    esac
}

check_cmd() {
    local name="$1"
    local hint="$2"
    if command -v "$name" &>/dev/null; then
        printf "  ${GREEN}OK${NC}    %s\n" "$name"
    else
        printf "  ${RED}MISS${NC} %s — %s\n" "$name" "$hint"
        MISSING=$((MISSING + 1))
    fi
}

check_pkg() {
    local name="$1"
    local rpm_pkg="$2"

    if [ "$PM" = "rpm" ]; then
        if rpm -q "$rpm_pkg" &>/dev/null 2>&1; then
            local ver
            ver=$(rpm -q --qf '%{VERSION}-%{RELEASE}' "$rpm_pkg" 2>/dev/null)
            printf "  ${GREEN}OK${NC}    %s (rpm: %s)\n" "$name" "$ver"
        else
            printf "  ${RED}MISS${NC} %s — install with: sudo ${PKG_MGR} install -y %s\n" "$name" "$rpm_pkg"
            MISSING=$((MISSING + 1))
        fi
    elif [ "$PM" = "dpkg" ]; then
        local dpkg_name
        dpkg_name=$(pkg_dpkg_name "$rpm_pkg")
        if dpkg -s "$dpkg_name" &>/dev/null 2>&1; then
            local ver
            ver=$(dpkg -s "$dpkg_name" 2>/dev/null | grep '^Version:' | awk '{print $2}')
            printf "  ${GREEN}OK${NC}    %s (dpkg: %s)\n" "$name" "$ver"
        else
            printf "  ${RED}MISS${NC} %s — install with: sudo ${PKG_MGR} install -y %s\n" "$name" "$dpkg_name"
            MISSING=$((MISSING + 1))
        fi
    else
        printf "  ${YELLOW}???${NC}  %s — unknown package manager, check manually\n" "$name"
    fi
}

check_kernel_ver() {
    local min_major="$1"
    local min_minor="$2"
    local ver
    ver=$(uname -r | cut -d. -f1-2)
    local major minor
    major=$(echo "$ver" | cut -d. -f1)
    minor=$(echo "$ver" | cut -d. -f2)
    if [ "$major" -gt "$min_major" ] || ([ "$major" -eq "$min_major" ] && [ "$minor" -ge "$min_minor" ]); then
        printf "  ${GREEN}OK${NC}    Linux kernel %s (%s)\n" "$(uname -r)" ">= ${min_major}.${min_minor}"
    else
        printf "  ${RED}FAIL${NC}  Linux kernel %s (need >= ${min_major}.${min_minor})\n" "$(uname -r)"
        MISSING=$((MISSING + 1))
    fi
}

check_btf() {
    if [ -f /sys/kernel/btf/vmlinux ]; then
        printf "  ${GREEN}OK${NC}    BTF support (/sys/kernel/btf/vmlinux)\n"
    else
        printf "  ${YELLOW}WARN${NC}  BTF not found — eBPF CO-RE may not work\n"
    fi
}

pkg_install_hint() {
    if [ "$PM" = "rpm" ]; then
        echo "  sudo ${PKG_MGR} install -y openssl-devel elfutils-libelf-devel perl-IPC-Cmd libbpf-devel clang llvm bpftool"
    elif [ "$PM" = "dpkg" ]; then
        echo "  sudo apt install -y pkg-config libssl-dev libelf-dev perl libbpf-dev clang llvm linux-tools-common"
    else
        echo "  (unknown package manager — consult your distribution's documentation)"
    fi
}

echo "agentsight build dependency check"
echo "=================================="
if [ "$PM" != "unknown" ]; then
    printf "Detected: %s-based system (installer: %s)\n" "$PM" "$PKG_MGR"
else
    echo "Detected: unknown system"
fi
echo ""

echo "System packages:"
check_pkg "OpenSSL devel" "openssl-devel"
check_pkg "libelf devel" "elfutils-libelf-devel"
check_pkg "Perl IPC::Cmd" "perl-IPC-Cmd"
check_pkg "libbpf devel" "libbpf-devel"
echo ""

echo "Build tools:"
check_cmd "cargo" "install via https://rustup.rs"
check_cmd "rustc" "install via https://rustup.rs"
check_cmd "clang" "install with: sudo ${PKG_MGR} install -y clang"
check_cmd "llvm-strip" "install with: sudo ${PKG_MGR} install -y llvm"
pkg_install_cmd="$(pkg_install_hint | head -1 | sed 's/^  //')"
check_cmd "bpftool" "install with: ${pkg_install_cmd% openssl*}" # just show the base install hint
echo ""

echo "Kernel:"
check_kernel_ver 5 8
check_btf
echo ""

echo "Rust toolchain:"
RUSTC_VER=$(rustc --version 2>/dev/null | awk '{print $2}' || echo "N/A")
printf "  rustc %s\n" "$RUSTC_VER"

if [ "$MISSING" -gt 0 ]; then
    echo ""
    printf "${RED}%d dependency(s) missing.${NC}\n" "$MISSING"
    echo ""
    echo "Install missing dependencies:"
    pkg_install_hint
    exit 1
else
    echo ""
    printf "${GREEN}All dependencies present. Ready to build.${NC}\n"
    echo ""
    echo "Next: cd src/agentsight && cargo build --release"
    exit 0
fi
