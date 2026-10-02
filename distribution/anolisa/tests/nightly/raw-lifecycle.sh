#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CASES_FILE="${ANOLISA_E2E_CASES_FILE:-${SCRIPT_DIR}/raw-lifecycle-cases.tsv}"
RAW_REPO_URL="${ANOLISA_RAW_REPO_URL:-https://anolisa.oss-cn-hangzhou.aliyuncs.com/anolisa-releases/anolisa/v1/}"
REPO_CONFIG_PATH="${ANOLISA_E2E_REPO_CONFIG_PATH:-/etc/anolisa/repo.toml}"
CURL_CONNECT_TIMEOUT_SECS="${ANOLISA_E2E_CONNECT_TIMEOUT_SECS:-10}"
CURL_MAX_TIME_SECS="${ANOLISA_E2E_MAX_TIME_SECS:-120}"
CURL_RETRY_MAX_TIME_SECS="${ANOLISA_E2E_RETRY_MAX_TIME_SECS:-300}"

log() {
  printf '[raw-lifecycle] %s\n' "$*"
}

fail() {
  printf '[raw-lifecycle] ERROR: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is missing: $1"
}

artifact_arch() {
  case "$(uname -m)" in
    x86_64) printf 'x86_64\n' ;;
    aarch64 | arm64) printf 'aarch64\n' ;;
    *) fail "unsupported test architecture: $(uname -m)" ;;
  esac
}

published_version_candidates() {
  local component="$1"
  local arch="$2"
  local index_file="$3"

  # This is intentionally only broad enumeration. The CLI dry-run below is
  # authoritative for host selectors, installability, and version ordering.
  awk -v wanted_component="$component" -v wanted_arch="$arch" '
    function reset_entry() {
      component = ""
      version = ""
      channel = ""
      artifact_type = ""
      os = ""
      arch = ""
      install_modes = ""
    }
    function value_after_equals(line, value) {
      value = line
      sub(/^[^=]*=[[:space:]]*/, "", value)
      gsub(/^"|"$/, "", value)
      return value
    }
    function emit_entry() {
      if (component == wanted_component &&
          version != "" &&
          channel == "stable" &&
          artifact_type == "tar_gz" &&
          os == "linux" &&
          (arch == wanted_arch || arch == "any") &&
          install_modes ~ /"system"/ &&
          !seen[version]++) {
        print version
      }
    }
    /^\[\[entries\]\]$/ {
      if (inside_entry) {
        emit_entry()
      }
      reset_entry()
      inside_entry = 1
      next
    }
    inside_entry && /^component[[:space:]]*=/ { component = value_after_equals($0); next }
    inside_entry && /^version[[:space:]]*=/ { version = value_after_equals($0); next }
    inside_entry && /^channel[[:space:]]*=/ { channel = value_after_equals($0); next }
    inside_entry && /^artifact_type[[:space:]]*=/ { artifact_type = value_after_equals($0); next }
    inside_entry && /^os[[:space:]]*=/ { os = value_after_equals($0); next }
    inside_entry && /^arch[[:space:]]*=/ { arch = value_after_equals($0); next }
    inside_entry && /^install_modes[[:space:]]*=/ { install_modes = $0; next }
    END {
      if (inside_entry) {
        emit_entry()
      }
    }
  ' "$index_file"
}

run_cli_json() {
  local output
  log "anolisa $*" >&2
  if ! output="$(anolisa --json --no-color "$@")"; then
    printf '%s\n' "$output" >&2
    return 1
  fi
  printf '%s\n' "$output"
}

assert_json() {
  local document="$1"
  shift
  if ! printf '%s\n' "$document" | jq -e "$@" >/dev/null; then
    printf '%s\n' "$document" | jq . >&2 || printf '%s\n' "$document" >&2
    fail "JSON assertion failed"
  fi
}

resolve_raw_version() {
  local component="$1"
  local version="${2:-}"
  local args=(
    --dry-run install "$component"
    --backend raw
    --repo "$RAW_REPO_URL"
  )
  if [[ -n "$version" ]]; then
    args+=(--version "$version")
  fi

  local output
  if ! output="$(run_cli_json "${args[@]}")"; then
    return 1
  fi
  assert_json "$output" \
    --arg component "$component" \
    '.ok == true and
     .data.component == $component and
     .data.backend == "raw" and
     .data.action == "planned" and
     .data.dry_run == true and
     (.data.version | type == "string" and length > 0)'
  printf '%s\n' "$output" | jq -r '.data.version'
}

configure_raw_repo() {
  local config_path="$REPO_CONFIG_PATH"
  [[ -f "$config_path" ]] || fail "anolisa repo config not found: ${config_path}"
  if [[ "$RAW_REPO_URL" == *\"* || "$RAW_REPO_URL" == *\\* || "$RAW_REPO_URL" == *$'\n'* ]]; then
    fail "raw repository URL contains characters that cannot be written to repo.toml"
  fi

  local temp_config
  temp_config="$(mktemp "${config_path}.XXXXXX")"
  if ! awk -v base_url="$RAW_REPO_URL" '
    /^\[backends\.raw\]$/ {
      in_raw = 1
      print
      next
    }
    /^\[/ { in_raw = 0 }
    in_raw && /^base_url[[:space:]]*=/ {
      printf "base_url = \"%s\"\n", base_url
      replaced = 1
      next
    }
    { print }
    END { if (!replaced) exit 1 }
  ' "$config_path" > "$temp_config"; then
    fail "raw backend base_url is missing from ${config_path}"
  fi
  chmod 0644 "$temp_config"
  mv -f "$temp_config" "$config_path"
  log "configured raw lifecycle repository: ${RAW_REPO_URL}"
}

run_case() {
  local component="$1"
  local rpm_package="$2"
  local index_file="${ANOLISA_E2E_INDEX_FILE:?ANOLISA_E2E_INDEX_FILE is required}"
  local arch
  arch="$(artifact_arch)"

  require_command anolisa
  require_command dnf
  require_command jq
  require_command rpm

  if rpm -q "$rpm_package" >/dev/null 2>&1; then
    log "removing preinstalled RPM ${rpm_package} so raw ownership is unambiguous"
    dnf remove -y "$rpm_package"
  fi
  if rpm -q "$rpm_package" >/dev/null 2>&1; then
    fail "RPM ${rpm_package} is still installed"
  fi

  mapfile -t versions < <(published_version_candidates "$component" "$arch" "$index_file")
  if ((${#versions[@]} < 2)); then
    fail "${component} needs at least two raw version candidates for linux/${arch}; found ${#versions[@]}"
  fi

  local latest_version
  if ! latest_version="$(resolve_raw_version "$component")"; then
    fail "anolisa could not resolve the latest installable ${component} version"
  fi

  local previous_version=""
  local candidate resolved_candidate
  for candidate in "${versions[@]}"; do
    [[ "$candidate" == "$latest_version" ]] && continue
    if resolved_candidate="$(resolve_raw_version "$component" "$candidate")" &&
      [[ "$resolved_candidate" == "$candidate" ]]; then
      previous_version="$candidate"
      break
    fi
  done
  [[ -n "$previous_version" ]] ||
    fail "${component} needs an older installable raw version before ${latest_version}"
  log "testing ${component}: install ${previous_version}, update from live raw index, uninstall"

  local install_json
  install_json="$(run_cli_json install "$component" \
    --backend raw \
    --repo "$RAW_REPO_URL" \
    --version "$previous_version")"
  assert_json "$install_json" \
    --arg component "$component" \
    --arg version "$previous_version" \
    '.ok == true and
     .data.component == $component and
     .data.backend == "raw" and
     .data.action == "installed" and
     .data.version == $version and
     .data.resolved_version == $version'

  local status_json
  status_json="$(run_cli_json status "$component")"
  assert_json "$status_json" \
    --arg component "$component" \
    --arg version "$previous_version" \
    '.ok == true and
     ([.data.components[] |
       select(.name == $component and .scope == "system" and
              .status == "installed" and .version == $version)] | length) == 1'

  local update_json
  update_json="$(run_cli_json update "$component")"
  assert_json "$update_json" \
    --arg component "$component" \
    --arg from "$previous_version" \
    '.ok == true and
     .data.component == $component and
     .data.from_version == $from and
     (.data.to_version | type == "string" and length > 0) and
     .data.to_version != $from and
     .data.updated == true and
     .data.dry_run == false'

  local updated_version
  updated_version="$(jq -er '.data.to_version' <<<"$update_json")"
  log "updated ${component}: ${previous_version} -> ${updated_version}"

  status_json="$(run_cli_json status "$component")"
  assert_json "$status_json" \
    --arg component "$component" \
    --arg version "$updated_version" \
    '.ok == true and
     ([.data.components[] |
       select(.name == $component and .scope == "system" and
              .status == "installed" and .version == $version)] | length) == 1'

  local uninstall_json
  uninstall_json="$(run_cli_json uninstall "$component")"
  assert_json "$uninstall_json" \
    --arg component "$component" \
    '.ok == true and
     .data.component == $component and
     .data.package_removal == "owned files removed" and
     .data.state_dropped == true and
     .data.dry_run == false'

  status_json="$(run_cli_json status "$component")"
  assert_json "$status_json" \
    --arg component "$component" \
    '.ok == true and
     ([.data.components[] |
       select(.name == $component and .status == "not_installed")] | length) == 1'

  log "PASS ${component}"
}

list_cases() {
  while IFS=$'\t' read -r component rpm_package status reason; do
    [[ -z "$component" || "$component" == \#* ]] && continue
    printf '%s\t%s\t%s\t%s\n' "$component" "$rpm_package" "$status" "$reason"
  done < "$CASES_FILE"
}

if [[ "${1:-}" == "--list" ]]; then
  list_cases
  exit 0
fi

if [[ "${1:-}" == "--versions" ]]; then
  [[ $# -eq 4 ]] || fail "usage: $0 --versions COMPONENT ARCH INDEX_FILE"
  published_version_candidates "$2" "$3" "$4"
  exit 0
fi

if [[ "${1:-}" == "--case" ]]; then
  [[ $# -eq 3 ]] || fail "usage: $0 --case COMPONENT RPM_PACKAGE"
  run_case "$2" "$3"
  exit 0
fi

if [[ "${1:-}" == "--configure-repo" ]]; then
  [[ $# -eq 1 ]] || fail "usage: $0 --configure-repo"
  configure_raw_repo
  exit 0
fi

require_command curl
[[ -f "$CASES_FILE" ]] || fail "case inventory not found: ${CASES_FILE}"
configure_raw_repo

index_file="$(mktemp)"
trap 'rm -f "$index_file"' EXIT
curl --fail --silent --show-error --location \
  --connect-timeout "$CURL_CONNECT_TIMEOUT_SECS" \
  --max-time "$CURL_MAX_TIME_SECS" \
  --retry 3 \
  --retry-max-time "$CURL_RETRY_MAX_TIME_SECS" \
  --output "$index_file" "${RAW_REPO_URL%/}/index.toml"

executed=0
skipped=0
failed=0
while IFS=$'\t' read -r component rpm_package status reason; do
  [[ -z "$component" || "$component" == \#* ]] && continue
  case "$status" in
    run)
      executed=$((executed + 1))
      if ANOLISA_E2E_INDEX_FILE="$index_file" \
        bash "$0" --case "$component" "$rpm_package"; then
        :
      else
        failed=$((failed + 1))
      fi
      ;;
    skip)
      skipped=$((skipped + 1))
      log "SKIP ${component}: ${reason}"
      ;;
    *)
      fail "unknown status '${status}' for ${component}"
      ;;
  esac
done < "$CASES_FILE"

log "summary: ${executed} executed, ${skipped} skipped, ${failed} failed"
((executed > 0)) || fail "the inventory has no runnable cases"
((failed == 0)) || fail "${failed} raw lifecycle case(s) failed"
