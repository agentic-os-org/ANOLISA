# Use sec-core scanning through AW

[中文版](../../zh/user-entrypoint/aw-sec-core.md)

`aw-provider-sec-core` lets an AW policy call the existing `agent-sec-cli scan-code`
command. You choose which tool inputs contain code and whether reported risks
produce an observation or a block candidate. sec-core supplies the scanner and
rules; AW supplies configuration, protocol validation and bounded invocation.

This is a Linux source delivery for local Provider integration. The integrated Preview is described separately below. The
Provider does not start either daemon, install Agent Hooks or persist AW audits.
Tool-after support records an observation; it does not scan or replace results.

For a prebuilt AW service + Qoder + sec-core integration, use the
[Preview package guide](aw-preview.md). The standalone source bridge below
is also available.

## Prerequisites and build

Install sec-core using the [sec-core guide](../agent-security/agent-sec-core/QUICKSTART.md).
This bridge requires the Rust V2 CLI with `--socket`, `--timeout-ms` and
`scan-code --code --language --mode`, plus an accessible, running sec-core daemon.
Check the deployed CLI's help; the legacy Python CLI is not certified by this
bridge. AW adds no command or dependency to sec-core.

Build the AW Provider and local Host example from source:

```bash
cd src/aw
cargo build --locked -p aw-provider-sec-core
cargo build --locked -p aw-host --example host
```

The Provider binary is `target/debug/aw-provider-sec-core`. Its invocation is:

```text
aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH
```

`--cli` selects the public `agent-sec-cli` executable; `--socket` selects its
existing daemon. Both paths are required and absolute. `--help` prints usage.
The process reads one EOF-terminated AW Provider JSON request on stdin and writes
one response on stdout. It is normally launched by the Host, not interactively.

## Configure a Provider

Copy [the complete sample](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider-sec-core/examples/aw.yaml)
to `aw.sec-core.yaml` and replace its executable and socket paths. The sample
starts in `mode: observe` with `on_error: report` so local evaluation does not
request blocking. For blocking, set `mode: block` and admit `[observe, block]`
in the before step. `on_error: block` separately requests blocking on execution
failure; a failure is never reported as a scanner risk verdict.

The Provider's private `config` accepts:

| Field | Meaning |
| --- | --- |
| `version` | Required, exactly `1` |
| `mode` | Required, `observe` or `block`; only affects `scan_code` |
| `tools` | Required object with 1–128 exact normalized tool names |
| `tools.<name>.language` | `bash` or `python` |
| `tools.<name>.input_pointer` | JSON Pointer into `event.tool.input`; the selected value must be a string |

Unknown fields, unsupported languages and invalid pointers are rejected.
An empty pointer selects a root string. `/command` selects a field;
`/items/0` selects an array element. Names are case-sensitive. Unmapped tools
produce `observe/tool_unmapped`, which means they were not scanned. A mapped tool
with missing or non-string code fails with `invalid_tool_input`.

| Operation | Event | Result |
| --- | --- | --- |
| `scan_code` | `tool.before` | `ok: true, verdict: pass` → `observe/code_pass`; `warn` or `deny` → `observe/code_risk` or `block/code_risk`, according to `mode` |
| `observe_tool` | `tool.after` | `observe/tool_observed`; no scanner call |

`describe`, private-config validation and after observation do not contact
sec-core. Before scanning uses literal arguments and `--mode regex`; code is
never run as a shell command. Exit 0 means the CLI completed a scan, not that its
verdict was `pass`. Nonzero/signal exits, malformed JSON, invalid verdicts,
timeouts and stream limits produce Provider errors without candidate effects.

The caller must enforce a deadline for the entire Provider process, including
stdin receipt and stdout delivery. `aw-host` does this through `aw-exec`. A direct
caller must also close stdin after the request and terminate the process if I/O
stalls; the Provider does not enforce a standalone wall-clock timeout. After EOF,
the bridge charges time already spent reading and parsing against the invoke
budget, capped at 60 seconds, and gives only the remainder to scanning.
CLI stdout is capped at 1 MiB and stderr at 64 KiB.
It forwards its Host-selected environment to the CLI. Responses contain
reason codes and the bound input digest, not source code, findings or CLI stderr.
The CLI receives code through argv, subject to OS argument visibility and size
limits. No stdin/file fallback is provided by this CLI contract.

## Try candidate effects locally

From `src/aw`, after editing `aw.sec-core.yaml` and setting `mode: block`:

```bash
./target/debug/examples/host aw.sec-core.yaml tool.before shell '{"command":"echo safe"}'
./target/debug/examples/host aw.sec-core.yaml tool.before shell '{"command":"rm -rf /"}'
./target/debug/examples/host aw.sec-core.yaml tool.after shell '{"command":"echo safe"}'
```

These commands submit synthetic tool events; they do not execute the code being
scanned. With the built-in regex rules, the expected candidates are `code_pass`,
`block/code_risk` and `tool_observed`. The Host prints `adoption: not_tested`:
a block candidate does not prove an Agent blocked a tool. Native before/after
integration and persistent AW auditing still require the daemon and adapters.

Remove the Provider and its event steps to disable this integration. There is no
sec-core patch or packaging change to roll back. Existing Agent-native sec-core
Hooks continue using the same public CLI.
