#!/usr/bin/env bash
# =============================================================================
# install-claude-code.sh
# Automated Claude Code installer for Alibaba Cloud Linux 4 (Alinux 4)
#
# Usage:
#   bash install-claude-code.sh [--config] [--skip-tokenless]
#
# Options:
#   --config    Also write DashScope/Qwen configuration to ~/.claude/settings.json
#               (will prompt for API key interactively, or use CLAUDE_API_KEY env var)
#   --skip-tokenless  Skip tokenless plugin auto-installation
#
# Installation priority:
#   1. Native installer (curl from claude.ai)
#   2. npm global install (if Node.js 18+ exists)
#   3. nvm + npm (install Node.js via nvm first, then npm install)
#
# Target OS: Alinux 4 (based on RHEL 9 / glibc)
# =============================================================================

set -euo pipefail

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()   { echo -e "${RED}[ERROR]${NC} $*"; }

# --- Flags ---
WRITE_CONFIG=false
SKIP_TOKENLESS=false
for arg in "$@"; do
  case "$arg" in
    --config) WRITE_CONFIG=true ;;
    --skip-tokenless) SKIP_TOKENLESS=true ;;
    *) warn "Unknown argument: $arg" ;;
  esac
done

# --- Helpers ---
command_exists() { command -v "$1" &>/dev/null; }

node_version_ok() {
  if ! command_exists node; then return 1; fi
  local ver
  ver=$(node --version 2>/dev/null | sed 's/v//')
  local major
  major=$(echo "$ver" | cut -d. -f1)
  [[ "$major" -ge 18 ]]
}

ensure_path() {
  local dir="$1"
  if [[ ":$PATH:" != *":$dir:"* ]]; then
    export PATH="$dir:$PATH"
    local rc="$HOME/.bashrc"
    if ! grep -qF "$dir" "$rc" 2>/dev/null; then
      echo "export PATH=\"$dir:\$PATH\"" >> "$rc"
      info "Added $dir to PATH in $rc"
    fi
  fi
}

# =============================================================================
# Alinux 4 System Check & Prerequisites
# =============================================================================
check_alinux() {
  info "Checking system environment..."

  if [[ ! -f /etc/os-release ]]; then
    warn "/etc/os-release not found — cannot verify OS."
    return 0
  fi

  local os_id os_version
  os_id=$(. /etc/os-release && echo "${ID:-unknown}")
  os_version=$(. /etc/os-release && echo "${VERSION_ID:-0}" | cut -d. -f1)

  # Alinux 4 reports ID as "alinux" with VERSION_ID starting with "4"
  # Also accept "anolis" (AnolisOS) and generic "rhel"/"centos" 9 as compatible
  case "$os_id" in
    alinux)
      if [[ "$os_version" -lt 4 ]]; then
        warn "Detected Alinux $os_version — this script targets Alinux 4+."
      else
        ok "Alinux $os_version detected."
      fi
      ;;
    anolis|centos|rhel|rocky|almalinux)
      info "Detected compatible RHEL-family OS: $os_id $os_version"
      ;;
    *)
      warn "Detected OS: $os_id $os_version — this script is designed for Alinux 4."
      warn "Continuing anyway, but some steps may not work."
      ;;
  esac
}

install_sys_deps() {
  info "Installing system prerequisites via dnf..."

  local pkgs_to_install=()

  command_exists curl  || pkgs_to_install+=(curl)
  command_exists git   || pkgs_to_install+=(git)
  command_exists tar   || pkgs_to_install+=(tar)
  command_exists gzip  || pkgs_to_install+=(gzip)

  # glibc and libstdc++ should already be present on Alinux 4, but ensure
  rpm -q glibc        &>/dev/null || pkgs_to_install+=(glibc)
  rpm -q libstdc++    &>/dev/null || pkgs_to_install+=(libstdc++)

  if [[ ${#pkgs_to_install[@]} -eq 0 ]]; then
    ok "All system prerequisites already installed."
    return 0
  fi

  info "Installing: ${pkgs_to_install[*]}"
  if command_exists sudo; then
    sudo dnf install -y "${pkgs_to_install[@]}"
  else
    dnf install -y "${pkgs_to_install[@]}"
  fi
  ok "System prerequisites installed."
}

# =============================================================================
# Method A: Native Installer
# =============================================================================
try_native_install() {
  info "Attempting native installer (recommended)..."

  if ! command_exists curl; then
    warn "curl not found — skipping native installer."
    return 1
  fi

  if curl -fsSL https://claude.ai/install.sh | bash; then
    ensure_path "$HOME/.local/bin"
    if command_exists claude; then
      ok "Claude Code installed via native installer."
      return 0
    fi
    if [[ -x "$HOME/.local/bin/claude" ]]; then
      ok "Claude Code installed via native installer (at ~/.local/bin/claude)."
      return 0
    fi
  fi

  warn "Native installer failed."
  return 1
}

# =============================================================================
# Method B: npm Global Install
# =============================================================================
try_npm_install() {
  info "Attempting npm global install..."

  if ! node_version_ok; then
    warn "Node.js 18+ not found — skipping npm install."
    return 1
  fi

  if ! command_exists npm; then
    warn "npm not found — skipping npm install."
    return 1
  fi

  # Fix prefix if needed to avoid EACCES
  local npm_prefix
  npm_prefix=$(npm config get prefix 2>/dev/null)
  if [[ "$npm_prefix" == "/usr" || "$npm_prefix" == "/usr/local" ]]; then
    info "Fixing npm global prefix to avoid permission issues..."
    mkdir -p "$HOME/.npm-global"
    npm config set prefix "$HOME/.npm-global"
    ensure_path "$HOME/.npm-global/bin"
  fi

  if npm install -g @anthropic-ai/claude-code; then
    ok "Claude Code installed via npm."
    return 0
  fi

  warn "npm install failed."
  return 1
}

# =============================================================================
# Method C: nvm + npm
# =============================================================================
try_nvm_install() {
  info "Attempting nvm + npm install..."

  if ! command_exists curl && ! command_exists wget; then
    err "Neither curl nor wget found — cannot install nvm."
    return 1
  fi

  # Install nvm
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
  if [[ ! -d "$NVM_DIR" ]]; then
    info "Installing nvm..."
    curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
  fi

  # Load nvm
  # shellcheck disable=SC1091
  [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"

  if ! command_exists nvm; then
    warn "nvm installation did not load correctly."
    return 1
  fi

  info "Installing Node.js LTS via nvm..."
  nvm install --lts
  nvm use --lts

  if npm install -g @anthropic-ai/claude-code; then
    ok "Claude Code installed via nvm + npm."
    return 0
  fi

  warn "nvm + npm install failed."
  return 1
}

# =============================================================================
# Configuration
# =============================================================================
write_config() {
  local api_key="${CLAUDE_API_KEY:-}"

  if [[ -z "$api_key" ]]; then
    echo ""
    read -rp "Enter your DashScope API Key: " api_key
  fi

  if [[ -z "$api_key" ]]; then
    warn "No API key provided — skipping configuration."
    return 1
  fi

  mkdir -p "$HOME/.claude"
  local settings_file="$HOME/.claude/settings.json"

  if [[ -f "$settings_file" ]]; then
    local backup="$settings_file.bak.$(date +%s)"
    cp "$settings_file" "$backup"
    info "Existing settings backed up to $backup"
  fi

  cat > "$settings_file" <<JSONEOF
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "${api_key}",
    "ANTHROPIC_MODEL": "qwen3-coder-plus",
    "ANTHROPIC_SMALL_FAST_MODEL": "qwen3-coder-plus"
  }
}
JSONEOF

  ok "Configuration written to $settings_file"
}

# =============================================================================
# Tokenless Plugin Installation
# =============================================================================
find_tokenless_adapter_dir() {
  local candidates=(
    "${ANOLISA_TOKENLESS_ADAPTER_DIR:-}"
    "/usr/share/anolisa/adapters/tokenless"
    "$HOME/.local/share/anolisa/adapters/tokenless"
  )
  # Source tree layout relative to this script
  local script_dir
  script_dir="$(cd "$(dirname "$0")" && pwd)"
  candidates+=("$script_dir/../../../../tokenless/adapters/tokenless")

  for candidate in "${candidates[@]}"; do
    [ -n "$candidate" ] || continue
    [ -d "$candidate" ] && [ -f "$candidate/manifest.json" ] && echo "$candidate" && return 0
  done
  return 1
}

install_tokenless_plugin() {
  if [[ "$SKIP_TOKENLESS" == true ]]; then
    info "Skipping tokenless plugin (--skip-tokenless)"
    return 0
  fi

  info "Installing tokenless Claude Code plugin..."

  local adapter_dir
  adapter_dir="$(find_tokenless_adapter_dir)" || {
    warn "tokenless adapter not found — skipping tokenless plugin installation."
    info "Install tokenless first, then run: make -C src/tokenless claude-code-install"
    return 0
  }

  local install_script="$adapter_dir/claude-code/scripts/install.sh"
  if [[ ! -f "$install_script" ]]; then
    warn "tokenless install script not found: $install_script"
    return 0
  fi

  info "tokenless adapter: $adapter_dir"
  if ANOLISA_ADAPTER_DIR="$adapter_dir" \
     ANOLISA_COMPONENT="tokenless" \
     ANOLISA_TARGET="claude-code" \
     bash "$install_script"; then
    ok "tokenless Claude Code plugin installed"
  else
    warn "tokenless plugin installation failed (non-fatal)"
    info "You can install it later with:"
    info "  ANOLISA_ADAPTER_DIR=$adapter_dir bash $install_script"
  fi
}

# =============================================================================
# Main
# =============================================================================
main() {
  echo ""
  echo "============================================"
  echo "  Claude Code Installer for Alinux 4"
  echo "============================================"
  echo ""

  # Step 0: Check OS
  check_alinux

  # Step 1: Install system deps
  install_sys_deps

  # Step 2: Try each install method in order
  if try_native_install; then
    :
  elif try_npm_install; then
    :
  elif try_nvm_install; then
    :
  else
    err "All installation methods failed."
    err "Please check your network connection and try again."
    err "For manual installation, visit: https://code.claude.com/docs/en/setup"
    exit 1
  fi

  # Step 3: Verify
  echo ""
  info "Verifying installation..."
  if command_exists claude || [[ -x "$HOME/.local/bin/claude" ]]; then
    ok "claude found at: $(which claude 2>/dev/null || echo "$HOME/.local/bin/claude")"
    claude --version 2>/dev/null && ok "Version check passed." || true
  else
    warn "claude binary not found in PATH. You may need to restart your shell."
    warn "Try: source ~/.bashrc && claude --version"
  fi

  # Step 4: Config
  if [[ "$WRITE_CONFIG" == true ]]; then
    echo ""
    info "Writing API configuration..."
    write_config
  fi

  # Step 6: Install tokenless plugin
  install_tokenless_plugin

  echo ""
  ok "Done! Run 'claude' to get started."
  echo ""
}

main "$@"
