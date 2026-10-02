# Session Compaction

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/session-compaction.md)

Compaction shortens the conversation history sent to the model without deleting the persisted transcript. Use it when a long Agent session is running out of context.

## Compact manually

Run these commands from the shell prompt:

```text
/session compact
/session compact status
/session compact cancel
```

`/session compact` works on the active or selected resumable cosh-core session. The shell remains usable while Agent requests pause. `status` reports the background job; `cancel` leaves the saved conversation and current model context unchanged.

Compaction only uses completed Agent runs. The active run is never summarized. If there is no complete prefix to compact, the provider fails, or the session changes while the job runs, cosh reports an actionable error and keeps the previous model context.

## Automatic compaction

Automatic compaction is enabled by default. It normally starts after model-visible history reaches 70% of the usable context window, targets 30%, and keeps the two most recent complete Agent runs verbatim. At 90%, emergency protection runs before the next provider request when more space is needed.

These limits affect only what the model receives; the saved conversation remains complete. Lowering the model output limit can reserve more room for history, but it also shortens the longest reply.

## Configuration

```toml
[session.compaction]
enabled = true
auto = true
trigger_ratio = 0.70
emergency_ratio = 0.90
target_ratio = 0.30
preserve_recent_runs = 2
```

Optional overrides include `auto_compact_token_limit`, `model_context_window`, and `model_max_output_tokens`. See [Configuration](../configuration.md) before changing them.
