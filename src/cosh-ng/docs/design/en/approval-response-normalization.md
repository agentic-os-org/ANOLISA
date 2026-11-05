# Approval response normalization

Core normalizes one outstanding approval's raw JSONL frame in
`crates/cosh-core/src/core/approval_response.rs`. The ordinary tool, sandbox-bypass,
and Prompt Hook waits share this owner. Auth, question, and ShellEvidence exchanges
keep their existing shared protocol DTOs and do not use its strict approval body.

## Invariants

1. The outer `request_id` is the existing authority correlation. A valid foreign,
   nonempty string ID is ignored before decoding its body. A missing, empty,
   wrong-typed, or duplicated ID cannot authorize execution or undo a checkpoint.
2. Only a matching `control_response` with a `success` envelope can yield a
   decision. A malformed current frame terminates the wait as `Unconfirmed`;
   a later bare denial cannot replace that uncertainty. One whole-frame owner
   returns `Ignore`, `Receipt`, `Interrupted`, or `Decision`; the wait never runs a
   weaker second parser. Empty lines, EOF, and the existing deadline retain their
   lifecycle. A receipt is strictly `type` plus a nonempty string `request_id`;
   an execution-bearing or otherwise malformed receipt is `Unconfirmed`.
   Valid known non-approval messages keep their shared DTO contract and are
   ignored only when neither top nor nested ID owns this approval. A valid
   interrupt still cancels the provider. Other current-ID frame kinds, unknown
   kinds, and malformed frames are `Unconfirmed`.
3. A private Serde tagged union defines disjoint bodies: allow, deny, shell result,
   checkpoint result, or checkpoint error. Each variant rejects unrelated fields,
   including an execution-payload key whose value is null. Null does not prove
   execution; the contradictory shape cannot prove non-execution either.
4. Every approval handoff, including Prompt Hook and PreToolUse Ask for
   ReadOnly/Other tools, durably arms the existing checkpoint and sets memory
   pending before delivery. A requested tool kind cannot prove a buggy Host did
   not execute. Unknown, malformed, mismatched, timed-out, or interrupted replies
   retain this protection, even if no completion was read before process death.
   The original pending state is sampled before arming for mandatory-approval
   policy; the temporary exposure must not make every initial handoff mandatory.
   Only strict plain decisions with no execution across the complete tool/bypass
   lifecycle restore this exposure's separate memory and checkpoint baselines.
   Prompt/read-only plain allow may restore that new protection; an approved
   local side effect keeps it armed through execution. Earlier pending failure
   and the valid memory=false/checkpoint=true pre-commit split are preserved.
   A Host/local approved success still uses the existing final success/commit
   clear boundary. Any fallible audit or early exit keeps the armed protection.
5. Input is decoded directly from the original line. Serde rejects duplicate
   known header, behavior, payload, typed outcome, and metadata safety fields;
   authorization never uses a Value map that has discarded duplicate fields.
6. The Shell's `metadata.approval_id` is its internal approval identity, while
   `metadata.tool_use_id` and legacy `toolUseID` are diagnostic tool identities.
   They retain string/null validation and do not become new authority selectors.
   Gateway-allocated checkpoint IDs retain the existing canonical validation.

## Input matrix

| Input | Normalized outcome |
| --- | --- |
| Plain allow; optional message string/null, updatedPermissions array/null, toolUseID string/null | Allowed under existing tool/profile rules |
| Plain deny; optional message string/null | Denied, known unexecuted |
| Allow or deny with result, checkpointResult, or checkpointError, including null/wrong type | Unconfirmed; no local replay or checkpoint undo |
| Unknown fields in an approval body, unknown behavior, malformed envelope/body, wrong subtype | Unconfirmed |
| Matching malformed frame followed by matching bare deny | First frame remains Unconfirmed |
| Valid foreign request ID followed by matching bare deny | Foreign frame ignored; current denial accepted |
| Bare current/foreign receipt | Current disarms its receipt deadline; foreign is ignored |
| Receipt with execution payload/unknown field or invalid ID | Unconfirmed; cannot hide execution before a later bare deny |
| Unsupported kind, including current ID plus result | Unconfirmed |
| Valid unrelated user, registry, or non-interrupt control message | Ignore under existing shared DTO contract |
| Known non-approval kind with current top or nested request ID | Unconfirmed; valid interrupt remains Interrupted |
| Valid legacy shell result with only exit_code=0 | Confirmed shell success |
| Shell result with returnDisplay=null | Valid optional presentation |
| Missing/null metadata or exit_code | Unknown shell completion; pending guard retained |
| Non-null exit_code not an integer representable as i32 | Unconfirmed, with invalid exit_code diagnostic |
| exit_code=0 and supplied status is completed, signal null/absent, and any input_wait says interrupted=false | Confirmed shell success |
| exit_code=0 and supplied unknown/null/wrong-typed status, non-null signal, or unknown/true interrupted evidence | Unconfirmed; zero cannot clear conflicting evidence |
| Valid nonzero exit_code | Shell failure; ordinary diagnostic status does not grant authority or clear the guard |
| Shell response containing a checkpoint payload, or shell result for a non-shell/brokered tool | Unconfirmed |
| Checkpoint result for the exact checkpoint profile and HostedSideEffect kind | Existing typed result validator: valid created/skipped succeeds |
| Checkpoint error for that same profile/kind | Existing typed failed/uncertain result retains the guard |
| Typed payload missing/null/wrong type, mixed payload families, invalid ID/outcome/bounds, unknown typed fields | Unconfirmed |
| Duplicated known fields, including exit_code=1 followed by exit_code=0 | Unconfirmed; no last-value authorization |
| Prompt Hook or ReadOnly/Other Ask receives a Host completion or execution-bearing plain decision | Unconfirmed; retain exposure guard regardless of requested kind |
| Plain allow/deny on a non-effect approval, with no execution in later bypass | Restore only this exposure; retain the previous memory/checkpoint values |
| Death after approval delivery but before reading any completion | Resume requires a fresh approval |
| Checkpoint arm/identity/write/sync failure | No approval request is delivered and no effect starts |

The `status`, `signal`, and `input_wait.interrupted` consistency checks constrain
confirmation of zero-exit success. They do not reinterpret a nonzero failure or
restrict ordinary future metadata diagnostics. Absence of status preserves legacy
exit-code-only responses; explicit null status cannot confirm success.

## Producer compatibility and verification

The maintained Shell's allow writer emits a bare allow; its denial writer and
Gateway's denial writers emit only behavior and message. They do not add nullable
execution payloads to a denial. Shell completion presentation/diagnostics include
legitimate null fields, which remain supported. Legacy allow fields already read
by Core (`updatedPermissions` and `toolUseID`) retain their shape contract. No
public control-wire field, auth/question schema, or dependency is added here.

The classification matrix in `core/tests/approval_response_matrix.rs` drives the
existing wait entrypoint using raw frames. The real turn/persist scenarios in
`headless/approval_response_safety_tests.rs` write a Host effect when its response
is read, then check durable pending state, absence of local replay, blocked Auto
followups, and bypass handling. Existing denial, successful-clear, receipt,
transport, range, typed-profile, and recovery controls remain required. A pure
parser pass alone is insufficient evidence of the durable contract.
