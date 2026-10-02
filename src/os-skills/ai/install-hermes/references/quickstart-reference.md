# Hermes Agent 详细参考

本文档为 SKILL.md 的补充参考，包含更详细的配置说明和使用场景。

## 安装器详细行为

安装器自动完成以下工作：

1. 检测操作系统和架构（Linux/macOS/WSL2/Termux）
2. 安装 **uv**
3. 通过 uv 安装 **Python 3.11**
4. 安装 **Node.js v22**（浏览器自动化和 WhatsApp 桥接）
5. 安装 **ripgrep**（快速文件搜索）和 **ffmpeg**（音频转换）
6. 克隆 hermes-agent 仓库
7. 创建虚拟环境并安装依赖
8. 配置全局 `hermes` 命令
9. 引导 LLM Provider 配置

## config.yaml 配置结构

Hermes 将配置分为两个文件：

### ~/.hermes/.env（密钥）

```env
OPENROUTER_API_KEY=sk-or-xxxxx
ANTHROPIC_API_KEY=sk-ant-xxxxx
OPENAI_API_KEY=sk-xxxxx
TELEGRAM_BOT_TOKEN=xxxxx
DISCORD_BOT_TOKEN=xxxxx
ELEVENLABS_API_KEY=xxxxx
```

### ~/.hermes/config.yaml（非密钥设置）

```yaml
model:
  provider: "alibaba"          # 使用的 Provider：alibaba / deepseek / zai / openrouter ...
  default: "qwen-plus"         # 默认模型（需满足 64K 上下文要求）

terminal:
  backend: local          # local | docker | ssh | daytona | singularity | modal

# MCP 服务器集成
mcp_servers:
  github:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "ghp_xxx"
  filesystem:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

### 配置管理 CLI

```bash
hermes config set model anthropic/claude-opus-4.6    # 设置模型
hermes config set terminal.backend docker             # 设置终端后端
hermes config set OPENROUTER_API_KEY sk-or-...        # 设置 API Key（自动写入 .env）
hermes config check                                    # 检查配置
hermes config migrate                                  # 迁移旧配置格式
```

## 终端后端详解

Hermes 支持 6 种终端后端，决定智能体在哪里执行命令：

| 后端 | 说明 | 适用场景 |
|------|------|---------|
| `local` | 直接在本机执行 | 开发、个人使用 |
| `docker` | Docker 容器隔离 | 安全沙箱 |
| `ssh` | 远程服务器执行 | 远程开发 |
| `daytona` | Serverless 持久化 | 闲时休眠，按需唤醒 |
| `singularity` | HPC 容器 | GPU 集群 |
| `modal` | Serverless GPU | GPU 推理、闲时近零成本 |

## 消息网关平台支持

完整的 15+ 平台列表：

| 平台 | 说明 |
|------|------|
| Telegram | 最完善支持 |
| Discord | 包含语音频道支持 |
| Slack | 工作团队 |
| WhatsApp | 需 Node.js |
| Signal | 安全通信 |
| Email | 邮件交互 |
| Matrix | 开源协议 |
| Mattermost | 自托管团队 |
| SMS | 短信 |
| DingTalk（钉钉） | 企业通信 |
| Feishu（飞书） | 企业通信 |
| WeCom（企微） | 企业微信 |
| BlueBubbles | iMessage 桥接 |
| Home Assistant | 智能家居 |

### 网关管理命令

```bash
hermes gateway setup     # 交互式配置（选择平台、填入 token）
hermes gateway start     # 启动网关进程
hermes gateway stop      # 停止网关
hermes gateway status    # 查看各平台连接状态
```

## Slash 命令参考

### CLI 和消息平台通用

| 命令 | 说明 |
|------|------|
| `/new` 或 `/reset` | 新建对话 |
| `/model [provider:model]` | 切换模型 |
| `/personality [name]` | 设置性格 |
| `/retry` | 重试上一轮 |
| `/undo` | 撤销上一轮 |
| `/compress` | 压缩上下文 |
| `/usage` | 查看 token 使用 |
| `/insights [--days N]` | 使用洞察 |
| `/skills` | 浏览技能 |
| `/tools` | 查看可用工具 |
| `/help` | 帮助 |
| `/save` | 保存对话 |
| `/voice on` | 开启语音模式 |

### CLI 专用

- **多行输入**: `Alt+Enter` 或 `Ctrl+J` 换行
- **中断**: 输入新消息按 Enter，或 `Ctrl+C`
- **语音录制**: `Ctrl+B`

## OpenClaw 迁移详情

### 迁移预设

| 预设 | 包含内容 |
|------|---------|
| `full`（默认） | 所有内容（包括密钥） |
| `user-data` | 用户数据（不含密钥） |

### 迁移选项

```bash
hermes claw migrate --help              # 查看所有选项
hermes claw migrate --dry-run           # 预览（不执行）
hermes claw migrate --overwrite         # 覆盖已有冲突
hermes claw migrate --workspace-target  # 包含 AGENTS.md 等工作区文件
```

### 迁移内容清单

- **SOUL.md** → 人格文件
- **MEMORY.md / USER.md** → 记忆条目
- **用户技能** → `~/.hermes/skills/openclaw-imports/`
- **命令白名单** → approval patterns
- **消息平台配置** → 平台 config、allowed users、工作目录
- **API 密钥** → Telegram, OpenRouter, OpenAI, Anthropic, ElevenLabs
- **TTS 资源** → 工作区音频文件
- **AGENTS.md** → 需 `--workspace-target` 参数

## 更新与维护

```bash
hermes update     # 更新到最新版本
hermes doctor     # 诊断环境问题
```

## 相关链接

- 完整文档: https://hermes-agent.nousresearch.com/docs
- GitHub: https://github.com/NousResearch/hermes-agent
