# Alibaba Cloud Model Studio for OpenClaw

Source: https://help.aliyun.com/zh/model-studio/openclaw

Use this reference when selecting Alibaba Cloud Model Studio billing for OpenClaw. The OpenClaw help-center examples use the Anthropic provider shape:

```json
{
  "api": "anthropic-messages",
  "baseUrl": ".../apps/anthropic"
}
```

Do not use the OpenAI-compatible `/compatible-mode/v1` endpoints for OpenClaw unless the user explicitly asks for a custom OpenAI-compatible provider.

## Billing Matrix

| Billing | Provider ID | Base URL | Default model | API key source |
|---|---|---|---|---|
| Pay-as-you-go | `bailian` | `https://dashscope.aliyuncs.com/apps/anthropic` | `qwen3.6-plus` | https://help.aliyun.com/zh/model-studio/get-api-key |
| Pay-as-you-go Singapore | `bailian` | `https://dashscope-intl.aliyuncs.com/apps/anthropic` | `qwen3.6-plus` | https://help.aliyun.com/zh/model-studio/get-api-key |
| Coding Plan | `bailian-coding-plan` | `https://coding.dashscope.aliyuncs.com/apps/anthropic` | `qwen3.6-plus` | https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan |
| Token Plan | `bailian-token-plan` | `https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic` | `qwen3.6-plus` | https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/overview |

## API Key Rules

- The API key must belong to the same billing mode as the Base URL.
- Pay-as-you-go uses an Alibaba Cloud Model Studio API key, commonly formatted like `sk-xxxxx`.
- Coding Plan uses the Coding Plan dedicated API key from the Coding Plan console, formatted like `sk-sp-xxxxx`.
- Token Plan uses the Token Plan team dedicated API key and the token-plan Base URL.
- A 401 usually means the key is invalid, expired, copied with whitespace, or mismatched with the selected billing endpoint.
- Do not select Coding Plan or Token Plan only by looking at the raw key string.
  If the user only gives a normal DashScope/百炼 key and does not name a plan,
  use pay-as-you-go.

When the user has not provided an API key, give the matching source URL instead of asking generically:

- Pay-as-you-go: https://help.aliyun.com/zh/model-studio/get-api-key
- Coding Plan: https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan
- Token Plan: https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/overview

Also provide the relevant model catalog when model availability matters:

- Pay-as-you-go model marketplace: https://bailian.console.aliyun.com/?tab=model#/model-market
- Coding Plan supported models: https://help.aliyun.com/zh/model-studio/coding-plan
- Token Plan supported models: https://help.aliyun.com/zh/model-studio/token-plan-overview

## Local Gateway Auth

The help-center examples set:

```json
{
  "gateway": {
    "mode": "local",
    "auth": { "mode": "none" }
  }
}
```

This is only suitable for local single-machine use. For shared or remote access, run `openclaw doctor --fix` to configure token auth.

## Model Sets

Use these as known-good examples. The exact available list can change with the user's plan subscription.

Pay-as-you-go examples:

- `qwen3.6-plus`
- `MiniMax-M2.5`
- `glm-5`
- `deepseek-v3.2`

Coding Plan examples:

- `qwen3.6-plus`
- `qwen3.5-plus`
- `qwen3-max-2026-01-23`
- `qwen3-coder-next`
- `qwen3-coder-plus`
- `MiniMax-M2.5`
- `glm-5`
- `glm-4.7`
- `kimi-k2.5`

Token Plan examples:

- `qwen3.7-max`
- `qwen3.6-plus`
- `qwen3.6-flash`
- `deepseek-v4-pro`
- `deepseek-v4-flash`
- `deepseek-v3.2`
- `kimi-k2.6`
- `kimi-k2.5`
- `glm-5.1`
- `glm-5`
- `MiniMax-M2.5`

## Safe Merge

Do not replace the whole `~/.openclaw/openclaw.json` on machines that may already have DingTalk or other channels. Merge only:

- `models.mode`
- `models.providers.<providerId>`
- `agents.defaults.model.primary`
- `agents.defaults.models.<providerId>/<model>`
- local gateway fields when requested
