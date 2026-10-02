#!/usr/bin/env bash
set -euo pipefail

readonly INSTALL_MARKER=".cosh-acp-adapters"
readonly CODEX_PACKAGE="@agentclientprotocol/codex-acp"
readonly CODEX_VERSION="1.6.2"
readonly CLAUDE_PACKAGE="@agentclientprotocol/claude-agent-acp"
readonly CLAUDE_VERSION="0.66.0"

usage() {
  echo "usage: $0 --prefix ABSOLUTE_PRIVATE_DIRECTORY" >&2
}

prefix=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      prefix="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

[[ "$prefix" = /* && "$prefix" != / ]] || {
  echo "--prefix must be an absolute non-root directory" >&2
  exit 2
}
command -v npm >/dev/null 2>&1 || { echo "npm is required" >&2; exit 1; }
command -v node >/dev/null 2>&1 || { echo "node is required" >&2; exit 1; }

prepare_prefix() {
  local parent
  parent=$(dirname -- "$prefix")
  [[ -d "$parent" && ! -L "$parent" ]] || {
    echo "adapter prefix parent must be an existing non-symlink directory" >&2
    exit 2
  }
  [[ "$(readlink -f -- "$parent")/$(basename -- "$prefix")" == "$prefix" ]] || {
    echo "adapter prefix must be normalized and have no symlink ancestors" >&2
    exit 2
  }

  if [[ -e "$prefix" || -L "$prefix" ]]; then
    [[ -d "$prefix" && ! -L "$prefix" ]] || {
      echo "adapter prefix must be a non-symlink directory" >&2
      exit 2
    }
    [[ "$(stat -c '%u' -- "$prefix")" == "$(id -u)" ]] || {
      echo "adapter prefix must be owned by the current user" >&2
      exit 2
    }
    (( (8#$(stat -c '%a' -- "$prefix") & 8#077) == 0 )) || {
      echo "adapter prefix must not grant group or other permissions" >&2
      exit 2
    }
    if [[ -n "$(find "$prefix" -mindepth 1 -maxdepth 1 -print -quit)" &&
          ! -f "$prefix/$INSTALL_MARKER" ]]; then
      echo "non-empty adapter prefix is not managed by COSH" >&2
      exit 2
    fi
  else
    mkdir -m 0700 -- "$prefix"
  fi
}

verify_package_bin() {
  local command_name="$1"
  local package_name="$2"
  local expected_version="$3"
  local candidate="$prefix/node_modules/.bin/$command_name"
  local package_dir="$prefix/node_modules/$package_name"

  [[ -x "$candidate" ]] || {
    echo "missing installed adapter: $command_name" >&2
    exit 1
  }
  [[ -d "$package_dir" && ! -L "$package_dir" ]] || {
    echo "missing installed package: $package_name" >&2
    exit 1
  }

  node - "$candidate" "$package_dir" "$package_name" "$expected_version" "$command_name" <<'NODE'
const fs = require("fs");
const path = require("path");

const [candidate, packageDir, expectedName, expectedVersion, commandName] =
  process.argv.slice(2);
const prefixModules = path.dirname(path.dirname(packageDir));
const packageJsonPath = path.join(packageDir, "package.json");
const manifest = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
const bin = typeof manifest.bin === "string" ? manifest.bin : manifest.bin?.[commandName];

if (manifest.name !== expectedName || manifest.version !== expectedVersion) {
  throw new Error(`unexpected package identity for ${commandName}`);
}
if (typeof bin !== "string" || path.isAbsolute(bin)) {
  throw new Error(`invalid package bin mapping for ${commandName}`);
}

const canonicalCandidate = fs.realpathSync(candidate);
const canonicalTarget = fs.realpathSync(path.join(packageDir, bin));
const canonicalModules = fs.realpathSync(prefixModules) + path.sep;
if (canonicalCandidate !== canonicalTarget || !canonicalTarget.startsWith(canonicalModules)) {
  throw new Error(`adapter provenance mismatch for ${commandName}`);
}
NODE
}

prepare_prefix

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
manifest_dir="$script_dir/acp-adapters"
install -m 0600 "$manifest_dir/package.json" "$prefix/package.json"
install -m 0600 "$manifest_dir/package-lock.json" "$prefix/package-lock.json"
printf '%s\n' "cosh-ng-acp-adapters-v1" >"$prefix/$INSTALL_MARKER"
chmod 0600 "$prefix/$INSTALL_MARKER"

# This explicit developer operation is the only network-capable package step.
# COSH runtime resolution never invokes npm, npx, a shell, or a downloader.
npm ci --prefix "$prefix" --omit=dev --ignore-scripts --no-audit --no-fund

verify_package_bin codex-acp "$CODEX_PACKAGE" "$CODEX_VERSION"
verify_package_bin claude-agent-acp "$CLAUDE_PACKAGE" "$CLAUDE_VERSION"

printf 'Installed pinned ACP adapter bundle below %s\n' "$prefix"
printf '  codex-acp: %s\n' "$CODEX_VERSION"
printf '  claude-agent-acp: %s\n' "$CLAUDE_VERSION"
printf 'Pass an exact adapter path to cosh-gateway; do not add the bundle to global PATH.\n'
