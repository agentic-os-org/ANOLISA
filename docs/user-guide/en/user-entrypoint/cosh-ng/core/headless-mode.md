# Headless mode

[中文版](../../../../zh/user-entrypoint/cosh-ng/core/headless-mode.md)

Headless mode is a line-delimited JSON protocol on stdin/stdout. Use the
one-shot form for scripts and the long-running form for a frontend adapter.

## One-shot prompt

```bash
cosh-core --headless "Check disk usage; do not modify anything"
```

Core streams events, writes an `assistant` message, and finishes with a
`result` object. Use `--model`, `--approval-mode`, `--tools`, or
`--allowed-tools` when the script needs different settings.

## Long-running client

Start the process and send one JSON object per line:

```bash
cosh-core --headless
```

```json
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}
{"type":"user","message":{"role":"user","content":"List files in current directory"}}
```

The first response is an initialization acknowledgement followed by a system
summary:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{...}}}}
{"type":"system","subtype":"init","session_id":"...","session_resumable":true,"model":"...","tools":[...]}
```

Keep the `session_id` only when `session_resumable` is `true`. A client should
read `stream_event` lines until the message stops, then consume the final
`assistant` and `result` messages.

## Control requests

Core may pause for a tool decision or user input. Reply with the same request
ID and a `control_response`:

```json
{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}
{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow","toolUseID":"toolu-1"}}}
```

Other Core requests include `ask_user`, `auth_required`, and the optional
`shell_evidence` request. A frontend can interrupt or stop the process with:

```json
{"type":"control_request","request_id":"stop-1","request":{"subtype":"shutdown"}}
```

If credentials are missing, answer `auth_required` with the selected
`provider_id`, its `values`, and `persist` according to the provider form. See
[Providers](providers.md) for the available choices.

## Resume or compact a session

```bash
cosh-core --headless --resume <session-id>
cosh-core --headless --resume <session-id> --compact
```

The ID is workspace-scoped. The workspace is the current directory unless the
frontend passes `--workspace <path>`. Set `session.auto_persist = false` to keep
history only in memory; such a session is not resumable.

## Results and errors

Successful turns end with a result like:

```json
{"type":"result","subtype":"success","is_error":false,"result":"completed","session_id":"...","duration_ms":1234}
```

Failures use `is_error: true` and include `errors`; session load and persistence
failures also include `session_error_code` and `session_error_phase`. Invalid
JSONL input produces an error result and exits with a non-zero status. Keep
stdout reserved for protocol messages; diagnostics are logged separately.

For the complete schema, see the developer [IPC protocol reference](../../../../../developer-guide/en/cosh-ng/ipc-protocol.md).
