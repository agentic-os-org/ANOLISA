#!/usr/bin/env bash
# install.sh — lightweight installer for the anolisa CLI.
#
# Usage:
#   curl -fsSL https://get.agentic-os.sh | bash
#   curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
#   curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --upgrade
#   curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --uninstall
#
# Environment overrides:
#   ANOLISA_VERSION      version to install      (default: stable)
#   ANOLISA_MIRROR       OSS mirror base URL     (default: https://anolisa.oss-cn-hangzhou.aliyuncs.com)
#   ANOLISA_UPDATE_URL   CLI release manifest URL (default: derived from mirror)
#   ANOLISA_INSTALL_DIR  binary install directory (default: ~/.local/bin)

set -euo pipefail

VERSION="${ANOLISA_VERSION:-stable}"
MIRROR="${ANOLISA_MIRROR:-https://anolisa.oss-cn-hangzhou.aliyuncs.com}"
UPDATE_URL="${ANOLISA_UPDATE_URL:-${MIRROR}/anolisa-releases/anolisa/v1/cli/release-manifest.toml}"
INSTALL_DIR="${ANOLISA_INSTALL_DIR:-$HOME/.local/bin}"
TMPDIR_INSTALL=""
STAGED_BINARY=""
MANIFEST_SCHEMA=""
RESOLVED_VERSION=""
ARTIFACT_URL=""
ARTIFACT_SHA256=""
VERIFIED_ARCHIVE=""
VERIFIED_ARCHIVE_SHA256=""
COMPONENT=""
COMPONENT_BACKEND="raw"
COMPONENT_BACKEND_SET=0
COMPONENT_ACTION="install"
COMPONENT_ACTION_SET=0
INSTALL_MODE=""
COMPONENT_SYSTEM_SCOPE=0
COMPONENT_USE_SUDO=0

log()  { printf '\033[1;32m%s\033[0m %s\n' "==>" "$*"; }
warn() { printf '\033[1;33m%s\033[0m %s\n' "warn:" "$*" >&2; }
err()  { printf '\033[1;31m%s\033[0m %s\n' "error:" "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: install.sh [OPTIONS]

Install or update the anolisa CLI. Optionally manage one ANOLISA component
after the CLI is ready. The CLI validates component support for this host.

Options:
  --component NAME  Manage NAME (installs it by default)
  --cosh-ng         Shorthand for --component cosh-ng
  --backend BACKEND Use BACKEND for installation (default: raw)
  --install-mode MODE
                    Use user or system scope; system uses sudo when needed
  --upgrade         Update the selected component
  --uninstall       Uninstall the selected component
  -h, --help        Show this help text and exit

Without --install-mode, ANOLISA uses user scope for non-root and system for root.

Piped examples:
  curl -fsSL https://get.agentic-os.sh | bash
  curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
  curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --upgrade
  curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --uninstall
EOF
}

select_component() {
  local component="$1"
  # Keep only registry-shaped data here; the CLI owns identity and target checks.
  case "$component" in
    ""|*[!a-z0-9-]*|-*|*-)
      err "invalid component name: ${component:-empty}"
      ;;
  esac
  if [ -n "$COMPONENT" ]; then
    err "only one component can be selected"
  fi
  COMPONENT="$component"
}

select_component_backend() {
  local backend="$1"
  # Keep only backend-shaped data here; the CLI owns backend support checks.
  case "$backend" in
    ""|*[!a-z0-9-]*|-*|*-)
      err "invalid backend: ${backend:-empty}"
      ;;
  esac
  if [ "$COMPONENT_BACKEND_SET" -eq 1 ]; then
    err "only one component backend can be selected"
  fi
  COMPONENT_BACKEND="$backend"
  COMPONENT_BACKEND_SET=1
}

select_install_mode() {
  local mode="$1"
  case "$mode" in
    user|system) ;;
    *) err "invalid install mode: ${mode:-empty} (expected user or system)" ;;
  esac
  if [ -n "$INSTALL_MODE" ]; then
    err "only one install mode can be selected"
  fi
  INSTALL_MODE="$mode"
}

select_component_action() {
  local action="$1"
  if [ "$COMPONENT_ACTION_SET" -eq 1 ] && [ "$COMPONENT_ACTION" != "$action" ]; then
    err "--upgrade and --uninstall cannot be used together"
  fi
  COMPONENT_ACTION="$action"
  COMPONENT_ACTION_SET=1
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --component)
        [ "$#" -ge 2 ] || err "--component requires a component name"
        select_component "$2"
        shift 2
        ;;
      --component=*)
        select_component "${1#*=}"
        shift
        ;;
      --cosh-ng)
        select_component "cosh-ng"
        shift
        ;;
      --backend)
        [ "$#" -ge 2 ] || err "--backend requires a backend name"
        select_component_backend "$2"
        shift 2
        ;;
      --backend=*)
        select_component_backend "${1#*=}"
        shift
        ;;
      --install-mode)
        [ "$#" -ge 2 ] || err "--install-mode requires user or system"
        select_install_mode "$2"
        shift 2
        ;;
      --install-mode=*)
        select_install_mode "${1#*=}"
        shift
        ;;
      --upgrade)
        select_component_action "update"
        shift
        ;;
      --uninstall)
        select_component_action "uninstall"
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        err "unknown argument: $1"
        ;;
    esac
  done

  if [ "$COMPONENT_ACTION_SET" -eq 1 ] && [ -z "$COMPONENT" ]; then
    err "--upgrade and --uninstall require --component NAME"
  fi
  if [ -n "$INSTALL_MODE" ] && [ -z "$COMPONENT" ]; then
    err "--install-mode requires --component NAME"
  fi
  if [ "$COMPONENT_BACKEND_SET" -eq 1 ] && [ -z "$COMPONENT" ]; then
    err "--backend requires --component NAME"
  fi
  if [ "$COMPONENT_BACKEND_SET" -eq 1 ] && [ "$COMPONENT_ACTION_SET" -eq 1 ]; then
    err "--backend is only valid when installing a component"
  fi
}

check_component_action_prerequisites() {
  local euid
  euid="$(id -u)"
  if [ "$INSTALL_MODE" = "system" ] ||
    { [ -z "$INSTALL_MODE" ] && [ "$euid" -eq 0 ]; }; then
    COMPONENT_SYSTEM_SCOPE=1
  fi
  if [ "$COMPONENT_SYSTEM_SCOPE" -eq 1 ] && [ "$euid" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 ||
      err "sudo is required for --install-mode system"
    COMPONENT_USE_SUDO=1
  fi
}

cleanup() {
  [ -z "$TMPDIR_INSTALL" ] || rm -rf "$TMPDIR_INSTALL"
  [ -z "$STAGED_BINARY" ] || rm -f "$STAGED_BINARY"
}

detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)  OS="linux";  MANIFEST_OS="linux" ;;
    Darwin) OS="darwin"; MANIFEST_OS="macos" ;;
    *)      err "unsupported OS: $os (only Linux and macOS are supported)" ;;
  esac

  case "$arch" in
    x86_64|amd64)   ARCH="x86_64";  ARCH_SHORT="x86_64" ;;
    aarch64|arm64)   ARCH="aarch64"; ARCH_SHORT="aarch64" ;;
    *)               err "unsupported architecture: $arch" ;;
  esac

  case "$OS" in
    linux)  TARGET="${ARCH}-unknown-linux-gnu" ;;
    darwin) TARGET="${ARCH}-apple-darwin" ;;
  esac
}

resolve_stable_release() {
  local manifest_file="${TMPDIR_INSTALL}/release-manifest.toml"
  local record

  log "resolving stable release for ${MANIFEST_OS}/${ARCH_SHORT}"
  if ! curl -fsSL --connect-timeout 15 --max-time 60 \
    -o "$manifest_file" "$UPDATE_URL"; then
    err "failed to download release manifest from ${UPDATE_URL}"
  fi

  if ! record="$(
    awk -v wanted_os="$MANIFEST_OS" -v wanted_arch="$ARCH_SHORT" '
      function value(line) {
        sub(/^[^=]*=[[:space:]]*/, "", line)
        sub(/[[:space:]]*$/, "", line)
        sub(/^"/, "", line)
        sub(/"$/, "", line)
        return line
      }

      function emit() {
        if (!found &&
            artifact_os == wanted_os &&
            artifact_arch == wanted_arch &&
            artifact_url != "" &&
            artifact_sha256 != "") {
          print schema_version "\t" release_version "\t" \
            artifact_url "\t" artifact_sha256
          found = 1
        }
      }

      /^[[:space:]]*#/ || /^[[:space:]]*$/ {
        next
      }

      !in_artifact && /^[[:space:]]*schema_version[[:space:]]*=/ {
        schema_version = value($0)
        next
      }

      !in_artifact && /^[[:space:]]*version[[:space:]]*=/ {
        release_version = value($0)
        next
      }

      /^[[:space:]]*\[\[artifacts\]\][[:space:]]*$/ {
        emit()
        if (found) {
          exit 0
        }
        in_artifact = 1
        artifact_os = ""
        artifact_arch = ""
        artifact_url = ""
        artifact_sha256 = ""
        next
      }

      in_artifact && /^[[:space:]]*os[[:space:]]*=/ \
        { artifact_os = value($0); next }
      in_artifact && /^[[:space:]]*arch[[:space:]]*=/ \
        { artifact_arch = value($0); next }
      in_artifact && /^[[:space:]]*url[[:space:]]*=/ \
        { artifact_url = value($0); next }
      in_artifact && /^[[:space:]]*sha256[[:space:]]*=/ \
        { artifact_sha256 = value($0); next }

      END {
        emit()
        if (!found) {
          exit 1
        }
      }
    ' "$manifest_file"
  )"; then
    err "release manifest has no artifact for ${MANIFEST_OS}/${ARCH_SHORT}"
  fi

  IFS="$(printf '\t')" read -r \
    MANIFEST_SCHEMA RESOLVED_VERSION ARTIFACT_URL ARTIFACT_SHA256 <<< "$record"

  [ "$MANIFEST_SCHEMA" = "1" ] ||
    err "unsupported release manifest schema: ${MANIFEST_SCHEMA:-missing}"
  [ -n "$RESOLVED_VERSION" ] ||
    err "release manifest does not declare a version"
  case "$ARTIFACT_URL" in
    https://*) ;;
    *) err "release manifest contains an unsupported artifact URL" ;;
  esac
  if [ "${#ARTIFACT_SHA256}" -ne 64 ]; then
    err "release manifest contains an invalid SHA256 digest"
  fi
  case "$ARTIFACT_SHA256" in
    *[!0-9a-fA-F]*) err "release manifest contains an invalid SHA256 digest" ;;
  esac
}

sha256_verify() {
  local file="$1" expected="$2"
  local actual
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    warn "sha256sum/shasum not found, skipping checksum verification"
    return 0
  fi
  if [ "$actual" != "$expected" ]; then
    err "sha256 mismatch (expected: $expected, got: $actual)"
  fi
  log "checksum verified"
}

run_system_anolisa() {
  [ -n "$VERIFIED_ARCHIVE" ] || err "verified CLI archive is unavailable"
  if [ "${#VERIFIED_ARCHIVE_SHA256}" -ne 64 ]; then
    err "a valid CLI checksum is required for system component actions"
  fi
  case "$VERIFIED_ARCHIVE_SHA256" in
    *[!0-9a-fA-F]*)
      err "a valid CLI checksum is required for system component actions"
      ;;
  esac

  local runner
  runner='
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
umask 077

expected_sha=$1
shift
workdir=$(mktemp -d /tmp/anolisa-system.XXXXXX)
cleanup_root() {
  rm -rf "$workdir"
}
trap cleanup_root EXIT

archive="$workdir/anolisa.tar.gz"
cat >"$archive"
if command -v sha256sum >/dev/null 2>&1; then
  actual_sha=$(sha256sum "$archive")
elif command -v shasum >/dev/null 2>&1; then
  actual_sha=$(shasum -a 256 "$archive")
else
  echo "error: sha256sum/shasum is required for system component actions" >&2
  exit 1
fi
actual_sha=${actual_sha%% *}
if [ "$actual_sha" != "$expected_sha" ]; then
  echo "error: privileged CLI archive checksum mismatch" >&2
  exit 1
fi

tar -xzf "$archive" -C "$workdir"
if [ ! -f "$workdir/anolisa" ]; then
  echo "error: privileged CLI archive does not contain anolisa" >&2
  exit 1
fi
chmod 0755 "$workdir/anolisa"
"$workdir/anolisa" --install-mode system "$@"
'

  if [ "$COMPONENT_USE_SUDO" -eq 1 ]; then
    sudo -- /bin/sh -c "$runner" sh "$VERIFIED_ARCHIVE_SHA256" "$@" \
      <"$VERIFIED_ARCHIVE"
  else
    /bin/sh -c "$runner" sh "$VERIFIED_ARCHIVE_SHA256" "$@" \
      <"$VERIFIED_ARCHIVE"
  fi
}

run_anolisa() {
  if [ "$COMPONENT_SYSTEM_SCOPE" -eq 1 ]; then
    run_system_anolisa "$@"
  elif [ -n "$INSTALL_MODE" ]; then
    "${INSTALL_DIR}/anolisa" --install-mode "$INSTALL_MODE" "$@"
  else
    "${INSTALL_DIR}/anolisa" "$@"
  fi
}

run_component_action() {
  case "$COMPONENT_ACTION" in
    install)
      log "installing ${COMPONENT}"
      run_anolisa install "$COMPONENT" --backend "$COMPONENT_BACKEND"
      ;;
    update)
      log "updating ${COMPONENT}"
      run_anolisa update "$COMPONENT"
      ;;
    uninstall)
      log "uninstalling ${COMPONENT}"
      run_anolisa uninstall "$COMPONENT"
      ;;
  esac
}

# Resolve symlinks and symlinked directories without relying on GNU readlink.
canonical_path() {
  local path="$1" link dir hops=0
  [ -e "$path" ] || [ -L "$path" ] || return 1
  case "$path" in /*) ;; *) path="${PWD}/$path" ;; esac

  while [ -L "$path" ]; do
    hops=$((hops + 1))
    [ "$hops" -le 20 ] || return 1
    link="$(readlink "$path")" || return 1
    case "$link" in
      /*) path="$link" ;;
      *) path="${path%/*}/$link" ;;
    esac
  done

  dir="$(cd "${path%/*}" 2>/dev/null && pwd -P)" || return 1
  printf '%s/%s\n' "$dir" "${path##*/}"
}

# Fail prominently when PATH still selects a CLI owned by another channel.
check_active_cli() {
  local installed_path="$1" installed_version="$2"
  local active_path active_real installed_real active_version

  active_path="$(
    { hash -r 2>/dev/null ||:; command -v anolisa; } 2>/dev/null
  )" || active_path=""
  [ -n "$active_path" ] || return 0
  # File identity also covers case-insensitive paths and hard links.
  [ "$installed_path" -ef "$active_path" ] && return 0

  active_real="$(canonical_path "$active_path")" || active_real="$active_path"
  installed_real="$(canonical_path "$installed_path")" ||
    installed_real="$installed_path"
  [ "$active_real" != "$installed_real" ] || return 0

  if ! active_version="$("$active_path" --version </dev/null 2>&1)"; then
    active_version="<failed to run>"
  fi
  # Keep foreign CLI output on one line without terminal control sequences.
  active_version="${active_version%%$'\n'*}"
  active_version="$(printf '%s' "$active_version" |
    LC_ALL=C sed $'s/\033\\[[0-?]*[ -/]*[@-~]//g' | LC_ALL=C tr -d '[:cntrl:]')"
  active_version="${active_version:-unknown}"

  {
    warn "PATH resolves anolisa to a different installation"
    printf '    installed: %s (%s)\n' "$installed_path" "$installed_version"
    printf '    active:    %s (%s)\n' "$active_path" "$active_version"
    printf '    put the new install first, then refresh the shell lookup:\n'
    printf '      export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    printf '      hash -r\n'
    case "$active_real" in
      */node_modules/@anolisa/cli/*)
        printf '    or remove the npm-owned CLI yourself:\n'
        printf '      npm uninstall -g @anolisa/cli\n'
        ;;
      */Cellar/anolisa/*)
        printf '    or remove the Homebrew-owned CLI yourself:\n'
        printf '      brew uninstall anolisa\n'
        ;;
    esac
    printf 'error: the newly installed anolisa is shadowed\n'
  } >&2
  return 1
}

main() {
  parse_args "$@"
  check_component_action_prerequisites
  detect_platform
  command -v curl >/dev/null 2>&1 || err "curl is required but not found"
  command -v tar  >/dev/null 2>&1 || err "tar is required but not found"

  TMPDIR_INSTALL="$(mktemp -d)"
  trap cleanup EXIT

  local artifact release_dir label tar_url sha_url expected_sha
  expected_sha=""
  if [ "$VERSION" = "stable" ]; then
    resolve_stable_release
    artifact="${ARTIFACT_URL##*/}"
    artifact="${artifact%%\?*}"
    tar_url="$ARTIFACT_URL"
    expected_sha="$ARTIFACT_SHA256"
    label="$RESOLVED_VERSION"
  else
    artifact="anolisa-cli-${VERSION}-${TARGET}.tar.gz"
    release_dir="$VERSION"
    label="$VERSION"
    local base_url="${MIRROR}/anolisa-releases/anolisa/v1/cli/releases/${release_dir}/artifacts/${MANIFEST_OS}/${ARCH_SHORT}"
    tar_url="${base_url}/${artifact}"
    sha_url="${tar_url}.sha256.txt"
  fi

  log "installing anolisa ${label} (${TARGET})"

  log "downloading ${artifact}"
  if ! curl -fSL --connect-timeout 15 --max-time 300 --progress-bar \
    -o "${TMPDIR_INSTALL}/${artifact}" "$tar_url"; then
    err "download failed — check version/platform or set ANOLISA_MIRROR"
  fi

  log "verifying checksum"
  if [ -n "$expected_sha" ]; then
    sha256_verify "${TMPDIR_INSTALL}/${artifact}" "$expected_sha"
  elif expected_sha="$(
    curl -fsSL --connect-timeout 15 --max-time 60 "$sha_url" 2>/dev/null |
      awk '{print $1}'
  )"; then
    sha256_verify "${TMPDIR_INSTALL}/${artifact}" "$expected_sha"
  else
    warn "checksum file not available, skipping verification"
  fi
  VERIFIED_ARCHIVE="${TMPDIR_INSTALL}/${artifact}"
  VERIFIED_ARCHIVE_SHA256="$expected_sha"

  log "extracting binary"
  tar -xzf "${TMPDIR_INSTALL}/${artifact}" -C "$TMPDIR_INSTALL"

  mkdir -p "$INSTALL_DIR"
  STAGED_BINARY="$(mktemp "${INSTALL_DIR}/.anolisa.XXXXXX")"
  install -m 0755 "${TMPDIR_INSTALL}/anolisa" "$STAGED_BINARY"

  local installed_version
  if ! installed_version="$("$STAGED_BINARY" --version 2>&1)"; then
    err "downloaded binary failed validation: ${installed_version}"
  fi
  if [ -n "$RESOLVED_VERSION" ] &&
    [ "$installed_version" != "anolisa ${RESOLVED_VERSION}" ]; then
    err "downloaded binary version does not match release manifest"
  fi

  mv -f "$STAGED_BINARY" "${INSTALL_DIR}/anolisa"
  STAGED_BINARY=""
  log "installed to ${INSTALL_DIR}/anolisa"

  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      warn "${INSTALL_DIR} is not in your PATH"
      echo "    add it with:  export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac

  log "$installed_version"
  [ -z "$COMPONENT" ] || run_component_action

  check_active_cli "$INSTALL_DIR/anolisa" "$installed_version"
  log "done"
}

main "$@"
