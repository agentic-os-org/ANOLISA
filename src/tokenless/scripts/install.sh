#!/usr/bin/env bash
# Standalone installer for Tokenless CLI.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
#
# Environment variables:
#   TOKENLESS_VERSION   Version to install (default: latest npm release)
#   TOKENLESS_INSTALL_DIR  Binary install directory (default: ~/.local/bin)
#   TOKENLESS_FORCE_BUILD  Set to 1 to force source build even when npm binary exists

set -euo pipefail

REPO="alibaba/anolisa"
NPM_PACKAGE="anolisa-tokenless"
NPM_REGISTRY="https://registry.npmjs.org"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; }
die()  { err "$@"; exit 1; }

detect_platform() {
  local os arch
  case "$(uname -s)" in
    Linux)  os="linux" ;;
    Darwin) os="darwin" ;;
    *)      die "Unsupported OS: $(uname -s). Only Linux and macOS are supported." ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64)  arch="x64" ;;
    aarch64|arm64) arch="arm64" ;;
    *)             die "Unsupported architecture: $(uname -m)" ;;
  esac
  if [ "$os" = "linux" ] && ldd --version 2>&1 | grep -qi musl; then
    die "musl-based Linux distributions (e.g. Alpine) are not supported by prebuilt binaries. Build from source instead."
  fi
  PLATFORM_OS="$os"
  PLATFORM_ARCH="$arch"
  PLATFORM_KEY="${os}-${arch}"
}

resolve_version() {
  if [ -n "${TOKENLESS_VERSION:-}" ]; then
    VERSION="$TOKENLESS_VERSION"
    info "Using specified version: $VERSION"
    return
  fi
  local latest
  latest=$(curl -fsSL "${NPM_REGISTRY}/${NPM_PACKAGE}/latest" 2>/dev/null) || die "Failed to fetch latest version from npm registry"
  VERSION=$(printf '%s' "$latest" | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4)
  [ -n "$VERSION" ] || die "Could not determine latest version"
  info "Latest version: $VERSION"
}

try_npm_install() {
  if ! command -v npm &>/dev/null; then
    warn "npm not found, skipping npm install method"
    return 1
  fi
  info "Installing via npm (prebuilt binaries for ${PLATFORM_KEY})..."
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  mkdir -p "$install_dir"

  local npm_prefix
  npm_prefix=$(npm config get prefix 2>/dev/null || echo "${HOME}/.npm-global")

  npm install -g "${NPM_PACKAGE}@${VERSION}" --prefix "$npm_prefix" 2>&1 | tail -5

  local npm_bin
  npm_bin="${npm_prefix}/bin"
  if [ ! -f "${npm_bin}/tokenless" ]; then
    npm_bin="${npm_prefix}/lib/node_modules/${NPM_PACKAGE}/bin"
  fi
  if [ ! -f "${npm_bin}/tokenless" ]; then
    warn "npm install succeeded but binary not found at expected path"
    return 1
  fi

  for bin in tokenless rtk toon; do
    if [ -f "${npm_bin}/${bin}" ] || [ -L "${npm_bin}/${bin}" ]; then
      ln -sf "$(readlink -f "${npm_bin}/${bin}" 2>/dev/null || echo "${npm_bin}/${bin}")" "${install_dir}/${bin}"
      chmod +x "${install_dir}/${bin}" 2>/dev/null || true
    fi
  done

  info "Installed to ${install_dir}"
  return 0
}

try_source_build() {
  info "Building from source..."
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  mkdir -p "$install_dir"

  local tmpdir
  tmpdir=$(mktemp -d)
  trap 'rm -rf "$tmpdir"' EXIT

  info "Downloading source tarball..."
  curl -fsSL "https://github.com/${REPO}/archive/refs/tags/tokenless/v${VERSION}.tar.gz" \
    -o "${tmpdir}/tokenless-${VERSION}.tar.gz" || \
  curl -fsSL "https://github.com/${REPO}/archive/refs/heads/main.tar.gz" \
    -o "${tmpdir}/tokenless-${VERSION}.tar.gz" || \
  die "Failed to download source tarball"

  info "Extracting..."
  tar -xzf "${tmpdir}/tokenless-${VERSION}.tar.gz" -C "$tmpdir"

  local src_dir
  src_dir=$(find "$tmpdir" -maxdepth 2 -name 'Cargo.toml' -path '*/tokenless/*' -exec dirname {} \; | head -1)
  [ -n "$src_dir" ] || die "Could not find tokenless source in tarball"

  if ! command -v cargo &>/dev/null; then
    die "Rust toolchain (cargo) is required for source build. Install via https://rustup.rs"
  fi

  info "Building (this may take a few minutes)..."
  (cd "$src_dir" && cargo build --release --locked -p tokenless-cli 2>&1) || die "Build failed"

  install -p -m 0755 "${src_dir}/target/release/tokenless" "${install_dir}/tokenless"
  info "Installed tokenless to ${install_dir}/tokenless"
  warn "Source build only installs tokenless CLI. For rtk and toon, install via anolisa CLI or npm."
  return 0
}

ensure_path() {
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  case ":${PATH}:" in
    *":${install_dir}:"*) return 0 ;;
  esac
  info "Adding ${install_dir} to PATH"
  local rc_file
  if [ -n "${ZSH_VERSION:-}" ] || [ "$(basename "${SHELL:-/bin/bash}")" = "zsh" ]; then
    rc_file="${HOME}/.zshrc"
  else
    rc_file="${HOME}/.bashrc"
  fi
  printf '\n# Added by tokenless installer\nexport PATH="%s:$PATH"\n' "$install_dir" >> "$rc_file"
  export PATH="${install_dir}:${PATH}"
  info "Added to ${rc_file}. Run 'source ${rc_file}' or open a new shell to use tokenless."
}

main() {
  info "Tokenless Installer"
  detect_platform
  resolve_version

  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  info "Platform: ${PLATFORM_KEY}"
  info "Install directory: ${install_dir}"

  if [ "${TOKENLESS_FORCE_BUILD:-0}" = "1" ]; then
    try_source_build || die "Source build failed"
  else
    if try_npm_install; then
      :
    elif try_source_build; then
      :
    else
      die "All installation methods failed"
    fi
  fi

  ensure_path

  if command -v tokenless &>/dev/null || [ -x "${install_dir}/tokenless" ]; then
    local ver
    ver=$("${install_dir}/tokenless" --version 2>/dev/null || echo "unknown")
    info "Tokenless installed successfully: ${ver}"
  else
    warn "Installation completed but tokenless binary not found in PATH"
    warn "Try: export PATH=\"${install_dir}:\$PATH\""
  fi
}

main "$@"
