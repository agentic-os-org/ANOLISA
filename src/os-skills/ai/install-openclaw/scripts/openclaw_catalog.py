# SPDX-License-Identifier: Apache-2.0
"""Declarative billing and model defaults for the OpenClaw installer."""

BILLING_ALIASES = {
    "payg": "payg",
    "standard": "payg",
    "dashscope": "payg",
    "postpaid": "payg",
    "pay-as-you-go": "payg",
    "coding": "coding",
    "coding-plan": "coding",
    "token": "token",
    "token-plan": "token",
}

REGION_ALIASES = {
    "china": "china",
    "cn": "china",
    "beijing": "china",
    "global": "singapore",
    "intl": "singapore",
    "international": "singapore",
    "singapore": "singapore",
    "sg": "singapore",
}

BILLING_PLANS = {
    "payg": {
        "name": "Pay-as-you-go",
        "provider_id": "bailian",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://help.aliyun.com/zh/model-studio/get-api-key",
        "model_catalog_url": "https://bailian.console.aliyun.com/?tab=model#/model-market",
        "base_urls": {
            "china": "https://dashscope.aliyuncs.com/apps/anthropic",
            "singapore": "https://dashscope-intl.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.6-plus",
            "MiniMax-M2.5",
            "glm-5",
            "deepseek-v3.2",
        ],
        "key_env": ["BAILIAN_API_KEY", "DASHSCOPE_API_KEY", "QWEN_API_KEY"],
    },
    "coding": {
        "name": "Coding Plan",
        "provider_id": "bailian-coding-plan",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan",
        "model_catalog_url": "https://help.aliyun.com/zh/model-studio/coding-plan",
        "base_urls": {
            "china": "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.6-plus",
            "qwen3.5-plus",
            "qwen3-max-2026-01-23",
            "qwen3-coder-next",
            "qwen3-coder-plus",
            "MiniMax-M2.5",
            "glm-5",
            "glm-4.7",
            "kimi-k2.5",
        ],
        "key_env": ["CODING_PLAN_API_KEY", "QWEN_API_KEY", "BAILIAN_API_KEY"],
    },
    "token": {
        "name": "Token Plan",
        "provider_id": "bailian-token-plan",
        "default_model": "qwen3.6-plus",
        "api_key_url": "https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/overview",
        "model_catalog_url": "https://help.aliyun.com/zh/model-studio/token-plan-overview",
        "base_urls": {
            "china": "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        },
        "models": [
            "qwen3.7-max",
            "qwen3.6-plus",
            "qwen3.6-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v3.2",
            "kimi-k2.6",
            "kimi-k2.5",
            "glm-5.1",
            "glm-5",
            "MiniMax-M2.5",
        ],
        "key_env": ["BAILIAN_TOKEN_PLAN_API_KEY", "TOKEN_PLAN_API_KEY", "BAILIAN_API_KEY"],
    },
}

MODEL_DEFAULTS = {
    "qwen3.7-max": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3.6-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3.6-flash": {"contextWindow": 1_000_000, "maxTokens": 32_768},
    "qwen3.5-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-max-2026-01-23": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-coder-next": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "qwen3-coder-plus": {"contextWindow": 1_000_000, "maxTokens": 65_536},
    "deepseek-v4-pro": {"contextWindow": 163_840, "maxTokens": 32_768},
    "deepseek-v4-flash": {"contextWindow": 163_840, "maxTokens": 16_384},
    "deepseek-v3.2": {"contextWindow": 163_840, "maxTokens": 16_384},
    "kimi-k2.6": {"contextWindow": 262_144, "maxTokens": 32_768},
    "kimi-k2.5": {"contextWindow": 262_144, "maxTokens": 16_384},
    "glm-5.1": {"contextWindow": 202_752, "maxTokens": 16_384},
    "glm-5": {"contextWindow": 202_752, "maxTokens": 16_384},
    "glm-4.7": {"contextWindow": 128_000, "maxTokens": 16_384},
    "MiniMax-M2.5": {"contextWindow": 204_800, "maxTokens": 131_072},
}

VISION_MODELS = {
    "qwen3.6-plus",
    "qwen3.6-flash",
    "qwen3.5-plus",
    "qwen3-coder-plus",
    "kimi-k2.6",
    "kimi-k2.5",
}

OPENAI_THINKING_FORMAT_MODELS = {
    "qwen3.7-max",
    "qwen3.6-plus",
    "qwen3.6-flash",
    "qwen3.5-plus",
    "qwen3-max-2026-01-23",
    "qwen3-coder-next",
    "qwen3-coder-plus",
    "deepseek-v3.2",
    "kimi-k2.6",
    "kimi-k2.5",
    "glm-5.1",
    "glm-5",
    "glm-4.7",
}
