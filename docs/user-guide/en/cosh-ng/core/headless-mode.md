# Headless mode

[中文版](../../../zh/cosh-ng/core/headless-mode.md)

The installed `cosh` entry runs one bounded, non-interactive Core task. The
separate `cosh-core --headless` interface retains its JSONL protocol for
existing clients and frontend adapters.

## Public one-shot task

```bash
cosh --headless 'Explain Linux load average'
cosh --headless --output json - < prompt.txt
cosh --headless --workspace "$PWD" --file-tools=read 'Find the startup entry'
cosh --headless --workspace "$PWD" --file-tools=write --exec=host \
  'Reproduce the failing test, fix it, and rerun it'
```

The full option list is available offline with `cosh --headless --help`.
The prompt is one non-empty UTF-8 argument, or `-` to read non-terminal stdin
through EOF. Input is limited to 1 MiB. A prompt argument does not consume
stdin.

File tools default to `none`; `read` enables read, grep, glob, and directory
listing, while `write` also enables edit and write. Either grant requires an
existing `--workspace`. Core file operations are confined to that pinned
workspace, including protection against path and symlink escapes. Public mode
does not implicitly load global or project `.copilot-shell/CONTEXT.md` files.

`--exec=host` independently enables the `run_command` tool, which accepts a
program and an argv array. It requires `--workspace`, uses that directory as
the initial cwd, and runs as the calling UID. **This is not a Cosh sandbox.**
Commands retain the account's ability to modify files, access paths outside
the workspace, and use the network. Commands start in a new session without a
controlling terminal, receive closed stdin, and have bounded output and
supervised process-group cleanup.

A nonzero command status is returned to the model as a tool result so it can
repair and retry. The `cosh` status describes the overall Agent run, not proof
that a caller's own acceptance check passed. Run that check separately in CI.

Text is the default stdout format. `--output json` emits one object with
`schema_version`, `status`, `error_code`, and `answer`. Exit `0` means
`completed`; exit `1` covers provider, authorization, policy, budget, timeout,
and cleanup failures; exit `2` is invalid input. Missing credentials return
`auth_required`, required user input returns `user_input_required`, and a tool
requiring human approval returns `approval_required` without sending a control
request or starting later calls in that batch.
Failure to install the task's cancellation handlers returns `runtime_unavailable`,
not a policy-loading error, and exits with code `1` before Provider contact.

The default timeout is 2 minutes for text/read tasks and 5 minutes when write
or host execution is enabled. `--timeout` accepts seconds or minutes up to
30m, including result output. A blocked or broken output pipe exits nonzero;
the caller must treat a missing or partial result as unknown. SIGINT and SIGTERM
cancel the task and clean up the supervised command group; JSON reports
`cancelled` when the output transport permits it. SIGKILL and processes that
deliberately leave the supervised group are not covered. `--max-turns` accepts
1–50 and defaults to 12. A task also has a fixed 30-call cumulative tool budget.

Public mode does not discover or start project runtime configuration, Skills,
Hooks, MCP, or Extensions, and does not write a resumable transcript. It uses
the trusted user/system Provider configuration without interactive login.
An administrator can set `[headless] allow_host_exec = false` in the protected
`/etc/copilot-shell/config.toml`. Setting `AGENT_SEC_COSH_SANDBOX_ALL_SHELL=1`
or `true` also rejects `--exec=host` before Provider contact. Unreadable,
malformed, or untrusted mandatory system policy fails as `policy_unavailable`.
Applicable system, user, and explicitly selected audit policies are enforced
together; an additional policy can only narrow the grants above. A nonempty
`COSH_AUDIT_POLICY` must name a readable regular policy file; missing or dangling
paths fail as `policy_unavailable`. Host commands are checked against both
`cosh/run_command` and existing `shell` rules with exact argv, using the supplied
program spelling and its basename. These checks do not inspect commands run
inside an authorized interpreter and do not provide sandboxing. Policy decision
versions are recorded without logging raw argv. The Shell entry also verifies
that its sibling Core has the same package version before starting the task.

## Core JSONL one-shot prompt

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
`shell_evidence` request. Send `interrupt` to cancel the current request while
Core is waiting for an approval required by the tool policy before execution,
an answer to `ask_user`, or a `shell_evidence` response.

```json
{"type":"control_request","request_id":"interrupt-1","request":{"subtype":"interrupt"}}
```

Set `request.capabilities.can_handle_turn_cancelled` to `true` in
`initialize` to receive a `result` with `error_code: "turn_cancelled"`
when one of these waits is interrupted. Wait for that result before sending
the next user request on the same process. To close the process, send
`shutdown` at the idle boundary.

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
{"type":"result","subtype":"success","is_error":false,"session_id":"...","duration_ms":1234}
```

The optional `result` field is omitted on success.

Check for `error_code: "turn_cancelled"` before handling generic error results.
Failure results use `is_error: true` and include `errors`. Session load and
persistence failures also include `session_error_code` and `session_error_phase`. Invalid
JSONL input produces an error result and exits with a non-zero status. Keep
stdout reserved for protocol messages; diagnostics are logged separately.

See the developer [IPC protocol reference](../../../../../src/cosh-ng/docs/design/en/ipc-protocol.md#cosh-core-headless-jsonl-cancellation) for the result frame and v1 compatibility behavior.
