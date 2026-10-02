#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
script="$repo_root/scripts/managed-task-dev.sh"
unit="$repo_root/scripts/assets/systemd/cosh-gateway-dev@.service"
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/cosh-managed-task-dev-test.XXXXXX")
trap 'rm -rf -- "$temp_root"' EXIT

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

assert_contains() {
  local file="$1" expected="$2"
  grep -Fq -- "$expected" "$file" || fail "missing output: $expected"
}

assert_not_contains() {
  local file="$1" unexpected="$2"
  if grep -Fq -- "$unexpected" "$file"; then
    fail "unexpected output: $unexpected"
  fi
}

run_ok() {
  local output="$1"
  shift
  if ! "$@" >"$output" 2>&1; then
    sed -n '1,160p' "$output" >&2
    fail "command unexpectedly failed"
  fi
}

run_fail() {
  local output="$1"
  shift
  if "$@" >"$output" 2>&1; then
    sed -n '1,160p' "$output" >&2
    fail "command unexpectedly succeeded"
  fi
}

mock_bin="$temp_root/bin"
mock_home="$temp_root/home"
workspace="$temp_root/workspace"
mkdir -p "$mock_bin" "$mock_home" "$workspace"

cat >"$mock_bin/getent" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  passwd)
    printf '%s:x:%s:%s:Developer:%s:/bin/bash\n' "$(id -un)" "$(id -u)" "$(id -g)" "$MOCK_HOME"
    ;;
  group)
    printf 'dev:x:%s:%s\n' "$(id -g)" "${MOCK_GROUP_MEMBERS:-}"
    ;;
  *) exit 2 ;;
esac
EOF
cat >"$mock_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == is-active ]]; then
  unit="${3:-}"
  if [[ "${MOCK_PRODUCTION_ACTIVE:-0}" == 1 && "$unit" == cosh-gateway@*.service ]]; then
    exit 0
  fi
  if [[ "${MOCK_LEGACY_ACTIVE:-0}" == 1 && "$unit" == cosh-gateway-acp@*.service ]]; then
    exit 0
  fi
  if [[ "${MOCK_DEV_ACTIVE:-0}" == 1 && "$unit" == cosh-gateway-dev@*.service ]]; then
    exit 0
  fi
  exit 3
fi
exit 0
EOF
cat >"$mock_bin/node" <<'EOF'
#!/usr/bin/env bash
[[ "${1:-}" == - && $# -eq 4 ]] || exit 2
python3 - "$2" "$3" "$4" <<'PY'
import json
import os
import sys

candidate, package_dir, expected_version = sys.argv[1:]
with open(os.path.join(package_dir, "package.json"), encoding="utf-8") as stream:
    manifest = json.load(stream)
binary = manifest.get("bin")
if isinstance(binary, dict):
    binary = binary.get("codex-acp")
valid = (
    manifest.get("name") == "@agentclientprotocol/codex-acp"
    and manifest.get("version") == expected_version
    and isinstance(binary, str)
    and not os.path.isabs(binary)
    and os.path.realpath(candidate) == os.path.realpath(os.path.join(package_dir, binary))
)
raise SystemExit(0 if valid else 1)
PY
EOF
chmod +x "$mock_bin/getent" "$mock_bin/systemctl" "$mock_bin/node"

test_path="$mock_bin:$PATH"
base_env=(env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME=)

default_output="$temp_root/default.out"
run_ok "$default_output" "${base_env[@]}" "$script" setup --dry-run --workspace "$workspace"
assert_contains "$default_output" 'cargo build --locked'
assert_contains "$default_output" '/usr/local/libexec/cosh-ng-dev/'
assert_contains "$default_output" '/run/systemd/system/cosh-gateway-dev@.service'
assert_contains "$default_output" 'Checkpoint: not configured; Auto records a downgrade and On fails closed'
assert_contains "$default_output" 'allow_all'
assert_contains "$default_output" 'not sandboxed'
assert_contains "$default_output" 'rerun setup after reboot'

escaped_workspace="$temp_root/work space \"quoted\""
mkdir "$escaped_workspace"
escaping_output="$temp_root/escaping.out"
run_ok "$escaping_output" "${base_env[@]}" "$script" setup --dry-run \
  --codex off --environment off --workspace "$escaped_workspace"
assert_contains "$escaping_output" "Workspace: $escaped_workspace"

invalid_output="$temp_root/invalid.out"
run_fail "$invalid_output" "${base_env[@]}" "$script" setup --dry-run --workspace relative/path
assert_contains "$invalid_output" 'workspace must be an absolute non-root path'
run_fail "$invalid_output" "${base_env[@]}" "$script" setup --dry-run --workspace "$mock_home"
assert_contains "$invalid_output" 'workspace must not be the entire login home'

mkdir -m 0700 "$mock_home/.codex"
required_output="$temp_root/required.out"
run_fail "$required_output" "${base_env[@]}" "$script" setup --dry-run \
  --codex required --environment off --workspace "$workspace"
assert_contains "$required_output" 'pinned codex-acp 1.6.2 was not found'

adapter_prefix="$mock_home/.local/lib/cosh/acp-adapters"
adapter_package="$adapter_prefix/node_modules/@agentclientprotocol/codex-acp"
mkdir -p "$adapter_prefix/node_modules/.bin" "$adapter_package/dist"
cat >"$adapter_package/package.json" <<'EOF'
{"name":"@agentclientprotocol/codex-acp","version":"1.6.2","bin":{"codex-acp":"dist/index.js"}}
EOF
cat >"$adapter_package/dist/index.js" <<'EOF'
#!/usr/bin/env node
EOF
chmod +x "$adapter_package/dist/index.js"
ln -s ../@agentclientprotocol/codex-acp/dist/index.js \
  "$adapter_prefix/node_modules/.bin/codex-acp"

chmod 0775 "$mock_home/.codex"
cat >"$mock_home/.codex/config.toml" <<'EOF'
[model_providers.custom]
env_key = "CUSTOM_PROVIDER_TOKEN"
env_http_headers = { "X-Custom" = "CUSTOM_PROVIDER_HEADER" }
EOF
private_group_output="$temp_root/private-group.out"
run_ok "$private_group_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  CODEX_API_KEY='stable-api-secret' OPENAI_API_KEY='openai-secret' \
  CUSTOM_PROVIDER_TOKEN='provider-secret' CUSTOM_PROVIDER_HEADER='header-secret' \
  NPM_TOKEN='installer-secret' CODEX_CI='internal-ci-secret' \
  CODEX_REMOTE_PAYLOAD='internal-payload-secret' SSH_AUTH_SOCK='/tmp/ssh-secret.sock' \
  LD_LIBRARY_PATH='/tmp/ld-secret' \
  "$script" setup --dry-run --codex auto --workspace "$workspace"
assert_contains "$private_group_output" 'Codex: pinned adapter enabled'
assert_contains "$private_group_output" 'Environment: inherited variable names: CODEX_API_KEY OPENAI_API_KEY CUSTOM_PROVIDER_HEADER CUSTOM_PROVIDER_TOKEN'
assert_contains "$private_group_output" 'credentials were snapshotted into the root-owned mode 0600 Gateway/Adapter environment and may be readable by same-UID processes: CODEX_API_KEY OPENAI_API_KEY CUSTOM_PROVIDER_HEADER CUSTOM_PROVIDER_TOKEN'
assert_not_contains "$private_group_output" 'stable-api-secret'
assert_not_contains "$private_group_output" 'provider-secret'
assert_not_contains "$private_group_output" 'header-secret'
assert_not_contains "$private_group_output" 'openai-secret'
assert_not_contains "$private_group_output" 'installer-secret'
assert_not_contains "$private_group_output" 'NPM_TOKEN'
assert_not_contains "$private_group_output" 'CODEX_CI'
assert_not_contains "$private_group_output" 'CODEX_REMOTE_PAYLOAD'
assert_not_contains "$private_group_output" 'SSH_AUTH_SOCK'
assert_not_contains "$private_group_output" 'LD_LIBRARY_PATH'

cat >"$mock_home/.codex/config.toml" <<'EOF'
[model_providers.custom]
env_key = "INVALID-NAME"
EOF
provider_error="$temp_root/provider-error.out"
run_fail "$provider_error" "${base_env[@]}" "$script" setup --dry-run \
  --codex auto --workspace "$workspace"
assert_contains "$provider_error" 'Codex environment inheritance could not be resolved'
cat >"$mock_home/.codex/config.toml" <<'EOF'
[model_providers.custom]
env_key = "LD_PRELOAD"
EOF
run_fail "$provider_error" "${base_env[@]}" "$script" setup --dry-run \
  --codex auto --workspace "$workspace"
assert_contains "$provider_error" 'Codex environment inheritance could not be resolved'
cat >"$mock_home/.codex/config.toml" <<'EOF'
[model_providers.custom]
env_key = "CUSTOM_PROVIDER_TOKEN"
env_http_headers = { "X-Custom" = "CUSTOM_PROVIDER_HEADER" }
EOF

overlap_output="$temp_root/adapter-overlap.out"
run_ok "$overlap_output" "${base_env[@]}" "$script" setup --dry-run \
  --codex auto --environment off --workspace "$adapter_prefix"
assert_contains "$overlap_output" 'Codex auto-discovery skipped: workspace overlaps the Codex adapter prefix'
run_fail "$overlap_output" "${base_env[@]}" "$script" setup --dry-run \
  --codex required --environment off --workspace "$adapter_prefix"
assert_contains "$overlap_output" 'workspace overlaps the Codex adapter prefix'

proxy_output="$temp_root/proxy.out"
proxy_secret='http://proxy.example.invalid:7443/secret-token'
run_ok "$proxy_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  HTTPS_PROXY="$proxy_secret" https_proxy="$proxy_secret" HTTP_PROXY= http_proxy= \
  ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  "$script" setup --dry-run --codex off --workspace "$workspace"
assert_contains "$proxy_output" 'Environment: inherited variable names: HTTPS_PROXY https_proxy'
assert_not_contains "$proxy_output" 'secret-token'
assert_not_contains "$proxy_output" 'proxy.example.invalid'

proxy_auth_output="$temp_root/proxy-auth.out"
run_ok "$proxy_auth_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  HTTPS_PROXY='http://alice:password@proxy.example.invalid:7443' https_proxy= \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  "$script" setup --dry-run --codex off --workspace "$workspace"
assert_contains "$proxy_auth_output" 'Environment: inherited variable names: HTTPS_PROXY'
assert_contains "$proxy_auth_output" 'credentials were snapshotted into the root-owned mode 0600 Gateway/Adapter environment and may be readable by same-UID processes: HTTPS_PROXY'
assert_not_contains "$proxy_auth_output" 'password'
assert_not_contains "$proxy_auth_output" 'alice'

pair_output="$temp_root/pair.out"
run_ok "$pair_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  HTTPS_PROXY='http://one.invalid:1' https_proxy='http://two.invalid:2' \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  "$script" setup --dry-run --codex off --workspace "$workspace"
assert_contains "$pair_output" 'Environment: inherited variable names: HTTPS_PROXY https_proxy'
assert_not_contains "$pair_output" 'one.invalid'
assert_not_contains "$pair_output" 'two.invalid'

core_only_output="$temp_root/core-only.out"
run_ok "$core_only_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  CODEX_API_KEY='unused-core-only-secret' \
  "$script" setup --dry-run --codex off --workspace "$workspace"
assert_contains "$core_only_output" 'Environment: no variables inherited'
assert_not_contains "$core_only_output" 'CODEX_API_KEY'
assert_not_contains "$core_only_output" 'unused-core-only-secret'
assert_not_contains "$core_only_output" 'credentials were snapshotted'

control_error="$temp_root/control-error.out"
run_fail "$control_error" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  RUST_LOG=$'info\nunsafe' "$script" setup --dry-run --codex auto --workspace "$workspace"
assert_contains "$control_error" 'RUST_LOG contains a control character'

active_output="$temp_root/active.out"
run_fail "$active_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  MOCK_PRODUCTION_ACTIVE=1 "$script" setup --dry-run --codex off --environment off \
  --workspace "$workspace"
assert_contains "$active_output" 'rerun with --stop-production'
run_ok "$active_output" env -i PATH="$test_path" MOCK_HOME="$mock_home" CODEX_HOME= \
  MOCK_PRODUCTION_ACTIVE=1 "$script" setup --dry-run --codex off --environment off \
  --stop-production --workspace "$workspace"
assert_contains "$active_output" 'systemctl stop cosh-gateway@'
assert_not_contains "$active_output" 'systemctl stop cosh-gateway-acp@'

uninstall_output="$temp_root/uninstall.out"
run_ok "$uninstall_output" "${base_env[@]}" "$script" uninstall --dry-run
assert_contains "$uninstall_output" 'Preserved Task state at /var/lib/cosh-gateway-dev-'
assert_contains "$uninstall_output" 'Workspace, CODEX_HOME, and ACP adapters were not changed.'
assert_not_contains "$uninstall_output" 'rm -rf -- /var/lib/cosh-gateway-dev-'

run_ok "$temp_root/status.out" "${base_env[@]}" "$script" status --dry-run
assert_contains "$temp_root/status.out" 'cosh-gateway-dev@'
run_ok "$temp_root/down.out" "${base_env[@]}" "$script" down --dry-run
assert_contains "$temp_root/down.out" 'systemctl stop cosh-gateway-dev@'
run_ok "$temp_root/shell.out" "${base_env[@]}" "$script" shell --dry-run
assert_contains "$temp_root/shell.out" 'COSH_GATEWAY_SOCKET=/run/cosh-gateway-dev-'
assert_contains "$temp_root/shell.out" 'PATH=/usr/local/libexec/cosh-ng-dev/'
assert_contains "$temp_root/shell.out" 'cd -- /run/cosh-gateway-dev-'

run_ok "$temp_root/help.out" "$script" --help
assert_contains "$temp_root/help.out" '--no-build'
assert_contains "$temp_root/help.out" '--environment inherit|off'
assert_not_contains "$temp_root/help.out" '--proxy'

grep -Fqx '# cosh-ng-managed-task-dev-v1' "$unit" || fail 'unit marker is missing'
grep -Fqx 'Conflicts=cosh-gateway@%i.service cosh-gateway-acp@%i.service' "$unit" ||
  fail 'unit production conflicts are missing'
grep -Fqx 'EnvironmentFile=/run/cosh-gateway-dev-%i.env' "$unit" ||
  fail 'unit does not use the transient environment'
grep -Fqx 'KillMode=control-group' "$unit" || fail 'unit containment is incomplete'
grep -Fqx 'SendSIGKILL=yes' "$unit" || fail 'unit containment is incomplete'
grep -Fqx 'FinalKillSignal=SIGKILL' "$unit" || fail 'unit containment is incomplete'
grep -Fqx 'Delegate=no' "$unit" || fail 'unit containment is incomplete'
grep -Fqx 'Restart=on-failure' "$unit" || fail 'unit containment is incomplete'
grep -Fq "stat -c '%u' -- \"\$path/.managed-task-dev\"" "$script" ||
  fail 'stage marker ownership validation is missing'
grep -Fq "stat -c '%a' -- \"\$path\"" "$script" ||
  fail 'stage mode validation is missing'
grep -Fq "! -L \"\$repo_root/target/debug/\$binary\"" "$script" ||
  fail 'source binary symlink rejection is missing'
grep -Fq -- '--output jsonl capabilities' "$script" ||
  fail 'Gateway capabilities smoke does not use the supported JSONL output'
if grep -Fq -- '--output json capabilities' "$script"; then
  fail 'Gateway capabilities smoke still uses the unsupported JSON output'
fi
grep -Fq 'wait_for_gateway_capabilities' "$script" ||
  fail 'Gateway setup does not wait for daemon readiness'
grep -Fq "unit_is_active \"\$unit\" || return 1" "$script" ||
  fail 'Gateway readiness wait does not fail when the unit exits'

printf 'managed-task-dev tests passed\n'
