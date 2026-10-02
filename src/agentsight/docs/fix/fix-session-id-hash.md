# session_id 为 SHA256 hash 而非真实 UUID 的排查与修复

## 问题现象

`genai_events.db` 中部分记录的 `session_id` 是 32 位无连字符的 SHA256 hash 格式（如 `6bf0590167b6e42badc73e29d3f4ee5b`），而非 agent 的真实 UUID session_id（如 `a8c0435d-a507-41e9-b91d-d48ee5aadfdd`）。

预期：session_id 应从 OpenClaw 的 `.jsonl` 文件名（UUID）中提取，通过 `ResponseSessionMapper` 映射 responseId → sessionId 写入 DB。

## 排查过程

### 第一轮：验证 complete_pending 是否更新 session_id

**假设**：`complete_pending` UPDATE 语句缺少 `session_id` 字段，导致 mapper 解析出的真实 UUID 没有写回 DB。

**验证方法**：
```bash
# 清理 DB，启动 trace，触发 LLM 请求
rm -f /var/log/sysak/.agentsight/genai_events.db
RUST_LOG=debug /root/agentsight trace > /tmp/debug.log 2>&1 &
openclaw agent -m 'hello' --agent main --timeout 30
sleep 15
sqlite3 /var/log/sysak/.agentsight/genai_events.db 'SELECT session_id, LENGTH(session_id) FROM genai_events'
```

**结果**：session_id 仍为 hash。修复 UPDATE 语句后重新验证，session_id 仍为 hash。

**结论**：根因不在 `complete_pending`，mapper 根本没命中（没有真实 session_id 可写入）。

### 第二轮：定位 mapper 未命中的原因

**关键日志分析**：

```
[06:05:37Z DEBUG] Analyzing aggregated result(http2_stream_complete)
[06:05:37Z DEBUG] Path '' does not match any known LLM API endpoint
[06:05:37Z DEBUG] [GenAI] Parsing SSE body with 7 chunks
[06:05:37Z DEBUG] [GenAI] Promoted pending→complete
[06:05:37Z DEBUG] ResponseSessionMapper: responseId=chatcmpl-xxx → sessionId=a8c0435d-...
```

**发现**：

1. **AggregatedResult 类型是 `http2_stream_complete`**，而非 `SseComplete` — OpenClaw 使用 HTTP/2 协议，LLM 流式请求走 HTTP/2 stream

2. **`parsed_message` 为 None** — `extract_message_from_http` 对 `Http2StreamComplete` 返回 None，导致 `ParsedApiMessage` 不生成，`response_id()` 无法从 parsed message 提取

3. **事件时序**：LLM 事件处理先于 FileWrite → mapper 在 LLM 事件处理时尚未有 responseId→sessionId 映射 → mapper 查询未命中 → fallback 到 SHA256 hash

4. **`build_with_pending` 中 `pending_response_id` 只依赖 `parsed_message`** — parsed_message 为 None 时 response_id 也为 None，事件不进入 PendingGenAI 队列 → mapper 后续的映射无法触发 session_id 更新

5. **HTTP/2 stream 的 path 为空** — HPACK 动态表索引导致 `decode_headers_stateless` 无法解码 `:path` pseudo-header，返回空字符串

### 第三轮：找到有效的修复路径

问题链路：

```
Http2StreamComplete → parsed_message=None → response_id=None
                                       ↓
                              mapper 未命中 → session_id=fallback hash
                                       ↓
                      pending_response_id=None → 不入队 → mapper 后续映射无效
```

虽然 `parsed_message` 为 None，但 `build_llm_call` 内部的 `extract_response_id` 有 SSE fallback，能从 `http.response_body`（JSON array 格式）中提取 response_id。关键断点在于：

- `extract_response_id` 的 SSE fallback 只处理 `data:` 行格式，不支持 JSON array 格式（HTTP/2 stream 产出的 response_body）
- `build_with_pending` 不使用 `build_llm_call` 产出的 response_id（来自 SSE fallback），只使用 `parsed_message` 的 response_id

## 修复内容

### 修复 1：`extract_response_id` 增加 JSON array 格式支持

**文件**：`src/genai/builder.rs`

**原因**：HTTP/2 SSE stream 的 `response_body` 是 JSON array 格式（如 `[{"id":"chatcmpl-xxx",...}]`），而非 HTTP/1.1 的 `data: {...}` 行格式。旧代码只处理行格式。

**改动**：在 SSE fallback 中先尝试 JSON array 格式解析，再 fallback 到行格式：

```rust
// 先尝试 JSON array 格式（HTTP/2 stream 聚合产出）
if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
    if let Some(arr) = v.as_array() {
        for chunk in arr {
            if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
}
// 再尝试 SSE 行格式（HTTP/1.1 产出）
for line in body.lines() { ... }
```

**验证**：trace_id 从内部生成 ID 变为真实 `chatcmpl-xxx`，但 session_id 仍为 hash。

### 修复 2：`build_with_pending` 使用 call.metadata 判断 pending

**文件**：`src/genai/builder.rs`

**原因**：旧代码用 `parsed_message.response_id()` 设置 `pending_response_id`。当 parsed_message 为 None 时（HTTP/2 stream 场景），pending_response_id 也为 None，事件不进入 PendingGenAI 队列，mapper 后续解析的映射无法更新 DB。

**改动**：改为从 `build_llm_call` 产出的 `call.metadata["response_id"]` 判断 pending（该值来自 SSE fallback，HTTP/2 场景下有值）：

```rust
// 旧：依赖 parsed_message（HTTP/2 场景为 None）
let response_id = parsed_message.as_ref().and_then(|m| m.response_id());

// 新：使用 call.metadata（包含 SSE fallback 提取的 response_id）
let response_id = llm_call.metadata.get("response_id").cloned();
let mapper_hit = response_id.as_deref()
    .and_then(|rid| response_mapper.get_session_by_response_id(rid))
    .is_some();
if response_id.is_some() && !mapper_hit {
    pending_response_id = response_id;
}
```

**验证**：session_id 变为真实 UUID ✓

### 修复 3：`complete_pending` UPDATE 增加 session_id

**文件**：`src/storage/sqlite/genai.rs`

**原因**：`complete_pending` 的 UPDATE 语句不更新 `session_id` 字段。即使后续 `resolve_pending_genai` 在内存中解析了真实 session_id，写入 DB 时也不会覆盖 hash fallback。

**改动**：UPDATE 增加 `session_id = ?3`，参数增加 `call.metadata.get("session_id")`。

```sql
-- 旧：不更新 session_id
UPDATE genai_events SET status='complete', trace_id=?1, conversation_id=?2, ...

-- 新：更新 session_id
UPDATE genai_events SET status='complete', trace_id=?1, conversation_id=?2, session_id=?3, ...
```

## 数据流对比

### 修复前

```
SSL events → Http2StreamComplete → parsed_message=None
    → response_id=None (parsed_message) → mapper 未命中 → session_id=hash
    → pending_response_id=None → 不入队 → FileWrite mapper 映射无用
```

### 修复后

```
SSL events → Http2StreamComplete → parsed_message=None
    → build_llm_call 内 SSE fallback → response_id="chatcmpl-xxx" (metadata)
    → mapper 未命中（FileWrite 还没到） → pending_response_id="chatcmpl-xxx"
    → 入队 PendingGenAI
    → FileWrite 到达 → mapper 映射 chatcmpl-xxx → a8c0435d-UUID
    → resolve_pending_genai → session_id=UUID
    → complete_pending UPDATE → DB session_id=UUID ✓
```

## 教训

1. **不要只看旧 DB 数据下结论** — 必须从零运行 trace + 触发请求，观察完整数据生成过程
2. **先看 debug 日志定位断点** — 凭代码推测容易走偏（如第一轮以为 UPDATE 缺字段是根因，实际 mapper 都没命中）
3. **关注事件处理时序** — agentsight 事件循环是串行的，LLM 事件和 FileWrite 事件的到达顺序影响 mapper 命中
4. **不要顺手改无关代码** — 如给 `Http2StreamComplete` 加 `analyze_message` 支持，因为 path 为空（HPACK 问题）实际无效
5. **多轮迭代** — 一个问题可能涉及多个断点，每轮只修一个，验证后再分析下一个