# Qoder CLI gateway observation

[中文版](qoder-gateway_zh.md)

Qoder CLI's HTTP/SSE chat endpoint uses a custom Base64 alphabet, dollar-sign
padding, and an outer-third swap on the request body. This is reversible
encoding; TLS plaintext capture alone does not produce JSON.

The existing static SSL attachment handles Bun-packaged Qoder binaries. No
additional BPF program, authentication implementation, or API gateway is needed.
The message adapter decodes requests and maps their separate system prompt,
model configuration, and parameters onto the OpenAI request model. Raw decoded
request context retains the native session ID for GenAI attribution.

Responses may wrap each OpenAI-shaped chunk in a JSON object whose string
`body` contains the actual chunk. An envelope marker (`statusCode` or
`statusCodeValue`) is required before unwrapping. Shared parser helpers make
raw SSE, compact reduction, token extraction, and owned delivery read the same
payload. Object-valued tool arguments are normalized for the typed message
parser without changing other providers' argument handling.

Integration tests replay synthetic capture records, including fragmented
envelopes, content, thinking, object-valued tool arguments, and a final
usage-only chunk. Input tokens already include cache hits; cached tokens are
reported separately without increasing the total.

Both modes share bounded HTTP/SSE framing. Raw mode keeps complete events;
compact mode coalesces deltas. HTTP chunk termination needs no SSE `[DONE]`.
Both preserve frame-completion timestamps and reject over-budget streams.
Qoder configuration subscriptions are not recorded as LLM calls.

Coverage is limited to `/algo/api/v2/service/pro/sse/agent_chat_generation`.
Qoder IDE, QoderWork, gRPC, and HTTP/3 need separate compatibility evidence.
Process classification selects the existing SSL resolver; stripped binaries
still need usable symbols, a recognized pattern, or a configured offset entry.
Remove the Qoder discovery rules to disable capture, or revert this change.
