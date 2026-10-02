---
name: install-hermes
description: One-click installation and configuration of Hermes Agent by Nous Research. Handles installing Hermes from scratch, adding LLM providers (OpenRouter, Anthropic, OpenAI, z.ai/GLM, Kimi, DeepSeek, Alibaba Qwen, Ollama, etc.), setting up messaging platforms (Telegram, Discord, Slack, DingTalk, Feishu/Lark, Weixin, WeCom), installing Hermes Workspace web UI, and migrating from OpenClaw. Use when the user asks about installing Hermes, setting up a provider, configuring a messaging platform, adding a bot, or any Hermes setup task.
---

# Hermes Agent 一键安装与配置
**操作系统**: Linux / macOS / WSL2 / Android(Termux)。不支持原生 Windows。

## 意图识别与行动映射

当用户表达以下意图时，按对应工作流执行：

| 用户说 | 对应工作流 |
|--------|-----------|
| "安装 Hermes" / "从零开始" | [工作流 1: 全新安装](#工作流-1-全新安装) |
| "安装 [Provider 名]" / "配置 [模型名]" / "接入 [供应商]" | [工作流 2: 添加 Provider](#工作流-2-添加-provider) |
| "接入 [平台名]" / "配置 [Telegram/钉钉/飞书/微信]" / "安装 Bot" | [工作流 3: 添加消息平台](#工作流-3-添加消息平台) |
| "安装 Workspace" / "Web UI" / "可视化界面" | [工作流 4: 安装 Hermes Workspace](#工作流-4-安装-hermes-workspace-web-ui) |
| "从 OpenClaw 迁移" | [工作流 5: OpenClaw 迁移](#工作流-5-从-openclaw-迁移) |
| "检查配置" / "出问题" / "不能用" | [工作流 6: 诊断与修复](#工作流-6-诊断与修复) |

---

## 工作流 1: 全新安装

### 前提检查

```bash
git --version   # 必须可用
```

### 执行安装

```bash
bash ./scripts/install.sh
source ~/.bashrc   # 或 source ~/.zshrc
hermes doctor      # 验证
```

> **安装过程说明**：
> - 脚本默认**最小安装**（跳过 daytona/playwright/WhatsApp 等重型组件），安装速度快
> - 需要完整功能（浏览器工具、WhatsApp 等）时加 `--full` 参数
> - 脚本默认**跳过**交互式配置向导，安装完后运行 `hermes setup` 再配置
> - 如需安装时直接配置，加 `--with-setup` 参数
> - CI/自动化场景直接运行即可，不会出现任何交互阻塞
> - **安装完成后会自动安装 tokenless 插件**（token 用量追踪和响应压缩），无需额外操作

**安装参数说明**：

| 参数 | 说明 |
|------|------|
| （无参数） | 最小安装，仅核心包，速度最快（**推荐新手**） |
| `--full` | 安装所有 extras（daytona、playwright、WhatsApp 等） |
| `--with-setup` | 安装完后启动配置向导 |
| `--skip-tokenless` | 跳过 tokenless 插件自动安装 |

### 首次配置

```bash
hermes model       # 选择 Provider 和模型
hermes             # 启动首次对话验证
```

---

## 工作流 2: 添加 Provider

> 用户意图："我要安装 OpenRouter"、"接入 DeepSeek"、"用 Claude"、"配置 Kimi" 等。

### Step 1: 确定 Provider 类型

从以下速查表识别用户指定的 Provider：

| Provider | 环境变量名 | 交互式命令 | 详细配置参考 |
|----------|-----------|-----------|-------------|
| Alibaba / 通义千问 | `DASHSCOPE_API_KEY` | `hermes model` | [model.md](references/model.md) |
| DeepSeek | `DEEPSEEK_API_KEY` | `hermes model` | [model.md](references/model.md) |
| z.ai / 智谱 GLM | `GLM_API_KEY` | `hermes model` | [model.md](references/model.md) |
| Kimi / Moonshot 中国版 | `KIMI_CN_API_KEY` | `hermes model` | [model.md](references/model.md) |
| MiniMax 中国 | `MINIMAX_CN_API_KEY` | `hermes model` | [model.md](references/model.md) |
| Ollama (本地) | 无需密钥 | `hermes model` | [model.md](references/model.md) |

### Step 2: 收集必要信息

向用户询问 Provider 所需的凭证：

- **API Key 类型**: 绝大多数 Provider 只需要一个 API Key
- **OAuth 类型**（Nous Portal / Codex / GitHub Copilot / Anthropic / Google Gemini）: 终端会显示扫码或浏览器链接，用户按提示完成授权
- **本地模型**（Ollama）: 确认 Ollama 已运行且模型已加载

### Step 3: 一键配置

**推荐方式 — 交互式向导**：

```bash
hermes model
# 按提示选择 Provider → 粘贴 API Key / 扫码授权 → 选择模型
```

**脚本方式 — 已知 API Key 时**：

```bash
hermes config set <ENV_VAR_NAME> <api-key-value>
hermes config set model <provider>/<model-id>
```

例如配置 DeepSeek：

```bash
hermes config set DEEPSEEK_API_KEY sk-xxx
hermes config set model deepseek/deepseek-chat
```

### Step 4: 验证

```bash
hermes doctor
hermes          # 启动对话，确认 banner 显示正确模型
```

> **模型最低要求**: 64K token 上下文窗口。不足 64K 的模型会在启动时被拒绝。
> **辅助模型**: 即使主模型已配置，部分工具（视觉、网页摘要）默认使用 OpenRouter 上的 Gemini Flash。建议同时配置 `OPENROUTER_API_KEY` 以解锁完整工具能力。

---

## 工作流 3: 添加消息平台

> 用户意图："接入钉钉"、"配置飞书 Bot"、"微信机器人"、"Telegram Bot" 等。

### Step 1: 确定平台

| 平台 | 凭证要求 | 连接方式 | 详细配置参考 |
|------|---------|---------|-------------|
| 钉钉 | Client ID + Client Secret + User ID | Stream Mode (WebSocket) | [cn-messaging-platforms.md](references/cn-messaging-platforms.md) |
| 飞书 / Lark | App ID + App Secret | WebSocket / Webhook | [cn-messaging-platforms.md](references/cn-messaging-platforms.md) |
| 微信 | 扫码登录（无手动密钥） | 长轮询 | [cn-messaging-platforms.md](references/cn-messaging-platforms.md) |
| 企业微信 | Bot ID + Secret（或扫码） | WebSocket | [cn-messaging-platforms.md](references/cn-messaging-platforms.md) |
| Telegram | Bot Token | HTTP 长轮询 | `hermes gateway setup` |
| Discord | Bot Token | WebSocket Gateway | `hermes gateway setup` |
| Slack | Bot Token + Signing Secret | HTTP 事件 | `hermes gateway setup` |
| WhatsApp | 无需密钥（扫码） | WhatsApp Web 桥接 | `hermes gateway setup` |
| Signal | 无需密钥 | Signal 桥接 | `hermes gateway setup` |

### Step 2: 收集凭证

**中国平台凭证收集清单**：

| 平台 | 需要用户提供 | 获取方式 |
|------|-------------|---------|
| 钉钉 | Client ID, Client Secret, 允许的用户 ID | [钉钉开发者后台](https://open-dev.dingtalk.com/) |
| 飞书 | App ID, App Secret | [飞书开放平台](https://open.feishu.cn/) |
| 微信 | 无需凭证，扫码即可 | 微信手机 App 扫码 |
| 企微 | Bot ID, Secret（或扫码） | 企微管理后台 |

### Step 3: 一键配置

**交互式配置（推荐）**：

```bash
hermes gateway setup
# 选择平台 → 按提示输入凭证 / 扫码 → 完成
```

对于中国平台，交互式向导支持**扫码自动创建应用**（飞书、企微）或**扫码授权**（钉钉）。

**手动配置**（已知凭证时）：

编辑 `~/.hermes/.env`，写入对应环境变量：

```bash
# 钉钉
DINGTALK_CLIENT_ID=your-app-key
DINGTALK_CLIENT_SECRET=your-app-secret
DINGTALK_ALLOWED_USERS=user-id-1

# 飞书
FEISHU_APP_ID=cli_xxx
FEISHU_APP_SECRET=secret_xxx
FEISHU_CONNECTION_MODE=websocket
FEISHU_ALLOWED_USERS=ou_xxx

# 企微
WECOM_BOT_ID=your-bot-id
WECOM_SECRET=your-secret
```

### Step 4: 启动与验证

```bash
hermes gateway          # 启动网关
hermes gateway status   # 检查各平台状态
```

向 Bot 发送一条测试消息验证连通性。

---

## 工作流 4: 安装 Hermes Workspace (Web UI)

> 用户意图："安装可视化界面"、"Web UI"、"Workspace"、"Web 控制台"。

Hermes Workspace 是一个基于 Web 的可视化管理界面，支持聊天、记忆浏览、技能管理、终端操作。

### 前提

- Node.js 22+（`node --version`）
- Hermes Agent 已安装且 gateway 可访问

### 一键安装

```bash
git clone https://github.com/outsourc-e/hermes-workspace.git
cd hermes-workspace
pnpm install
cp .env.example .env
```

### 连接到现有 Hermes

```bash
# 编辑 .env
echo 'HERMES_API_URL=http://127.0.0.1:8642' >> .env
echo 'HERMES_DASHBOARD_URL=http://127.0.0.1:9119' >> .env

# 启动
pnpm dev    # http://localhost:3000
```

### Docker 一键启动（含 Agent + Workspace）

```bash
git clone https://github.com/outsourc-e/hermes-workspace.git
cd hermes-workspace
cp .env.example .env
# 编辑 .env，至少添加一个 Provider API Key
docker compose up
# 打开 http://localhost:3000
```

### 验证

```bash
curl http://127.0.0.1:8642/health         # gateway 健康
curl http://127.0.0.1:9119/api/status     # dashboard 状态
```

---

## 工作流 5: 从 OpenClaw 迁移

> 用户意图："从 OpenClaw 迁移"、"导入 OpenClaw 数据"。

### 自动检测

首次运行 `hermes setup` 时，若检测到 `~/.openclaw` 会自动提示迁移。

### 手动迁移

```bash
hermes claw migrate --dry-run    # 预览
hermes claw migrate              # 执行迁移
```

### 迁移内容

SOUL.md、记忆、技能、命令白名单、平台配置、API 密钥、TTS 资源、AGENTS.md。

---

## 工作流 6: 诊断与修复

当用户说"出问题了"、"不能用"、"报错"时，按此顺序执行：

```bash
hermes doctor          # 1. 诊断环境和配置
hermes model           # 2. 重选/修复 Provider
hermes setup           # 3. 完整设置向导
hermes sessions list   # 4. 检查会话
hermes --continue      # 5. 恢复会话
hermes gateway status  # 6. 检查网关
```

### 常见问题速查

| 症状 | 原因 | 修复 |
|------|------|------|
| `hermes: command not found` | Shell 未重载 | `source ~/.bashrc` |
| 空白/错误回复 | Provider 认证错误 | `hermes model` 重配 |
| 自定义端点乱码 | 错误的 base URL | 独立客户端验证端点 |
| 网关启动但无消息 | Bot token/allowlist 不完整 | `hermes gateway setup` |
| `--continue` 无旧会话 | Profile 不匹配 | `hermes sessions list` 检查 |
| Alibaba 401 错误 | 使用了国际版端点 | 设置 `DASHSCOPE_BASE_URL`，见坑位 #8 |
| `Unknown TERMINAL_ENV 'auto'` | terminal.backend 未显式设置 | `hermes config set terminal.backend local` |
| 钉钉 `Unauthorized user` | ALLOWED_USERS 填了工号而非真实 ID | 从网关日志获取 `sender_id`，见坑位 #9 |

---

## 安装踩坑速查

### 坑 1：Python 依赖安装超时（pip 回溯卡死）

**内置脚本**：
- 默认改用 `uv pip install`
- 默认最小安装（不含重型 extras），需要完整功能时加 `--full`：
  ```bash
  bash install.sh --full
  ```

### 坑 2：Python venv 重复安装（已内置优化）

脚本已内置检查：若 venv 存在且 `pip check` 通过，自动跳过重建和依赖安装，无需手动处理。

### npm 默认源超时（已内置优化）

**默认最小安装会完全跳过 Node.js 依赖**，npm 超时问题在默认模式下不存在。

若使用 `--full` 安装浏览器工具时遇到 npm 超时：脚本已自动设置 npmmirror 镜像，所有 `npm install` 均使用 `--registry=https://registry.npmmirror.com`。

若遇到 `ENOTEMPTY` 报错（上次安装中断留有残留），清理后重试：
```bash
rm -rf ~/.hermes/hermes-agent/node_modules
npm install --registry=https://registry.npmmirror.com --prefix ~/.hermes/hermes-agent
```

### 坑 4：Playwright 下载超时（已内置优化）

脚本已设置 `PLAYWRIGHT_DOWNLOAD_HOST=https://npmmirror.com/mirrors/playwright`，并统一改为 `npx --yes playwright install chromium` 跳过交互确认。

手动重试：
```bash
export PLAYWRIGHT_DOWNLOAD_HOST=https://npmmirror.com/mirrors/playwright
cd ~/.hermes/hermes-agent && npx --yes playwright install chromium
```

### 坑 5：WhatsApp Bridge GitHub 依赖卡死（已内置 timeout）

部分依赖直接从 GitHub 拉取，脚本已加 `timeout 120`。

需要 WhatsApp 时，等 GitHub 可访问后手动安装：
```bash
cd ~/.hermes/hermes-agent/scripts/whatsapp-bridge && npm install
```

### 坑 6：安装结束弹出交互向导（已默认跳过）

脚本默认 `RUN_SETUP=false`，全程无交互阻塞。需要向导时显式开启：
```bash
bash install.sh --with-setup
```

### 坑 7：terminal.backend 报错

`config.yaml` 默认值 `auto` 会触发报错 `Unknown TERMINAL_ENV 'auto'`。

修复（安装后必做）：
```bash
hermes config set terminal.backend local
```

### 坑 8：通义千问（Alibaba/DashScope）完整配置步骤

Hermes 默认调用**国际版**端点 `dashscope-intl.aliyuncs.com`，中国区 Key 会出现 401。且 provider 必须显式设为 `alibaba`（不能用 `custom`），API Key 必须写入 `.env` 而非 `config.yaml`。

**Step 1：在 `~/.hermes/.env` 中添加**：
```bash
DASHSCOPE_API_KEY=sk-xxxxxxxxxxxx
DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
```

**Step 2：在 `~/.hermes/config.yaml` 中设置**：
```yaml
model:
  provider: "alibaba"
  default: "qwen-plus"  # qwen-max 只有 32K 上下文，不满足 Hermes 64K 最低要求！
                        # qwen-plus / qwen-turbo / qwen-long 均有 131K+ 上下文，可正常使用
```

**Step 3：验证**：
```bash
hermes doctor   # 应显示 ✓ Alibaba/DashScope
hermes          # 启动对话验证
```

> **常见错误**：
> - 使用 `provider: custom` 会报 "not a recognised provider"（虽然 doctor 不报错，但行为异常）
> - 把 `api_key` 写在 `config.yaml` 的 `model:` 块下无效，必须通过 `DASHSCOPE_API_KEY` 环境变量
> - **`qwen-max` 不可用**：上下文窗口仅 32K，低于 Hermes 64K 最低要求，启动时报错
> - 推荐使用 `qwen-plus`（131K）、`qwen-turbo`（131K）或 `qwen-long`（10M）

### 坑 9：钉钉 ALLOWED_USERS 填工号无效

`DINGTALK_ALLOWED_USERS` 需要的是钉钉系统内的 **User ID**（格式如 `$:LWCP_v1:$xxxx`），与员工工号不同。

**获取方法**：
1. 暂不设置 `DINGTALK_ALLOWED_USERS`，启动网关
2. 从钉钉给 Bot 发一条消息
3. 在日志中找到：
   ```
   WARNING gateway.run: Unauthorized user: $:LWCP_v1:$MJqzUSC2... (姓名) on dingtalk
   ```
4. 把这个 ID 填入 `~/.hermes/.env`：
   ```bash
   DINGTALK_ALLOWED_USERS=$:LWCP_v1:$MJqzUSC2TvsmFU4Pp/Q2osUNJaBYAh1P
   ```
5. 重启网关：`hermes gateway stop && hermes gateway run &`

---

## 配置文件速查

| 文件 | 用途 | 典型内容 |
|------|------|---------|
| `~/.hermes/.env` | 密钥和 Token | API Key、Bot Token |
| `~/.hermes/config.yaml` | 非密钥配置 | 模型、终端后端、平台设置 |
| `~/.hermes/auth.json` | OAuth 凭证 | 自动刷新 token |

### 常用 CLI 命令

| 命令 | 说明 |
|------|------|
| `hermes` | 启动对话 |
| `hermes --tui` | 现代 TUI 界面 |
| `hermes model` | 选择/配置 Provider |
| `hermes setup` | 完整设置向导 |
| `hermes doctor` | 诊断问题 |
| `hermes update` | 更新到最新版本 |
| `hermes gateway` | 消息网关管理 |
| `hermes config set` | 设置配置项 |
| `hermes tools` | 配置工具 |
| `hermes skills` | 浏览/安装技能 |
| `hermes claw migrate` | 从 OpenClaw 迁移 |

---

## 参考文档

- **Provider 详细配置**（含国产模型 z.ai/GLM、Kimi、DeepSeek、Alibaba Qwen、MiniMax 及国际 Provider）: [references/model.md](references/model.md)
- **中国消息平台接入**（钉钉 Stream Mode、飞书 WebSocket/Webhook、微信 iLink Bot、企微 AI Bot）: [references/cn-messaging-platforms.md](references/cn-messaging-platforms.md)
- **通用配置参考**（终端后端、MCP、技能、语音等）: [references/quickstart-reference.md](references/quickstart-reference.md)
- **官方文档**: https://hermes-agent.nousresearch.com/docs
- **GitHub**: https://github.com/NousResearch/hermes-agent
