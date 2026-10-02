# Authentication

Copilot Shell supports multiple authentication methods for connecting to AI
models. This guide covers the configuration and usage of each method.

## Authentication Methods Overview

| Method | Use Case | Configuration |
|--------|----------|---------------|
| Alibaba Cloud Auth | Alibaba Cloud ECS or enterprise users | Auto-detect / AK-SK |
| OpenAI Compatible | Third-party model endpoints | API Key + Base URL |

## Alibaba Cloud Authentication (Default)

Alibaba Cloud authentication is the default method. It automatically selects
the authentication flow based on the runtime environment.

### On ECS Instances

Copilot Shell auto-detects the ECS environment and starts Web authentication:

1. Launch `cosh`; the system displays a browser link and QR code
2. Scan the code or open the link in a browser to complete authentication
3. After successful authentication, control returns to the terminal

### Non-ECS Environments

Use AK/SK (AccessKey ID / AccessKey Secret) directly:

1. Launch `cosh`
2. Select "Alibaba Cloud Auth"
3. Enter your AccessKey ID and AccessKey Secret

### Model Selection

After successful Alibaba Cloud authentication, use the `/model` command to
switch between available models. Previously used models are recorded in
`security.auth.aliyunModels` for quick switching.

## OpenAI Compatible Authentication

Works with any OpenAI API-compatible endpoint, including:

- **DashScope** (Alibaba Cloud Bailian)
- **DeepSeek**
- **Kimi** (Moonshot AI)
- **GLM** (Zhipu AI)
- **MiniMax**

### Configuration Steps

1. Launch `cosh` or run `/auth`
2. Select "OpenAI Compatible"
3. Provide the following:
   - **Base URL**: API endpoint (e.g., `https://dashscope.aliyuncs.com/compatible-mode/v1`)
   - **API Key**: The key from your provider
   - **Model name**: The model to use (e.g., `qwen3.7-max`)

### Via Configuration File

Edit `~/.copilot-shell/settings.json` directly:

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

### Via Environment Variables

```bash
export OPENAI_API_KEY="sk-xxx"
export OPENAI_BASE_URL="https://dashscope.aliyuncs.com/compatible-mode/v1"
```

## Model Providers Configuration

Copilot Shell supports configuring multiple models per authentication type.
Use the `modelProviders` field to preset multiple model options:

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

After configuration, use `/model` to quickly switch between preset models.

## Switching Authentication

Use the `/auth` command at any time within a session:

```
/auth
```

The system will guide you through selecting a new method and completing setup.

## Enforced Authentication Type

Administrators can force a specific authentication method via system-level
configuration.

In `/etc/copilot-shell/settings.json`:

```json
{
  "security": {
    "auth": {
      "enforcedType": "aliyun"
    }
  }
}
```

When the enforced type does not match the user's selection, the system prompts
re-authentication.

## Troubleshooting

**Authentication failure**

- Check network connectivity
- Confirm the API key has not expired
- Verify the Base URL format (usually ends with `/v1`)

**ECS Web authentication timeout**

- Confirm the ECS security group allows the callback port
- Try AK/SK as a fallback

**Model unavailable**

- Use `/model` to view available models
- Confirm the current authentication method supports the target model
