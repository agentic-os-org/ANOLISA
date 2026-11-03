#!/bin/bash
# AgentSight 单测覆盖率一键获取脚本
# 用法: ./test.sh [dashboard|rust|all]

set -e

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODE="${1:-all}"

# 颜色
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

print_header() {
  echo ""
  echo -e "${BLUE}════════════════════════════════════════════════════════════${NC}"
  echo -e "${BLUE}  $1${NC}"
  echo -e "${BLUE}════════════════════════════════════════════════════════════${NC}"
  echo ""
}

# ─── Dashboard 前端覆盖率 ───────────────────────────────────────────────────────

# 全局统计变量
DASHBOARD_COVERAGE=""
DASHBOARD_TESTS=0
RUST_COVERAGE=""
RUST_TESTS=0

# 前端套件以 node:test 编写，并通过 dashboard/package.json 的 npm scripts 串联。
# 这里曾调用 vitest，但它不是本包的依赖（只有 package-lock.json 里残留着一条记录），
# 命令替换的 "command not found" 在 set -e 下直接终止脚本，前端一个用例都没跑，
# 后端的 tarpaulin 分支也因此永远不会执行。
DASHBOARD_SUITES="test:api-client test:i18n test:navigation test:stale-load \
test:round-model test:token-savings-selection test:agent-sessions-subagent-count \
test:product-branding test:agent-health-notifier"

run_dashboard() {
  print_header "Dashboard 前端单测 (node:test)"

  cd "$ROOT_DIR/dashboard"

  if [ ! -d "node_modules" ]; then
    echo -e "${YELLOW}安装依赖...${NC}"
    npm install --silent
  fi

  echo "运行测试..."
  echo ""
  local output=""
  local failures=""
  local suite
  for suite in $DASHBOARD_SUITES; do
    local result
    if result=$(npm run --silent "$suite" 2>&1); then
      :
    else
      failures="${failures} ${suite}"
    fi
    output="${output}${result}"$'\n'
  done
  echo "$output"

  # node:test 的汇总行形如 "ℹ pass 16"
  DASHBOARD_TESTS=$(echo "$output" | grep -oP '^\s*ℹ pass \K\d+' | awk '{s+=$1} END {print s+0}')
  # `[ -z x ] && x=0` is an AND list whose failure status aborts the script
  # under `set -e`, so it only survives while the variable is empty.
  if [ -z "$DASHBOARD_TESTS" ]; then
    DASHBOARD_TESTS=0
  fi

  if [ -n "$failures" ]; then
    echo -e "${RED}失败的前端套件:${failures}${NC}"
    DASHBOARD_COVERAGE="N/A"
  else
    # 前端没有配置覆盖率工具。如实报告，而不是让一个已删除的运行器给出数字。
    DASHBOARD_COVERAGE="N/A"
  fi

  cd "$ROOT_DIR"
}

# ─── Rust 后端覆盖率 ──────────────────────────────────────────────────────────

run_rust() {
  print_header "Rust 后端单测覆盖率 (cargo-tarpaulin)"

  cd "$ROOT_DIR"

  # 检查 tarpaulin 是否安装
  if ! command -v cargo-tarpaulin &>/dev/null; then
    echo -e "${YELLOW}安装 cargo-tarpaulin...${NC}"
    cargo install cargo-tarpaulin
  fi

  echo "运行测试并收集覆盖率（仅可测试的纯逻辑模块）..."
  echo -e "${YELLOW}提示: 首次运行较慢（需重新编译），后续会利用缓存加速${NC}"
  echo ""

  START_TIME=$(date +%s)

  # 运行 tarpaulin
  # --skip-clean: 不清理构建缓存
  # --timeout 180: 单个测试超时时间
  # --jobs: 并行编译任务数
  # --exclude-files: 排除不可单测的模块（eBPF/FFI/bin/server/存储/IO密集型）
  OUTPUT=$(cargo tarpaulin \
    --skip-clean \
    --timeout 180 \
    --jobs $(nproc) \
    --exclude-files \
      "src/bpf/*" "target/*" \
      "src/probes/*" \
      "src/ffi.rs" \
      "src/unified.rs" \
      "src/bin/*" \
      "src/server/*" \
      "src/storage/*" \
      "src/health/*" \
      "src/genai/sls.rs" \
      "src/genai/storage.rs" \
      "src/genai/exporter.rs" \
      "src/genai/mod.rs" \
      "src/tokenizer/llm_tok.rs" \
      "src/tokenizer/multi_model.rs" \
      "src/tokenizer/mod.rs" \
      "src/aggregator/proctrace/*" \
      "src/aggregator/mod.rs" \
      "src/aggregator/result.rs" \
      "src/parser/proctrace.rs" \
      "src/parser/mod.rs" \
      "src/parser/result.rs" \
      "src/parser/http/parser.rs" \
      "src/parser/http/response.rs" \
      "src/parser/http/mod.rs" \
      "src/parser/sse/mod.rs" \
      "src/interruption/oom_recovery.rs" \
      "src/interruption/mod.rs" \
      "src/discovery/scanner.rs" \
      "src/discovery/mod.rs" \
      "src/discovery/agents/*" \
      "src/analyzer/audit/*" \
      "src/analyzer/unified.rs" \
      "src/analyzer/mod.rs" \
      "src/analyzer/result.rs" \
      "src/analyzer/message/mod.rs" \
      "src/analyzer/message/sysom.rs" \
      "src/analyzer/token/mod.rs" \
      "src/aggregator/unified.rs" \
      "src/parser/unified.rs" \
      "src/aggregator/http2.rs" \
      "src/aggregator/http/*" \
      "src/parser/http2/*" \
      "src/token_breakdown/breakdown.rs" \
      "src/token_breakdown/cli.rs" \
      "src/token_breakdown/mod.rs" \
      "src/atif/mod.rs" \
      "src/atif/converter.rs" \
      "src/genai/builder.rs" \
      "src/analyzer/message/openai.rs" \
      "src/analyzer/message/anthropic.rs" \
      "src/lib.rs" \
    2>&1 || true)

  END_TIME=$(date +%s)
  ELAPSED=$((END_TIME - START_TIME))

  # 提取 test case 数量 (tarpaulin 输出中的 "running X tests" 累加)
  RUST_TESTS=$(echo "$OUTPUT" | grep -oP 'running \K\d+(?= tests?)' | awk '{s+=$1} END {print s}')
  if [ -z "$RUST_TESTS" ]; then
    RUST_TESTS=0
  fi

  # 提取覆盖率：优先从 tarpaulin 汇总行解析 (格式: "XX.XX% coverage, COVERED/TOTAL lines covered")
  # 每次命令替换都以 `|| true` 收尾：grep 无匹配时退出码为 1，赋值因此失败，
  # 而 `set -e` 会把整段脚本带走——覆盖率输出与预期格式不同（tarpaulin 失败、
  # 版本升级、本就没有覆盖率数字）时，脚本会静默中止。
  TARP_SUMMARY=$(echo "$OUTPUT" | grep -oP '\d+\.\d+% coverage, \d+/\d+ lines covered' || true)
  if [ -n "$TARP_SUMMARY" ]; then
    RATE=$(echo "$TARP_SUMMARY" | grep -oP '^\d+\.\d+' || true)
    COVERED=$(echo "$TARP_SUMMARY" | grep -oP '\d+(?=/\d+ lines)' || true)
    TOTAL=$(echo "$TARP_SUMMARY" | grep -oP '(?<=/)\d+(?= lines)' || true)
    RUST_COVERAGE="${RATE}%"
    echo ""
    echo -e "${GREEN}Rust 覆盖率: ${COVERED}/${TOTAL} = ${RATE}%${NC}"
    echo -e "${GREEN}Rust test cases: ${RUST_TESTS}${NC}"
  else
    # 回退：从逐文件行累加 (兼容 "|| src/" 和 "src/" 两种格式)
    COVERED=$(echo "$OUTPUT" | grep -E '(^|\|\| )src/' | grep -oP '\d+(?=/\d+)' | awk '{s+=$1} END {print s}' || true)
    TOTAL=$(echo "$OUTPUT" | grep -E '(^|\|\| )src/' | grep -oP '(?<=/)\d+' | awk '{s+=$1} END {print s}' || true)
    if [ -n "$TOTAL" ] && [ "$TOTAL" -gt 0 ]; then
      RATE=$(awk "BEGIN {printf \"%.2f\", ($COVERED/$TOTAL)*100}")
      RUST_COVERAGE="${RATE}%"
      echo ""
      echo -e "${GREEN}Rust 覆盖率: ${COVERED}/${TOTAL} = ${RATE}%${NC}"
      echo -e "${GREEN}Rust test cases: ${RUST_TESTS}${NC}"
    else
      RUST_COVERAGE="N/A"
      echo "$OUTPUT" | tail -5
    fi
  fi

  echo -e "${YELLOW}耗时: ${ELAPSED}s${NC}"
  cd "$ROOT_DIR"
}

# ─── 汇总 ─────────────────────────────────────────────────────────────────────

run_all() {
  run_dashboard
  run_rust

  # 整体汇总
  print_header "整体汇总"

  TOTAL_TESTS=$((DASHBOARD_TESTS + RUST_TESTS))

  # 计算加权平均覆盖率
  D_RATE=$(echo "$DASHBOARD_COVERAGE" | grep -oP '\d+\.?\d*' || true)
  R_RATE=$(echo "$RUST_COVERAGE" | grep -oP '\d+\.?\d*' || true)
  if [ -n "$D_RATE" ] && [ -n "$R_RATE" ]; then
    TOTAL_COVERAGE=$(awk "BEGIN {printf \"%.2f\", ($D_RATE * $DASHBOARD_TESTS + $R_RATE * $RUST_TESTS) / ($DASHBOARD_TESTS + $RUST_TESTS)}")
    TOTAL_COVERAGE="${TOTAL_COVERAGE}%"
  elif [ -n "$D_RATE" ]; then
    TOTAL_COVERAGE="$DASHBOARD_COVERAGE"
  elif [ -n "$R_RATE" ]; then
    TOTAL_COVERAGE="$RUST_COVERAGE"
  else
    TOTAL_COVERAGE="N/A"
  fi

  echo -e "${GREEN}┌─────────────────────────────────────────────────┐${NC}"
  echo -e "${GREEN}│  模块          覆盖率          Test Cases       │${NC}"
  echo -e "${GREEN}├─────────────────────────────────────────────────┤${NC}"
  printf "${GREEN}│  Dashboard     %-16s %-16s │${NC}\n" "$DASHBOARD_COVERAGE" "$DASHBOARD_TESTS"
  printf "${GREEN}│  Rust          %-16s %-16s │${NC}\n" "$RUST_COVERAGE" "$RUST_TESTS"
  echo -e "${GREEN}├─────────────────────────────────────────────────┤${NC}"
  printf "${GREEN}│  Total         %-16s %-16s │${NC}\n" "$TOTAL_COVERAGE" "$TOTAL_TESTS"
  echo -e "${GREEN}└─────────────────────────────────────────────────┘${NC}"
}

# ─── 入口 ─────────────────────────────────────────────────────────────────────

case "$MODE" in
  dashboard|front|frontend)
    run_dashboard
    ;;
  rust|backend)
    run_rust
    ;;
  all|"")
    run_all
    ;;
  *)
    echo "用法: $0 [dashboard|rust|all]"
    echo "  dashboard  - 仅获取前端覆盖率"
    echo "  rust       - 仅获取 Rust 后端覆盖率"
    echo "  all        - 获取全部覆盖率（默认）"
    exit 1
    ;;
esac
