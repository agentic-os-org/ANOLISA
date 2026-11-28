#!/usr/bin/env bash
# Run SDK authentication and a synthetic model request in a loopback namespace.
set -euo pipefail
if [[ $# != 4 || $EUID == 0 ]]; then
  echo "usage (non-root): $0 VENV_PYTHON QODER_SDK_ENTRY PRIVATE_TOKEN_FILE OUTPUT_DIR" >&2
  exit 2
fi
runner_python=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
qoder_sdk=$(realpath "$2")
token_file=$(realpath -s "$3")
output_root=$(realpath -m "$4")
source_root=$(cd "$(dirname "$0")/.." && pwd)
fixture_user=$(id -un)
umask 077
mkdir "$output_root"
broker_socket="$output_root/auth.sock"
"$runner_python" "$source_root/fixtures/sdk_transport_auth.py" \
  --socket "$broker_socket" --audit "$output_root/auth-audit.jsonl" >/dev/null 2>&1 &
broker_pid=$!
cleanup() {
  kill "$broker_pid" 2>/dev/null || true
  wait "$broker_pid" 2>/dev/null || true
  rm -f "$broker_socket"
  rm -rf -- "$output_root/results/qoder/home" "$output_root/results/qoder/tmp"
}
trap cleanup EXIT
for ((attempt = 0; attempt < 100; attempt++)); do
  [[ -S "$broker_socket" ]] && break
  kill -0 "$broker_pid"
  sleep 0.02
done
[[ -S "$broker_socket" ]]
sudo -n unshare --net sh -c \
  'ip link set lo up; fixture_user=$1; shift; exec runuser -u "$fixture_user" -- "$@"' \
  sdk-transport "$fixture_user" "$runner_python" \
  "$source_root/scripts/sdk_transport_cases.py" \
  --qoder-sdk "$qoder_sdk" --qoder-token-file "$token_file" \
  --auth-broker "$broker_socket" --output "$output_root/results" --case qoder
