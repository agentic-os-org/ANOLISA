# Classify Output Guide

Use `sysom-osops memory classify` as the local entry point for unclear memory
symptoms. It returns the same default Agent envelope as remote deep actions.

## What To Read

| Field | Use |
|-------|-----|
| `agent.summary` | Overall memory verdict and dominant issue. |
| `agent.findings[].category` | Prefer `root_cause` when present. |
| `agent.findings[].detail` | Agent-visible root-cause entities and missing evidence. |
| `agent.next_steps[]` | Focused remote actions that can fill missing evidence. |

Do not call backend-only collectors from the Skill. Do not depend on legacy or
internal fields outside the default Agent envelope.

## Choosing The First Follow-Up

1. If a root-cause finding already contains the required entities, answer from
   classify.
2. If the dominant finding lacks a required entity, run the first relevant
   `kind=command` next step.
3. If the visible envelope names an ownership, currentness, attribution, or
   cleanup gap, follow the focused SysOM action that closes that entity gap.
4. If a later deep action returns concrete holder or owner entities, combine
   them with classify instead of re-running broad local checks.

## When No Memory Action Is Needed

If classify returns no memory finding, or all findings are informational and do
not match the user's symptom, report that current SysOM memory evidence is
healthy or inconclusive. Pivot to IO, load, network, or Java only when the
visible envelope or the user's symptom supports that domain.
