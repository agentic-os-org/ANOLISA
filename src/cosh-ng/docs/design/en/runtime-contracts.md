# Runtime and Control Contracts

## Status

This document describes the implemented cosh-shell and cosh-core runtime
contracts. It covers provider process ownership, control protocol negotiation,
approval and tool naming, and the explicit one-shot Agent Composer.

ACP clients and the durable Task Execution Plane are separate designs and do
not use these implementation paths yet.

## Goals

- Keep one lifecycle owner for each provider subprocess.
- Reject incompatible shell/core peers before admitting a user turn.
- Use the same approval vocabulary in configuration, CLI, shell, and core.
- Resolve provider tool aliases through one canonical shell catalog.
- Offer an explicit one-shot Agent entry point without changing ordinary
  shell-first routing.

## Runtime ownership

`AgentAdapter`, `AgentRunHandle`, and `AgentEvent` remain the public lifecycle
boundary. Claude and Qwen retain provider-specific parsers, prompt argument
encoding, capability compatibility, and approval response encoding, but share
the process driver responsible for:

- spawn and process-group termination;
- cancellation and terminal-event reduction;
- bounded stderr collection;
- session commit after successful completion;
- nonzero exit and stream failure handling.

The persistent cosh-core service remains the cancellable production path.
`run_sync_cosh_core_process` remains available for the live synchronous path.
The superseded cancellable and control-protocol cosh-core entry points are not
part of the runtime surface.

## Shell/core initialization

The shell-owned cosh-core transport sends an `initialize` control request with
`protocol_version` set to `1`. A one-shot transport may also set
`fire_session_start` to `false`; generic headless clients cannot use that field
to suppress their historical lifecycle behavior.

The core follows these rules:

1. A missing or null version identifies a legacy peer and remains compatible.
2. An explicit version must equal the current protocol version exactly.
3. A mismatch produces a correlated error response and terminates before a
   user turn is admitted.
4. A successful response echoes the negotiated version and announces control
   capabilities.

The persistent shell service matches the initialize response by request ID and
does not send the first user turn until negotiation succeeds. Later turns reuse
the capabilities recorded for that process.

## Approval contract

The canonical modes are:

| Mode | Contract |
| --- | --- |
| `recommend` | Read-only tools may run; other tools require approval. |
| `auto` | Locally bounded edits may run; external boundaries and sensitive writes require approval. |
| `trust` | Registered tools may run without an approval prompt. |

Legacy `balanced`, `strict`, and `suggest` inputs normalize to `recommend`.
Invalid configuration fails closed to `recommend`, while invalid CLI values are
rejected. Explicit allowlist entries remain authoritative for registered tools.

Approval mode is a typed value across CLI parsing, layered configuration,
shell state, initialization overrides, and core policy. No runtime boundary
uses an unvalidated approval string. This convergence intentionally changes
the Rust-facing `CoshConfig.approval_mode` field from `String` to
`CoshApprovalMode`; callers must use the typed variants instead of constructing
arbitrary strings.

## Canonical tool catalog

The shell catalog owns canonical names and provider aliases used for:

- tool classification and approval policy;
- display labels and streamed argument status;
- provider event parsing;
- control-protocol staging;
- shell/core prompt contracts.

Aliases resolve before policy evaluation. Native provider question names remain
distinct from the core `ask_user_question` control path. Unknown core tools
fail closed rather than inheriting a permissive default.

## One-shot Agent Composer

`/agent` opens the existing draft editor with an explicit runtime label. A
submission may contain a leading `/skill:<name>` directive and workspace-local
`@` references.

Reference handling is bounded and validates that each referenced file or
directory remains inside the workspace. The Composer passes reference metadata
to cosh-core without reading or embedding referenced contents. Invalid or
out-of-workspace references are reported in the structured Composer envelope.

`/agent` is the only Composer command. The former `/draft` compatibility alias
is not intercepted and follows native shell routing. Ordinary command input
continues through shell-first routing, and cancellation restores the native
Bash or Zsh prompt without submitting a turn.

## Compatibility and rollback

Legacy versionless shell/core peers and legacy approval configuration remain
compatible. Provider-specific wire formats do not change.

The six implementation commits are ordered so rollback can proceed from the
Composer and canonical contracts back through the shared driver cleanup. The
protocol version commit must not be reverted independently after either side
starts relying on a newer explicit version.
