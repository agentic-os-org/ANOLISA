#!/usr/bin/env bash
# Uninstall a Tokenless CLI installation created by scripts/install.sh.
#
# The installer records every path it created in a receipt file. This script
# removes exactly those recorded paths and nothing else, so it stays symmetric
# with the install and never deletes binaries, adapters, or data that belong to
# another installation method (anolisa CLI, a manual `npm install -g`, or a
# custom TOKENLESS_INSTALL_DIR you manage yourself).
#
# Usage:
#   bash scripts/uninstall.sh [--dry-run] [--purge] [--receipt <path>]
#
# Options:
#   --dry-run        Print what would be removed, change nothing.
#   --purge          Also delete the runtime data directory (~/.tokenless,
#                    which holds stats.db and stash.db). Off by default so an
#                    uninstall does not destroy collected statistics.
#   --receipt PATH   Read a non-default receipt (mirrors TOKENLESS_RECEIPT).
#
# Environment variables:
#   TOKENLESS_RECEIPT  Receipt path (default:
#                      ${XDG_DATA_HOME:-$HOME/.local/share}/tokenless/install-receipt)
#   TOKENLESS_DATA_DIR Runtime data directory (default: ~/.tokenless)

set -euo pipefail

NPM_PACKAGE="anolisa-tokenless"
DEFAULT_DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
RECEIPT="${TOKENLESS_RECEIPT:-${DEFAULT_DATA_DIR}/tokenless/install-receipt}"
RUNTIME_DATA_DIR="${TOKENLESS_DATA_DIR:-${HOME}/.tokenless}"
MARKER="# Added by tokenless installer"

DRY_RUN=0
PURGE=0

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; }
die()  { err "$@"; exit 1; }

usage() {
  printf '%s\n' \
    "Usage: bash scripts/uninstall.sh [--dry-run] [--purge] [--receipt <path>]" \
    "" \
    "  --dry-run        Print what would be removed, change nothing." \
    "  --purge          Also delete the runtime data directory (~/.tokenless)." \
    "  --receipt PATH   Read a non-default receipt (mirrors TOKENLESS_RECEIPT)." \
    "  -h, --help       Show this help."
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)  DRY_RUN=1; shift ;;
    --purge)    PURGE=1; shift ;;
    --receipt)  [ "$#" -ge 2 ] || die "--receipt requires a path"; RECEIPT="$2"; shift 2 ;;
    -h|--help)  usage; exit 0 ;;
    *)          usage >&2; die "Unknown option: $1" ;;
  esac
done

run() {
  if [ "$DRY_RUN" = "1" ]; then
    info "[dry-run] $*"
  else
    "$@"
  fi
}

METHOD=""
TL_VERSION=""
INSTALL_DIR=""
NPM_PREFIX=""
ADAPTERS_DIR=""
PATH_RC=""
FILES=()

if [ ! -f "$RECEIPT" ]; then
  err "No install receipt found at ${RECEIPT}"
  err "This script only removes what scripts/install.sh recorded, so it will not guess."
  err "Uninstall manually instead, matching how you installed Tokenless:"
  err "  anolisa CLI : anolisa uninstall tokenless"
  err "  npm         : npm uninstall -g ${NPM_PACKAGE}"
  err "  source build: rm -f <install-dir>/tokenless"
  exit 1
fi

while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    ''|'#'*) continue ;;
  esac
  key="${line%%=*}"
  value="${line#*=}"
  case "$key" in
    method)        METHOD="$value" ;;
    version)       TL_VERSION="$value" ;;
    install_dir)   INSTALL_DIR="$value" ;;
    npm_prefix)    NPM_PREFIX="$value" ;;
    adapters_dir)  ADAPTERS_DIR="$value" ;;
    path_rc_file)  PATH_RC="$value" ;;
    file)          FILES+=("$value") ;;
  esac
done < "$RECEIPT"

info "Tokenless uninstaller"
info "Receipt      : ${RECEIPT}"
info "Method       : ${METHOD:-unknown}"
info "Version      : ${TL_VERSION:-unknown}"
info "Install dir  : ${INSTALL_DIR:-unknown}"

# 1. Recorded binaries in the install directory. Nothing else in that directory
#    is touched, so a foreign `rtk`/`toon` or an anolisa-managed CLI survives.
if [ "${#FILES[@]}" -gt 0 ]; then
  for f in "${FILES[@]}"; do
    if [ ! -e "$f" ] && [ ! -L "$f" ]; then
      warn "Already gone, skipping: ${f}"
      continue
    fi
    if [ -d "$f" ] && [ ! -L "$f" ]; then
      warn "Refusing to remove directory not owned by this installer: ${f}"
      continue
    fi
    if [ "$DRY_RUN" = "1" ]; then
      info "[dry-run] would remove ${f}"
    else
      rm -f "$f"
      info "Removed ${f}"
    fi
  done
else
  warn "Receipt lists no installed files"
fi

# 2. npm global package — only for the npm method, only from the recorded prefix.
if [ "$METHOD" = "npm" ] && [ -n "$NPM_PREFIX" ]; then
  if command -v npm &>/dev/null; then
    run npm uninstall -g "$NPM_PACKAGE" --prefix "$NPM_PREFIX"
    info "Uninstalled npm package ${NPM_PACKAGE} from prefix ${NPM_PREFIX}"
  else
    warn "npm not found; remove the global package yourself: npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
  fi
fi

# 3. Adapter resources — recorded only when the npm postinstall placed them.
#    A source build installs no adapters, so this step is skipped for it and an
#    adapter tree owned by the anolisa CLI is left alone.
if [ -n "$ADAPTERS_DIR" ]; then
  if [ -d "$ADAPTERS_DIR" ]; then
    run rm -rf "$ADAPTERS_DIR"
    info "Removed adapter resources ${ADAPTERS_DIR}"
  else
    warn "Already gone, skipping: ${ADAPTERS_DIR}"
  fi
fi

# 4. PATH entry appended by the installer, only in the recorded rc file and only
#    when it references the recorded install directory.
if [ -n "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -n "$INSTALL_DIR" ]; then
  if grep -Fq "$MARKER" "$PATH_RC"; then
    if [ "$DRY_RUN" = "1" ]; then
      info "[dry-run] would strip the tokenless PATH entry from ${PATH_RC}"
    else
      tmp_rc="${PATH_RC}.tokenless-uninstall.$$"
      awk -v marker="$MARKER" -v dir="$INSTALL_DIR" '
        BEGIN { skip = 0 }
        {
          if (skip == 1) {
            skip = 0
            if ($0 ~ /^export PATH=/ && index($0, dir) > 0) next
            print marker
          }
          if ($0 == marker) { skip = 1; next }
          print
        }
      ' "$PATH_RC" > "$tmp_rc" && cat "$tmp_rc" > "$PATH_RC" && rm -f "$tmp_rc"
      info "Removed the tokenless PATH entry from ${PATH_RC}"
    fi
  fi
fi

# 5. Runtime data (stats.db / stash.db) is user data, not an installed file.
if [ "$PURGE" = "1" ]; then
  if [ -d "$RUNTIME_DATA_DIR" ]; then
    run rm -rf "$RUNTIME_DATA_DIR"
    info "Purged runtime data ${RUNTIME_DATA_DIR}"
  fi
else
  if [ -d "$RUNTIME_DATA_DIR" ]; then
    info "Kept runtime data ${RUNTIME_DATA_DIR} (re-run with --purge to delete stats/stash)"
  fi
fi

# 6. The receipt itself.
if [ "$DRY_RUN" = "1" ]; then
  info "[dry-run] would remove receipt ${RECEIPT}"
  info "[dry-run] nothing was changed"
  exit 0
fi
rm -f "$RECEIPT"
info "Removed receipt ${RECEIPT}"
receipt_parent=$(dirname "$RECEIPT")
if [ -d "$receipt_parent" ] && [ -z "$(ls -A "$receipt_parent" 2>/dev/null)" ]; then
  rmdir "$receipt_parent"
fi

info "Tokenless uninstall complete"
