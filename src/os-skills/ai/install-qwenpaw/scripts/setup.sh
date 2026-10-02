#!/usr/bin/env bash
# QwenPaw 一键安装部署脚本
# 用法: bash setup.sh <百炼API_KEY> <钉钉CLIENT_ID> <钉钉CLIENT_SECRET> [模型名称]
#
# 示例:
#   bash setup.sh sk-xxxx dingxxxx your_secret
#   bash setup.sh sk-xxxx dingxxxx your_secret qwen3-235b-a22b-thinking-2507

set -euo pipefail

# ── 参数校验 ──────────────────────────────────────────────
DASHSCOPE_API_KEY="${1:-}"
DINGTALK_CLIENT_ID="${2:-}"
DINGTALK_CLIENT_SECRET="${3:-}"
MODEL_NAME="${4:-qwen3-max}"

if [ -z "$DASHSCOPE_API_KEY" ] || [ -z "$DINGTALK_CLIENT_ID" ] || [ -z "$DINGTALK_CLIENT_SECRET" ]; then
  echo "用法: bash setup.sh <百炼API_KEY> <钉钉CLIENT_ID> <钉钉CLIENT_SECRET> [模型名称]"
  echo ""
  echo "必填参数:"
  echo "  百炼API_KEY        以 sk- 开头，从 https://bailian.console.aliyun.com/ 获取"
  echo "  钉钉CLIENT_ID      即 AppKey，从钉钉开发者后台获取"
  echo "  钉钉CLIENT_SECRET  即 AppSecret，从钉钉开发者后台获取"
  echo ""
  echo "可选参数:"
  echo "  模型名称           默认 qwen3-max，可选 qwen3-235b-a22b-thinking-2507, deepseek-v3.2 等"
  exit 1
fi

if [[ "$DASHSCOPE_API_KEY" != sk-* ]]; then
  echo "错误: 百炼 API Key 应以 sk- 开头，请检查"
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
QWENPAW_DIR="$HOME/.qwenpaw"
SECRET_DIR="$HOME/.qwenpaw.secret"

echo "=============================="
echo " QwenPaw 安装部署"
echo "=============================="
echo "百炼 API Key:  ${DASHSCOPE_API_KEY:0:8}..."
echo "钉钉 Client ID: ${DINGTALK_CLIENT_ID:0:8}..."
echo "模型: $MODEL_NAME"
echo ""

# ── 步骤 1: 检查并安装 uv ─────────────────────────────────
echo "[1/6] 检查 uv..."
if command -v uv &>/dev/null; then
  echo "  uv 已安装: $(uv --version 2>/dev/null)"
else
  echo "  uv 未安装，正在安装..."
  if pip install uv -i https://mirrors.aliyun.com/pypi/simple/ -q 2>/dev/null; then
    echo "  uv 安装完成 (pip): $(uv --version 2>/dev/null)"
  else
    echo "  pip 安装失败，尝试官方脚本..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    export PATH="$HOME/.local/bin:$PATH"
    echo "  uv 安装完成 (script): $(uv --version 2>/dev/null)"
  fi
fi

# ── 步骤 2: 安装 QwenPaw ──────────────────────────────────
echo "[2/6] 安装 QwenPaw..."
if command -v qwenpaw &>/dev/null; then
  echo "  qwenpaw 已安装: $(qwenpaw --version 2>/dev/null || echo '未知版本')"
else
  if uv pip install qwenpaw --index-url https://mirrors.aliyun.com/pypi/simple/ 2>/dev/null; then
    echo "  安装完成 (阿里云镜像)"
  else
    echo "  阿里云镜像安装失败，尝试官方脚本..."
    curl -fsSL https://qwenpaw.agentscope.io/install.sh | bash
  fi
  export PATH="$HOME/.qwenpaw/bin:$PATH"
  # 写入 bashrc（如果还没有的话）
  if ! grep -q '.qwenpaw/bin' "$HOME/.bashrc" 2>/dev/null; then
    echo 'export PATH="$HOME/.qwenpaw/bin:$PATH"' >> "$HOME/.bashrc"
  fi
  echo "  安装完成: $(qwenpaw --version 2>/dev/null || echo '请重新打开终端')"
fi

# ── 步骤 3: 创建目录结构 ──────────────────────────────────
echo "[3/6] 创建目录结构..."
mkdir -p "$QWENPAW_DIR/media"
mkdir -p "$QWENPAW_DIR/active_skills"
mkdir -p "$QWENPAW_DIR/customized_skills"
mkdir -p "$SECRET_DIR/providers/builtin"
mkdir -p "$SECRET_DIR/providers/custom"
chmod 700 "$SECRET_DIR" "$SECRET_DIR/providers" "$SECRET_DIR/providers/builtin" "$SECRET_DIR/providers/custom"
echo "  目录创建完成"

# ── 步骤 4: 写入配置文件 ──────────────────────────────────
echo "[4/6] 写入配置文件..."

# 3a: config.json — 从模板替换占位符
CONFIG_TEMPLATE="$SKILL_DIR/reference/config.json.example"
if [ ! -f "$CONFIG_TEMPLATE" ]; then
  echo "  错误: 找不到模板文件 $CONFIG_TEMPLATE"
  exit 1
fi
sed \
  -e "s|{DINGTALK_CLIENT_ID}|${DINGTALK_CLIENT_ID}|g" \
  -e "s|{DINGTALK_CLIENT_SECRET}|${DINGTALK_CLIENT_SECRET}|g" \
  "$CONFIG_TEMPLATE" > "$QWENPAW_DIR/config.json"
echo "  config.json 已写入"

# 3b: dashscope.json — 百炼提供商
DASHSCOPE_TEMPLATE="$SKILL_DIR/reference/dashscope.json"
if [ ! -f "$DASHSCOPE_TEMPLATE" ]; then
  echo "  错误: 找不到模板文件 $DASHSCOPE_TEMPLATE"
  exit 1
fi
sed "s|{DASHSCOPE_API_KEY}|${DASHSCOPE_API_KEY}|g" \
  "$DASHSCOPE_TEMPLATE" > "$SECRET_DIR/providers/builtin/dashscope.json"
chmod 600 "$SECRET_DIR/providers/builtin/dashscope.json"
echo "  dashscope.json 已写入"

# 3c: active_model.json — 活跃模型
ACTIVE_MODEL_TEMPLATE="$SKILL_DIR/reference/active_model.json"
if [ ! -f "$ACTIVE_MODEL_TEMPLATE" ]; then
  echo "  错误: 找不到模板文件 $ACTIVE_MODEL_TEMPLATE"
  exit 1
fi
sed "s|{MODEL_NAME}|${MODEL_NAME}|g" \
  "$ACTIVE_MODEL_TEMPLATE" > "$SECRET_DIR/providers/active_model.json"
chmod 600 "$SECRET_DIR/providers/active_model.json"
echo "  active_model.json 已写入"

# 3d: Markdown 文件 — 复制到 ~/.qwenpaw/
for md_file in AGENTS.md SOUL.md PROFILE.md MEMORY.md BOOTSTRAP.md HEARTBEAT.md; do
  src="$SKILL_DIR/reference/$md_file"
  if [ -f "$src" ]; then
    cp "$src" "$QWENPAW_DIR/$md_file"
  else
    echo "  警告: 找不到 $src，跳过"
  fi
done
echo "  Markdown 文件已复制"

# ── 步骤 5: 验证文件完整性 ──────────────────────────────
echo "[5/6] 验证文件完整性..."
ALL_OK=true
for f in \
  "$QWENPAW_DIR/config.json" \
  "$SECRET_DIR/providers/builtin/dashscope.json" \
  "$SECRET_DIR/providers/active_model.json" \
  "$QWENPAW_DIR/AGENTS.md" \
  "$QWENPAW_DIR/SOUL.md" \
  "$QWENPAW_DIR/PROFILE.md" \
  "$QWENPAW_DIR/MEMORY.md" \
  "$QWENPAW_DIR/BOOTSTRAP.md" \
  "$QWENPAW_DIR/HEARTBEAT.md"; do
  if [ -f "$f" ]; then
    echo "  OK: $f"
  else
    echo "  MISSING: $f"
    ALL_OK=false
  fi
done

# 验证 JSON 合法性
for jf in "$QWENPAW_DIR/config.json" "$SECRET_DIR/providers/builtin/dashscope.json" "$SECRET_DIR/providers/active_model.json"; do
  if python3 -c "import json; json.load(open('$jf'))" 2>/dev/null; then
    echo "  JSON OK: $jf"
  else
    echo "  JSON ERROR: $jf"
    ALL_OK=false
  fi
done

if [ "$ALL_OK" = false ]; then
  echo ""
  echo "有文件缺失或 JSON 格式错误，请检查后重试"
  exit 1
fi

# ── 步骤 6: 启动服务 ─────────────────────────────────────
echo "[6/6] 启动服务..."

# 先停掉旧进程（如果有）
if pgrep -f "qwenpaw app" > /dev/null 2>&1; then
  echo "  停止已有 QwenPaw 进程..."
  kill $(pgrep -f "qwenpaw app") 2>/dev/null || true
  sleep 2
fi

nohup qwenpaw app --host 0.0.0.0 --port 8088 > "$QWENPAW_DIR/qwenpaw.log" 2>&1 &
QWENPAW_PID=$!
echo "  QwenPaw 已启动 (PID: $QWENPAW_PID)"
echo "  等待服务就绪..."
sleep 5

# 验证服务是否正常
if curl -s -o /dev/null -w "%{http_code}" "http://localhost:8088/" 2>/dev/null | grep -q "200\|404"; then
  echo "  服务已就绪"
else
  echo "  警告: 服务可能未就绪，请查看日志: tail -f $QWENPAW_DIR/qwenpaw.log"
fi

echo ""
echo "=============================="
echo " 部署完成!"
echo "=============================="
echo ""
echo "服务地址:  http://localhost:8088/"
echo "日志文件:  $QWENPAW_DIR/qwenpaw.log"
echo "停止服务:  kill $QWENPAW_PID"
echo ""
echo "在钉钉中搜索你的机器人名称即可开始对话。"
echo ""
echo "如需查看钉钉频道状态:"
echo "  qwenpaw channels list"
