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
# Set when --config was asked for but the document could not be written. The
# tokenless step still runs, then the script exits non-zero: callers only see
# the exit code, and "configured" must not be reported for a failed write.
CONFIG_WRITE_FAILED=false
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

  # The four keys go into the document the user already has, not over it: this
  # file carries their permissions, hooks and status line, and "Also write ..."
  # in --help means a merge. Without a JSON parser there is nothing that can
  # merge, so a host that already has a file keeps it untouched; a fresh host
  # has nothing to preserve.
  if ! command_exists python3; then
    if [[ -f "$settings_file" ]]; then
      warn "python3 not found — leaving $settings_file untouched."
      warn "Install python3, or merge the four env keys by hand, then re-run --config."
      return 0
    fi
    # Nothing to merge into: escape the key so the document stays parseable.
    # A control character has no hand-written JSON escape here, and writing a
    # document Claude Code rejects is the defect this script exists to fix.
    if [[ "$api_key" == *[[:cntrl:]]* ]]; then
      warn "CLAUDE_API_KEY contains control characters — leaving $settings_file untouched."
      warn "Install python3, or set the key by hand, then re-run --config."
      return 0
    fi
    local escaped_key
    escaped_key="$(printf '%s' "$api_key" | sed 's/[\"]/\\&/g')"
    cat > "$settings_file" <<JSONEOF
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "${escaped_key}",
    "ANTHROPIC_MODEL": "qwen3-coder-plus",
    "ANTHROPIC_SMALL_FAST_MODEL": "qwen3-coder-plus"
  }
}
JSONEOF
    ok "Configuration written to $settings_file"
    return 0
  fi

  # The helper copies the original just before it replaces the file, so a
  # document it refuses to merge never leaves a pointless .bak behind.
  local backup=""
  if [[ -f "$settings_file" ]]; then
    backup="$settings_file.bak.$(date +%s)"
  fi

  local merge_rc=0
  CLAUDE_SETTINGS_API_KEY="$api_key" python3 - "$settings_file" "$backup" <<'PYEOF' || merge_rc=$?
import json
import os
import shutil
import stat
import sys

path = sys.argv[1]
backup = sys.argv[2] if len(sys.argv) > 2 else ""
# The key travels in the environment, not in argv: /proc/<pid>/cmdline is
# world-readable, while /proc/<pid>/environ is restricted to the same user.
api_key = os.environ["CLAUDE_SETTINGS_API_KEY"]


def merge():
    try:
        with open(path, "r", encoding="utf-8") as handle:
            document = json.load(handle)
    except FileNotFoundError:
        document = {}
    except (OSError, ValueError) as error:
        # A document we cannot read is not ours to drop, and overwriting it is what
        # this fix exists to stop: report and let the caller keep it.
        print(f"cannot read {path}: {error}", file=sys.stderr)
        sys.exit(3)
    if not isinstance(document, dict):
        print(f"{path} is not a JSON object", file=sys.stderr)
        sys.exit(3)
    env = document.get("env")
    if not isinstance(env, dict):
        env = {}
        document["env"] = env
    env["ANTHROPIC_BASE_URL"] = "https://dashscope.aliyuncs.com/apps/anthropic"
    env["ANTHROPIC_AUTH_TOKEN"] = api_key
    env["ANTHROPIC_MODEL"] = "qwen3-coder-plus"
    env["ANTHROPIC_SMALL_FAST_MODEL"] = "qwen3-coder-plus"

    # Same file, atomically: a half-written settings.json is a broken Claude Code
    # on the next start.
    tmp = f"{path}.tmp"
    try:
        # The token lives in this file, so keep the mode the user chose for it
        # (0600 is common); a fresh file starts private instead of umask-default.
        try:
            keep_mode = stat.S_IMODE(os.stat(path).st_mode)
        except FileNotFoundError:
            keep_mode = 0o600
        if backup:
            shutil.copy2(path, backup)
            print(f"Existing settings backed up to {backup}")
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump(document, handle, indent=2, ensure_ascii=False)
            handle.write("\n")
        os.chmod(tmp, keep_mode)
        os.replace(tmp, path)
    finally:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
    print(f"merged the DashScope env block into {path}")


try:
    merge()
except SystemExit:
    raise
except Exception as error:  # one actionable line beats a traceback in the terminal
    print(f"failed to update {path}: {error}", file=sys.stderr)
    sys.exit(1)
PYEOF

  case "$merge_rc" in
    0) ok "Configuration written to $settings_file" ;;
    3) warn "left $settings_file untouched — it is not a JSON document this script can merge into." ;;
    *) CONFIG_WRITE_FAILED=true; err "Failed to update $settings_file (python3 exited $merge_rc)"; return 0 ;;
  esac
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
  if [[ "$CONFIG_WRITE_FAILED" == true ]]; then
    err "Finished, but the API configuration was not written; see the error above."
    exit 1
  fi
  ok "Done! Run 'claude' to get started."
  echo ""
}

main "$@"
