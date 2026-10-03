# 认证

Copilot Shell 支持多种认证方式连接 AI 模型。本文介绍每种认证方式的配置
和使用方法。

## 认证方式概览

| 认证方式 | 适用场景 | 配置方式 |
|----------|----------|----------|
| 阿里云认证 | 阿里云 ECS 或企业用户 | 自动检测 / AK-SK |
| OpenAI 兼容 | 第三方模型端点 | API Key + Base URL |

## 阿里云认证（默认）

阿里云认证是 Copilot Shell 的默认方式，根据运行环境自动选择认证流程。

### 在 ECS 实例上

Copilot Shell 自动检测 ECS 环境并启动 Web 认证：

1. 启动 `cosh`，系统显示浏览器链接和二维码
2. 扫码或在浏览器中打开链接完成认证
3. 认证成功后自动返回终端

### 在非 ECS 环境

直接使用 AK/SK（AccessKey ID / AccessKey Secret）认证：

1. 启动 `cosh`
2. 选择「阿里云认证」
3. 输入你的 AccessKey ID 和 AccessKey Secret

### 模型选择

阿里云认证成功后，可使用 `/model` 命令切换可用模型。已使用过的模型会记录
在 `security.auth.aliyunModels` 中供快速切换。

## OpenAI 兼容认证

适用于任何兼容 OpenAI API 格式的端点，包括：

- **DashScope**（阿里云百炼）
- **DeepSeek**
- **Kimi**（月之暗面）
- **GLM**（智谱 AI）
- **MiniMax**

### 配置步骤

1. 启动 `cosh` 或执行 `/auth`
2. 选择「OpenAI 兼容」
3. 输入以下信息：
   - **Base URL**：API 端点地址（如 `https://dashscope.aliyuncs.com/compatible-mode/v1`）
   - **API Key**：服务商提供的密钥
   - **模型名称**：要使用的模型（如 `qwen3.7-max`）

### 通过配置文件设置

也可以直接编辑 `~/.copilot-shell/settings.json`：

```json
{
  "security": {
    "auth": {
      "selectedType": "openai-compatible",
      "apiKey": "sk-xxx",
      "baseUrl": "https://dashscope.aliyuncs.com/compatible-mode/v1",
      "openaiModel": "qwen3.7-max"
    }
  }
}
```

### 通过环境变量设置

```bash
export OPENAI_API_KEY="sk-xxx"
export OPENAI_BASE_URL="https://dashscope.aliyuncs.com/compatible-mode/v1"
```

## 模型提供商配置

Copilot Shell 支持按认证类型配置多个模型。通过 `modelProviders` 字段，
可以为同一认证方式预设多个模型选项：

```json
{
  "modelProviders": {
    "openai-compatible": [
      {
        "name": "deepseek-chat",
        "baseUrl": "https://api.deepseek.com/v1",
        "apiKey": "sk-xxx"
      },
      {
        "name": "qwen3.7-max",
        "baseUrl": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "apiKey": "sk-yyy"
      }
    ]
  }
}
```

配置后，通过 `/model` 命令在预设模型间快速切换。

## 切换认证方式

在会话内随时使用 `/auth` 命令切换认证方式：

```
/auth
```

系统会引导你选择新的认证方式并完成配置。

## 强制认证类型

管理员可通过系统级配置强制用户使用指定的认证方式：

在 `/etc/copilot-shell/settings.json` 中设置：

```json
{
  "security": {
    "auth": {
      "enforcedType": "aliyun"
    }
  }
}
```

当强制类型与用户选择的类型不匹配时，系统会提示重新认证。

## 故障排查

**认证失败**

- 检查网络连接是否正常
- 确认 API Key 未过期
- 验证 Base URL 格式是否正确（通常以 `/v1` 结尾）

**ECS Web 认证超时**

- 确认 ECS 安全组放行了回调端口
- 尝试使用 AK/SK 方式作为备选

**模型不可用**

- 通过 `/model` 查看当前可用模型列表
- 确认当前认证方式是否支持目标模型
