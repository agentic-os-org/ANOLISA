#!/usr/bin/env bash
# Standalone installer for Tokenless CLI.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
#
# Environment variables:
#   TOKENLESS_VERSION      Version to install (default: latest npm release).
#                          Treated as a hard pin: the source build only accepts
#                          the matching `tokenless/v<VERSION>` tag and never
#                          falls back to `main`, so a bad pin fails loudly
#                          instead of installing unselected trunk code.
#   TOKENLESS_INSTALL_DIR  Binary install directory (default: ~/.local/bin)
#   TOKENLESS_FORCE_BUILD  Set to 1 to force source build even when npm binary exists
#   TOKENLESS_RECEIPT      Install receipt path (default:
#                          ${XDG_DATA_HOME:-$HOME/.local/share}/tokenless/install-receipt).
#                          Records what this run created; consumed by
#                          scripts/uninstall.sh.
#
# Installation methods and what each one produces:
#   npm     prebuilt `tokenless` + `rtk` binaries and the bundled Agent adapters
#   source  the `tokenless` CLI only — no `rtk`, no adapters (CLI-only install)
# See docs/user-guide/{en,zh}/token-saving/tokenless/QUICKSTART.md for the
# adapter-enable path that matches each method.

set -euo pipefail

REPO="alibaba/anolisa"
NPM_PACKAGE="anolisa-tokenless"
NPM_REGISTRY="https://registry.npmjs.org"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
DEFAULT_DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
RECEIPT_FILE="${TOKENLESS_RECEIPT:-${DEFAULT_DATA_DIR}/tokenless/install-receipt}"

# Shared state consumed by write_receipt(). Populated by the install helpers.
INSTALL_METHOD=""
INSTALLED_FILES=()
NPM_PREFIX_USED=""
ADAPTERS_DIR_USED=""
VERSION_PINNED=0
SRC_TMPDIR=""

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; }
die()  { err "$@"; exit 1; }

# SRC_TMPDIR is a global, not a `local` inside try_source_build(), so the EXIT
# trap still sees a bound value after that function has returned. Under
# `set -u` a trap referencing an out-of-scope local aborts with
# "tmpdir: unbound variable" and leaks the temporary source tree.
cleanup_src_tmpdir() {
  if [ -n "$SRC_TMPDIR" ] && [ -d "$SRC_TMPDIR" ]; then
    rm -rf "$SRC_TMPDIR"
  fi
  SRC_TMPDIR=""
}

trap cleanup_src_tmpdir EXIT

detect_platform() {
  local os arch
  case "$(uname -s)" in
    Linux)  os="linux" ;;
    Darwin) os="darwin" ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
      die "Windows is not supported. Run this installer inside WSL2 with a supported Linux distribution." ;;
    *)      die "Unsupported OS: $(uname -s). Only Linux and macOS are supported." ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64)  arch="x64" ;;
    aarch64|arm64) arch="arm64" ;;
    *)             die "Unsupported architecture: $(uname -m)" ;;
  esac
  MUSL_LINUX=0
  if [ "$os" = "linux" ] && ldd --version 2>&1 | grep -qi musl; then
    warn "musl-based Linux detected (e.g. Alpine). Prebuilt binaries are not available; source build will be used."
    MUSL_LINUX=1
  fi
  PLATFORM_OS="$os"
  PLATFORM_ARCH="$arch"
  PLATFORM_KEY="${os}-${arch}"
}

resolve_version() {
  if [ -n "${TOKENLESS_VERSION:-}" ]; then
    VERSION="$TOKENLESS_VERSION"
    VERSION_PINNED=1
    info "Using specified version: $VERSION"
    return
  fi
  local latest
  latest=$(curl -fsSL "${NPM_REGISTRY}/${NPM_PACKAGE}/latest" 2>/dev/null) || die "Failed to fetch latest version from npm registry"
  VERSION=$(printf '%s' "$latest" | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4)
  [ -n "$VERSION" ] || die "Could not determine latest version"
  info "Latest version: $VERSION"
}

# Records exactly the paths this run created, so scripts/uninstall.sh can be
# symmetric with the install and never delete files owned by another method
# (anolisa CLI, a manual npm install, or a custom TOKENLESS_INSTALL_DIR).
write_receipt() {
  local method="$1"
  local receipt_dir entry
  receipt_dir=$(dirname "$RECEIPT_FILE")
  if ! mkdir -p "$receipt_dir" 2>/dev/null; then
    warn "Could not create ${receipt_dir}; install receipt not written."
    warn "Re-run with TOKENLESS_RECEIPT=<writable path> if you want scripted uninstall."
    return 0
  fi
  if ! {
    printf '# Tokenless installer receipt (schema 1).\n'
    printf '# Written by scripts/install.sh, consumed by scripts/uninstall.sh.\n'
    printf '# Only the paths listed below belong to this installation.\n'
    printf 'schema=1\n'
    printf 'method=%s\n' "$method"
    printf 'version=%s\n' "$VERSION"
    printf 'install_dir=%s\n' "${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
    if [ -n "$NPM_PREFIX_USED" ]; then
      printf 'npm_prefix=%s\n' "$NPM_PREFIX_USED"
    fi
    if [ -n "$ADAPTERS_DIR_USED" ]; then
      printf 'adapters_dir=%s\n' "$ADAPTERS_DIR_USED"
    fi
    printf 'installed_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    if [ "${#INSTALLED_FILES[@]}" -gt 0 ]; then
      for entry in "${INSTALLED_FILES[@]}"; do
        printf 'file=%s\n' "$entry"
      done
    fi
  } > "$RECEIPT_FILE"; then
    warn "Could not write install receipt to ${RECEIPT_FILE}"
    return 0
  fi
  chmod 0644 "$RECEIPT_FILE" 2>/dev/null || true
  INSTALL_METHOD="$method"
  info "Recorded install receipt: ${RECEIPT_FILE}"
}

record_path_rc() {
  [ -f "$RECEIPT_FILE" ] || return 0
  printf 'path_rc_file=%s\n' "$1" >> "$RECEIPT_FILE" 2>/dev/null || true
}

try_npm_install() {
  if [ "${MUSL_LINUX:-0}" = "1" ]; then
    warn "Skipping npm install on musl Linux (prebuilt binaries not available)"
    return 1
  fi
  if ! command -v npm &>/dev/null; then
    warn "npm not found, skipping npm install method"
    return 1
  fi
  info "Installing via npm (prebuilt binaries for ${PLATFORM_KEY})..."
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  mkdir -p "$install_dir"

  local npm_prefix
  npm_prefix=$(npm config get prefix 2>/dev/null || echo "${HOME}/.npm-global")

  if ! npm install -g "${NPM_PACKAGE}@${VERSION}" --prefix "$npm_prefix" 2>&1 | tail -5; then
    warn "npm install failed (possible EACCES or network issue)"
    warn "To fix npm permissions: mkdir -p ~/.npm-global && npm config set prefix '~/.npm-global'"
    return 1
  fi

  local npm_bin
  npm_bin="${npm_prefix}/bin"
  if [ ! -f "${npm_bin}/tokenless" ]; then
    npm_bin="${npm_prefix}/lib/node_modules/${NPM_PACKAGE}/bin"
  fi
  if [ ! -f "${npm_bin}/tokenless" ]; then
    warn "npm install succeeded but binary not found at expected path"
    return 1
  fi

  # `toon` is no longer a standalone binary (see the tokenless-cli crate), so
  # only the binaries the npm package actually ships are linked and recorded.
  INSTALLED_FILES=()
  local bin
  for bin in tokenless rtk; do
    if [ -f "${npm_bin}/${bin}" ] || [ -L "${npm_bin}/${bin}" ]; then
      ln -sf "$(readlink -f "${npm_bin}/${bin}" 2>/dev/null || echo "${npm_bin}/${bin}")" "${install_dir}/${bin}"
      chmod +x "${install_dir}/${bin}" 2>/dev/null || true
      INSTALLED_FILES+=("${install_dir}/${bin}")
    fi
  done

  if [ "${#INSTALLED_FILES[@]}" -eq 0 ]; then
    warn "npm install succeeded but no binaries were linked into ${install_dir}"
    return 1
  fi

  NPM_PREFIX_USED="$npm_prefix"
  # The package postinstall copies the bundled adapters here and replaces
  # whatever was in that directory, so this npm run owns it.
  local adapters_dir="${HOME}/.local/share/anolisa/adapters/tokenless"
  if [ -d "$adapters_dir" ]; then
    ADAPTERS_DIR_USED="$adapters_dir"
  fi
  write_receipt npm

  info "Installed to ${install_dir}"
  case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) warn "${install_dir} is not in PATH. Run: export PATH=\"${install_dir}:\$PATH\"" ;;
  esac
  return 0
}

try_source_build() {
  info "Building from source..."
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  mkdir -p "$install_dir"

  if ! command -v cargo &>/dev/null; then
    die "Rust toolchain (cargo) is required for source build. Install via https://rustup.rs"
  fi

  SRC_TMPDIR=$(mktemp -d "${TMPDIR:-/tmp}/tokenless-src.XXXXXX")

  # A pinned version must come from its own tag. Falling back to `main` would
  # silently install unselected trunk code under the requested version, so a
  # missing tag is a hard error instead.
  local tag="tokenless/v${VERSION}"
  local tag_url="https://github.com/${REPO}/archive/refs/tags/${tag}.tar.gz"
  local tarball="${SRC_TMPDIR}/tokenless-${VERSION}.tar.gz"

  info "Downloading source tarball for tag ${tag}..."
  local curl_status=0
  curl -fsSL "$tag_url" -o "$tarball" || curl_status=$?
  if [ "$curl_status" -ne 0 ]; then
    rm -f "$tarball"
    err "Failed to download ${tag_url} (curl exit ${curl_status})"
    if [ "$curl_status" -eq 22 ]; then
      err "The tag ${tag} does not exist on ${REPO}."
      err "List published tags with:"
      err "  git ls-remote --tags https://github.com/${REPO} 'refs/tags/tokenless/*'"
      if [ "$VERSION_PINNED" = "1" ]; then
        err "TOKENLESS_VERSION=${VERSION} has no matching tag; unset it to install the latest npm release."
      fi
    else
      err "This is a transport failure (network, proxy or TLS), not a missing tag. Retry once the connection is healthy."
    fi
    err "This installer never falls back to the 'main' branch, so a version pin cannot silently install trunk code."
    exit 1
  fi

  info "Extracting..."
  tar -xzf "$tarball" -C "$SRC_TMPDIR"

  # GitHub archives unpack as <archive-root>/src/tokenless/Cargo.toml, which is
  # four levels below the temporary directory. Match the component manifest
  # exactly so neither the archive root nor a nested crate is picked up.
  local src_dir
  src_dir=$(find "$SRC_TMPDIR" -maxdepth 5 -type f -name Cargo.toml \
              -path '*/tokenless/Cargo.toml' -exec dirname {} \; 2>/dev/null | sort | head -1)
  [ -n "$src_dir" ] || die "Could not find tokenless source (src/tokenless/Cargo.toml) in tarball"

  info "Building (this may take a few minutes)..."
  (cd "$src_dir" && cargo build --release --locked -p tokenless-cli 2>&1) || die "Build failed"

  install -p -m 0755 "${src_dir}/target/release/tokenless" "${install_dir}/tokenless"
  INSTALLED_FILES=("${install_dir}/tokenless")
  NPM_PREFIX_USED=""
  ADAPTERS_DIR_USED=""
  write_receipt source

  info "Installed tokenless to ${install_dir}/tokenless"
  warn "Source build installs the tokenless CLI only: no rtk and no Agent adapters."
  warn "Adapter enablement therefore does not apply to this install method — see the"
  warn "'Install Tokenless' table in the Quick Start for the path that matches it."

  cleanup_src_tmpdir
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
  record_path_rc "$rc_file"
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
    info "Install method: ${INSTALL_METHOD:-unknown} (receipt: ${RECEIPT_FILE})"
    info "To remove exactly what this installer created, run:"
    info "  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/src/tokenless/scripts/uninstall.sh | bash"
  else
    warn "Installation completed but tokenless binary not found in PATH"
    warn "Try: export PATH=\"${install_dir}:\$PATH\""
  fi
}

main "$@"
