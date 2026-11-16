# AW development contract

Read the [repository rules](../../AGENTS.md) first. AW is Linux-only.
Read the applicable references in the task index before editing.

## Product contract

AW is the unified Agent runtime integration layer of ANOLISA. Replace repeated
per-framework and per-component integration with shared semantic Hook contracts,
reusable runtime adapters, independently deployable Providers and consistent
lifecycle management.

Maintain three first-class integration experiences:

1. AW-managed launch through `aw run`.
2. Application-owned native launch with supported bootstrap or attachment.
3. Application-owned integration through public TypeScript or Python SDKs.

All supported modes MUST reuse semantic Hook requirements, capability admission,
instance/task identity, task-stable policy revisions, Provider execution, audit
and bounded cleanup contracts. Preserve the supported lifecycle bindings:
acknowledged child Scopes for Scope-based entries and complete request/tool
occupancy for admitted native roots. See
[lifecycle bindings](docs/design/en/development/hooks.md#lifecycle-bindings).
Entry mode determines process ownership and preparation. Do not introduce
separate policy engines or framework-specific execution systems. Preserve native
model configuration, permissions, UI, scheduling and application ownership.

The 16 semantic Hooks share one vocabulary. Select coverage per Hook and required
effect: prefer verified native Hooks, then suitable language instrumentation or
observation Sources. Blocking, modification and final-delivery guarantees require
verified native adoption. Keep optional instrumentation out of core SDK imports.

Limit framework-specific integration to capability profiles, thin event mapping,
installation, verification and necessary native execution extensions. Adding an
Agent MUST reuse common registration, execution, Provider and audit logic.

AW owns runtime admission, execution policy, instance/task identity and Hook
coverage. Distribution owns software installation, upgrades and composition;
retain AW's existing local Provider-extension management boundary. Designated OS
components own resource provisioning and sandbox execution. AW retains bounded
invocation, cancellation and cleanup for its own resources.

Every new integration or major architecture change must demonstrate a concrete
contribution to reusable integration, composable system capabilities, unified
runtime management, or installable and upgradeable delivery.

## Essential constraints

- Keep framework-specific code out of Service, Core and Host. Concrete Providers
  belong in `providers/` or independent repositories, outside framework crates.
- Keep the Hook path Adapter → Service → Host → executor → Provider, returning
  results to the Adapter for adoption. Keep `Core::prepare/execute` separate.
- Attach needs verified ownership and supported wiring; a PID does not prove
  injection support. App Ready, AW Ready and actual adoption are separate facts.
- Optional panels must stay outside tool admission and execution.
- Preserve denial, cancellation, deadlines and task policy identity. Reject
  unsupported applicable enabled events, effects and failure actions regardless
  of `required`; auxiliary observation may degrade only when it carries no
  configured requirement. Never replay tools or uncertain Provider effects.
- Distinguish implemented, experimentally verified, planned and unavailable
  capabilities. Source SPI and Resolved Hook Plan remain development targets;
  current executable support comes from code, profiles and acceptance evidence.

## Required reading by task

Read every matching row; linked development rules apply to the same task even
when the edited files are outside `src/aw`. Keep one canonical explanation per
contract and update its index when moving rules.

| Task | Required reference |
| --- | --- |
| Crate boundaries, Providers, packaging or schemas | [Framework and Provider rules](docs/design/en/development/architecture.md) |
| Agent/SDK/Source, Hook effects, attach, readiness or policy updates | [Hook and Source rules](docs/design/en/development/hooks.md) |
| Source code, tests, gates, workflows or releases | [Layout and CI rules](docs/design/en/development/validation.md) · [Contributing](CONTRIBUTING.md) |
| Documentation, support claims or navigation | [Documentation rules](docs/design/en/development/documentation.md) · [Repository standard](../../specs/documentation-standard.md) |

For current support, consult the [capability reference](../../docs/user-guide/en/aw/capabilities.md)
and its linked Agent profiles. For call paths and contracts, use the
[design index](docs/design/en/README.md). Native and release evidence follows
[acceptance guidance](../../contribute/en/aw/testing.md).

## Verification

AW/Provider and AW workflow changes require `python3 src/aw/scripts/check.py`
from the repository root. The CI reference defines scope and native acceptance.
Run documentation link/navigation checks for changed pages. Preserve existing
regression assertions, bound process lifetimes and clean up owned resources.

## Documentation audiences and writing

For AW documentation under `docs/`, `contribute/` and Provider guides, follow
[documentation rules](docs/design/en/development/documentation.md). This anchor
retains the existing contribution-guide entrypoint.
