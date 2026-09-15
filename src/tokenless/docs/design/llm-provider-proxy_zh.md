# LLM Provider 请求拦截代理

[English](llm-provider-proxy.md)

## 问题背景

Tokenless 通过在工具定义和工具返回结果进入 LLM 上下文窗口之前进行压缩来节省 token。目前这种压缩能力依赖 agent 框架提供的 hook（如 `pre_tool_call`、`transform_tool_result`、`tool.definition` 等）。

如果某个 agent 框架没有暴露这些 hook，Tokenless 就无法接入。因此需要一条不依赖 agent 原生 hook 系统的压缩路径。

## 方案概述

引入一个可选的 **LLM Provider 代理**，位于 agent 与上游 LLM Provider（OpenAI 兼容 API）之间。代理拦截 chat-completions 的请求与响应流，对模型需要付费 token 的部分应用 Tokenless 压缩，再把压缩后的报文转发出去。

agent 只需要把 `base_url` / `api_base` 指向代理地址，不需要修改 agent 侧代码。

## 可压缩的部分

| 请求/响应部分 | 压缩策略 | token 影响 |
|---|---|---|
| `tools[].function` 定义 | `tokenless compress-schema` | 工具数量多时影响大 |
| `messages[*].tool` 结果 | `tokenless compress-response` + TOON | API/Shell 密集型 agent 影响大 |
| 流式响应 chunk | 透传 | 影响小；v1 保持原样 |

> **注：** RTK rewrite 是执行前的命令改写，仅通过框架 adapter 可用。代理只能在工具执行后看到结果，无法应用 RTK。需要 RTK 的 Shell 密集型 agent 应使用 adapter 路径（见[与现有 Adapter 的关系](#与现有-adapter-的关系)）。

## 高层架构

```
┌─────────────┐     HTTP/HTTPS      ┌─────────────────────────────┐     ┌──────────────────┐
│   Agent     │ ─────────────────── │  Tokenless Provider Proxy   │────▶│  LLM Provider    │
│  (任意)      │                     │  - 请求拦截器                │     │  (OpenAI API)    │
└─────────────┘                     │  - 响应拦截器                │     └──────────────────┘
                                    │  - 压缩管线                  │
                                    └─────────────────────────────┘
```

### 组件

1. **代理服务器** (`tokenless proxy serve`)
   - 监听本地端口（默认 `localhost:11435`）。
   - 将每个请求转发到配置好的上游 Provider。
   - 支持 HTTP/HTTPS 上游；除非显式配置用于 inspection，否则不终止 TLS。

2. **请求拦截器**
   - 解析 JSON chat-completions 请求。
   - 对 `tools` 运行 `compress-schema`。
   - 对 `tool` 角色的消息（内容为 JSON）运行 `compress-response` + TOON。

3. **响应拦截器**
   - 第一版对流式响应只做透传。
   - 非流式响应可记录 tool-call 参数用于统计，但不修改内容（Provider 已经收到压缩后的 schema）。

4. **压缩管线**
   - 复用 `adapters/tokenless/common/hooks/` 中的共享 hook 脚本，保证与各框架 adapter 行为一致。
   - 当请求头提供 agent/session ID 时，通过 `tokenless-stats` 记录压缩统计。

## CLI 形态

```bash
# 启动代理，默认转发到 OpenAI 官方端点
tokenless proxy serve

# 转发到自定义 Provider 端点
tokenless proxy serve --upstream https://api.example.com/v1

# 监听其他端口
tokenless proxy serve --port 8080

# 关闭 schema 压缩（例如调试时）
tokenless proxy serve --no-schema-compression
```

## 用于上下文的请求头

| Header | 含义 |
|---|---|
| `X-Tokenless-Agent-Id` | 用于统计分组的 agent 标识 |
| `X-Tokenless-Session-Id` | 用于统计分组的 session 标识 |

这些请求头由代理消费，不会转发给 Provider。

## 待决设计问题

1. **流式压缩** — 流式 delta 中可能包含跨多个 chunk 的 tool 结果，压缩较复杂。第一版对**所有**请求（无论 `stream` 标志是否为 true）都执行请求体压缩（schema 与 tool 结果），因为完整 JSON 请求在上游响应流开启之前就已可用。流式响应 chunk 不做任何内容修改，仅透传；对流式响应的压缩推迟到未来版本。未来引入流式压缩时需额外进行安全性与兼容性评审。

2. **Tool 结果格式** — Provider 通常把 tool 结果以字符串形式放在 `messages[*].content` 中，且常为 JSON。代理需要识别 JSON 并判断是否适合 `compress-response`，同时不破坏 Provider 约定。

3. **多轮对话** — 压缩后的 tool 结果会累积在对话历史中。代理**必须**跳过对已包含 `<<tokenless:HASH>>` 标记的内容的压缩；该标记是幂等性保护。若标记格式错误或无法解析，代理应原样透传内容并记录告警，而不是尝试压缩。

4. **鉴权** — 代理必须把 `Authorization` 请求头原样转发给上游 Provider，不得查看或保存。代理不得将 `Authorization` 或其他敏感凭据字段写入日志、缓存或任何持久存储。仅脱敏后的聚合统计数据可持久化。

## 建议的 Phase 1 范围

- 在 `tokenless-cli` 中新增 `proxy serve` 子命令。
- 实现一个最小可用的 HTTP 透传代理。
- 为非流式 chat-completions 添加请求拦截：
  - `tools` schema 压缩。
  - `tool` 消息响应压缩 + TOON 编码。
- 响应只做透传。
- 使用 mock provider 编写集成测试。

## 与现有 Adapter 的关系

代理是一条 fallback 路径，不会取代框架 adapter。能够使用原生 hook 的 adapter 仍应优先使用，因为它们可以修改工具参数（RTK rewrite）并获取更丰富的生命周期事件。代理面向完全不暴露 hook 的 agent。
