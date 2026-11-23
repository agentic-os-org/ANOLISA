# AW documentation development rules

[中文版](../../zh/development/documentation.md)

These are required development rules for the tasks indexed in [AGENTS.md](../../../../AGENTS.md). Read the relevant sections before editing.

These rules also apply to AW pages under `docs/`, `contribute/` and the paired
Provider integration guides. Read the repository documentation and change-description standards first.

| Reader and task | Canonical location | Content |
| --- | --- | --- |
| Evaluate AW and start using it | README, `docs/user-guide/{en,zh}/aw/` | Core benefits, visible support matrix, installation, configuration, useful tool result, routine maintenance |
| Integrate or deploy an application | `docs/developer-guide/{en,zh}/aw/` | Agent factories, Provider integration, worker deployment, persistence, updates, failure recovery |
| Change or release AW | CONTRIBUTING, `contribute/{en,zh}/aw/` | Build dependencies, targeted checks, native acceptance, evidence and cleanup |
| Change a protocol or implementation boundary | `src/aw/docs/design/{en,zh}/` | Ownership, invariants, ordering and design rationale |

User chapters must let a reader complete one task: give prerequisites, commands,
the expected visible result and necessary stop or rollback steps. Explain the
capability and its practical support range before configuration details. Link
to developer references for SDK wiring, deployment controls, field limits,
protocol envelopes, package transactions and test fixtures. Do not make users
assemble service RPCs or read implementation history to run an example.

Developer chapters add only details needed to integrate, deploy or diagnose.
Split Agent instructions under `agents/`, deployment under `deployment/`, and
command references under `reference/`. Keep each component manual under its
component directory with `index.md` as the entrypoint; express reading order
through indexes and `docs/navigation.yaml`. Build and test commands belong in contribution documentation.
Keep current behavior separate from explicitly labelled design proposals.

Maintain one canonical explanation per contract and link from other chapters.
Rewrite the relevant section when behavior changes; do not append a release
narrative, review history, repeated non-goals or boilerplate assurances. Replace
long prose comparisons with a short table when readers need to choose a mode.
Use concrete conditions and actions for errors and limitations.

Keep the generated Hook matrix expanded near the top of both READMEs and
capability pages, never inside `details`. Generate all copies from the same
regression manifest. Put MCP and update summaries in the capability page and
exact versions, wiring and recovery in the corresponding developer chapter.

Update both languages, links, moved-section anchors and website navigation in
the same PR. Keep command blocks identical, define variables before use and
state the working directory for source examples. Check documentation links and
navigation; run gate behavior tests when changing generators or doc assertions.
