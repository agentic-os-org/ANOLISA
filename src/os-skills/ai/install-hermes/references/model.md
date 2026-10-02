# Hermes Agent 模型提供商配置指南（国内版）

---

## Provider 速查总表

| Provider | 环境变量 | Provider ID | 推荐模型 |
|----------|---------|-------------|---------|
| **Alibaba / 通义千问** | `DASHSCOPE_API_KEY` | `alibaba` | `qwen-turbo` / `qwen-plus` |
| **DeepSeek** | `DEEPSEEK_API_KEY` | `deepseek` | `deepseek-chat` |
| **z.ai / 智谱 GLM** | `GLM_API_KEY` | `zai` | `glm-4-plus` |
| **Kimi / Moonshot（中国）** | `KIMI_CN_API_KEY` | `kimi-coding-cn` | `kimi-k2.5` |
| **MiniMax（中国）** | `MINIMAX_CN_API_KEY` | `minimax-cn` | `MiniMax-M2.7` |
| **Ollama（本地）** | 无需密钥 | `custom` | 按本地已拉取模型 |

### 快速配置命令

```bash
# 设置 API Key + 模型（二合一）
hermes config set <ENV_VAR> <your-api-key>
hermes config set model <provider-id>/<model-name>

# 示例：配置通义千问
hermes config set DASHSCOPE_API_KEY sk-xxx
hermes config set DASHSCOPE_BASE_URL https://dashscope.aliyuncs.com/compatible-mode/v1
hermes config set model alibaba/qwen-turbo

# 交互式配置（推荐初次使用）
hermes model
```

### 通用命令速查

```bash
hermes model              # 交互式添加/切换 Provider
/model                    # 在会话内快速切换（仅限已配置）
hermes config set model <provider>/<model>    # 直接设置默认模型
hermes doctor             # 诊断 Provider 配置
```

---

## Alibaba Cloud / 通义千问（DashScope）

阿里云的通义千问（Qwen）系列模型，通过 DashScope 平台提供。

### 前置准备

1. 注册 [阿里云百炼](https://bailian.console.aliyun.com/) 账号
2. 创建 API Key

### 配置步骤

```bash
hermes config set DASHSCOPE_API_KEY sk-xxxxxxxxxxxxxxxx
hermes config set DASHSCOPE_BASE_URL https://dashscope.aliyuncs.com/compatible-mode/v1
hermes config set model alibaba/qwen-turbo
```

> ⚠️ **中国大陆必须设置 `DASHSCOPE_BASE_URL`**
>
> Hermes 默认使用国际版端点 `dashscope-intl.aliyuncs.com`，中国区 Key 会返回 401。
> 必须在 `~/.hermes/.env` 中追加：
> ```bash
> DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
> ```

### 可用模型

| 模型 | 上下文窗口 | 特点 | 适用场景 |
|------|-----------|------|----------|
| `qwen3-235b-a22b` | 131K | 最强旗舰 | 复杂推理 |
| `qwen3.5-plus` | 131K | 最新增强版 | 代码、推理 |
| `qwen-plus` ⭐ | 131K | 性价比首选 | 日常使用（**推荐**） |
| `qwen-turbo` | 131K | 快速版 | 高并发 |
| `qwen-long` | 10M | 超长文本 | 文档分析 |

> ⚠️ **`qwen-max` 不可用**：上下文窗口仅 32K，低于 Hermes Agent 64K 最低要求，启动时报错。请使用 `qwen-plus` 或 `qwen-turbo`（均为 131K）。

### 完整 .env 示例

```bash
DASHSCOPE_API_KEY=sk-xxxxxxxxxxxxxxxx
DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
```

---

## DeepSeek

### 前置准备

1. 注册 [DeepSeek 开放平台](https://platform.deepseek.com/)
2. 创建 API Key

### 配置步骤

```bash
hermes config set DEEPSEEK_API_KEY sk-xxx
hermes config set model deepseek/deepseek-chat
```

### 可用模型

| 模型 | 特点 |
|------|------|
| `deepseek-chat` | 通用对话，平衡性能 |
| `deepseek-reasoner` | 强化推理，适合数学/逻辑 |
| `deepseek-coder` | 代码专用 |

---

## z.ai / 智谱 GLM

### 前置准备

1. 注册 [智谱 AI 开放平台](https://open.bigmodel.cn/)
2. 在控制台创建 API Key

### 配置步骤

```bash
hermes config set GLM_API_KEY your-glm-api-key
hermes config set model zai/glm-4-plus
```

### 可用模型

| 模型 | 特点 | 适用场景 |
|------|------|---------|
| `glm-4-plus` | 平衡性能与成本 | 日常对话、文本分析 |
| `glm-4-air` | 轻量快速 | 简单任务、高并发 |
| `glm-4-flash` | 极速响应 | 实时交互 |

---

## Kimi / Moonshot（中国版）

针对中国大陆用户的优化版本，使用国内 API 端点。

### 前置准备

1. 注册 [Moonshot AI](https://platform.moonshot.cn/) 账号（国内站）
2. 创建 API Key

### 配置步骤

```bash
hermes config set KIMI_CN_API_KEY your-kimi-cn-api-key
hermes config set model kimi-coding-cn/kimi-k2.5
```

### 可用模型

| 模型 | 说明 |
|------|------|
| `kimi-k2.5` | Kimi 2.5，性能优化版 |
| `kimi-k2` | Kimi 2.0 稳定版 |
| `moonshot-v1-128k` | 128K 超长上下文 |

---

## MiniMax（中国版）

### 前置准备

1. 注册 [MiniMax 开放平台](https://platform.minimaxi.com/)
2. 创建 API Key

### 配置步骤

```bash
hermes config set MINIMAX_CN_API_KEY your-minimax-cn-api-key
hermes config set model minimax-cn/MiniMax-M2.7
```

### 可用模型

| 模型 | 特点 |
|------|------|
| `MiniMax-M2.7` | 最新旗舰模型 |
| `abab6.5-chat` | 通用对话模型 |
| `abab6.5s-chat` | 快速响应版本 |

---

## Ollama（本地模型）

适合离线场景或需要数据隐私的场景。

### 前置准备

1. 安装 [Ollama](https://ollama.com/)
2. 拉取模型：`ollama pull qwen2.5:7b`（推荐国内可用模型）

### 配置步骤

```bash
hermes model
# → 选择 "Custom endpoint"
# → 输入 http://127.0.0.1:11434/v1
# → 选择已拉取的模型
```

---

## 多 Provider 故障转移

```yaml
# ~/.hermes/config.yaml
model:
  provider: "alibaba"
  default: "qwen-plus"

  fallback_providers:
    - provider: "deepseek"
      model: "deepseek-chat"
    - provider: "zai"
      model: "glm-4-plus"
```

---

## 常见问题

### Q: 通义千问配置后返回 401？

必须设置国内端点，见上方 Alibaba 节的说明。

### Q: 如何切换已配置的 Provider？

```bash
hermes model     # 交互式切换
/model           # 在会话中快速切换
```

### Q: API Key 存储在哪里？

所有 API Key 存储在 `~/.hermes/.env`，权限 600，仅当前用户可读。

### Q: 如何验证配置是否生效？

```bash
hermes doctor    # 诊断配置
hermes           # 启动对话测试
```

### Q: 如何查看用量和配额？

- 阿里云：https://dashscope.console.aliyun.com/usage
- DeepSeek：https://platform.deepseek.com/usage
- 智谱 AI：https://open.bigmodel.cn/usercenter/apikeys
- Moonshot：https://platform.moonshot.cn/console/usage
- MiniMax：https://platform.minimaxi.com/user-center/billing

---

## 最佳实践

1. 首次使用从 `hermes model` 交互式配置开始，避免手动编辑出错
2. 通义千问务必设置 `DASHSCOPE_BASE_URL`，否则必然 401
3. 建议同时配置 2 个 Provider 做故障转移（如 Alibaba + DeepSeek）
4. 定期在各平台控制台检查用量，避免超额
5. 定期备份 `~/.hermes/.env` 和 `~/.hermes/config.yaml`
