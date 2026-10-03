#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

args=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --apikey)
      args+=(--api-key "${2:?missing value for --apikey}")
      shift 2
      ;;
    --provider)
      provider="${2:?missing value for --provider}"
      case "$provider" in
        aliyun-mode|aliyun|bailian|dashscope|payg|pay-as-you-go)
          args+=(--billing payg)
          ;;
        coding|coding-plan)
          args+=(--billing coding)
          ;;
        token|token-plan)
          args+=(--billing token)
          ;;
        *)
          args+=(--provider-id "$provider")
          ;;
      esac
      shift 2
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

# ${args[@]+...} guard: "${args[@]}" on an empty array is an unbound-variable
# error under `set -u` on bash < 4.4 (e.g. macOS /bin/bash 3.2, Alinux 3's 4.2).
exec python3 "$SCRIPT_DIR/install_openclaw.py" ${args[@]+"${args[@]}"}
