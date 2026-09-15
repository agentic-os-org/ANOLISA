# LLM Provider Request Interception Proxy

[中文版](llm-provider-proxy_zh.md)

## Problem Statement

Tokenless compresses tool schemas and tool responses before they enter the LLM
context window. Today this compression is delivered through agent-framework
hooks (`pre_tool_call`, `transform_tool_result`, `tool.definition`, etc.).

If an agent framework does not expose these hooks, Tokenless cannot be
integrated. We need a compression path that does not depend on the agent's
native hook system.

## Proposal

Introduce an optional **LLM Provider proxy** that sits between an agent and the
upstream LLM provider (OpenAI-compatible API). The proxy intercepts the
chat-completions request and response streams, applies Tokenless compression to
the parts the model will pay tokens for, and forwards the compressed payload.

The agent only needs to point its `base_url` / `api_base` at the proxy; no
agent-side code changes are required.

## Where Compression Can Be Applied

| Request/response part | Compression strategy | Token impact |
|---|---|---|
| `tools[].function` definitions | `tokenless compress-schema` | High when many tools are registered |
| `messages[*].tool` results | `tokenless compress-response` + TOON | High for API/shell-heavy agents |
| Streaming response chunks | Pass-through | Low; keep as-is in v1 |

> **Note:** RTK rewrite is a pre-execution command transformation available only
> through framework adapters. The proxy cannot apply it because it sees tool
> results only after execution. Shell-heavy agents that need RTK should use the
> adapter path (see [Relationship to Existing Adapters](#relationship-to-existing-adapters)).

## High-Level Architecture

```
┌─────────────┐     HTTP/HTTPS      ┌─────────────────────────────┐     ┌──────────────────┐
│   Agent     │ ─────────────────── │  Tokenless Provider Proxy   │────▶│  LLM Provider    │
│  (any)      │                     │  - request interceptor      │     │  (OpenAI API)    │
└─────────────┘                     │  - response interceptor     │     └──────────────────┘
                                    │  - compression pipeline     │
                                    └─────────────────────────────┘
```

### Components

1. **Proxy server** (`tokenless proxy serve`)
   - Listens on a local port (default `localhost:11435`).
   - Forwards every request to the configured upstream provider.
   - Supports HTTP and HTTPS upstreams; TLS certificates are not terminated
     unless explicitly configured for inspection.

2. **Request interceptor**
   - Parses the JSON chat-completions request.
   - Runs `compress-schema` on `tools`.
   - Runs `compress-response` + TOON on any `tool` role messages whose content
     is JSON.

3. **Response interceptor**
   - Passes through streaming responses unchanged in the first version.
   - Non-streaming responses may have tool-call arguments logged for stats but
     are not modified (the provider already received compressed schemas).

4. **Compression pipeline**
   - Reuses the shared hook scripts in `adapters/tokenless/common/hooks/` so
     behavior stays consistent with framework adapters.
   - Records compression statistics via `tokenless-stats` when agent/session IDs
     are supplied through request headers.

## CLI Surface

```bash
# Start the proxy, forwarding to the default OpenAI endpoint
tokenless proxy serve

# Forward to a custom provider endpoint
tokenless proxy serve --upstream https://api.example.com/v1

# Listen on a different port
tokenless proxy serve --port 8080

# Disable schema compression (e.g. while debugging)
tokenless proxy serve --no-schema-compression
```

## Request Headers for Context

| Header | Meaning |
|---|---|
| `X-Tokenless-Agent-Id` | Agent identifier for stats grouping |
| `X-Tokenless-Session-Id` | Session identifier for stats grouping |

These headers are consumed by the proxy and are not forwarded to the provider.

## Open Design Questions

1. **Streaming compression** — Compressing tool results that arrive inside
   streaming deltas is complex because a single tool result may span many
   chunks. The first version applies request-body compression (schema and
   tool-result) to **all** requests regardless of the `stream` flag, since the
   full JSON request body is available before the upstream response stream
   opens. Streaming response chunks are passed through untouched; compressing
   them is deferred to a future version.

2. **Tool result format** — Providers return tool results inside
   `messages[*].content` as a string, often JSON. The proxy must detect JSON
   and decide whether it is a candidate for `compress-response` without
   breaking the provider contract.

3. **Multi-turn conversation** — Compressed tool results accumulate in the
   conversation history. The proxy **must** skip compression for any content
   that already contains a `<<tokenless:HASH>>` marker; the marker is the
   idempotency guard. If a marker is malformed or cannot be parsed, the proxy
   passes the content through unchanged and logs a warning rather than
   attempting compression.

4. **Authentication** — The proxy must forward `Authorization` headers to the
   upstream provider without inspecting or storing them. The proxy must not
   write `Authorization` or other sensitive credential fields to logs, caches,
   or any persistent storage. Only desensitized aggregate statistics may be
   persisted.

## Suggested Phase 1 Scope

- Add a `proxy serve` subcommand to `tokenless-cli`.
- Implement a minimal HTTP pass-through proxy.
- Add request interception for non-streaming chat-completions:
  - `tools` schema compression.
  - `tool` message response compression + TOON encoding.
- Add response pass-through.
- Add integration tests using a mock provider.

## Relationship to Existing Adapters

The proxy is a fallback path, not a replacement for framework adapters.
Adapters that can use native hooks should continue to do so because they can
modify tool arguments (RTK rewrite) and have richer lifecycle events. The proxy
is for agents that expose no hooks at all.
