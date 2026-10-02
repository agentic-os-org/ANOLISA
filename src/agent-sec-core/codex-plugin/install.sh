#!/usr/bin/env bash
# ============================================================================
# agent-sec-core Codex 插件一键安装脚本
#
# 用法：
#   bash install.sh          # 自动注册 marketplace 并安装插件
#   bash install.sh --remove # 卸载插件
#
# 前置条件：
#   1. codex CLI 已安装且在 PATH 中
#   2. agent-sec-cli 已安装且在 PATH 中
# ============================================================================

set -euo pipefail

# -- 常量 ------------------------------------------------------------------

PLUGIN_NAME="agent-sec-core"
MARKETPLACE_NAME="agent-sec"
INSTALL_ID="${PLUGIN_NAME}@${MARKETPLACE_NAME}"

# -- 自动获取路径 ------------------------------------------------------------

# 脚本所在目录即为 codex-plugin 目录（marketplace 根目录）
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MARKETPLACE_DIR="${SCRIPT_DIR}"

# -- 颜色输出 ----------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# -- 前置检查 ----------------------------------------------------------------

preflight_check() {
    local has_error=0

    if ! command -v codex &>/dev/null; then
        error "codex 命令未找到，请先安装 Codex CLI"
        has_error=1
    fi

    if ! command -v agent-sec-cli &>/dev/null; then
        error "agent-sec-cli 命令未找到，请先安装 agent-sec-core"
        has_error=1
    fi

    if [[ ! -f "${MARKETPLACE_DIR}/.agents/plugins/marketplace.json" ]]; then
        error "marketplace.json 不存在于 ${MARKETPLACE_DIR}/.agents/plugins/"
        error "请确保此脚本位于 codex-plugin 目录内"
        has_error=1
    fi

    if [[ ${has_error} -ne 0 ]]; then
        exit 1
    fi
}

# -- 安装 --------------------------------------------------------------------

do_install() {
    info "开始安装 agent-sec-core Codex 插件..."
    info "Marketplace 目录: ${MARKETPLACE_DIR}"
    echo ""

    # 步骤 1: 注册 marketplace
    info "[1/2] 注册 marketplace..."
    if codex plugin marketplace add "${MARKETPLACE_DIR}" 2>&1; then
        info "  ✅ Marketplace 注册成功"
    else
        # 如果已注册，尝试继续
        warn "  Marketplace 可能已注册，继续执行..."
    fi

    echo ""

    # 步骤 2: 安装插件
    info "[2/2] 安装插件 ${INSTALL_ID}..."
    if codex plugin add "${INSTALL_ID}" 2>&1; then
        info "  ✅ 插件安装成功"
    else
        warn "  插件可能已安装，请检查错误信息"
    fi

    echo ""
    echo "============================================"
    info "安装完成！"
    echo ""
    info "⚠️  首次启动 Codex 时会弹出 Hook Trust Review 界面"
    info "   请选择信任 (Trust) 以下 hook 使其生效："
    info "   - code_scanner_hook.py   (PreToolUse/Bash)"
    info "   - prompt_scanner_hook.py (UserPromptSubmit)"
    info "   - pii_checker_hook.py    (UserPromptSubmit + PostToolUse + PreToolUse)"
    info "   - skill_ledger_hook.py   (UserPromptSubmit)"
    info "   - observability_hook.py  (UserPromptSubmit + PreToolUse + PostToolUse + Stop)"
    echo ""
    info "启动命令（可选环境变量）："
    info "  CODE_SCANNER_MODE=block PROMPT_SCANNER_MODE=deny SKILL_LEDGER_MODE=block PII_CHECKER_MODE=block codex"
    info ""
    info "  CODE_SCANNER_MODE    - 代码扫描模式（observe/block，deny 兼容 block）"
    info "  PROMPT_SCANNER_MODE  - 提示词注入检测透出模式"
    info "  SKILL_LEDGER_MODE   - Skill Ledger hook policy（默认 ask）"
    info "  PII_CHECKER_MODE    - PII Checker hook policy（默认 observe）"
    info ""
    info "  Hook policy 可选值: observe | warn | ask | block"
    echo "============================================"
}

# -- 卸载 --------------------------------------------------------------------

do_remove() {
    info "开始卸载 agent-sec-core Codex 插件..."

    # 卸载插件
    info "[1/2] 卸载插件..."
    if codex plugin remove "${PLUGIN_NAME}" 2>&1; then
        info "  ✅ 插件已卸载"
    else
        warn "  插件可能未安装"
    fi

    # 移除 marketplace
    info "[2/2] 移除 marketplace..."
    if codex plugin marketplace remove "${MARKETPLACE_NAME}" 2>&1; then
        info "  ✅ Marketplace 已移除"
    else
        warn "  Marketplace 可能未注册"
    fi

    echo ""
    info "卸载完成"
}

# -- 主入口 ------------------------------------------------------------------

main() {
    preflight_check

    case "${1:-}" in
        --remove|--uninstall|-r)
            do_remove
            ;;
        --help|-h)
            echo "用法："
            echo "  bash install.sh          安装插件"
            echo "  bash install.sh --remove 卸载插件"
            echo "  bash install.sh --help   显示帮助"
            ;;
        *)
            do_install
            ;;
    esac
}

main "$@"
