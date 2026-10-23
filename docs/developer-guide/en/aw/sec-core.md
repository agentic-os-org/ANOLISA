# sec-core Provider integration reference

[中文版](../../zh/aw/sec-core.md)

This reference is for integration and maintenance. For installation and daily use,
see the [user guide](../../../user-guide/en/aw/security.md).

`aw-provider-sec-core` lets an AW policy call the existing `agent-sec-cli scan-code`
command. You choose which tool inputs contain code and whether reported risks
produce an observation or a block candidate. sec-core supplies the scanner and
rules; AW supplies configuration, protocol validation and bounded invocation.

This is a Linux source delivery for local Provider integration. The integrated Preview is described separately below. The
Provider does not start either daemon, install Agent Hooks or persist AW audits.
Tool-after support records an observation; it does not scan or replace results.

For a prebuilt AW service + Qoder + sec-core integration, use the
[Preview package guide](../../../user-guide/en/aw/installation.md). The standalone source bridge below
is also available.

For the general component integration path, start with the
[AW walkthrough](integration.md). sec-core's CLI
and daemon remain an independently managed backend; the AW service owns AW
audit storage. See [container persistence](native-agents.md#containers-and-persistent-data).
The Preview generator maps Qoder `Bash` and OpenClaw `exec` inputs. Before using
QwenPaw or Hermes, add and validate their actual tool names and input pointers
in `config.tools`; an unmapped tool is observed without scanning.

## Prerequisites and build

Install sec-core using the [sec-core guide](../../../user-guide/en/agent-sec-core/index.md).
This bridge requires the Rust V2 CLI with `--socket`, `--timeout-ms` and
`scan-code --code --language --mode`, plus an accessible, running sec-core daemon.
Check the deployed CLI's help; the legacy Python CLI is not certified by this
bridge. AW adds no command or dependency to sec-core.

Build the AW Provider and local Host example from source:

```bash
cd src/aw
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
cargo build --locked -p aw-host --example host
```

The Provider binary is `../../providers/agent-sec-core/target/debug/aw-provider-sec-core`. Its invocation is:

```text
aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH
```

`--cli` selects the public `agent-sec-cli` executable; `--socket` selects its
existing daemon. Both paths are required and absolute. `--help` prints usage.
The process reads one EOF-terminated AW Provider JSON request on stdin and writes
one response on stdout. It is normally launched by the Host, not interactively.

## Packaged backend and explicit health diagnostics

The Preview sec-core Provider package includes `agent-sec-cli`,
`agent-sec-daemon`, and `agent-sec-daemon-health` from the same source build.
Their hashes and executable modes are covered by the component receipt in both
split and all-in-one installations. Official systemd/configuration templates are
included under `share/aw/providers/sec-core/` as reference assets; installing the
AW package does not register a service or modify `/etc`.

Reuse an existing root `agent-sec-core.service` and its socket. For a new system
backend, use the [sec-core installation guide](../../../user-guide/en/agent-sec-core/index.md)
and the official system package's service lifecycle. Its current component
manifest advertises Linux x86_64; a locally built AW ARM64 package does not imply
an official sec-core ARM64 release. The source `make install-core-v2` target can
consume the packaged binaries through `V2_BIN_BUILD_DIR`, but overwrites its
system binary/unit destinations and must not be run over an existing managed
installation. Do not point a root service at a user-writable AW prefix.

An installer or diagnostic consumer can explicitly query the installed helper:

```bash
AW_SEC_PROVIDER="$AW_PREVIEW_PREFIX/libexec/aw/providers/sec-core"
"$AW_SEC_PROVIDER/aw-provider-sec-core" --health \
  --helper "$AW_SEC_PROVIDER/agent-sec-daemon-health" \
  --socket /run/agent-sec-core/daemon.sock
```

This is an optional component diagnostic, not an additional Agent startup step.
Continue starting Agents with `aw run`. The command invokes only the existing
`daemon.health` probe, with a one-second process deadline and 4 KiB stream
limits. It never starts a daemon, scans code, or performs target admission.
The helper authenticates the daemon's root peer credentials; AW strips the
sec-core testing bypass environment variable. Nonzero helper exits, invalid or
unhealthy replies, a non-root peer, missing components and deadlines remain
unavailable. Private helper output is not returned.

The separate `aw-sec-core-health/v1alpha1` JSON schema is published at
`providers/agent-sec-core/schemas/health-v1alpha1.schema.json` with
reachable/unavailable fixtures in `tests/fixtures/`. `helper_installed` means
only that the helper file exists; `socket_configured` means an absolute socket
path was supplied. Only successful authenticated health yields
`backend: authenticated_reachable`. `admission` and `check_result` remain
`unknown`: neither health nor a recent execution's `status=ok` proves that a
target admitted a Provider or that a scanner found no risk. This does not change
`aw inspect`, the existing Provider view protocol, or their `unknown` fields.
Diagnostic success/failure does not replace each invocation's configured
`on_error` behavior; backend loss still blocks when `on_error: block` applies.

AW uninstall removes its receipt-owned helper/templates but retains the
independently managed system service, `/etc` configuration and backend data.
Upgrade or remove that backend through its owning system package. AW currently
has no automatic service installation, upgrade or removal bridge.

## Configure a Provider

Copy [the complete sample](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/agent-sec-core/examples/aw.yaml)
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
