# cosh-shell Large-File Inventory

> Tracked technical debt for `crates/cosh-shell/src` production files that
> exceed the 700-line layout threshold enforced by
> `crates/cosh-shell/scripts/check-layout.sh`.

Registration in this inventory records that a large file is *known* debt with a
named owner and a split plan. It does **not** mean the file is acceptable
long-term — every entry is a standing invitation to split. The audit fails if a
new over-threshold file appears that is not listed here.

## Rules

- Only files that already exceed the threshold on `main`/`HEAD` may be
  registered (`git show HEAD:<path> | wc -l`). Files newly *created* over the
  threshold must be split instead of waived — the automatic-compaction feature
  split its new `slash/session/compact.rs` (732 lines) into
  `compact.rs` + `compact/process.rs` + `compact/runtime.rs` rather than
  registering it here.
- The `Lines` column is the current size and is advisory; the audit matches on
  the `File` path only.
- Do not raise the 700-line threshold, suppress the check, or bulk-register
  files without a baseline and a reason.

## Registered files

| Lines | File | Owner | Reason / split plan |
|------:|------|-------|---------------------|
| 1717 | `src/ui/agent_render/health.rs` | ui | Health banner rendering; pre-existing. Split per-severity renderers. |
| 1628 | `src/diagnostics/health/collectors.rs` | diagnostics | Per-subsystem collectors; pre-existing. Split by collector family. |
| 991 | `src/runtime/state.rs` | runtime | Central inline state; approval state extracted to `approval_state.rs` (main baseline 871, back under the 1000-line growth bar). The #1940 approval-terminal-state fix adds the `approval_ledger` field plus two accessors, which co-locate with their `ControlState` host. The #2068 startup auth hint adds the `StartupAuthState` probe holder next to its `StartupHealthState` sibling. Task form/snapshot holders remain with their `InlineState` lifecycle; see D21 in `decisions.md`. Extract a Task presentation state holder before further Task growth and continue per-domain splits before the 1000-line growth bar. |
| 912 | `src/evidence/output_policy.rs` | evidence | Output excerpt policy; pre-existing. Split bounding from classification. |
| 906 | `src/adapter/fake.rs` | adapter | Test fake adapter; pre-existing. Split scripted-scenario builders. |
| 952 | `src/ui/agent_render/approval.rs` | ui | Approval card rendering; action-row rendering extracted to `approval_actions.rs` (turn consent, #1773); irrecoverable warning extracted to `approval_warning.rs` and reason-line rendering to `approval_reason.rs` (#2064). Split per-card-kind renderers next. |
| 943 | `src/auth/runtime.rs` | auth | Auth control-protocol runtime; prompt rendering extracted to `auth/prompt.rs`, ESC back-navigation to `auth/navigation.rs` (#1760 added dispatcher glue only). Extract provider-specific flows next. |
| 769 | `src/activity/runtime.rs` | activity | Activity runtime; pre-existing. Shell handoff row builders (tracked and untracked) extracted to `activity/shell_handoff.rs`; extract remaining lifecycle from bookkeeping. |
| 712 | `src/agent/poll.rs` | agent | Agent run polling loop; pre-existing debt (main baseline 839). The compaction feature added the `compaction_recommended_v1` status capture; the #1940 approval-terminal-state fix extracted the inline test module to `agent/poll/tests.rs`, pulling the file back under the 1000-line growth bar. Split event routing from rendering next. |
| 856 | `src/parser/mod.rs` | parser | Event parser (legacy `mod.rs`); pre-existing. Split per-event-kind parsers and rename off `mod.rs`. |
| 855 | `src/activity/runtime_render.rs` | activity | Activity rendering; pre-existing. Split per-panel renderers. |
| 853 | `src/i18n/en.rs` | i18n | English catalog; pre-existing. Partition by message domain. |
| 823 | `src/i18n/zh.rs` | i18n | Chinese catalog; pre-existing. Partition by message domain. |
| 987 | `src/shell_host/osc.rs` | shell_host | OSC sequence handling; main baseline 995. Alt-screen tracking lives in `osc/alt_screen.rs`, while marker framing and history-file tracking live in `osc/marker_sequence.rs`. Keep split priority raised and continue extracting sequence kinds. |
| 788 | `src/runtime/shell_evidence.rs` | runtime | Shell evidence runtime; pre-existing. Extract capture from rendering. |
| 786 | `src/tools/display.rs` | tools | Tool display formatting; pre-existing. Split per-tool formatters. |
| 778 | `src/ui/agent_render/activity.rs` | ui | Activity card rendering; pre-existing. Split per-activity renderers. |
| 770 | `src/diagnostics/health/recommendation.rs` | diagnostics | Health recommendations; pre-existing. Split rule families. |
| 770 | `src/agent/failed_command.rs` | agent | Failed-command analysis; pre-existing debt (main baseline 760). The compaction feature added the suppression-aware start disposition handling; split request-building from gating. |
| 874 | `src/agent/approval_bridge.rs` | agent | Approval bridge; pre-existing (main baseline 805, registered on main at 737). The #1940 lifecycle-ledger growth and the #2064 high-risk action-set gate co-locate with their request-mapping host. Split request mapping from replay. |
| 724 | `src/runtime/hooks.rs` | runtime | Runtime hook dispatch; pre-existing. Split per-hook-type handling. |
| 722 | `src/i18n/message_id_all.rs` | i18n | `MessageId::ALL` mirror of the enum; near-threshold historical file (main baseline 699, zero headroom). The compaction feature added 23 required user-facing message ids, crossing the threshold. Split plan: generate the enum + `ALL` array from a single macro so new ids do not grow either file. |
| 722 | `src/i18n/message_id.rs` | i18n | `MessageId` enum; near-threshold historical file (main baseline 699, zero headroom). The compaction feature added 23 required user-facing message ids, crossing the threshold. Split plan: partition `MessageId` into domain sub-enums (or macro-generate it) as a follow-up i18n refactor. |
| 710 | `src/slash/hooks.rs` | slash | Slash hook command; pre-existing. Split subcommands. |
| 700 | `src/adapter/control_protocol.rs` | adapter | Control-protocol wire surface; near-threshold historical file (main baseline 690). The #1940 approval-terminal-state fix added the `ApprovalChannelMessage` envelope and the receipt-serializer re-export, crossing the threshold. Split plan: move per-provider wire DTOs into `control_protocol/` submodules alongside `serialization.rs`, leaving only channel orchestration here. |
