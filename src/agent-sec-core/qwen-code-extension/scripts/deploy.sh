#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
EXTENSION_DIR="${1:-${DEFAULT_EXTENSION_DIR}}"
QWEN_BIN="${QWEN_BIN:-qwen}"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

require_command() {
    local command_name="$1"
    command -v "${command_name}" >/dev/null 2>&1 || fail "${command_name} is not available in PATH"
}

absolute_path() {
    python3 -c \
        'import os, sys; print(os.path.abspath(os.path.expanduser(sys.argv[1])))' \
        "$1"
}

json_field() {
    local json_path="$1"
    local field_name="$2"
    python3 -c \
        'import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream).get(sys.argv[2], "")
print(value if isinstance(value, str) else "")' \
        "${json_path}" "${field_name}"
}

require_command python3
require_command "${QWEN_BIN}"
require_command agent-sec-cli

EXTENSION_DIR="$(absolute_path "${EXTENSION_DIR}")"
MANIFEST_PATH="${EXTENSION_DIR}/qwen-extension.json"
OBSERVABILITY_HOOK_PATH="${EXTENSION_DIR}/hooks/observability_hook.py"
SKILL_LEDGER_HOOK_PATH="${EXTENSION_DIR}/hooks/skill_ledger_hook.py"
CODE_SCANNER_HOOK_PATH="${EXTENSION_DIR}/hooks/code_scanner_hook.py"
TRACE_CONTEXT_PATH="${EXTENSION_DIR}/hooks/trace_context.py"

[[ -f "${MANIFEST_PATH}" ]] || fail "missing extension manifest: ${MANIFEST_PATH}"
[[ -f "${OBSERVABILITY_HOOK_PATH}" ]] || fail \
    "missing observability hook: ${OBSERVABILITY_HOOK_PATH}"
[[ -f "${SKILL_LEDGER_HOOK_PATH}" ]] || fail \
    "missing skill-ledger hook: ${SKILL_LEDGER_HOOK_PATH}"
[[ -x "${SKILL_LEDGER_HOOK_PATH}" ]] || fail \
    "skill-ledger hook is not executable: ${SKILL_LEDGER_HOOK_PATH}"
[[ -f "${CODE_SCANNER_HOOK_PATH}" ]] || fail \
    "missing code scanner hook: ${CODE_SCANNER_HOOK_PATH}"
[[ -f "${TRACE_CONTEXT_PATH}" ]] || fail \
    "missing trace-context helper: ${TRACE_CONTEXT_PATH}"

EXTENSION_NAME="$(json_field "${MANIFEST_PATH}" name)"
EXTENSION_VERSION="$(json_field "${MANIFEST_PATH}" version)"
[[ -n "${EXTENSION_NAME}" ]] || fail "qwen-extension.json must define a string name"
[[ -n "${EXTENSION_VERSION}" ]] || fail "qwen-extension.json must define a string version"

QWEN_HOME_DIR="$(absolute_path "${QWEN_HOME:-${HOME}/.qwen}")"
TARGET_DIR="${QWEN_HOME_DIR}/extensions/${EXTENSION_NAME}"
TARGET_MANIFEST="${TARGET_DIR}/qwen-extension.json"
INSTALL_METADATA="${TARGET_DIR}/.qwen-extension-install.json"

if [[ -d "${TARGET_DIR}" ]]; then
    [[ -f "${TARGET_MANIFEST}" ]] || fail \
        "existing extension is not a copied local install; uninstall it before deploying"
    [[ -f "${INSTALL_METADATA}" ]] || fail \
        "existing extension has no install metadata; uninstall it before deploying"

    INSTALL_TYPE="$(json_field "${INSTALL_METADATA}" type)"
    INSTALL_SOURCE="$(json_field "${INSTALL_METADATA}" source)"
    [[ "${INSTALL_TYPE}" == "local" ]] || fail \
        "existing extension uses install type '${INSTALL_TYPE:-unknown}'; uninstall it before deploying"
    [[ -n "${INSTALL_SOURCE}" ]] || fail "existing extension install metadata has no source"

    INSTALL_SOURCE="$(absolute_path "${INSTALL_SOURCE}")"
    [[ "${INSTALL_SOURCE}" == "${EXTENSION_DIR}" ]] || fail \
        "existing extension points to ${INSTALL_SOURCE}; uninstall it before deploying from ${EXTENSION_DIR}"

    INSTALLED_VERSION="$(json_field "${TARGET_MANIFEST}" version)"
    if [[ "${INSTALLED_VERSION}" == "${EXTENSION_VERSION}" ]]; then
        echo "Qwen Code extension ${EXTENSION_NAME} ${EXTENSION_VERSION} is already installed."
    else
        echo "Updating ${EXTENSION_NAME}: ${INSTALLED_VERSION:-unknown} -> ${EXTENSION_VERSION}"
        "${QWEN_BIN}" extensions update "${EXTENSION_NAME}"
    fi
else
    echo "Installing ${EXTENSION_NAME} ${EXTENSION_VERSION} from ${EXTENSION_DIR}"
    "${QWEN_BIN}" extensions install "${EXTENSION_DIR}" --consent --scope user
fi

[[ -f "${TARGET_MANIFEST}" ]] || fail "Qwen Code did not create ${TARGET_MANIFEST}"
DEPLOYED_NAME="$(json_field "${TARGET_MANIFEST}" name)"
DEPLOYED_VERSION="$(json_field "${TARGET_MANIFEST}" version)"
[[ "${DEPLOYED_NAME}" == "${EXTENSION_NAME}" ]] || fail \
    "deployed extension name is '${DEPLOYED_NAME}', expected '${EXTENSION_NAME}'"
[[ "${DEPLOYED_VERSION}" == "${EXTENSION_VERSION}" ]] || fail \
    "deployed extension version is '${DEPLOYED_VERSION}', expected '${EXTENSION_VERSION}'"

"${QWEN_BIN}" extensions enable --scope user "${EXTENSION_NAME}"

echo "Deployed ${EXTENSION_NAME} ${EXTENSION_VERSION} to ${TARGET_DIR}"
echo "Restart running Qwen Code sessions to load the extension."
