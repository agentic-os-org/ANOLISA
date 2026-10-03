# Code safety through an AW Provider

[中文版](../../../zh/agent-security/agent-sec-core/aw-provider.md)

The AW Provider reuses local Bash and Python safety rules with no model or Token
cost. One Provider configuration can select code fields from any normalized tool
name. Before execution it reports a scan result or requests blocking; after
execution it observes the after-tool event without rescanning or changing the tool result.

This entrypoint is available in the source-built Rust V2 `agent-sec-cli`. The
Python V1 CLI does not implement it. Native Agent installation and enforcement
remain the AW adapter's responsibility; a successful Provider response alone
does not prove that an Agent adopted its effect.

## Prepare the Provider

Build from the repository root:

```sh
cargo build --manifest-path src/agent-sec-core/v2/Cargo.toml --locked -p asc-cli
export PATH="$PWD/src/agent-sec-core/v2/target/debug:$PATH"
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","request_id":"describe-1","method":"describe"}' \
  | agent-sec-cli aw-provider
```

`describe` and `validate_config` work without a running sec daemon. Actual code
scanning requires the [existing V2 daemon deployment](skillsec-v2.md). Select its
endpoint with `--socket /run/agent-sec-core/daemon.sock`; the CLI never starts a
daemon or runs a local scan when that endpoint is unavailable. `--timeout-ms`
bounds the inner daemon call (default 5000 ms), further restricted by the AW
invocation's remaining `budget_ms`.

## Configure AW

Start from the complete [aw.yaml example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/agent-sec-core/v2/examples/aw.yaml).
Its Provider command is:

```yaml
argv: [agent-sec-cli, --socket, /run/agent-sec-core/daemon.sock, aw-provider]
```

Ensure this resolves to the Rust binary built above. The private object belongs
under `spec.providers.security.config`; AW passes it unchanged:

```yaml
version: 1
mode: observe
tools:
  shell: {language: bash, input_pointer: /command}
  python: {language: python, input_pointer: /code}
```

| Field | Accepted values and behavior |
|---|---|
| `version` | Required integer `1`. |
| `mode` | Required `observe` or `block`. No implicit blocking default. |
| `tools` | Required object with 1–128 exact `event.tool.name` mappings. |
| `tools.<name>.language` | `bash` or `python`; uses all built-in regex rules. |
| `tools.<name>.input_pointer` | RFC 6901 JSON Pointer within `event.tool.input`; the selected value must be a string. Empty pointer selects the entire input. |

Unknown fields, unsupported languages and malformed pointers fail configuration
validation. Tool names and pointers are limited to 1024 UTF-8 bytes. Names are
case sensitive. An adapter may normalize a native name differently: configure
the actual `event.tool.name`, without assuming that `Bash`, `exec` and `shell`
are interchangeable. Nested inputs and names containing `/` or `~` are supported
through ordinary JSON Pointer escaping.

An unmapped tool emits `observe / tool_unmapped`; it has **not been scanned**.
A mapped tool whose pointer is missing or does not select a string returns
`invalid_tool_input`. This Provider can map arbitrary tool names, but its current
safety analysis covers only Bash and Python source. It does not scan arbitrary
tool output, PII, prompts or files.

## Select operations and failure behavior

| AW event | Provider operation | Result |
|---|---|---|
| `tool.before` | `scan_code` | `observe / code_pass`, `observe / code_risk`, or `block / code_risk`. |
| `tool.after` | `observe_tool` | `observe / tool_observed`; does not call the scanner. |

`mode: block` requests blocking for scanner `warn` and `deny`, matching the
existing Code Scanner hook's explicit `enable_block` threshold. It requires
`block` in the AW step's `effects`; the example includes this capability but
keeps `mode: observe` until explicitly changed. `observe` must also be admitted
to record clean, skipped and after-tool outcomes. A block is a successful
policy result and exits zero. It adds a restriction; a clean scan does not
override the Agent's own permission checks.

Scanner failure (`scan_error`), daemon failure (`daemon_error`), invalid scan
output (`invalid_scan_result`), transport failure (`daemon_transport_error`) and
timeout (`deadline_exceeded`) return `status: error` without candidate effects.
AW applies the configured `on_error` policy; the example uses `report`. A stricter
before-tool policy can explicitly select `on_error: block` when its Agent
adapter supports blocking. Failures are never reported as a successful safety
decision. No request is automatically retried.

`ask`, input/result replacement, final security guards and OS enforcement are
unsupported. `observe_tool` acknowledges the received after-tool event, not tool success or the
correctness of its result.

## Audit and disable

AW records the Provider call, configuration revision, candidate effect and
failure state. The sec daemon retains its existing scanner audit lifecycle.
The Provider response contains only stable reason codes; it does not copy source
code, raw findings, daemon diagnostics or tool results into stdout/stderr.
The AW input digest is checked and echoed unchanged on successful invocation.

To disable this integration, remove its event steps and Provider object from
`aw.yaml`. This does not change existing sec-core hooks, daemon state or scanner
rules. Rollback does not require stopping the sec daemon.
