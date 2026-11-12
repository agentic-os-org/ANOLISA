# Codex CLI Adapter

[中文版](../../zh/adapters/codex-adapter.md)

The `codex` Adapter uses `aw run` to bind a local Codex process to the shared
Service and Provider chain. `aw install` writes one stable AW command Hook for
each of UserPromptSubmit, PreToolUse and PostToolUse. Codex owns session, model,
sandbox, approval and native Hook scheduling. AW owns launch admission, ordered
Provider steps within each AW command, and the resulting allow/block decision.

Installation has one publication boundary. Cancellation observed before the atomic
exchange (or initial no-clobber creation) leaves the live Hooks unchanged.
After publication, sync and verification finish even if cancellation arrives.
The displaced inode is retained for native editors that do not share AW's lock.
Any retained backup blocks all subsequent installation paths, including no-op
success, until the operator reconciles it. Its presence is the durable recovery
state across crashes; the installer never infers reconciliation from matching
live AW definitions. The [integration guide](../../../../../../docs/developer-guide/en/aw/agents/codex.md)
describes that manual acknowledgement. Cancellation, failed publication and
repeated installation are tested as consecutive state transitions.

Preflight records the reported CLI version and checks the public Hook inventory,
enabled state, trust and exact AW definitions. Version 0.163.0 is the native
acceptance baseline, not a runtime allowlist. A required `tool.before` also
rejects other PreToolUse Hooks: Codex runs matching Hooks concurrently and may
execute another Hook's `updatedInput` after AW checked the original input.
Optional checks retain this native composition and attest only to original input.

The Adapter normalizes each native event with session and tool-call identity.
One AW command invokes its admitted steps in order through Service, Host and
Provider. A before block exits with Codex's native denial status; an allow leaves
native approval intact. Codex 0.163.0 can turn a PostToolUse `continue:false`
reason into ordinary model-visible text. AW admits this version-specific path
only with Code Mode disabled and no peer PostToolUse Hooks. The final replacement
step uses `withhold_result` on Provider failure or unsupported projection.
Codex still captures and logs some original results before the Hook, and native
Hook command failures may expose the original; complete result enforcement
needs an independently verified result-delivery boundary.

See the [integration guide](../../../../../../docs/developer-guide/en/aw/agents/codex.md)
for setup and the [capability reference](../../../../../../docs/user-guide/en/aw/capabilities.md)
for supported effects.
