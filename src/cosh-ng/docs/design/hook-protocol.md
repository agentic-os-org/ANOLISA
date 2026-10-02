# cosh-ng Hook Wire Contract Design

Date: 2026-07-27

Related documents: [user guide](../../../../docs/user-guide/en/user-entrypoint/cosh-ng/core/hooks.md),
[extension guide](../../../../docs/user-guide/en/user-entrypoint/cosh-ng/core/extensions.md)

## Summary

Hooks are external processes. cosh-ng writes one JSON object to the child's stdin and reads one
JSON object from its stdout, so the wire shape — not any in-process type — is the compatibility
surface. This document fixes the two parts of that surface that let a hook *change* what the
runtime does, rather than merely observe it: `BeforeModel` tool declaration rewriting and hook
`env`. Both are deliberately narrower than "let the hook patch the request".

The shape follows copilot-shell's Hook Translator (`llm_request.model`, `llm_request.messages`,
`llm_request.config.tools`), so one hook binary can read either host's input. Field positions are
therefore a cross-component contract and must not move unilaterally.

Input compatibility only. copilot-shell's `fromHookLLMRequest` does not currently copy
`config.tools` back into the SDK request, so a returned tool rewrite is applied by cosh-ng and
silently ignored there. That output gap is a known copilot-shell limitation, not something this
contract papers over; a hook that depends on the rewrite taking effect must detect the runtime.

## BeforeModel: tool declaration rewriting

### Wire shape

Input carries the declarations for the request that is about to be sent:

```json
{
  "hook_event_name": "BeforeModel",
  "llm_request": {
    "model": "...",
    "messages": [{ "role": "user", "content": "..." }],
    "config": { "tools": [{ "name": "shell", "description": "...", "parameters": {} }] }
  }
}
```

A hook returns a replacement array at the same path. The tool set is unchanged — only
`description` and `parameters` are compressed:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "BeforeModel",
    "llm_request": {
      "config": {
        "tools": [{ "name": "shell", "description": "run cmd", "parameters": {} }]
      }
    }
  }
}
```

`llm_request.config.tools` is canonical. `llm_request.tools` is accepted on read for migration
only; presence of the canonical key decides, so a host declaring an empty tool set is never served
a stale legacy field. Writers must emit the canonical position.

### Invariants

| Invariant | Rationale |
|---|---|
| Applies to one `provider.generate()` call | The ToolRegistry and `tool_decls` stay authoritative for the next turn, so a hook cannot accumulate drift across a run. |
| Identical tool count, order, and names | Adding, dropping, or reordering tools silently changes tool-selection semantics. Filtering is a separate concern and needs its own protocol. |
| `parameters` must be a JSON object | A non-object schema is rejected by providers; failing here keeps the error local to the hook. |
| Rewrite may not grow the estimated token count | The turn's compaction prefix estimate is computed from the original declarations *before* the hook runs. Accepting growth would under-account the request, skip an emergency compaction, and overflow the provider context; the 1024-token prefix reserve cannot absorb unbounded growth. |
| Last valid array in configuration order wins | Deep-merging two independent rewrites has no well-defined result. An invalid array is discarded whole and never partially overwrites an accepted one. |
| Messages are not rewritable | Editing history would break tool-call/tool-result pairing and compaction projections. |

Any violation discards that whole candidate array — never part of it. The originals are used only
when no valid candidate exists; a rejected candidate does not undo an earlier hook's accepted
rewrite. A rejected candidate is a logged warning, never a turn failure — a broken hook must not be
able to stop the session.

### Redaction

The declaration subtree is exempt from key-based redaction in **both** directions. A JSON Schema
property named `api_key` is a declaration, not a secret; replacing it with `"<redacted>"` would hand
the hook — or the provider — a corrupt schema. The subtree still gets pattern-based scrubbing of its
string leaves, so a real secret *shape* is removed. Messages keep the stricter key-based redaction.

## Hook env

A hook definition may declare `env`, applied to that hook's child process only.

Precedence, lowest to highest:

1. inherited parent environment
2. hook manifest `env`
3. host attribution: `COSH_RUNTIME=cosh-ng`, `COSH_NG_VERSION`

`std::env::set_var` is never called, so the host process is unaffected and concurrently running
hooks cannot observe each other's values.

Names must match the POSIX rule `[A-Za-z_][A-Za-z0-9_]*`. A strict `schemaVersion: 1` manifest is
rejected at install time with `extension_hook_env_name_invalid`; config-file and legacy-manifest
hooks are re-validated at spawn time as defence in depth, dropping the entry and logging only the
name. Values are opaque and never logged or formatted.

`env` is part of the capability fingerprint: a previously inert field became executable capability,
so declaring or changing it must force re-consent. The key is omitted from the projection when
empty, so extensions that never used `env` keep their existing fingerprint.

### Security boundary

The host variables are applied after the declared map, so they take precedence over an `env` entry
of the same name. **They are a cooperative attribution signal, not a security boundary.** The same
manifest owns `command`, and the hook runs under `sh -c`, so it can trivially reassign or unset them:

```sh
COSH_RUNTIME=fake python3 hook.py
```

Nothing security-relevant may depend on these variables. They exist so a cooperating hook can
report which runtime it ran under (statistics attribution), which a shared extension manifest
cannot express on its own.

## Trade-offs

- **No general request-patch framework.** Each mutable field is added explicitly with its own
  invariants. This is more code per field but keeps the blast radius of a misbehaving hook bounded
  and reviewable.
- **Compression-only declarations.** Supporting growth means recomputing the effective prefix after
  the hook and re-running preflight without invalidating the message view the hook already saw.
  That reordering is deferred until a use case requires it.
- **Two accepted read positions for tools.** A migration cost carried on the read path only, so
  writers converge on one position.
