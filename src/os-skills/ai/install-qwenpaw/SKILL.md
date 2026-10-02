---
name: install-qwenpaw
version: 1.0.0
description: 在 Alibaba Cloud Linux 4 (Alinux 4) 服务器上完成 QwenPaw AI 助理的安装部署，包括脚本安装、直接写入配置文件、百炼模型 API Key 配置、钉钉频道接入。当用户需要安装 QwenPaw、部署 AI 助理、配置钉钉机器人或百炼模型时使用此技能。
layer: application
lifecycle: usage
---

# QwenPaw Linux 服务器安装部署

## 前置信息收集

开始前**必须**向用户获取以下信息，缺一不可：

1. **百炼 API Key** — 格式以 `sk-` 开头，从 https://bailian.console.aliyun.com/ 获取
2. **钉钉 Client ID** — 即 AppKey，从钉钉开发者后台获取
3. **钉钉 Client Secret** — 即 AppSecret，从钉钉开发者后台获取
4. **模型名称**（可选）— 默认 `qwen3-max`，可选 `qwen3-235b-a22b-thinking-2507`、`deepseek-v3.2` 等

---

## 一键部署（推荐）

本 skill 提供了一键部署脚本 `scripts/setup.sh`，收集到用户信息后直接执行：

```bash
bash scripts/setup.sh <百炼API_KEY> <钉钉CLIENT_ID> <钉钉CLIENT_SECRET> [模型名称]
```

示例：

```bash
bash scripts/setup.sh sk-76f003xxx dingxxxxx your_secret
bash scripts/setup.sh sk-76f003xxx dingxxxxx your_secret qwen3-235b-a22b-thinking-2507
```

脚本会自动完成：检查 uv → 安装 QwenPaw → 创建目录 → 写入配置（替换占位符）→ 验证文件 → 后台启动服务。

执行前需要先将本 skill 目录上传到服务器，或通过工具将 `scripts/setup.sh` 和 `reference/` 目录写入服务器。

---

## 手动分步部署

如果脚本不可用，按以下步骤手动操作。

### 步骤 1: 检查并安装 uv

QwenPaw 依赖 `uv` 作为 Python 包管理器。优先通过阿里云镜像安装，失败时用官方脚本兜底：

```bash
if command -v uv &>/dev/null; then
  echo "uv 已安装: $(uv --version)"
else
  # 方式 1: pip + 阿里云镜像（推荐）
  pip install uv -i https://mirrors.aliyun.com/pypi/simple/
  # 方式 2: 官方安装脚本（pip 不可用时）
  # curl -LsSf https://astral.sh/uv/install.sh | sh
fi
```

### 步骤 2: 安装 QwenPaw

```bash
# 方式 1: 阿里云镜像（推荐）
uv pip install qwenpaw --index-url https://mirrors.aliyun.com/pypi/simple/

# 方式 2: 官方安装脚本（镜像不可用时）
# curl -fsSL https://qwenpaw.agentscope.io/install.sh | bash
# source ~/.bashrc
```

验证：

```bash
qwenpaw --version
```

找不到命令时手动加 PATH：

```bash
export PATH="$HOME/.qwenpaw/bin:$PATH"
echo 'export PATH="$HOME/.qwenpaw/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### 步骤 3: 创建目录结构

```bash
mkdir -p ~/.qwenpaw/media
mkdir -p ~/.qwenpaw/active_skills
mkdir -p ~/.qwenpaw/customized_skills
mkdir -p ~/.qwenpaw.secret/providers/builtin
mkdir -p ~/.qwenpaw.secret/providers/custom
chmod 700 ~/.qwenpaw.secret ~/.qwenpaw.secret/providers ~/.qwenpaw.secret/providers/builtin ~/.qwenpaw.secret/providers/custom
```

### 步骤 4: 写入配置文件

读取 `reference/` 下的模板文件，替换占位符后写入目标路径。

#### 3a: config.json（含钉钉频道配置）

读取 `reference/config.json.example`，替换后写入 `~/.qwenpaw/config.json`：

```
替换:
  {DINGTALK_CLIENT_ID} → 用户的钉钉 Client ID
  {DINGTALK_CLIENT_SECRET} → 用户的钉钉 Client Secret
```

钉钉已预设 `"enabled": true`。`qwenpaw app` 启动时会自动迁移生成 `agent.json` 等文件。

#### 3b: dashscope.json

读取 `reference/dashscope.json`，替换后写入 `~/.qwenpaw.secret/providers/builtin/dashscope.json`：

```
替换: {DASHSCOPE_API_KEY} → 用户的百炼 API Key
```

```bash
chmod 600 ~/.qwenpaw.secret/providers/builtin/dashscope.json
```

#### 3c: active_model.json

读取 `reference/active_model.json`，替换后写入 `~/.qwenpaw.secret/providers/active_model.json`：

```
替换: {MODEL_NAME} → 用户指定的模型（默认 qwen3-max）
```

```bash
chmod 600 ~/.qwenpaw.secret/providers/active_model.json
```

#### 3d: Markdown 文件

原样复制到 `~/.qwenpaw/`（无占位符）：

```
reference/AGENTS.md    → ~/.qwenpaw/AGENTS.md
reference/SOUL.md      → ~/.qwenpaw/SOUL.md
reference/PROFILE.md   → ~/.qwenpaw/PROFILE.md
reference/MEMORY.md    → ~/.qwenpaw/MEMORY.md
reference/BOOTSTRAP.md → ~/.qwenpaw/BOOTSTRAP.md
reference/HEARTBEAT.md → ~/.qwenpaw/HEARTBEAT.md
```

#### 3e: 验证文件完整性

```bash
for f in \
  ~/.qwenpaw/config.json \
  ~/.qwenpaw.secret/providers/builtin/dashscope.json \
  ~/.qwenpaw.secret/providers/active_model.json \
  ~/.qwenpaw/AGENTS.md \
  ~/.qwenpaw/SOUL.md \
  ~/.qwenpaw/PROFILE.md \
  ~/.qwenpaw/MEMORY.md \
  ~/.qwenpaw/BOOTSTRAP.md \
  ~/.qwenpaw/HEARTBEAT.md; do
  [ -f "$f" ] && echo "OK: $f" || echo "MISSING: $f"
done
```

### 步骤 5: 启动服务

```bash
nohup qwenpaw app --host 0.0.0.0 --port 8088 > ~/.qwenpaw/qwenpaw.log 2>&1 &
echo "QwenPaw PID: $!"
sleep 5
```

> `--host 0.0.0.0` 使外部可访问。仅本机访问用 `127.0.0.1`。

### 步骤 6: 验证部署

```bash
curl -s -N -X POST "http://localhost:8088/api/agent/process" \
  -H "Content-Type: application/json" \
  -d '{"input":[{"role":"user","content":[{"type":"text","text":"你好"}]}],"session_id":"test123"}' \
  | head -c 500
```

返回 SSE 流式数据（`data:` 开头）表示服务正常。

```bash
qwenpaw channels list
```

确认 dingtalk 显示 `enabled: True`。

---

## 故障排查

### qwenpaw 命令找不到

```bash
export PATH="$HOME/.qwenpaw/bin:$PATH"
source ~/.bashrc
```

### 模型返回错误

```bash
cat ~/.qwenpaw.secret/providers/builtin/dashscope.json | python3 -m json.tool
cat ~/.qwenpaw.secret/providers/active_model.json | python3 -m json.tool
```

确认 `api_key` 以 `sk-` 开头，`model` 字段有值。

### 钉钉机器人不回复

```bash
python3 -c "
import json
with open('$HOME/.qwenpaw/config.json') as f:
    c = json.load(f)
print(json.dumps(c.get('channels',{}).get('dingtalk',{}), indent=2, ensure_ascii=False))
"
tail -100 ~/.qwenpaw/qwenpaw.log | grep -i dingtalk
```

钉钉侧检查：应用已发布、Stream 模式已启用、Client ID/Secret 正确。

### 端口占用

```bash
lsof -i :8088
nohup qwenpaw app --host 0.0.0.0 --port 9090 > ~/.qwenpaw/qwenpaw.log 2>&1 &
```

### 停止服务

```bash
kill $(pgrep -f "qwenpaw app")
```

---

## 钉钉应用创建指引（提供给用户）

如果用户还没有钉钉应用凭证，告知以下步骤：

1. 打开 https://open-dev.dingtalk.com/
2. 进入「应用开发 → 企业内部应用 → 钉钉应用 → 创建应用」
3. 在「应用能力 → 添加应用能力」中添加「机器人」
4. 配置机器人：消息接收模式选择 **Stream 模式**，点击「发布」
5. 在「应用发布 → 版本管理与发布」中创建新版本并保存
6. 在「基础信息 → 凭证与基础信息」中复制 **Client ID** 和 **Client Secret**
7. （可选）在「安全设置 → 服务器出口 IP」添加服务器公网 IP（`curl ifconfig.me` 获取），支持图片/文件下载

## 文件清单

```
install-qwenpaw/
├── SKILL.md
├── scripts/
│   └── setup.sh                 # 一键部署脚本
└── reference/
    ├── config.json.example      → ~/.qwenpaw/config.json
    ├── dashscope.json           → ~/.qwenpaw.secret/providers/builtin/dashscope.json
    ├── active_model.json        → ~/.qwenpaw.secret/providers/active_model.json
    ├── AGENTS.md                → ~/.qwenpaw/AGENTS.md
    ├── SOUL.md                  → ~/.qwenpaw/SOUL.md
    ├── PROFILE.md               → ~/.qwenpaw/PROFILE.md
    ├── MEMORY.md                → ~/.qwenpaw/MEMORY.md
    ├── BOOTSTRAP.md             → ~/.qwenpaw/BOOTSTRAP.md
    └── HEARTBEAT.md             → ~/.qwenpaw/HEARTBEAT.md
```
