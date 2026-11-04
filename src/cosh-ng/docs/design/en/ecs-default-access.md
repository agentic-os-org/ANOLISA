# ECS automatic model access

[中文版](../zh/ecs-default-access.md)

Cosh Shell can use OS Console model service on ECS without manual model
credentials. Existing configured providers retain their selection. Automatic
access defaults to `qwen3.7-plus` and preserves explicit and resumed models.
The service determines entitlement and quota; this is not a promise of
permanently free access. Cosh never enables paid access automatically.

## Ownership and protocol

Shell sends `can_handle_ecs_trial_v1` during initialization and a versioned
`ecs_trial_context` on user-initiated model requests. Core owns identity,
credentials and dispatch. Shell owns waiting UI and session suppression.
Older clients retain the existing authentication flow. Generic headless
clients cannot enable this path by selecting an identity configuration alone.

Core probes IMDSv2 without proxies, redirects or IMDSv1 fallback, acquires a
challenge, obtains an instance proof and exchanges it for constrained STS.
Secrets remain in memory. A per-provider manager shares refresh work;
cancelling the final waiter drops the exchange. Credentials use server TTL
and a monotonic deadline, refresh on demand before expiry, and are checked
again before model dispatch. Transient metadata requests retry at most once
within the existing budget. Refresh failure may reuse an old lease only for
transient errors with more than 30 seconds remaining; permission and malformed
response failures clear it. A model send failure or accepted stream is never
replayed as a credential refresh.

## User interaction and state

Only explicit pre-dispatch activation or credits-preparing errors enter bounded
polling. The waiting card displays the OS Console link and QR directly and
continues the same question when activation succeeds. Quota, permission and
transport failures are separate terminal outcomes. Summary requests use the
same admission and cancellation path as main model requests. Standalone manual
compaction cannot use identity access; idle background recommendations are
suppressed for this source. In-request compaction retains the admission path.

`Esc` cancels waiting; `n` opens suppression settings. Session suppression
belongs to the Shell process. Persistent suppression is stored as
`ecs_trial.auto_use` in `~/.copilot-shell/cosh/preferences.toml`, using an
owner-checked, locked, atomic update that preserves unknown fields. Read or
write errors fail closed. Disabling stops automatic use as well as prompts;
it does not disable another configured provider. Explicit `/auth` selection
can re-enable identity access without replaying a cancelled question.

Implicit use does not save a provider. Explicit selection stores only
`aliyun`/`ecs_identity` metadata and the selected model, preserves other
providers and avoids duplicate entries. Provider changes update live model
state; resumed sessions retain their model. Partial session-save failure is
reported separately from an already applied provider change.
