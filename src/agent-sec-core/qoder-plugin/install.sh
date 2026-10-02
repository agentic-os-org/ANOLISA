#!/usr/bin/env bash
# Install or remove the agent-sec-core Qoder CLI plugin.

set -euo pipefail

PLUGIN_NAME="agent-sec-core"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="${SCRIPT_DIR}"
SCOPE="user"
ACTION="install"

usage() {
    cat <<'EOF'
Usage:
  install.sh [--scope user|project|local]
  install.sh --remove [--scope user|project|local]
EOF
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

require_qodercli() {
    command -v qodercli >/dev/null 2>&1 || die "qodercli not found on PATH"
    qodercli plugins --help >/dev/null 2>&1 || die "qodercli found, but it does not support plugins; install a plugin-capable Qoder CLI"
}

require_install_command() {
    qodercli plugins install --help >/dev/null 2>&1 || die "qodercli plugins install is unavailable; upgrade Qoder CLI"
}

require_python_runtime() {
    command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
    python3 - >/dev/null 2>&1 <<'PY' || die "python3 >= 3.11 and < 3.12 is required"
import sys

raise SystemExit(0 if (3, 11) <= sys.version_info < (3, 12) else 1)
PY
}

require_agent_sec_cli() {
    command -v agent-sec-cli >/dev/null 2>&1 || die "agent-sec-cli not found on PATH"
    agent-sec-cli scan-pii --help >/dev/null 2>&1 || die "agent-sec-cli scan-pii is unavailable"
    agent-sec-cli skill-ledger check --help >/dev/null 2>&1 || die "agent-sec-cli skill-ledger check is unavailable"
    agent-sec-cli observability record --help >/dev/null 2>&1 || die "agent-sec-cli observability record is unavailable"
    agent-sec-cli scan-code --help >/dev/null 2>&1 || die "agent-sec-cli scan-code is unavailable"
}

require_plugin_files() {
    [[ -f "${PLUGIN_DIR}/.qoder-plugin/plugin.json" ]] || die "plugin manifest not found"
    [[ -f "${PLUGIN_DIR}/hooks/hooks.json" ]] || die "hook configuration not found"
    [[ -f "${PLUGIN_DIR}/hooks/qoder_hook_common.py" ]] || die "shared hook helper not found"
    [[ -f "${PLUGIN_DIR}/hooks/pii_checker_hook.py" ]] || die "PII hook not found"
    [[ -f "${PLUGIN_DIR}/hooks/observability_hook.py" ]] || die "observability hook not found"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --remove|--uninstall|-r)
            ACTION="remove"
            shift
            ;;
        --scope|-s)
            [[ $# -ge 2 ]] || die "--scope requires a value"
            SCOPE="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

case "$SCOPE" in
    user|project|local) ;;
    *) die "unsupported scope: $SCOPE" ;;
esac

require_qodercli

if [[ "$ACTION" == "remove" ]]; then
    qodercli plugins uninstall "$PLUGIN_NAME" --scope "$SCOPE"
    echo "Removed ${PLUGIN_NAME} from Qoder CLI (${SCOPE} scope)."
else
    require_install_command
    require_python_runtime
    require_agent_sec_cli
    require_plugin_files
    if qodercli plugins validate --help >/dev/null 2>&1; then
        qodercli plugins validate "$PLUGIN_DIR"
    else
        echo "WARNING: qodercli plugins validate is unavailable; continuing after install capability check."
    fi
    qodercli plugins install "$PLUGIN_DIR" --scope "$SCOPE"
    echo "Installed ${PLUGIN_NAME} for Qoder CLI (${SCOPE} scope)."
    echo "Restart Qoder CLI or run /plugins reload to apply changes."
fi
