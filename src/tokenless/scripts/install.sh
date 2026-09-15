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
#   TOKENLESS_FORCE_BUILD  Set to 1 to force source build even when npm binary
#                          exists. Linux only: on macOS the source build is
#                          refused and the installer exits instead of running
#                          cargo, because the macOS binaries are cross-compiled
#                          on Linux by the release pipeline and the fallback is
#                          validated on Linux only.
#   TOKENLESS_RECEIPT      Install receipt path (default:
#                          ${XDG_DATA_HOME:-$HOME/.local/share}/tokenless/install-receipt).
#                          Records what this run created; consumed by
#                          scripts/uninstall.sh. Re-running the installer with a
#                          different method retires the previous receipt first,
#                          so artefacts owned by the old method (npm global
#                          package, `rtk` link, adapter tree) are removed
#                          instead of being orphaned.
#
# Installation methods and what each one produces:
#   npm     prebuilt `tokenless` + `rtk` binaries and the bundled Agent adapters
#   source  the `tokenless` CLI only — no `rtk`, no adapters (CLI-only install)
#
# The npm route is transactional: everything it replaces is snapshotted first,
# and a failure after `npm install -g` puts the global package, the launcher
# links and the shared adapter directory back before the source-build fallback
# runs. The shared adapter directory is only recorded in the receipt when this
# installer owns it; a tree an anolisa component install or a direct
# `npm install -g` put there is restored untouched and stays out of the receipt.
# See docs/user-guide/{en,zh}/token-saving/tokenless/QUICKSTART.md for the
# adapter-enable path that matches each method.

set -euo pipefail

REPO="alibaba/anolisa"
NPM_PACKAGE="anolisa-tokenless"
NPM_REGISTRY="https://registry.npmjs.org"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
DEFAULT_DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
RECEIPT_FILE="${TOKENLESS_RECEIPT:-${DEFAULT_DATA_DIR}/tokenless/install-receipt}"
RECEIPT_SCHEMA=2
PATH_RC_MARKER="# Added by tokenless installer"
# Identity anchor inside the npm-owned adapter tree: package-npm.js stamps this
# manifest with the release version, so its digest tells "the tree this run
# placed" from "a tree another installer replaced it with".
ADAPTERS_IDENTITY_FILE="manifest.json"

# Shared state consumed by write_receipt(). Populated by the install helpers.
INSTALL_METHOD=""
INSTALLED_FILES=()
NPM_PREFIX_USED=""
ADAPTERS_DIR_USED=""
ADAPTERS_DIR_DIGEST=""
VERSION_PINNED=0
SRC_TMPDIR=""
CARRIED_PATH_RC=""
RECEIPT_WRITTEN=0
# Rollback state for the npm attempt. That route writes three things before the
# run can tell whether it succeeded: the global package, the launcher links in
# the install directory and — through the package postinstall — the shared
# adapter directory. A failure after any of them has to put all three back, or
# the source-build fallback reports success while leaving npm artefacts behind
# that no receipt records.
ROLLBACK_DIR=""
NPM_ATTEMPT_STARTED=0
NPM_ATTEMPT_PREFIX=""
NPM_PKG_PRE_EXISTED=0
ADAPTERS_FOREIGN=0
# Retirement of the *previous* receipt's npm package runs before this run writes
# anything; see retire_previous_npm_package().
PREV_RECEIPT_LOADED=0
PREV_NPM_RETIRED=0

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
  # The rollback snapshot is only meaningful while the attempt it belongs to is
  # running, so no exit path may leave it behind.
  if [ -n "$ROLLBACK_DIR" ] && [ -d "$ROLLBACK_DIR" ]; then
    rm -rf "$ROLLBACK_DIR"
  fi
  ROLLBACK_DIR=""
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

# sha256 of a file's content, following symlinks. Prints nothing when the file
# is unreadable or no sha256 tool exists, which callers treat as "no identity".
file_digest() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    { sha256sum "$f" 2>/dev/null || true; } | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    { shasum -a 256 "$f" 2>/dev/null || true; } | cut -d' ' -f1
  fi
  return 0
}

# Confirms the CLI this run wrote really exists and runs. Every install path is
# gated on it, so a receipt is never recorded for a binary that is not there —
# `install(1)`/`ln` failures are invisible otherwise, because both install
# helpers are called as `||`/`elif` conditions where Bash disables errexit for
# the whole function body.
verify_cli() {
  local bin="$1"
  if [ ! -e "$bin" ] && [ ! -L "$bin" ]; then
    warn "Expected the tokenless CLI at ${bin}, but nothing was written there"
    return 1
  fi
  if [ ! -x "$bin" ]; then
    warn "${bin} exists but is not executable"
    return 1
  fi
  if ! "$bin" --version >/dev/null 2>&1; then
    warn "${bin} --version did not run; refusing to report a successful install"
    return 1
  fi
  return 0
}

in_new_files() {
  local needle="$1" entry
  [ "${#INSTALLED_FILES[@]}" -gt 0 ] || return 1
  for entry in "${INSTALLED_FILES[@]}"; do
    [ "$entry" = "$needle" ] && return 0
  done
  return 1
}

# Frameworks register the adapter tree by reference — plugin directories, hook
# entries and symlinks that point into it. Deleting the tree first leaves those
# registrations dangling against a path that no longer exists, so each bundled
# adapter's own uninstall.sh runs before its resources disappear. Failures are
# warnings, not errors: the resources are going away either way, and a framework
# CLI the user already removed cannot be deregistered.
deregister_framework_adapters() {
  local adapters_dir="$1" script framework output status
  [ -d "$adapters_dir" ] || return 0
  for script in "$adapters_dir"/*/scripts/uninstall.sh; do
    [ -f "$script" ] || continue
    framework=$(basename "$(dirname "$(dirname "$script")")")
    # Deregistration only. These are the adapters' full uninstall scripts, and
    # at least the Codex one also removes ${PREFIX}/bin/tokenless. This caller
    # has already decided what happens to that binary, so the sub-script must
    # not revisit the decision: TOKENLESS_DEREGISTER_ONLY=1 limits it to the
    # framework registration. stdin is closed as well, so an interactive prompt
    # can neither block the run nor vanish into the captured output.
    output=$(TOKENLESS_DEREGISTER_ONLY=1 bash "$script" </dev/null 2>&1) && status=0 || status=$?
    if [ "$status" -ne 0 ]; then
      warn "Could not deregister the ${framework} adapter (exit ${status}); remove its registration manually:"
      warn "  bash ${script}"
      printf '%s\n' "$output" | sed 's/^/    /' >&2 || true
    else
      info "Deregistered the ${framework} adapter"
    fi
  done
  return 0
}

# Parses a receipt into the OLD_* globals. Tolerates schema 1 receipts, which
# carry no digests, and a truncated file.
OLD_METHOD="" OLD_INSTALL_DIR="" OLD_NPM_PREFIX=""
OLD_ADAPTERS_DIR="" OLD_ADAPTERS_DIR_DIGEST="" OLD_PATH_RC=""
OLD_FILES=() OLD_DIGESTS=()
read_receipt() {
  local path="$1" line key value
  OLD_METHOD="" OLD_INSTALL_DIR="" OLD_NPM_PREFIX=""
  OLD_ADAPTERS_DIR="" OLD_ADAPTERS_DIR_DIGEST="" OLD_PATH_RC=""
  OLD_FILES=() OLD_DIGESTS=()
  [ -f "$path" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      ''|'#'*) continue ;;
    esac
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      method)              OLD_METHOD="$value" ;;
      install_dir)         OLD_INSTALL_DIR="$value" ;;
      npm_prefix)          OLD_NPM_PREFIX="$value" ;;
      adapters_dir)        OLD_ADAPTERS_DIR="$value" ;;
      adapters_dir_digest) OLD_ADAPTERS_DIR_DIGEST="$value" ;;
      path_rc_file)        OLD_PATH_RC="$value" ;;
      file)                OLD_FILES+=("$value") ;;
      file_digest)         OLD_DIGESTS+=("$value") ;;
    esac
  done < "$path"
  return 0
}

# The user-level adapter resource directory every tokenless hook dispatcher
# searches. It is shared with the anolisa CLI and with a direct
# `npm install -g`, which is exactly why this installer may not adopt it just
# because it exists.
shared_adapters_dir() {
  printf '%s\n' "${HOME}/.local/share/anolisa/adapters/tokenless"
}

# Reads the previous receipt once, so the callers that run before the install
# and the ones that run after it agree on what the previous run owned.
load_previous_receipt() {
  [ "$PREV_RECEIPT_LOADED" = "1" ] && return 0
  PREV_RECEIPT_LOADED=1
  [ -f "$RECEIPT_FILE" ] || return 0
  read_receipt "$RECEIPT_FILE" || return 0
  return 0
}

# True only when the shared adapter directory is the tree a previous run of this
# installer recorded, still carrying the identity it recorded. Anything else —
# an anolisa component install, a direct `npm install -g`, a manual copy —
# belongs to somebody else and has to survive this run unchanged.
adapters_owned_by_previous_receipt() {
  local dir="$1" current
  load_previous_receipt
  [ -n "$OLD_ADAPTERS_DIR" ] || return 1
  [ "$OLD_ADAPTERS_DIR" = "$dir" ] || return 1
  if [ -n "$OLD_ADAPTERS_DIR_DIGEST" ]; then
    current=$(file_digest "${dir}/${ADAPTERS_IDENTITY_FILE}")
    [ "$current" = "$OLD_ADAPTERS_DIR_DIGEST" ] || return 1
  fi
  return 0
}

# Removes the global ${NPM_PACKAGE} installation under <prefix>.
#
# `npm uninstall -g --prefix P` deletes P/bin/tokenless and P/bin/rtk together
# with the module directory. When P/bin is also the directory this run installs
# into, that removes the CLI the source-build fallback has just written there,
# so in that case the package is retired by hand instead: the module directory
# goes, and a bin entry only goes while it is still a link into it.
remove_npm_package() {
  local prefix="$1" protect_dir="${2:-}"
  local pkg_dir="${prefix}/lib/node_modules/${NPM_PACKAGE}" bin link resolved
  if [ -n "$prefix" ] && { [ -z "$protect_dir" ] || [ "$protect_dir" != "${prefix}/bin" ]; }; then
    if command -v npm >/dev/null 2>&1 \
       && npm uninstall -g "$NPM_PACKAGE" --prefix "$prefix" >/dev/null 2>&1; then
      return 0
    fi
    warn "npm could not remove ${NPM_PACKAGE} from ${prefix}; removing the package files directly."
  fi
  for bin in tokenless rtk; do
    link="${prefix}/bin/${bin}"
    [ -L "$link" ] || continue
    resolved=$(readlink -f "$link" 2>/dev/null || true)
    case "$resolved" in
      "${pkg_dir}"*) rm -f "$link" 2>/dev/null || warn "Could not remove ${link}; remove it manually." ;;
    esac
  done
  if [ -d "$pkg_dir" ]; then
    rm -rf "$pkg_dir" 2>/dev/null || warn "Could not remove ${pkg_dir}; remove it manually."
  fi
  return 0
}

# Removes the previous run's recorded launcher links that point into <prefix>,
# before that prefix is retired. Retiring the package takes its bin directory
# with it, and a link left dangling has no content left to digest — the
# ownership check in retire_previous_receipt would then read the recorded path
# as "another installation took this over" and keep the broken link.
drop_links_into() {
  local prefix="$1" i=0 path digest resolved
  [ "${#OLD_FILES[@]}" -gt 0 ] || return 0
  for path in "${OLD_FILES[@]}"; do
    digest="${OLD_DIGESTS[$i]:-}"
    i=$((i + 1))
    [ -L "$path" ] || continue
    resolved=$(readlink -f "$path" 2>/dev/null || true)
    case "$resolved" in
      "${prefix}"/*) ;;
      *) continue ;;
    esac
    if [ -n "$digest" ] && [ "$(file_digest "$path")" != "$digest" ]; then
      warn "Keeping ${path}: it no longer matches the previous receipt, so another installation owns it now."
      continue
    fi
    if rm -f "$path" 2>/dev/null; then
      info "Removed ${path} left behind by the previous ${OLD_METHOD:-unknown} install"
    else
      warn "Could not remove ${path} left behind by the previous install; remove it manually."
    fi
  done
  return 0
}

# Retires the npm global package of the previous run *before* this run writes
# anything. <prospective> is the prefix this run is about to install into, and
# empty when it will not take the npm route at all. An unchanged prefix is left
# to the npm install that follows, which replaces that package in place.
retire_previous_npm_package() {
  local prospective="${1:-}"
  load_previous_receipt
  [ "$OLD_METHOD" = "npm" ] || return 0
  [ -n "$OLD_NPM_PREFIX" ] || return 0
  if [ -n "$prospective" ] && [ "$prospective" = "$OLD_NPM_PREFIX" ]; then
    return 0
  fi
  drop_links_into "$OLD_NPM_PREFIX"
  info "Removing the npm package left behind by the previous install..."
  remove_npm_package "$OLD_NPM_PREFIX" "${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  PREV_NPM_RETIRED=1
  return 0
}

# Snapshots everything the npm route is about to replace: the launcher links in
# the install directory, the shared adapter directory, and whether the global
# package was already there.
begin_npm_attempt() {
  local prefix="$1"
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  local adapters_dir bin
  adapters_dir=$(shared_adapters_dir)

  NPM_ATTEMPT_PREFIX="$prefix"
  NPM_PKG_PRE_EXISTED=0
  ADAPTERS_FOREIGN=0

  if ! ROLLBACK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/tokenless-rollback.XXXXXX" 2>/dev/null); then
    ROLLBACK_DIR=""
    warn "Cannot create a rollback directory under ${TMPDIR:-/tmp}."
    warn "The npm route needs one to undo a partial install, so it is skipped."
    return 1
  fi

  if [ -d "${prefix}/lib/node_modules/${NPM_PACKAGE}" ]; then
    NPM_PKG_PRE_EXISTED=1
  fi
  if [ -d "$adapters_dir" ]; then
    if ! adapters_owned_by_previous_receipt "$adapters_dir"; then
      ADAPTERS_FOREIGN=1
    fi
    if ! cp -a "$adapters_dir" "${ROLLBACK_DIR}/adapters-tokenless" 2>/dev/null; then
      if [ "$ADAPTERS_FOREIGN" = "1" ]; then
        warn "Cannot snapshot ${adapters_dir}, which belongs to another installation."
        warn "The npm postinstall would replace that tree irreversibly, so the npm route"
        warn "is skipped. Remove the directory first, or install through the anolisa CLI."
        end_npm_attempt
        return 1
      fi
      warn "Could not snapshot ${adapters_dir}; continuing without an adapter rollback."
    fi
  else
    # Nothing there yet, so the postinstall is what creates it — and a rollback
    # has to take it away again.
    : > "${ROLLBACK_DIR}/adapters-absent"
  fi
  for bin in tokenless rtk; do
    if [ -e "${install_dir}/${bin}" ] || [ -L "${install_dir}/${bin}" ]; then
      cp -a "${install_dir}/${bin}" "${ROLLBACK_DIR}/link-${bin}" 2>/dev/null || true
    fi
  done
  NPM_ATTEMPT_STARTED=1
  return 0
}

# Drops the rollback snapshot, once the attempt either succeeded or was undone.
# Any other exit path leaves it to the EXIT trap.
end_npm_attempt() {
  NPM_ATTEMPT_STARTED=0
  NPM_ATTEMPT_PREFIX=""
  NPM_PKG_PRE_EXISTED=0
  ADAPTERS_FOREIGN=0
  if [ -n "$ROLLBACK_DIR" ] && [ -d "$ROLLBACK_DIR" ]; then
    rm -rf "$ROLLBACK_DIR" 2>/dev/null || true
  fi
  ROLLBACK_DIR=""
  return 0
}

# Undoes a failed npm attempt: the launcher links it wrote, the adapter
# directory its postinstall replaced, and the global package it installed. The
# run can then fall back to the source build with nothing unowned left behind.
rollback_npm_attempt() {
  [ "$NPM_ATTEMPT_STARTED" = "1" ] || return 0
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  local prefix="$NPM_ATTEMPT_PREFIX" adapters_dir bin
  adapters_dir=$(shared_adapters_dir)

  for bin in tokenless rtk; do
    if [ -e "${ROLLBACK_DIR}/link-${bin}" ] || [ -L "${ROLLBACK_DIR}/link-${bin}" ]; then
      rm -f "${install_dir}/${bin}" 2>/dev/null || true
      cp -a "${ROLLBACK_DIR}/link-${bin}" "${install_dir}/${bin}" 2>/dev/null \
        || warn "Could not restore ${install_dir}/${bin} after the failed npm attempt."
    elif [ -L "${install_dir}/${bin}" ]; then
      # Written by this attempt, and nothing was there before it.
      case "$(readlink -f "${install_dir}/${bin}" 2>/dev/null || true)" in
        "${prefix}"/*) rm -f "${install_dir}/${bin}" 2>/dev/null || true ;;
      esac
    fi
  done
  INSTALLED_FILES=()

  if [ -d "${ROLLBACK_DIR}/adapters-tokenless" ]; then
    rm -rf "$adapters_dir" 2>/dev/null || true
    mkdir -p "$(dirname "$adapters_dir")" 2>/dev/null || true
    if cp -a "${ROLLBACK_DIR}/adapters-tokenless" "$adapters_dir" 2>/dev/null; then
      info "Restored ${adapters_dir} after the failed npm attempt"
    else
      warn "Could not restore ${adapters_dir}; the failed npm attempt replaced it."
    fi
  elif [ -f "${ROLLBACK_DIR}/adapters-absent" ] && [ -d "$adapters_dir" ]; then
    # The postinstall created this directory; without it the run would hand the
    # source-build fallback an adapter tree no receipt records.
    rm -rf "$adapters_dir" 2>/dev/null || true
    info "Removed ${adapters_dir} the failed npm attempt created"
  fi

  if [ "$NPM_PKG_PRE_EXISTED" != "1" ] && [ -n "$prefix" ] \
     && [ -d "${prefix}/lib/node_modules/${NPM_PACKAGE}" ]; then
    info "Removing the npm package the failed attempt installed..."
    remove_npm_package "$prefix" "$install_dir"
  fi

  end_npm_attempt
  return 0
}

# Re-running the installer overwrites the receipt, so the artefacts of the
# *previous* method would otherwise be orphaned: installing from source on top
# of an npm install leaves the `rtk` link, the npm global package and the adapter
# tree behind, and no later uninstall.sh run can see them any more. Retire them
# here — but only while their recorded identity still matches, because a path
# another installer has since taken over is no longer ours to delete.
retire_previous_receipt() {
  [ -f "$RECEIPT_FILE" ] || return 0
  load_previous_receipt

  local i=0 path digest current adapters_owned
  if [ "${#OLD_FILES[@]}" -gt 0 ]; then
    for path in "${OLD_FILES[@]}"; do
      digest="${OLD_DIGESTS[$i]:-}"
      i=$((i + 1))
      if in_new_files "$path"; then
        continue
      fi
      if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        continue
      fi
      if [ -n "$digest" ]; then
        current=$(file_digest "$path")
        if [ "$current" != "$digest" ]; then
          warn "Keeping ${path}: it no longer matches the previous receipt, so another installation owns it now."
          continue
        fi
      fi
      if rm -f "$path" 2>/dev/null; then
        info "Removed ${path} left behind by the previous ${OLD_METHOD:-unknown} install"
      else
        warn "Could not remove ${path} left behind by the previous install; remove it manually."
      fi
    done
  fi

  # The npm global package of a previous npm run. Normally already retired
  # before this run wrote anything (retire_previous_npm_package); this is the
  # path that is left when the npm route was attempted into the very same
  # prefix and then failed, so the fallback owns the install directory now.
  # Skipped when this run reused the prefix successfully — the package there is
  # the one just installed.
  if [ "$OLD_METHOD" = "npm" ] && [ -n "$OLD_NPM_PREFIX" ] \
     && [ "$OLD_NPM_PREFIX" != "$NPM_PREFIX_USED" ] && [ "$PREV_NPM_RETIRED" != "1" ]; then
    info "Removing the npm package left behind by the previous install..."
    remove_npm_package "$OLD_NPM_PREFIX" "${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  fi

  # The adapter tree of a previous npm run, deregistered before it is deleted.
  # Skipped when this run owns the same directory (npm over npm replaces it).
  if [ -n "$OLD_ADAPTERS_DIR" ] && [ "$OLD_ADAPTERS_DIR" != "$ADAPTERS_DIR_USED" ] \
     && [ -d "$OLD_ADAPTERS_DIR" ]; then
    adapters_owned=1
    if [ -n "$OLD_ADAPTERS_DIR_DIGEST" ]; then
      current=$(file_digest "${OLD_ADAPTERS_DIR}/${ADAPTERS_IDENTITY_FILE}")
      if [ "$current" != "$OLD_ADAPTERS_DIR_DIGEST" ]; then
        adapters_owned=0
        warn "Keeping ${OLD_ADAPTERS_DIR}: it no longer matches the previous receipt, so another installation owns it now."
      fi
    fi
    if [ "$adapters_owned" = "1" ]; then
      info "Retiring the adapter resources of the previous install..."
      deregister_framework_adapters "$OLD_ADAPTERS_DIR"
      if rm -rf "$OLD_ADAPTERS_DIR" 2>/dev/null; then
        info "Removed ${OLD_ADAPTERS_DIR}"
      else
        warn "Could not remove ${OLD_ADAPTERS_DIR}; remove it manually."
      fi
    fi
  fi

  # The PATH entry of a previous run survives when this run installs into the
  # same directory, so carry its rc file into the new receipt; otherwise
  # uninstall.sh would no longer know which file to clean.
  CARRIED_PATH_RC=""
  if [ -n "$OLD_PATH_RC" ] && [ -f "$OLD_PATH_RC" ] \
     && [ "$OLD_INSTALL_DIR" = "${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}" ] \
     && grep -Fq "$PATH_RC_MARKER" "$OLD_PATH_RC" 2>/dev/null; then
    CARRIED_PATH_RC="$OLD_PATH_RC"
  fi

  return 0
}

# Records exactly the paths this run created, so scripts/uninstall.sh can be
# symmetric with the install and never delete files owned by another method
# (anolisa CLI, a manual npm install, or a custom TOKENLESS_INSTALL_DIR).
# Each recorded path also carries the sha256 of its content at install time,
# which is what lets uninstall.sh tell our file from a foreign one that later
# took over the same path.
write_receipt() {
  local method="$1"
  local receipt_dir entry digest

  # Retire the previous method's artefacts before this run truncates the only
  # record of what they were.
  retire_previous_receipt

  receipt_dir=$(dirname "$RECEIPT_FILE")
  if ! mkdir -p "$receipt_dir" 2>/dev/null; then
    warn "Could not create ${receipt_dir}; install receipt not written."
    warn "Re-run with TOKENLESS_RECEIPT=<writable path> if you want scripted uninstall."
    RECEIPT_WRITTEN=0
    return 0
  fi
  if ! {
    printf '# Tokenless installer receipt (schema %s).\n' "$RECEIPT_SCHEMA"
    printf '# Written by scripts/install.sh, consumed by scripts/uninstall.sh.\n'
    printf '# Only the paths listed below belong to this installation.\n'
    printf '# Every file= line is followed by its file_digest= line: the sha256 of the\n'
    printf '# installed content. An empty digest means it could not be computed, and\n'
    printf '# uninstall.sh then has to fall back to the path alone.\n'
    printf 'schema=%s\n' "$RECEIPT_SCHEMA"
    printf 'method=%s\n' "$method"
    printf 'version=%s\n' "$VERSION"
    printf 'install_dir=%s\n' "${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
    if [ -n "$NPM_PREFIX_USED" ]; then
      printf 'npm_prefix=%s\n' "$NPM_PREFIX_USED"
    fi
    if [ -n "$ADAPTERS_DIR_USED" ]; then
      printf 'adapters_dir=%s\n' "$ADAPTERS_DIR_USED"
      printf 'adapters_dir_digest=%s\n' "$ADAPTERS_DIR_DIGEST"
    fi
    printf 'installed_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    if [ -n "$CARRIED_PATH_RC" ]; then
      printf 'path_rc_file=%s\n' "$CARRIED_PATH_RC"
    fi
    if [ "${#INSTALLED_FILES[@]}" -gt 0 ]; then
      for entry in "${INSTALLED_FILES[@]}"; do
        digest=$(file_digest "$entry")
        printf 'file=%s\n' "$entry"
        printf 'file_digest=%s\n' "$digest"
      done
    fi
  } > "$RECEIPT_FILE"; then
    warn "Could not write install receipt to ${RECEIPT_FILE}"
    warn "scripts/uninstall.sh will not be able to remove this install; delete"
    warn "the files listed above manually if you need to roll it back."
    RECEIPT_WRITTEN=0
    return 0
  fi
  chmod 0644 "$RECEIPT_FILE" 2>/dev/null || true
  RECEIPT_WRITTEN=1
  INSTALL_METHOD="$method"
  info "Recorded install receipt: ${RECEIPT_FILE}"
}

record_path_rc() {
  [ -f "$RECEIPT_FILE" ] || return 0
  if grep -qxF "path_rc_file=$1" "$RECEIPT_FILE" 2>/dev/null; then
    return 0
  fi
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
  # mkdir/ln/install are all checked explicitly: this function is called as an
  # `if` condition, where Bash disables errexit, so an unchecked failure here
  # would still end up reported as a successful install.
  if ! mkdir -p "$install_dir" 2>/dev/null; then
    warn "Cannot create the install directory ${install_dir}"
    return 1
  fi

  local npm_prefix
  npm_prefix=$(npm config get prefix 2>/dev/null || echo "${HOME}/.npm-global")

  # Everything the npm route replaces is snapshotted first, so each failure
  # below can be undone instead of handing the source-build fallback a machine
  # that already carries an unowned package, an `rtk` link and an adapter tree.
  begin_npm_attempt "$npm_prefix" || return 1

  if ! npm install -g "${NPM_PACKAGE}@${VERSION}" --prefix "$npm_prefix" 2>&1 | tail -5; then
    warn "npm install failed (possible EACCES or network issue)"
    warn "To fix npm permissions: mkdir -p ~/.npm-global && npm config set prefix '~/.npm-global'"
    rollback_npm_attempt
    return 1
  fi

  local npm_bin
  npm_bin="${npm_prefix}/bin"
  if [ ! -f "${npm_bin}/tokenless" ]; then
    npm_bin="${npm_prefix}/lib/node_modules/${NPM_PACKAGE}/bin"
  fi
  if [ ! -f "${npm_bin}/tokenless" ]; then
    warn "npm install succeeded but binary not found at expected path"
    rollback_npm_attempt
    return 1
  fi

  # `toon` is no longer a standalone binary (see the tokenless-cli crate), so
  # only the binaries the npm package actually ships are linked and recorded.
  INSTALLED_FILES=()
  local bin link_target link_path resolved
  for bin in tokenless rtk; do
    if [ ! -f "${npm_bin}/${bin}" ] && [ ! -L "${npm_bin}/${bin}" ]; then
      continue
    fi
    link_target=$(readlink -f "${npm_bin}/${bin}" 2>/dev/null || printf '%s' "${npm_bin}/${bin}")
    link_path="${install_dir}/${bin}"
    if ! ln -sf "$link_target" "$link_path" 2>/dev/null; then
      warn "Cannot write ${link_path} (is ${install_dir} writable?)"
      continue
    fi
    chmod +x "$link_path" 2>/dev/null || true
    # Verify the link that is now at that path really is the one just written.
    # A pre-existing file from another method survives a failed `ln`, and must
    # never be recorded as this run's artefact.
    resolved=$(readlink -f "$link_path" 2>/dev/null || true)
    if [ "$resolved" != "$link_target" ] || [ ! -x "$link_path" ]; then
      warn "${link_path} does not point at the binary this run installed; not recording it"
      continue
    fi
    INSTALLED_FILES+=("$link_path")
  done

  if [ "${#INSTALLED_FILES[@]}" -eq 0 ]; then
    warn "npm install succeeded but no binaries were linked into ${install_dir}"
    rollback_npm_attempt
    return 1
  fi
  if ! verify_cli "${install_dir}/tokenless"; then
    warn "npm install succeeded but ${install_dir}/tokenless is not a working CLI"
    rollback_npm_attempt
    return 1
  fi

  NPM_PREFIX_USED="$npm_prefix"

  # The package postinstall copies the bundled adapters into the shared
  # directory and replaces whatever was there. That makes the tree this run's
  # own only when nothing else owned it first: an anolisa component install, a
  # direct `npm install -g` or a manual copy all leave a tree there that this
  # run must neither adopt nor destroy. A receipt claiming it would let
  # uninstall.sh deregister every framework and delete resources that a
  # component record still refers to, and the manifest digest can only describe
  # the tree after the overwrite — it cannot identify the owner before it.
  local adapters_dir pkg_adapters
  adapters_dir=$(shared_adapters_dir)
  if [ "$ADAPTERS_FOREIGN" = "1" ]; then
    if [ -d "${ROLLBACK_DIR}/adapters-tokenless" ]; then
      rm -rf "$adapters_dir" 2>/dev/null || true
      if cp -a "${ROLLBACK_DIR}/adapters-tokenless" "$adapters_dir" 2>/dev/null; then
        info "Kept the adapter resources that were already in ${adapters_dir}"
      else
        warn "Could not put ${adapters_dir} back; the npm postinstall had replaced it."
      fi
    fi
    warn "${adapters_dir} already belonged to another installation, so this run did"
    warn "not take it over and the receipt does not record it. Its resources and"
    warn "framework registrations are unchanged, and scripts/uninstall.sh will not"
    warn "touch them."
    pkg_adapters="${npm_prefix}/lib/node_modules/${NPM_PACKAGE}/adapters/tokenless"
    if [ -d "$pkg_adapters" ]; then
      info "The adapter resources shipped by this npm package are at: ${pkg_adapters}"
    fi
    ADAPTERS_DIR_USED=""
    ADAPTERS_DIR_DIGEST=""
  elif [ -d "$adapters_dir" ]; then
    # Either this run created the directory, or it is the tree a previous npm
    # run of this installer owned — replacing it is expected, so it stays
    # recorded.
    ADAPTERS_DIR_USED="$adapters_dir"
    ADAPTERS_DIR_DIGEST=$(file_digest "${adapters_dir}/${ADAPTERS_IDENTITY_FILE}")
  fi
  end_npm_attempt
  write_receipt npm

  info "Installed to ${install_dir}"
  case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) warn "${install_dir} is not in PATH. Run: export PATH=\"${install_dir}:\$PATH\"" ;;
  esac
  return 0
}

try_source_build() {
  # macOS has no supported source-build route. The release pipeline produces the
  # macOS binaries by cross-compiling on Linux, and this fallback is validated on
  # Linux only, so building here would hand the user an unvalidated CLI. On Intel
  # macOS it is worse than that: no npm package is published for the platform
  # either, so an unvalidated build would be the only thing standing between the
  # installer and a reported "success".
  if [ "${PLATFORM_OS:-}" = "darwin" ]; then
    err "Source builds are not supported on macOS (${PLATFORM_KEY:-darwin})."
    err "The macOS binaries are cross-compiled on Linux by the release pipeline, and"
    err "this installer's source-build fallback is validated on Linux only."
    if [ "${PLATFORM_KEY:-}" = "darwin-x64" ]; then
      err "Intel macOS has no published npm package yet either"
      err "(@anolisa/tokenless-darwin-x64 is a release build target, not a registry"
      err "artifact), so this platform currently has no supported install route."
    else
      err "Install the prebuilt binaries instead: npm install -g ${NPM_PACKAGE}"
    fi
    return 1
  fi
  info "Building from source..."
  local install_dir="${TOKENLESS_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
  # Checked explicitly for the same reason as in try_npm_install: errexit is
  # disabled inside a function used as an `||`/`elif` condition.
  if ! mkdir -p "$install_dir" 2>/dev/null; then
    warn "Cannot create the install directory ${install_dir}"
    return 1
  fi

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

  # `install(1)` must be checked by hand: errexit is off here, so a failed
  # write (ENOSPC, EACCES, a read-only mount) would otherwise still be reported
  # as a successful install and recorded in the receipt.
  local install_status=0
  install -p -m 0755 "${src_dir}/target/release/tokenless" "${install_dir}/tokenless" || install_status=$?
  if [ "$install_status" -ne 0 ]; then
    err "install(1) failed with exit ${install_status} writing ${install_dir}/tokenless"
    return 1
  fi
  if ! verify_cli "${install_dir}/tokenless"; then
    err "The build finished but ${install_dir}/tokenless is not a working CLI"
    return 1
  fi

  INSTALLED_FILES=("${install_dir}/tokenless")
  NPM_PREFIX_USED=""
  ADAPTERS_DIR_USED=""
  ADAPTERS_DIR_DIGEST=""
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
  local export_line="export PATH=\"${install_dir}:\$PATH\""
  # Do not stack a second copy of the same entry: uninstall.sh strips one marker
  # block per run, so a re-run from a shell that has not sourced the rc file yet
  # would otherwise leave a PATH entry behind after the uninstall.
  if [ -f "$rc_file" ] && grep -qxF "$export_line" "$rc_file" 2>/dev/null; then
    export PATH="${install_dir}:${PATH}"
    record_path_rc "$rc_file"
    info "${rc_file} already adds ${install_dir} to PATH"
    return 0
  fi
  if ! printf '\n%s\n%s\n' "$PATH_RC_MARKER" "$export_line" >> "$rc_file"; then
    warn "Could not append the PATH entry to ${rc_file}. Add it yourself:"
    warn "  ${export_line}"
    return 0
  fi
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

  # Retire the previous run's npm global package before anything new is written.
  # `npm uninstall -g --prefix P` also removes P/bin/tokenless, so when P/bin is
  # this run's install directory, doing it after the install would delete the CLI
  # the source build has just placed there and fail the whole run.
  local prospective_prefix=""
  if [ "${TOKENLESS_FORCE_BUILD:-0}" != "1" ] && [ "${MUSL_LINUX:-0}" != "1" ] \
     && command -v npm >/dev/null 2>&1; then
    prospective_prefix=$(npm config get prefix 2>/dev/null || true)
  fi
  retire_previous_npm_package "$prospective_prefix"

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

  # Every success path above already verified the binary it recorded, so this is
  # a final assertion rather than a probe. `command -v tokenless` is deliberately
  # not used: it would also match a foreign CLI that was already on PATH and turn
  # a failed write into a reported success.
  local cli="${install_dir}/tokenless"
  if [ "${#INSTALLED_FILES[@]}" -gt 0 ] && [ -x "$cli" ] && "$cli" --version >/dev/null 2>&1; then
    local ver
    ver=$("$cli" --version 2>/dev/null || echo "unknown")
    info "Tokenless installed successfully: ${ver}"
    info "Install method: ${INSTALL_METHOD:-unknown} (receipt: ${RECEIPT_FILE})"
    if [ "$RECEIPT_WRITTEN" != "1" ]; then
      warn "No receipt was written, so scripts/uninstall.sh cannot remove this install."
    fi
    info "To remove exactly what this installer created, run:"
    info "  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/src/tokenless/scripts/uninstall.sh | bash"
  else
    err "Installation failed: ${cli} is missing or not runnable"
    exit 1
  fi
}

main "$@"
