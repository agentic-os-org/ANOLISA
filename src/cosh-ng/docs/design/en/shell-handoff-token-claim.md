# Shell handoff token claim (#2142)

Owner: cosh-shell `shell_host` (osc/handoff_claim, raw_relay/pty_emit, marker) and
`activity/shell_handoff`. Introduced by the #2142 fix; #2144 is the user-visible
composite of the same deadlock.

## Problem

Handoff closure used to match the marker-reported command text exactly. The
marker preexec hook rewrites that text for secret-bearing commands
(`<redacted sensitive command>`), so an approved handoff could never be
associated with its command block: the handoff pended forever, cosh-core waited
for the `host_executed_shell` result, and `has_active_handoff()` suppressed
every liveness path.

## Protocol

### Staging (Rust → shell)

`ShellHandoffRequest` mints a one-time claim token (`uuid` v4, OS CSPRNG) at
construction. `stage_handoff_files` writes, atomically as a set and fail-closed
(any error clears all three):

| file | content | mode |
| --- | --- | --- |
| `$COSH_HANDOFF_REQUEST_FILE` | command text (unchanged format) | inherited |
| `<request>.no-pager` | presence flag (pre-existing) | inherited |
| `<request>.token` | claim token | 0600 |

A request deserialized from before this protocol has an empty token; staging
then *removes* any stale `.token` sidecar so an old record can never inherit a
newer handoff's claim.

### Echo (shell → Rust)

The bash/zsh markers load the sidecar into `_COSH_HANDOFF_TOKEN` **only** on the
handoff branch whose (unwrapped) command text matches the staged request, then
delete the staged files (consume-then-clear, single shot). The preexec and
precmd markers for that command carry an optional JSON field:

```json
{"event":"preexec", ..., "generation":0, "handoff":"<token>"}
```

Markers without a token emit byte-identical JSON to the pre-protocol format
(golden suite pins this).

Lifecycle rule: **only consumption clears the staged files.** Unrelated
branches (a queued command racing ahead of the handoff, and that command's
precmd) must leave the sidecars alone — the Rust transport owns cleanup for
abandoned handoffs. A user-typed `COSH_SHELL_HANDOFF_BYPASS=1` line whose text
does not match the staged request receives no handoff treatment at all —
no active flag, no pager policy, no token — so neither its preexec nor its
precmd can disturb the staged claim.

### Claim (OscParser + activity)

`osc/handoff_claim.rs` owns the single pending slot. An explicit token is
exclusive:

1. marker token == staged token → claim (any reported text; redaction-proof);
2. marker carries a different token → `Unknown`, no identity adoption, slot
   survives (a wrong token on identical text is a replayed/forged claim);
3. no token + exact text match → claim (pre-protocol marker scripts);
4. no token + no match → `UserInteractive`, slot survives (an unrelated
   preexec cannot burn the handoff's slot);
5. a command-less prompt boundary (`ShellReady`) expires an unclaimed slot —
   the same boundary where the runtime closes the handoff as untracked —
   and signals the relay to remove the staged sidecars, so a later
   same-text user command cannot adopt the closed handoff's identity or
   read its plaintext command.

Closure matching (`shell_handoff_block_matches_request`) mirrors the
exclusivity: a block carrying any explicit `handoff_token` is decided by
token equality alone (two queued handoffs for the identical command must not
cross-pair via the text fallback); the text+origin+timestamp fallback exists
only for blocks without a token.

Durable-surface rule: the activity detail's `preview` and the untracked
closure's synthetic block command are redacted before they leave the handoff
path — evidence/journal/activity never carry the request's plaintext.

The claimed identity travels as `ShellCommandAuditIdentity.handoff_token`
through `ShellEvent` → `CommandBlock`; `shell_handoff_block_matches_request`
prefers it and falls back to text+origin+timestamp only when the request has
no token.

## Compatibility

| Rust | marker script | behaviour |
| --- | --- | --- |
| new | new | token claim (redaction-proof) |
| new | old (no sidecar support) | no `handoff` field → text fallback, exactly pre-fix behaviour |
| old | new | no `.token` staged → `_COSH_HANDOFF_TOKEN` empty → marker byte-identical |
| old persisted request | new | empty token deserialized, stale sidecar removed at staging |

## Security assumptions

- Token influences only closure attribution, never approval decisions.
- One-time: consume-then-clear in the marker, single-shot slot in the parser,
  precmd unset; forging it is bounded by the existing marker-token surface.
- 0600 sidecar in the 0700 session work dir; a reader already owns the user's
  shell.
- Provider-facing `metadata.command` echoes the request's original text
  (truncate-only) for `approved_provider_shell_tool` — the model authored the
  command; durable surfaces (journal/activity/audit/history) keep redaction.
