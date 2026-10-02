# cosh-ng Architecture

[中文版](../../zh/cosh-ng/architecture.md)

cosh-ng separates the interactive terminal, Agent runtime, deterministic OS
API, and Gateway Task Plane so each boundary can be tested and integrated
independently. The packaged Gateway path is intentionally narrow: it provides
durable local Task coordination around a contained Core Runtime, not a general
remote capability service.

## Upstream system view

```text
bash/zsh <--- cosh-shell
                  |
                  | JSONL
                  v
              cosh-core
                  |
                  +--> provider / tools / MCP
                  |
                  +--> cosh-platform ---> cosh-types

caller ---> cosh-cli ---> cosh-platform ---> cosh-types

caller ---> cosh agent task ---> cosh-gateway (local Unix/systemd)
                                      |
                                      +--> Task/Event/Outbox SQLite
                                      +--> contained cosh-core

cosh-cli checkpoint -----------> existing ws-ckpt path (separate)
```

The launcher installed as `cosh` normally executes `cosh-shell raw cosh-core`.
`cosh-shell` is compile-time independent of the other workspace crates, but it
owns a long-lived cosh-core child at runtime. The stdin/stdout protocol between
them must remain backward-aware because either side can fail or restart
independently.

The Gateway addition keeps the existing Shell/Core/CLI paths intact. The
`cosh agent task` entrypoint is a local Unix control surface; it is not a Shell
slash-command surface and it does not open a network listener. The existing
`cosh-cli checkpoint` path remains separate from the Gateway profile and may
continue to use the ws-ckpt protocol documented for that CLI domain.

## Gateway Task Plane

The Gateway addition adds two library crates and an installed local entrypoint:

```text
cosh-gateway-contracts --> TaskAggregate --> SQLite Task/event/receipt/Outbox transaction
        |
        +---------------> generic Capability/Permit/Execution contracts

cosh-gateway ----------> contained RuntimeSupervisor --> private COSH JSONL bridge
        |                          |
        +--> local Unix Task API   +--> fixed `core`/`gateway-brokered-v1` profile
        |
        +--> direct ACP `doctor`/`run` path (ungoverned by Task Plane)

task-only inventory: `ask_user_question`
```

The Task reducer, SQLite store, Runtime supervisor, and Outbox scheduler form
the local control plane. The packaged `serve` entrypoint requires live systemd
containment before it binds a socket, canonicalizes the configured workspace,
and admits the fixed target `workspace/cosh/task-only-v1` with the
`core`/`gateway-brokered-v1` selector. The system manager owns the complete
Runtime cgroup after a Gateway crash.

The task-only profile deliberately keeps the execution boundary side-effect
free. Its only Runtime inventory item is `ask_user_question`; it does not
provide checkpoint, write, Shell, slash-command, Web, channel, or remote
capabilities, and it has no approvable side effect. The Task API still exposes
`submit`, `get`, `events`, `append`, `cancel`, `retry`, and `resolve-approval` so
the durable contract can support future profiles. `append` answers a pending
question; this profile does not create an approval flow.

The direct ACP `doctor` and `run` commands remain useful for local adapter
interoperability, but they launch an adapter outside the durable Task Plane and
are not governed by the task-only inventory. The current Shell path is also
unchanged: `cosh-shell` owns its native PTY and its compatibility cosh-core
process. Shell slash commands remain a Shell concern, not Gateway commands.

## Crate responsibilities

| Crate | Binary | Owns | Must not own |
|---|---|---|---|
| `cosh-types` | — | Side-effect-free response, error, config, audit, and existing checkpoint wire types | OS access or runtime policy |
| `cosh-platform` | — | Distro detection, package/service adapters, audit policy/store, and the existing ws-ckpt client used by `cosh-cli checkpoint` | CLI rendering, Gateway Task policy, or Agent UX |
| `cosh-cli` | `cosh-cli` | Clap commands, JSON envelope, exit status | Distro-specific branching outside platform adapters |
| `cosh-core` | `cosh-core` | Providers, tool loop, hooks, Skills, MCP, extensions, registry, sessions, and compaction | Terminal ownership or foreground PTY interaction |
| `cosh-shell` | `cosh-shell` | PTY host, input routing, cards, approvals, evidence, UI, core process lifecycle | Provider implementation or direct OS API abstraction |
| `cosh-gateway-contracts` | — | Side-effect-free Task, Runtime, Capability, identity, header, and error contracts with bounded leaf strings/digests | Storage, process ownership, transport, provider, or OS execution |
| `cosh-gateway` | `cosh-gateway` | Durable Task reducer/store, Outbox scheduler, contained Core Runtime bridge, local Unix Task API, and direct ACP entrypoint | Shell PTY, checkpoint/write targets, remote listeners, Shell slash commands, or ungoverned side effects |

## Interactive data flow

1. `cosh-shell` starts bash/zsh in a PTY and installs OSC lifecycle markers.
2. Input routing sends shell syntax to the PTY, slash commands to the local
   control surface, and natural language to the Agent adapter.
3. The default adapter maintains a cosh-core process and sends one JSONL user
   message per Agent turn.
4. cosh-core resolves workspace config, the provider, Skills, extensions, MCP
   tools, and session state, then streams events back.
5. cosh-shell governs those events and renders text, question cards, or approval
   cards.
6. Approved shell execution is handed back to the foreground PTY. OSC evidence
   is correlated with the Agent run and returned to core when requested.
7. Registry mutations such as extension reload use the same long-lived core
   and publish changes at a safe generation boundary.

## Deterministic CLI data flow

```text
Clap command
  → command module validates arguments
  → cosh-platform selects the backend
  → backend returns typed data or CoshError
  → cosh-cli emits CoshResponse<T>
  → exit 0 on success, exit 1 on operation failure
```

Package and service writes support `--dry-run`. The existing `cosh-cli
checkpoint` domain crosses a Unix socket using bincode with a four-byte
little-endian length prefix; this ws-ckpt path is separate from the task-only
Gateway profile.

## cosh-shell ownership map

| Owner | Responsibility |
|---|---|
| `shell_host/` | PTY lifecycle, OSC parsing, shell integration, raw relay |
| `raw_input/` and `input/` | terminal modes, multiline input, input relay |
| `slash/` | slash parser, registry, and command-specific presentation |
| `adapter/` | provider/core adapters and control protocol transport |
| `agent/` | Agent run lifecycle and governed events |
| `runtime/` | orchestration, shared state, dispatch, and startup |
| `approval/` and `question/` | user decisions and control responses |
| `hooks/` | hook policy and execution; hands mutations to runtime boundaries |
| `tools/` | command risk model, read-only rules, tool presentation |
| `ui/` | terminal rendering and card components |
| `evidence/`, `journal/`, `ledger/` | bounded evidence and decision records |

New implementation files do not belong at the `cosh-shell/src/` root. Keep
owner boundaries visible and run `crates/cosh-shell/scripts/check-layout.sh`
after structural changes.

## Compatibility and safety contracts

- `CoshResponse<T>` is the stable automation envelope.
- The existing `cosh-cli checkpoint` ws-ckpt enum order is part of its binary
  wire format; the task-only Gateway does not depend on that daemon.
- cosh-core messages are newline-delimited JSON; stdout must not contain logs or
  UI prose in headless mode.
- A running Agent turn is pinned to its registry generation. A healthy candidate
  activates immediately only when idle; otherwise it waits for a safe point.
- Session state is workspace-scoped. Recovery restores model-visible
  conversation, not historical terminal evidence.
- Core read tools are pinned to the canonical startup workspace. A later `cd`
  changes the shell directory, not the read boundary; path and mount escapes
  fail closed.
- Foreground shell handoffs are serialized. Input-wait timeouts apply only when
  kernel evidence shows a foreground process waiting for input; pipelines and
  full-screen programs are exempt.
- Linux package routing may use the first recognized `ID_LIKE` family while
  preserving the distribution's real `ID` in typed and JSON output.
- Tool auto-approval fails closed. Raw command substring matching is not a
  security boundary.
- Gateway Task submissions are fixed to `workspace/cosh/task-only-v1` and the
  admitted `core`/`gateway-brokered-v1` selector. The durable API uses
  idempotency keys so uncertain client I/O can be retried without replaying an
  unknown side effect.

## Gateway and ACP delivery boundary

The packaged slice is a durable local Task Plane, not a general-purpose
production Gateway. Its supported boundary is the contained
`core`/`gateway-brokered-v1` Runtime, the fixed
`workspace/cosh/task-only-v1` target, the local Unix Task API, and the single
`ask_user_question` inventory item. Checkpoint, write, Shell, slash-command,
Web/channel, and remote capabilities remain outside this profile. Generic
approval and permit contracts stay available for later profiles, but this
profile has no approvable side effect.

The direct ACP `doctor`/`run` path is an interoperability entrypoint rather
than a governed Gateway Runtime. It intentionally does not claim Task
durability, capability admission, or remote execution.

Continue with [Developing cosh-ng](getting-started.md), [IPC protocols](ipc-protocol.md),
and [Testing](testing.md).
