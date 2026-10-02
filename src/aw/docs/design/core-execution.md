# Core execution and storage

[中文版](core-execution_zh.md)

`aw-core` executes policy-resolved capability plans through trusted embedding
ports. It reuses the existing registered contract schemas and validators;
`aw-contracts` remains side-effect-free and does not depend on Core.

## Prepare and execute

The caller creates `Core`, implements `ProviderHost` and `Clock`, and supplies a
`PrepareRequest` containing a plan, native boundary, runtime binding and one
`StepInput` per step. `prepare` admits every selected descriptor and invocation
before any Host call. A named unavailable provider rejects the whole request;
an explicitly empty route remains a gap governed by the plan's failure policy.

Preparation returns an immutable `PreparedPlan`. Its event key binds the full
scope and event ID; changing the plan must not allow the same event to run twice.
The caller assigns a distinct event ID to each native boundary occurrence
(including separate pre/post occurrences) and retains it across retries.

`execute` consumes the prepared plan, reserves the event in a `Journal`, and
runs steps serially. It rechecks provider descriptors and absolute deadlines
before dispatch, including after journal writes. Every selected provider within
a step is accounted for before reducing the decision. Denial, preservation or
cancellation stops later steps. The generated terminal record is validated
against the plan and actual invocation evidence before it can be returned.

The Host authenticates providers, implements their protocols, and enforces
execution/output limits. Core validates reported evidence and observed call
intervals, but cannot preempt a synchronous Host. Cancellation is sampled between
calls and after a call returns. The embedding owner supplies trustworthy clocks,
keeps runtime/boundary authority current, and handles process exit/restart races.

## Journal acknowledgements

Core records a claim and invocation start before dispatch, then receipts and step
outcomes before further execution. Each acknowledgement is immediately checked
by `Registry::validate_evidence` against the existing common/v1 evidence definition.
A malformed successful return is an error, including at intermediate writes.
This proves reference shape; durability and record authenticity remain Journal
implementation obligations. Terminal execution is returned only after its final
append is acknowledged.

`FileJournal` uses atomic file creation and synchronized writes on Linux in a
trusted, service-owned local directory. New directories/files have private
permissions. Claim/write failures retain the reservation; a failed append poisons
that writer. Another object or process cannot acquire write ownership for an
existing claim. `Journal::release` closes local write ownership and removes its
in-memory bookkeeping without changing the durable reservation or records. Core
releases every successful claim on return or unwinding, including malformed
acknowledgements, cancellation and Host/storage errors. Failed claims must clean
up resources from that attempt without releasing an existing writer. Direct
Journal callers must release their claims or drop the backend. Release must not
panic or perform fallible storage work; appends after release are rejected.
There is no retry, rollback, claim deletion or recovery API.

Format-1 envelopes contain sequence, previous digest, record and digest. This is
private storage framing, not another AW wire schema. Reads reject partial records
and broken chains. A complete prefix cannot expose removed tail records;
`read_verified` compares the tip with an acknowledgement retained independently.
Hashes do not protect against an attacker who can rewrite both copies. Other
operating systems require another `Journal` implementation.

Journal records contain plan metadata, IDs, digests, receipts and decisions, not
raw capability inputs/outputs. `Execution::calls` retains those payloads in memory;
the embedding application controls logging, retention and artifact access.

## Native ownership and verification

`Execution::record`, `calls` and `journal_ack` expose correlated execution facts.
A `proceed` result is not native dispatch permission or adoption evidence. Before
an actual tool action the native owner must obtain fresh intent/OS evidence and
apply `Registry::validate_dispatch` atomically with its action. Result adoption
requires independent native readback and `Registry::validate_plan_adoption`.

Run `python3 src/aw/scripts/check.py` from the repository root. It checks both
crates, requires non-ignored execution/journal tests, and enforces reviewed
crate dependencies and Rust source size limits. Core tests use synthetic Hosts,
controlled clocks and isolated temporary journals. They cover malformed
acknowledgements, failures, deadlines, cancellation, duplicate events, restarts,
concurrent claims, bounded writer lifetimes and corrupt/truncated storage. They do not certify power-loss
behavior, production Provider integration or native Agent adoption.
