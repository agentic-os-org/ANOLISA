#!/usr/bin/env bash
set -euo pipefail

readonly DEV_MARKER="cosh-ng-managed-task-dev-v1"
readonly CODEX_VERSION="1.6.2"
readonly UNIT_TEMPLATE="cosh-gateway-dev@.service"
readonly PROXY_NAMES=(
  HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY
  http_proxy https_proxy all_proxy no_proxy
)
readonly CODEX_RUNTIME_ENV_NAMES=(
  CODEX_SQLITE_HOME CODEX_API_KEY CODEX_ACCESS_TOKEN OPENAI_API_KEY
  OPENAI_FEDERATION_RULE_ID OPENAI_IDENTITY_TOKEN_FILE
  OPENAI_WORKLOAD_IDENTITY_CONTEXT CODEX_CA_CERTIFICATE SSL_CERT_FILE RUST_LOG
)
readonly SENSITIVE_ENV_NAMES=(
  CODEX_API_KEY CODEX_ACCESS_TOKEN OPENAI_API_KEY OPENAI_IDENTITY_TOKEN_FILE
  OPENAI_WORKLOAD_IDENTITY_CONTEXT
)

usage() {
  cat <<'EOF'
usage:
  managed-task-dev.sh setup [options]
  managed-task-dev.sh shell [--dry-run]
  managed-task-dev.sh status [--dry-run]
  managed-task-dev.sh down [--dry-run]
  managed-task-dev.sh uninstall [--purge-state] [--dry-run]

setup options:
  --no-build                 stage existing target/debug binaries
  --workspace ABSOLUTE_DIR   Task workspace (default: current directory)
  --codex auto|off|required  discover pinned codex-acp (default: auto)
  --environment inherit|off  copy allowlisted runtime variables (default: inherit)
  --checkpoint-socket PATH   configure an existing absolute Unix socket
  --stop-production          stop this user's production/legacy Gateway first
  --dry-run                  validate and print actions without changing the host

The dev Gateway uses allow_all and does not sandbox Codex workspace access.
Its unit and environment are transient below /run; rerun setup after reboot.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

quote_command() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
}

run() {
  if [[ "$dry_run" == true ]]; then
    quote_command "$@"
  else
    "$@"
  fi
}

contains_control() {
  # Bash strings cannot contain NUL; reject the line-breaking controls that can
  # alter systemd EnvironmentFile structure.
  [[ "$1" == *$'\n'* || "$1" == *$'\r'* ]]
}

require_safe_absolute_path() {
  local path="$1" label="$2"
  [[ "$path" == /* && "$path" != / ]] || die "$label must be an absolute non-root path"
  ! contains_control "$path" || die "$label contains a control character"
}

canonical_existing_dir() {
  local path="$1" label="$2" canonical
  require_safe_absolute_path "$path" "$label"
  [[ -d "$path" && ! -L "$path" ]] || die "$label must be an existing non-symlink directory"
  canonical=$(readlink -f -- "$path")
  [[ "$canonical" == "$path" ]] || die "$label must already be canonical: $canonical"
  printf '%s\n' "$canonical"
}

paths_overlap() {
  [[ "$1" == "$2" || "$1" == "$2"/* || "$2" == "$1"/* ]]
}

env_line() {
  local name="$1" value="$2"
  ! contains_control "$value" || die "$name contains a control character"
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '%s="%s"\n' "$name" "$value"
}

owned_file_has_marker() {
  local path="$1"
  [[ -f "$path" && ! -L "$path" ]] || return 1
  [[ "$(stat -c '%u' -- "$path")" == 0 &&
     "$(head -n 1 -- "$path")" == "# $DEV_MARKER" ]]
}

owned_stage_has_marker() {
  local path="$1"
  [[ -d "$path" && ! -L "$path" && -f "$path/.managed-task-dev" &&
     ! -L "$path/.managed-task-dev" ]] || return 1
  [[ "$(stat -c '%u' -- "$path")" == 0 &&
     "$(stat -c '%u' -- "$path/.managed-task-dev")" == 0 &&
     $((8#$(stat -c '%a' -- "$path") & 8#022)) -eq 0 &&
     "$(<"$path/.managed-task-dev")" == "$DEV_MARKER" ]]
}

unit_is_active() {
  systemctl is-active --quiet "$1" >/dev/null 2>&1
}

wait_for_gateway_capabilities() {
  local attempt
  for ((attempt = 0; attempt < 50; attempt++)); do
    if [[ -S "$socket_path" ]] &&
       "$stage_dir/cosh-gateway" task --socket "$socket_path" \
         --output jsonl capabilities >/dev/null 2>&1; then
      return 0
    fi
    unit_is_active "$unit" || return 1
    sleep 0.1
  done
  return 1
}

validate_codex_adapter() {
  local candidate="$1" target package_dir package_json
  [[ -x "$candidate" ]] || return 1
  target=$(readlink -f -- "$candidate") || return 1
  package_dir=$(dirname -- "$(dirname -- "$target")")
  package_json="$package_dir/package.json"
  [[ -f "$package_json" && ! -L "$package_dir" ]] || return 1
  command -v node >/dev/null 2>&1 || return 1
  node - "$candidate" "$package_dir" "$CODEX_VERSION" <<'NODE' >/dev/null 2>&1
const fs = require("fs");
const path = require("path");
const [candidate, packageDir, expectedVersion] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(path.join(packageDir, "package.json"), "utf8"));
const bin = typeof manifest.bin === "string" ? manifest.bin : manifest.bin?.["codex-acp"];
if (manifest.name !== "@agentclientprotocol/codex-acp" ||
    manifest.version !== expectedVersion || typeof bin !== "string" || path.isAbsolute(bin) ||
    fs.realpathSync(candidate) !== fs.realpathSync(path.join(packageDir, bin))) {
  process.exit(1);
}
NODE
}

discover_codex_adapter() {
  local candidate
  local -a candidates=(
    "$user_home/.local/lib/cosh/acp-adapters/node_modules/.bin/codex-acp"
    "$repo_root/scripts/acp-adapters/node_modules/.bin/codex-acp"
  )
  for candidate in "${candidates[@]}"; do
    if validate_codex_adapter "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

codex_adapter_prefix() {
  local target prefix
  target=$(readlink -f -- "$1") || return 1
  [[ "$target" == */node_modules/@agentclientprotocol/codex-acp/* ]] || return 1
  prefix=${target%%/node_modules/@agentclientprotocol/codex-acp/*}
  canonical_existing_dir "$prefix" "Codex adapter prefix"
}

validate_proxy_value() {
  local name="$1" value="$2"
  ! contains_control "$value" || die "$name contains a control character"
}

proxy_has_userinfo() {
  local value="$1" authority
  [[ "$value" == *://* ]] || return 1
  authority=${value#*://}
  authority=${authority%%/*}
  [[ "$authority" == *@* ]]
}

codex_provider_env_names() {
  local config="$codex_home/config.toml"
  [[ -e "$config" || -L "$config" ]] || return 0
  [[ -f "$config" && ! -L "$config" ]] || die "CODEX_HOME/config.toml must be a regular non-symlink file"
  command -v python3 >/dev/null 2>&1 || die "python3 is required to read CODEX_HOME/config.toml"
  python3 - "$config" <<'PY' || die "failed to read provider environment names from CODEX_HOME/config.toml"
import re
import sys
import tomllib

path = sys.argv[1]
try:
    with open(path, "rb") as stream:
        config = tomllib.load(stream)
except Exception:
    print("invalid Codex TOML configuration", file=sys.stderr)
    raise SystemExit(1)

providers = config.get("model_providers", {})
if not isinstance(providers, dict):
    print("model_providers must be a table", file=sys.stderr)
    raise SystemExit(1)

names = set()
pattern = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
denied_exact = {
    "BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS", "IFS", "PATH", "HOME",
    "USER", "LOGNAME", "SHELL", "NODE_OPTIONS", "NODE_PATH", "RUBYOPT",
    "PERL5OPT", "GCONV_PATH", "CODEX_HOME",
}
denied_prefixes = ("LD_", "DYLD_", "PYTHON", "COSH_GATEWAY_")

def add_name(name: object) -> None:
    if not isinstance(name, str) or not pattern.fullmatch(name):
        print("provider environment name is not a POSIX environment name", file=sys.stderr)
        raise SystemExit(1)
    if name in denied_exact or name.startswith(denied_prefixes):
        print("provider environment name can control the Gateway process", file=sys.stderr)
        raise SystemExit(1)
    names.add(name)

for provider in providers.values():
    if not isinstance(provider, dict):
        print("each model provider must be a table", file=sys.stderr)
        raise SystemExit(1)
    env_key = provider.get("env_key")
    if env_key is not None:
        add_name(env_key)
    headers = provider.get("env_http_headers", {})
    if not isinstance(headers, dict):
        print("provider env_http_headers must be a table", file=sys.stderr)
        raise SystemExit(1)
    for env_name in headers.values():
        add_name(env_name)

for name in sorted(names):
    print(name)
PY
}

is_sensitive_env_name() {
  local candidate="$1" sensitive
  for sensitive in "${SENSITIVE_ENV_NAMES[@]}"; do
    [[ "$candidate" == "$sensitive" ]] && return 0
  done
  return 1
}

codex_home_group_is_private() {
  local directory_gid current_gid other_primary group_members member
  directory_gid=$(stat -c '%g' -- "$codex_home")
  current_gid=$(id -g)
  [[ "$directory_gid" == "$current_gid" ]] || return 1
  other_primary=$(getent passwd 2>/dev/null | awk -F: -v gid="$directory_gid" -v user="$user_name" \
    '$4 == gid && $1 != user { print $1; exit }')
  [[ -z "$other_primary" ]] || return 1
  group_members=$(getent group "$directory_gid" 2>/dev/null | awk -F: 'NR == 1 { print $4 }')
  IFS=, read -r -a members <<<"$group_members"
  for member in "${members[@]}"; do
    [[ -z "$member" || "$member" == "$user_name" ]] || return 1
  done
}

validate_codex_home_security() {
  local mode
  [[ "$(stat -c '%u' -- "$codex_home")" == "$(id -u)" ]] || {
    codex_home_reason="CODEX_HOME is not owned by the current user"
    return 1
  }
  mode=$(stat -c '%a' -- "$codex_home")
  (( (8#$mode & 8#002) == 0 )) || die "CODEX_HOME must never be writable by other users"
  if (( (8#$mode & 8#020) != 0 )) && ! codex_home_group_is_private; then
    codex_home_reason="CODEX_HOME is writable by a non-private group"
    return 1
  fi
}

install_atomic() {
  local source="$1" target="$2" mode="$3" staged
  staged="${target}.new.$$"
  run sudo install -o root -g root -m "$mode" -- "$source" "$staged"
  run sudo mv -T -- "$staged" "$target"
}

validate_managed_targets() {
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    owned_file_has_marker "$unit_path" || die "refusing to replace unowned unit: $unit_path"
  fi
  if [[ -e "$env_path" || -L "$env_path" ]]; then
    owned_file_has_marker "$env_path" || die "refusing to replace unowned environment: $env_path"
    [[ "$(stat -c '%a' -- "$env_path")" == 600 ]] ||
      die "refusing to replace a dev environment that is not mode 0600"
  fi
  if [[ -e "$stage_dir" || -L "$stage_dir" ]]; then
    owned_stage_has_marker "$stage_dir" || die "refusing to replace unowned stage: $stage_dir"
  fi
  if [[ -e "$state_marker" || -L "$state_marker" ]]; then
    [[ -f "$state_marker" && ! -L "$state_marker" &&
       "$(stat -c '%u' -- "$state_marker")" == 0 &&
       "$(<"$state_marker")" == "$DEV_MARKER" ]] ||
      die "refusing to replace an unowned state marker: $state_marker"
  fi
}

validate_state_for_purge() {
  [[ -d "$state_dir" && ! -L "$state_dir" ]] || die "refusing to purge unsafe state path"
  [[ -f "$state_marker" && ! -L "$state_marker" ]] ||
    die "refusing to purge state without marker"
  [[ "$(<"$state_marker")" == "$DEV_MARKER" ]] ||
    die "refusing to purge state with an unknown marker"
  [[ "$(stat -c '%u' -- "$state_marker")" == 0 ]] ||
    die "refusing to purge state without a root-owned marker"
}

if [[ "${1:-}" == -h || "${1:-}" == --help ]]; then
  usage
  exit 0
fi

command_name="${1:-}"
[[ -n "$command_name" ]] || { usage >&2; exit 2; }
shift

workspace_input=""
codex_mode="auto"
environment_mode="inherit"
checkpoint_socket=""
no_build=false
dry_run=false
stop_production=false
purge_state=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --workspace)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      workspace_input="$2"
      shift 2
      ;;
    --codex)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      codex_mode="$2"
      shift 2
      ;;
    --environment)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      environment_mode="$2"
      shift 2
      ;;
    --checkpoint-socket)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      checkpoint_socket="$2"
      shift 2
      ;;
    --no-build) no_build=true; shift ;;
    --stop-production) stop_production=true; shift ;;
    --purge-state) purge_state=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

case "$command_name" in
  setup|shell|status|down|uninstall) ;;
  *) usage >&2; exit 2 ;;
esac

[[ "$(uname -s)" == Linux ]] || die "managed Task dev setup requires Linux"
[[ "$(id -u)" != 0 ]] || die "do not run this script directly as root; it uses sudo narrowly"
command -v systemctl >/dev/null 2>&1 || die "systemctl is required"
[[ "$dry_run" == true || -d /run/systemd/system ]] ||
  die "the system systemd manager is not running"
command -v sudo >/dev/null 2>&1 || die "sudo is required"

user_name=$(id -un)
[[ "$user_name" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,63}$ ]] || die "login name cannot be a systemd instance"
user_home=$(getent passwd "$(id -u)" | awk -F: 'NR == 1 { print $6 }')
user_home=$(canonical_existing_dir "$user_home" "login home")

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -- "$script_dir/.." && pwd -P)
unit_source="$script_dir/assets/systemd/$UNIT_TEMPLATE"
[[ -f "$repo_root/Cargo.toml" && -f "$unit_source" ]] || die "run the checked-in script from a cosh-ng source tree"

unit="cosh-gateway-dev@${user_name}.service"
production_unit="cosh-gateway@${user_name}.service"
legacy_unit="cosh-gateway-acp@${user_name}.service"
unit_path="/run/systemd/system/$UNIT_TEMPLATE"
env_path="/run/cosh-gateway-dev-${user_name}.env"
socket_path="/run/cosh-gateway-dev-${user_name}/gateway.sock"
client_cwd=$(dirname -- "$socket_path")
stage_dir="/usr/local/libexec/cosh-ng-dev/${user_name}"
state_dir="/var/lib/cosh-gateway-dev-${user_name}"
state_marker="/var/lib/.cosh-gateway-dev-${user_name}.managed-task-dev"

case "$command_name" in
  shell)
    [[ $# -eq 0 && "$no_build" == false && -z "$workspace_input" &&
       "$codex_mode" == auto && "$environment_mode" == inherit &&
       -z "$checkpoint_socket" && "$stop_production" == false &&
       "$purge_state" == false ]] || die "shell accepts only --dry-run"
    unit_is_active "$unit" || [[ "$dry_run" == true ]] || die "$unit is not active; run setup first"
    [[ -S "$socket_path" ]] || [[ "$dry_run" == true ]] || die "Gateway socket is unavailable: $socket_path"
    if [[ "$dry_run" == false ]]; then
      [[ "$(stat -c '%u' -- "$socket_path")" == "$(id -u)" ]] ||
        die "Gateway socket is not owned by the current user"
      owned_stage_has_marker "$stage_dir" || die "staged binaries are not marker-owned"
    fi
    [[ -x "$stage_dir/cosh-shell" ]] || [[ "$dry_run" == true ]] || die "staged cosh-shell is unavailable"
    printf 'Opening source-built shell against %s\n' "$unit"
    shell_path="$stage_dir:${PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
    if [[ "$dry_run" == true ]]; then
      quote_command cd -- "$client_cwd"
      quote_command env "PATH=$shell_path" \
        "COSH_GATEWAY_EXECUTABLE=$stage_dir/cosh-gateway" \
        "COSH_GATEWAY_SOCKET=$socket_path" "$stage_dir/cosh-shell" --isolated
    else
      # cosh-shell retains its launch cwd even when the embedded shell runs cd.
      # The isolated RuntimeDirectory keeps guarded Task rollback available.
      cd -- "$client_cwd"
      exec env "PATH=$shell_path" \
        "COSH_GATEWAY_EXECUTABLE=$stage_dir/cosh-gateway" \
        "COSH_GATEWAY_SOCKET=$socket_path" "$stage_dir/cosh-shell" --isolated
    fi
    ;;
  status)
    [[ "$no_build" == false && -z "$workspace_input" && "$codex_mode" == auto &&
       "$environment_mode" == inherit && -z "$checkpoint_socket" &&
       "$stop_production" == false && "$purge_state" == false ]] ||
      die "status accepts only --dry-run"
    run systemctl --no-pager --full status "$unit"
    if [[ "$dry_run" == true || -S "$socket_path" ]]; then
      run "$stage_dir/cosh-gateway" task --socket "$socket_path" --output jsonl capabilities
    fi
    ;;
  down)
    [[ "$no_build" == false && -z "$workspace_input" && "$codex_mode" == auto &&
       "$environment_mode" == inherit && -z "$checkpoint_socket" &&
       "$stop_production" == false && "$purge_state" == false ]] ||
      die "down accepts only --dry-run"
    if unit_is_active "$unit" || [[ "$dry_run" == true ]]; then
      run sudo systemctl stop "$unit"
    else
      printf '%s is already down.\n' "$unit"
    fi
    ;;
  uninstall)
    [[ "$no_build" == false && -z "$workspace_input" && "$codex_mode" == auto &&
       "$environment_mode" == inherit && -z "$checkpoint_socket" &&
       "$stop_production" == false ]] || die "uninstall accepts only --purge-state and --dry-run"
    [[ ! -e "$env_path" && ! -L "$env_path" ]] ||
      owned_file_has_marker "$env_path" || die "refusing to remove unowned environment: $env_path"
    [[ ! -e "$stage_dir" && ! -L "$stage_dir" ]] ||
      owned_stage_has_marker "$stage_dir" || die "refusing to remove unowned stage: $stage_dir"
    [[ ! -e "$unit_path" && ! -L "$unit_path" ]] ||
      owned_file_has_marker "$unit_path" || die "refusing to remove unowned unit: $unit_path"
    if [[ "$purge_state" == true && ( -e "$state_dir" || -L "$state_dir" ) ]]; then
      validate_state_for_purge
    fi
    if unit_is_active "$unit" || [[ -e "$unit_path" || -L "$unit_path" ]]; then
      run sudo systemctl stop "$unit"
    fi
    run sudo rm -f -- "$env_path"
    if [[ -e "$stage_dir" || -L "$stage_dir" || "$dry_run" == true ]]; then
      run sudo rm -rf -- "$stage_dir"
    fi
    if [[ "$purge_state" == true ]]; then
      run sudo rm -rf -- "$state_dir"
      run sudo rm -f -- "$state_marker"
    fi
    # The transient template is shared; remove it only when no dev instance is active.
    if ! systemctl list-units --type=service --state=active --no-legend \
         'cosh-gateway-dev@*.service' 2>/dev/null | grep -q 'cosh-gateway-dev@'; then
      run sudo rm -f -- "$unit_path"
    fi
    run sudo systemctl daemon-reload
    printf 'Removed dev unit environment and stage.\n'
    if [[ "$purge_state" == false ]]; then
      printf 'Preserved Task state at %s (use --purge-state to remove it).\n' "$state_dir"
    fi
    printf 'Workspace, CODEX_HOME, and ACP adapters were not changed.\n'
    ;;
  setup)
    [[ "$codex_mode" == auto || "$codex_mode" == off || "$codex_mode" == required ]] ||
      die "--codex must be auto, off, or required"
    [[ "$environment_mode" == inherit || "$environment_mode" == off ]] ||
      die "--environment must be inherit or off"
    [[ "$purge_state" == false ]] || die "--purge-state is valid only with uninstall"

    if [[ -z "$workspace_input" ]]; then
      workspace=$(pwd -P)
    else
      workspace=$(canonical_existing_dir "$workspace_input" "workspace")
    fi
    require_safe_absolute_path "$workspace" "workspace"
    [[ "$workspace" != "$user_home" ]] || die "workspace must not be the entire login home"

    codex_skip_reason=""
    codex_home="${CODEX_HOME:-$user_home/.codex}"
    if [[ "$codex_mode" != off ]]; then
      if [[ -d "$codex_home" && ! -L "$codex_home" ]]; then
        codex_home=$(canonical_existing_dir "$codex_home" "CODEX_HOME")
      elif [[ "$codex_mode" == required ]]; then
        die "CODEX_HOME must be an existing canonical directory when Codex is required"
      else
        codex_home=""
        codex_skip_reason="CODEX_HOME is not an existing canonical directory"
      fi
    fi
    runtime_dir="/run/cosh-gateway-dev-${user_name}"
    for protected in "$stage_dir" "$state_dir" "/etc/cosh" "/run/systemd/system" "$runtime_dir"; do
      ! paths_overlap "$workspace" "$protected" || die "workspace overlaps a managed dev path"
    done
    if [[ -n "$codex_home" ]]; then
      ! paths_overlap "$workspace" "$codex_home" || die "workspace overlaps CODEX_HOME"
    fi

    adapter=""
    if [[ "$codex_mode" != off && -n "$codex_home" ]]; then
      if ! validate_codex_home_security; then
        if [[ "$codex_mode" == required ]]; then
          die "$codex_home_reason"
        fi
        codex_home=""
      fi
    fi
    if [[ "$codex_mode" != off && -n "$codex_home" ]]; then
      adapter=$(discover_codex_adapter || true)
      [[ "$adapter" != *[[:space:]]* ]] || die "Codex adapter path must not contain whitespace"
      if [[ -n "$adapter" ]]; then
        adapter_prefix=$(codex_adapter_prefix "$adapter") || die "Codex adapter prefix is invalid"
        if paths_overlap "$workspace" "$adapter_prefix"; then
          if [[ "$codex_mode" == required ]]; then
            die "workspace overlaps the Codex adapter prefix"
          fi
          adapter=""
          codex_skip_reason="workspace overlaps the Codex adapter prefix"
        fi
      fi
    fi
    if [[ -z "$adapter" && "$codex_mode" == required ]]; then
      die "pinned codex-acp $CODEX_VERSION was not found; run scripts/install-acp-adapters.sh"
    fi

    if [[ -n "$checkpoint_socket" ]]; then
      require_safe_absolute_path "$checkpoint_socket" "checkpoint socket"
      [[ "$checkpoint_socket" != *[[:space:]]* ]] || die "checkpoint socket path must not contain whitespace"
      [[ -S "$checkpoint_socket" ]] || [[ "$dry_run" == true ]] ||
        die "checkpoint socket does not exist or is not a Unix socket"
    fi

    production_active=false
    legacy_active=false
    unit_is_active "$production_unit" && production_active=true
    unit_is_active "$legacy_unit" && legacy_active=true
    if [[ "$production_active" == true || "$legacy_active" == true ]]; then
      [[ "$stop_production" == true ]] ||
        die "a production Gateway is active; rerun with --stop-production to migrate explicitly"
      if [[ "$production_active" == true ]]; then
        run sudo systemctl stop "$production_unit"
      fi
      if [[ "$legacy_active" == true ]]; then
        run sudo systemctl stop "$legacy_unit"
      fi
    fi

    if unit_is_active "$unit"; then
      run sudo systemctl stop "$unit"
    fi

    validate_managed_targets
    if [[ "$no_build" == false ]]; then
      run cargo build --locked --manifest-path "$repo_root/Cargo.toml" \
        -p cosh-gateway-app -p cosh-core -p cosh-shell
    fi
    for binary in cosh-gateway cosh-core cosh-shell; do
      [[ -f "$repo_root/target/debug/$binary" && ! -L "$repo_root/target/debug/$binary" &&
         -x "$repo_root/target/debug/$binary" ]] ||
        [[ "$dry_run" == true && "$no_build" == false ]] ||
        die "debug binary must be an executable regular non-symlink file: target/debug/$binary"
    done

    temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/cosh-managed-task-dev.XXXXXX")
    trap 'rm -rf -- "$temp_dir"' EXIT
    env_file="$temp_dir/gateway.env"
    marker_file="$temp_dir/marker"
    {
      printf '# %s\n' "$DEV_MARKER"
      env_line COSH_GATEWAY_WORKSPACE "$workspace"
      if [[ -n "$adapter" ]]; then
        node_path=$(readlink -f -- "$(command -v node)")
        env_line CODEX_HOME "$codex_home"
        env_line PATH "$(dirname -- "$node_path"):/usr/bin:/bin"
        env_line COSH_GATEWAY_ACP_ARG "--acp-adapter=$adapter"
      fi
      if [[ -n "$checkpoint_socket" ]]; then
        env_line COSH_GATEWAY_CHECKPOINT_ARG "--checkpoint-socket=$checkpoint_socket"
      fi
      inherited=()
      sensitive_inherited=()
      declare -A inherited_seen=()
      if [[ "$environment_mode" == inherit ]]; then
        provider_env_names=()
        if [[ -n "$adapter" ]]; then
          if ! provider_env_output=$(codex_provider_env_names); then
            die "Codex environment inheritance could not be resolved"
          fi
          if [[ -n "$provider_env_output" ]]; then
            mapfile -t provider_env_names <<<"$provider_env_output"
          fi
        fi
        declare -A provider_env_seen=()
        for provider_env_name in "${provider_env_names[@]}"; do
          provider_env_seen["$provider_env_name"]=1
        done
        environment_names=("${PROXY_NAMES[@]}")
        if [[ -n "$adapter" ]]; then
          environment_names+=("${CODEX_RUNTIME_ENV_NAMES[@]}" "${provider_env_names[@]}")
        fi
        for environment_name in "${environment_names[@]}"; do
          if [[ -n "${!environment_name-}" && -z "${inherited_seen[$environment_name]-}" ]]; then
            case "$environment_name" in
              HTTP_PROXY|HTTPS_PROXY|ALL_PROXY|NO_PROXY|http_proxy|https_proxy|all_proxy|no_proxy)
                validate_proxy_value "$environment_name" "${!environment_name}"
                ;;
            esac
            env_line "$environment_name" "${!environment_name}"
            inherited+=("$environment_name")
            inherited_seen["$environment_name"]=1
            if is_sensitive_env_name "$environment_name" ||
               [[ -n "${provider_env_seen[$environment_name]-}" ]] ||
               { [[ "$environment_name" == *_PROXY || "$environment_name" == *_proxy ]] &&
                 proxy_has_userinfo "${!environment_name}"; }; then
              sensitive_inherited+=("$environment_name")
            fi
          fi
        done
      fi
    } >"$env_file"
    printf '%s\n' "$DEV_MARKER" >"$marker_file"
    chmod 0600 "$env_file" "$marker_file"

    run sudo install -d -o root -g root -m 0755 -- "$(dirname -- "$stage_dir")"
    run sudo install -d -o root -g root -m 0755 -- "$stage_dir"
    install_atomic "$marker_file" "$stage_dir/.managed-task-dev" 0644
    for binary in cosh-gateway cosh-core cosh-shell; do
      install_atomic "$repo_root/target/debug/$binary" "$stage_dir/$binary" 0755
    done
    install_atomic "$unit_source" "$unit_path" 0644
    install_atomic "$env_file" "$env_path" 0600
    run sudo systemctl daemon-reload
    run sudo systemctl start "$unit"
    install_atomic "$marker_file" "$state_marker" 0444

    if [[ "$dry_run" == false ]]; then
      if ! wait_for_gateway_capabilities; then
        sudo systemctl stop "$unit" || true
        die "Gateway started but the capabilities smoke check failed; the dev unit was stopped"
      fi
    else
      quote_command "$stage_dir/cosh-gateway" task --socket "$socket_path" --output jsonl capabilities
    fi

    printf 'Source dev Gateway is ready: %s\n' "$unit"
    printf 'Workspace: %s\n' "$workspace"
    if [[ -n "$adapter" ]]; then
      printf 'Codex: pinned adapter enabled; CODEX_HOME preserved\n'
    else
      printf 'Codex: disabled or pinned adapter not found; Core remains available\n'
      if [[ -n "$codex_skip_reason" ]]; then
        printf 'Codex auto-discovery skipped: %s\n' "$codex_skip_reason"
      fi
    fi
    if [[ "$environment_mode" == inherit && ${#inherited[@]} -gt 0 ]]; then
      printf 'Environment: inherited variable names: %s\n' "${inherited[*]}"
    else
      printf 'Environment: no variables inherited\n'
    fi
    if [[ ${#sensitive_inherited[@]} -gt 0 ]]; then
      printf 'WARNING: credentials were snapshotted into the root-owned mode 0600 Gateway/Adapter environment and may be readable by same-UID processes: %s\n' \
        "${sensitive_inherited[*]}"
    fi
    if [[ -n "$checkpoint_socket" ]]; then
      printf 'Checkpoint: configured\n'
    else
      printf 'Checkpoint: not configured; Auto records a downgrade and On fails closed\n'
    fi
    printf 'WARNING: managed Tasks default to allow_all; Codex workspace access is not sandboxed.\n'
    printf 'Transient setup: unit/env are below /run; rerun setup after reboot.\n'
    printf 'Next: %s shell\n' "$0"
    ;;
esac
