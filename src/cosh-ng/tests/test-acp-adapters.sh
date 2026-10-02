#!/usr/bin/env bash
set -euo pipefail

component_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
installer="$component_root/scripts/install-acp-adapters.sh"
conformance="$component_root/scripts/run-acp-conformance.sh"
temp_root=$(mktemp -d)
trap 'rm -rf -- "$temp_root"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

expect_exit() {
  local expected="$1"
  shift
  set +e
  "$@" >/dev/null 2>&1
  local actual=$?
  set -e
  [[ "$actual" == "$expected" ]] || fail "expected exit $expected, got $actual: $*"
}

fake_bin="$temp_root/bin"
mkdir -m 0700 "$fake_bin"
fake_npm="$fake_bin/npm"
cat >"$fake_npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

[[ "${1:-}" == ci ]] || exit 90
shift
prefix=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      prefix="$2"
      shift 2
      ;;
    --omit=dev|--ignore-scripts|--no-audit|--no-fund)
      shift
      ;;
    *) exit 91 ;;
  esac
done
[[ -n "$prefix" ]] || exit 92

create_package() {
  local scope="$1"
  local package="$2"
  local version="$3"
  local command_name="$4"
  local package_dir="$prefix/node_modules/$scope/$package"
  mkdir -p "$package_dir/dist" "$prefix/node_modules/.bin"
  cat >"$package_dir/package.json" <<JSON
{"name":"$scope/$package","version":"$version","bin":{"$command_name":"dist/index.js"}}
JSON
  cat >"$package_dir/dist/index.js" <<'JS'
#!/usr/bin/env node
process.exit(0);
JS
  chmod 0755 "$package_dir/dist/index.js"
  ln -s "../$scope/$package/dist/index.js" "$prefix/node_modules/.bin/$command_name"
}

codex_version="1.6.2"
[[ "${FAKE_BAD_CODEX_VERSION:-0}" == 1 ]] && codex_version="9.9.9"
create_package @agentclientprotocol codex-acp "$codex_version" codex-acp
create_package @agentclientprotocol claude-agent-acp 0.66.0 claude-agent-acp
SH
chmod 0700 "$fake_npm"

expect_exit 2 "$installer" --prefix relative
expect_exit 2 "$installer" --prefix /

public_prefix="$temp_root/public"
mkdir -m 0755 "$public_prefix"
expect_exit 2 env PATH="$fake_bin:$PATH" "$installer" --prefix "$public_prefix"

unmanaged_prefix="$temp_root/unmanaged"
mkdir -m 0700 "$unmanaged_prefix"
touch "$unmanaged_prefix/unrelated"
expect_exit 2 env PATH="$fake_bin:$PATH" "$installer" --prefix "$unmanaged_prefix"

managed_prefix="$temp_root/managed"
env PATH="$fake_bin:$PATH" "$installer" --prefix "$managed_prefix" >/dev/null
[[ "$(stat -c '%a' "$managed_prefix")" == 700 ]] || fail "managed prefix is not private"
[[ -f "$managed_prefix/.cosh-acp-adapters" ]] || fail "installation marker is missing"
[[ -x "$managed_prefix/node_modules/.bin/codex-acp" ]] || fail "codex adapter is missing"
[[ -x "$managed_prefix/node_modules/.bin/claude-agent-acp" ]] || fail "claude adapter is missing"

bad_prefix="$temp_root/bad-version"
expect_exit 1 env PATH="$fake_bin:$PATH" FAKE_BAD_CODEX_VERSION=1 \
  "$installer" --prefix "$bad_prefix"

workspace="$temp_root/workspace"
mkdir -m 0700 "$workspace"
gateway_stub="$temp_root/cosh-gateway"
cat >"$gateway_stub" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_GATEWAY_ERROR:-0}" == 1 ]]; then
  printf '%s\n' \
    '{"event":"error","code":"bad\u001b[31m","message":"failed\u001b]8;;https://example.invalid\u0007link"}'
  exit 12
fi
case "$1" in
  doctor)
    printf '%s\n' \
      '{"event":"initialized"}' \
      '{"event":"session_opened"}' \
      '{"event":"terminal"}' \
      '{"event":"doctor_ok"}'
    ;;
  run)
    cat >/dev/null
    case "${COSH_ACP_FAKE_SCENARIO:-real}" in
      typed_error)
        printf '%s\n' \
          '{"event":"initialized"}' \
          '{"event":"session_opened"}' \
          '{"event":"session_update","text":"partial"}' \
          '{"event":"request_failed","code":-32000}' \
          '{"event":"error","code":"agent_incomplete","message":"failed"}'
        exit 1
        ;;
      rpc_error)
        printf '%s\n' \
          '{"event":"initialized"}' \
          '{"event":"session_opened"}' \
          '{"event":"session_update","text":"partial"}' \
          '{"event":"request_failed","code":-32603}' \
          '{"event":"error","code":"agent_incomplete","message":"failed"}'
        exit 1
        ;;
      *)
        printf '%s\n' \
          '{"event":"initialized"}' \
          '{"event":"session_opened"}' \
          '{"event":"session_update","text":"never echoed"}' \
          '{"event":"session_update","text":"never persisted"}' \
          '{"event":"prompt_finished"}' \
          '{"event":"terminal"}'
        ;;
    esac
    ;;
  *) exit 93 ;;
esac
SH
chmod 0700 "$gateway_stub"
set +e
diagnostic=$(FAKE_GATEWAY_ERROR=1 "$conformance" fake \
  --gateway "$gateway_stub" --workspace "$workspace" 2>&1 >/dev/null)
diagnostic_exit=$?
set -e
[[ "$diagnostic_exit" != 0 ]] || fail "control-bearing diagnostic unexpectedly passed"
[[ "$diagnostic" == *'\u001b'* && "$diagnostic" == *'\u0007'* ]] || \
  fail "control-bearing diagnostic was not escaped"
[[ "$diagnostic" != *$'\033'* && "$diagnostic" != *$'\007'* ]] || \
  fail "control-bearing diagnostic reached the terminal"
expect_exit 2 "$conformance" real \
  --gateway "$gateway_stub" \
  --workspace "$workspace" \
  --profile codex \
  --adapter "$managed_prefix/node_modules/.bin/codex-acp"
printf '%s\n' 'private prompt' | "$conformance" real \
  --gateway "$gateway_stub" \
  --workspace "$workspace" \
  --profile codex \
  --adapter "$managed_prefix/node_modules/.bin/codex-acp" \
  --acknowledge-provider-run >/dev/null

if [[ -n "${COSH_GATEWAY_BIN:-}" ]]; then
  "$conformance" fake \
    --gateway "$COSH_GATEWAY_BIN" \
    --workspace "$workspace" >/dev/null
fi

echo "ACP adapter installer tests: PASS"
