#!/usr/bin/env bash
# =============================================================================
# Deploy agent-sec OpenClaw plugin
#
# Usage:
#   ./scripts/deploy.sh [PLUGIN_DIR]
#
# Supports:
#   - Fresh install
#   - Upgrade (using openclaw plugins install --force)
#   - Multi-plugin coexistence
#
# NOTE: This script ONLY registers the plugin with openclaw config.
#       It does NOT start/stop openclaw-gateway. Use systemd or manually
#       restart the service after deployment.
# =============================================================================

set -euo pipefail

PLUGIN_ID="agent-sec"
MIN_OPENCLAW_VERSION="2026.4.14"
MIN_CONVERSATION_ACCESS_VERSION="2026.4.24"
OPENCLAW_INSTALL_HELP=""
OPENCLAW_INSTALL_SUPPORTS_UNSAFE=0
OPENCLAW_INSTALL_REQUIRES_UNSAFE=0
OPENCLAW_INSTALL_SUPPORTS_ACCEPT_CAPABILITIES=0
OPENCLAW_INSPECT_HELP=""
OPENCLAW_INSPECT_SUPPORTS_RUNTIME=0
VERIFY_RUNTIME_TMPDIR=""
VERIFY_RUNTIME_TMPDIR_PREFIX="agent-sec-openclaw-inspect."
DEPLOY_TMPDIR_MARKER=".agent-sec-openclaw-deploy-owner"
DEPLOY_TMPDIR_OWNER="agent-sec-openclaw-deploy:${BASHPID:-$$}"

# Default PLUGIN_DIR: resolve relative to this script's location (scripts/ -> parent)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="${1:-$(dirname "$SCRIPT_DIR")}"

# Convert to absolute path if relative
PLUGIN_DIR="$(cd "$PLUGIN_DIR" && pwd)"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-${OPENCLAW_HOME:-}}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"

openclaw_cli() {
    if [[ -n "$OPENCLAW_STATE_DIR" ]]; then
        env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" openclaw "$@"
    else
        env -u OPENCLAW_HOME openclaw "$@"
    fi
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

cleanup_verify_runtime_tmpdir() {
    local tmpdir="${VERIFY_RUNTIME_TMPDIR:-}"

    VERIFY_RUNTIME_TMPDIR=""
    cleanup_owned_tmpdir "$tmpdir" "$VERIFY_RUNTIME_TMPDIR_PREFIX"
}

trap cleanup_verify_runtime_tmpdir EXIT

create_owned_tmpdir() {
    local prefix="$1"
    local tmp_parent="${TMPDIR:-/tmp}"
    local tmpdir

    if [[ "$tmp_parent" != /* || ! -d "$tmp_parent" ]]; then
        tmp_parent="/tmp"
    fi
    tmp_parent="${tmp_parent%/}"
    [[ -n "$tmp_parent" ]] || tmp_parent="/"

    tmpdir="$(mktemp -d "${tmp_parent}/${prefix}XXXXXXXXXX")"
    printf '%s\n' "$DEPLOY_TMPDIR_OWNER" >"${tmpdir}/${DEPLOY_TMPDIR_MARKER}"
    printf '%s\n' "$tmpdir"
}

cleanup_owned_tmpdir() {
    local tmpdir="$1"
    local prefix="$2"
    local tmpbase
    local marker
    local marker_owner

    tmpdir="${tmpdir%/}"
    [[ -n "$tmpdir" ]] || return 0

    tmpbase="${tmpdir##*/}"
    marker="${tmpdir}/${DEPLOY_TMPDIR_MARKER}"
    if [[ ! -d "$tmpdir" || "$tmpbase" != "$prefix"* || ! -f "$marker" ]]; then
        return 0
    fi

    marker_owner="$(<"$marker")" || return 0
    [[ "$marker_owner" == "$DEPLOY_TMPDIR_OWNER" ]] || return 0

    rm -rf -- "$tmpdir" || true
}

create_verify_runtime_tmpdir() {
    create_owned_tmpdir "$VERIFY_RUNTIME_TMPDIR_PREFIX"
}

extract_openclaw_version() {
    local raw="$1"

    printf '%s\n' "$raw" | grep -Eo '[0-9]{4}\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?' | head -n 1 || true
}

version_core() {
    local version="$1"

    version="${version%%+*}"
    version="${version%%-*}"
    printf '%s\n' "$version"
}

version_has_prerelease() {
    local version="$1"
    local without_build="${version%%+*}"

    [[ "$without_build" == *-* ]]
}

version_ge() {
    local current="$1"
    local minimum="$2"
    local current_year current_month current_patch
    local minimum_year minimum_month minimum_patch

    IFS=. read -r current_year current_month current_patch <<<"$(version_core "$current")"
    IFS=. read -r minimum_year minimum_month minimum_patch <<<"$(version_core "$minimum")"

    current_year="${current_year:-0}"
    current_month="${current_month:-0}"
    current_patch="${current_patch:-0}"
    minimum_year="${minimum_year:-0}"
    minimum_month="${minimum_month:-0}"
    minimum_patch="${minimum_patch:-0}"

    if (( current_year != minimum_year )); then
        (( current_year > minimum_year ))
        return
    fi
    if (( current_month != minimum_month )); then
        (( current_month > minimum_month ))
        return
    fi
    if (( current_patch != minimum_patch )); then
        (( current_patch > minimum_patch ))
        return
    fi

    if version_has_prerelease "$current" && ! version_has_prerelease "$minimum"; then
        return 1
    fi
    return 0
}

detect_openclaw_version() {
    local raw_version

    raw_version="$(openclaw_cli --version 2>/dev/null)" || die "openclaw 不在 PATH 中或无法执行"
    extract_openclaw_version "$raw_version"
}

verify_openclaw_compatibility() {
    local openclaw_version="$1"

    [[ -n "$openclaw_version" ]] || die "无法识别 OpenClaw 版本。请升级到 >=${MIN_OPENCLAW_VERSION} 后重试。"

    if ! version_ge "$openclaw_version" "$MIN_OPENCLAW_VERSION"; then
        die "agent-sec OpenClaw 插件要求 OpenClaw >=${MIN_OPENCLAW_VERSION}，当前为 ${openclaw_version}。请升级 OpenClaw，或降级使用支持旧 OpenClaw 的 agent-sec 插件包。"
    fi
}

verify_openclaw_install_cli() {
    local install_help_normalized

    OPENCLAW_INSTALL_HELP="$(openclaw_cli plugins install --help 2>&1)" || die "无法读取 openclaw plugins install --help，请升级 OpenClaw 到 >=${MIN_OPENCLAW_VERSION} 后重试。"
    install_help_normalized="$OPENCLAW_INSTALL_HELP"
    install_help_normalized="${install_help_normalized//$'\n'/ }"
    install_help_normalized="${install_help_normalized//$'\t'/ }"
    while [[ "$install_help_normalized" == *"  "* ]]; do
        install_help_normalized="${install_help_normalized//  / }"
    done

    [[ "$install_help_normalized" == *"--force"* ]] || die "当前 OpenClaw plugins install 不支持 --force。请升级 OpenClaw 到 >=${MIN_OPENCLAW_VERSION} 后重试。"
    if [[ "$install_help_normalized" == *"--dangerously-force-unsafe-install"* ]]; then
        OPENCLAW_INSTALL_SUPPORTS_UNSAFE=1
    fi
    if [[ "$install_help_normalized" == *"--accept-capabilities"* ]]; then
        OPENCLAW_INSTALL_SUPPORTS_ACCEPT_CAPABILITIES=1
    fi

    # Treat help output as the CLI contract: if the current OpenClaw installer
    # advertises the compatibility flag, pass it on the first install attempt
    # so deploy.sh remains a single-shot install command for all supported hosts.
    if [[ "$OPENCLAW_INSTALL_SUPPORTS_UNSAFE" == "1" ]]; then
        OPENCLAW_INSTALL_REQUIRES_UNSAFE=1
    fi
}

verify_openclaw_inspect_cli() {
    OPENCLAW_INSPECT_HELP="$(openclaw_cli plugins inspect --help 2>&1)" || die "无法读取 openclaw plugins inspect --help，请升级 OpenClaw 到 >=${MIN_OPENCLAW_VERSION} 后重试。"

    [[ "$OPENCLAW_INSPECT_HELP" == *"--json"* ]] || die "当前 OpenClaw plugins inspect 不支持 --json。请升级 OpenClaw 到 >=${MIN_OPENCLAW_VERSION} 后重试。"
    if [[ "$OPENCLAW_INSPECT_HELP" == *"--runtime"* ]]; then
        OPENCLAW_INSPECT_SUPPORTS_RUNTIME=1
    fi
}

install_plugin() {
    local install_args=("plugins" "install" "$PLUGIN_DIR" "--force")

    if [[ "$OPENCLAW_INSTALL_SUPPORTS_ACCEPT_CAPABILITIES" == "1" ]]; then
        echo "安装策略: OpenClaw ${OPENCLAW_VERSION_DETECTED} 安装器暴露 --accept-capabilities，将接受插件声明的 capabilities（含 hook 激活）。"
        install_args+=("--accept-capabilities")
    fi

    if [[ "$OPENCLAW_INSTALL_REQUIRES_UNSAFE" == "1" ]]; then
        echo "安装策略: OpenClaw ${OPENCLAW_VERSION_DETECTED} 安装器暴露 legacy --dangerously-force-unsafe-install，首次安装将使用该兼容参数。"
        echo "安装策略: deploy.sh 按当前 CLI help 暴露的参数执行，不基于版本或文案语义推断。"
        install_args+=("--dangerously-force-unsafe-install")
    else
        echo "安装策略: OpenClaw ${OPENCLAW_VERSION_DETECTED} 安装器未暴露 legacy --dangerously-force-unsafe-install。"
    fi

    openclaw_cli "${install_args[@]}"
}

configure_conversation_access() {
    local access_config_key="plugins.entries.agent-sec.hooks.allowConversationAccess"

    if version_ge "$OPENCLAW_VERSION_DETECTED" "$MIN_CONVERSATION_ACCESS_VERSION"; then
        echo "允许 agent-sec 检查大模型输入输出安全"
        echo "  openclaw config set ${access_config_key} true"
        openclaw_cli config set "$access_config_key" true
        return
    fi

    echo "OpenClaw ${OPENCLAW_VERSION_DETECTED} 不支持插件会话访问配置"
    echo "  跳过 plugins.entries.agent-sec.hooks.allowConversationAccess=true (OpenClaw ${MIN_CONVERSATION_ACCESS_VERSION} 引入, #71221)"
    echo "  llm_input/llm_output/agent_end 会话观测 hook 在当前 OpenClaw 版本中不可用"
}

verify_runtime_loaded() {
    local inspect_args=("plugins" "inspect" "$PLUGIN_ID" "--json")
    local inspect_label="openclaw plugins inspect ${PLUGIN_ID} --json"
    local inspect_tmpdir
    local inspect_stdout
    local inspect_stderr
    local inspect_json
    local jq_stderr
    local runtime_inspect_json
    local runtime_status

    if [[ "$OPENCLAW_INSPECT_SUPPORTS_RUNTIME" == "1" ]]; then
        inspect_args=("plugins" "inspect" "$PLUGIN_ID" "--runtime" "--json")
        inspect_label="openclaw plugins inspect ${PLUGIN_ID} --runtime --json"
    fi

    cleanup_verify_runtime_tmpdir
    inspect_tmpdir="$(create_verify_runtime_tmpdir)"
    VERIFY_RUNTIME_TMPDIR="$inspect_tmpdir"
    inspect_stdout="$inspect_tmpdir/inspect.stdout"
    inspect_stderr="$inspect_tmpdir/inspect.stderr"
    inspect_json="$inspect_tmpdir/inspect.json"
    jq_stderr="$inspect_tmpdir/jq.stderr"

    if ! openclaw_cli "${inspect_args[@]}" >"$inspect_stdout" 2>"$inspect_stderr"; then
        print_inspect_debug "$inspect_label" "$inspect_stdout" "$inspect_stderr" "$jq_stderr"
        die "插件已安装，但 ${inspect_label} 失败。请运行: ${inspect_label}"
    fi

    if ! extract_json_object_from_output "$inspect_stdout" "$inspect_json"; then
        print_inspect_debug "$inspect_label" "$inspect_stdout" "$inspect_stderr" "$jq_stderr"
        die "插件已安装，但 ${inspect_label} 输出中未找到 JSON 对象。请运行: ${inspect_label}"
    fi

    if ! runtime_status="$(jq -r '.plugin.status // "unknown"' <"$inspect_json" 2>"$jq_stderr")"; then
        print_inspect_debug "$inspect_label" "$inspect_stdout" "$inspect_stderr" "$jq_stderr"
        die "插件已安装，但 ${inspect_label} 输出不是可解析 JSON。请运行: ${inspect_label}"
    fi
    runtime_inspect_json="$(cat "$inspect_json")"

    if [[ "$runtime_status" != "loaded" ]]; then
        printf '%s\n' "$runtime_inspect_json" | jq -r '.diagnostics[]?.message' >&2
        die "插件已安装，但 ${inspect_label} 状态为 ${runtime_status}，未达到 loaded。请运行: ${inspect_label}"
    fi

    cleanup_verify_runtime_tmpdir
}

extract_json_object_from_output() {
    local input_file="$1"
    local output_file="$2"

    # OpenClaw 2026.4.x may print plugin registration diagnostics to stdout
    # before the requested --json payload. Keep deploy.sh compatible with that
    # host behavior while still parsing the official JSON object with jq.
    sed -n '/^[[:space:]]*{/,$p' "$input_file" >"$output_file"
    [[ -s "$output_file" ]]
}

print_debug_file_preview() {
    local title="$1"
    local file="$2"
    local bytes

    bytes="$(wc -c <"$file" 2>/dev/null || printf '0')"
    echo "----- ${title} (${bytes} bytes, first 160 lines) -----" >&2
    if [[ -s "$file" ]]; then
        sed -n '1,160p' "$file" >&2
    else
        echo "<empty>" >&2
    fi
}

print_inspect_debug() {
    local inspect_label="$1"
    local stdout_file="$2"
    local stderr_file="$3"
    local jq_stderr_file="$4"

    echo "DEBUG: ${inspect_label} raw output follows for compatibility analysis." >&2
    print_debug_file_preview "inspect stdout" "$stdout_file"
    print_debug_file_preview "inspect stderr" "$stderr_file"
    print_debug_file_preview "jq stderr" "$jq_stderr_file"
}

# 1. 前置检查
command -v agent-sec-cli >/dev/null 2>&1 || die "agent-sec-cli 不在 PATH 中"
command -v jq >/dev/null 2>&1 || die "jq 不在 PATH 中"
[[ -f "$PLUGIN_DIR/openclaw.plugin.json" ]] || die "清单文件不存在: $PLUGIN_DIR/openclaw.plugin.json"
[[ -d "$PLUGIN_DIR/dist" ]] || die "dist/ 不存在,请先运行 npm run build"
[[ -f "$PLUGIN_DIR/dist/index.js" ]] || die "dist/index.js 不存在,请先运行 npm run build"

OPENCLAW_VERSION_DETECTED="$(detect_openclaw_version)"
verify_openclaw_compatibility "$OPENCLAW_VERSION_DETECTED"
verify_openclaw_install_cli
verify_openclaw_inspect_cli

PLUGIN_VERSION=$(jq -r '.version' "$PLUGIN_DIR/openclaw.plugin.json")
echo "部署插件: agent-sec v${PLUGIN_VERSION}"
echo "  路径: $PLUGIN_DIR"
echo "  OpenClaw: ${OPENCLAW_VERSION_DETECTED}"

# 2. 使用官方命令安装插件
echo ""
echo "安装插件..."
install_plugin

echo "  ✓ 插件已安装/更新"
configure_conversation_access

echo ""
echo "校验插件安装和运行时加载..."
verify_runtime_loaded
echo "  ✓ OpenClaw 已记录插件 ${PLUGIN_ID}"
if [[ "$OPENCLAW_INSPECT_SUPPORTS_RUNTIME" == "1" ]]; then
    echo "  ✓ openclaw plugins inspect ${PLUGIN_ID} --runtime --json 已证明插件可加载"
else
    echo "  ✓ openclaw plugins inspect ${PLUGIN_ID} --json 已证明插件可加载"
fi

echo ""
echo "提示: 请重启 OpenClaw gateway 以加载插件"
echo "  openclaw gateway restart"
echo ""
echo "拦截 prompt 注入风险请求"
echo "  openclaw config set plugins.entries.agent-sec.config.promptScanBlock true"
echo "开启代码扫描审批模式（默认放行+日志记录）"
echo "  openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true"
echo "启用 PII deny 前置阻断（默认放行+日志记录）"
echo "  openclaw config set 'plugins.entries.agent-sec.config.capabilities.pii-scan-user-input.enableBlock' true"
echo "将 Skill Ledger 设置为直接阻断（默认为 ask 审批）"
echo "  openclaw config set 'plugins.entries.agent-sec.config.capabilities.skill-ledger.policy' block"
