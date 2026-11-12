"""The former 23 checks, integrated into one audit and one bounded deduction."""

from decimal import Decimal
from typing import Any

from agent_sec_cli.skill_score.contracts import (
    ContractError,
    ControlGap,
    Finding,
    PatternCheck,
)


def validate_review(
    review: dict[str, PatternCheck],
    catalog: dict[str, str],
    *,
    source_only: bool = True,
) -> None:
    """An omitted rule is not equivalent to a negative audit result."""
    if set(review) != set(RULES):
        raise ContractError("hygiene_review must cover all 23 rules exactly once")
    for rule, check in review.items():
        if not check.reason.strip() or set(check.refs) - catalog.keys():
            raise ContractError(
                f"{rule}: hygiene review needs a reason and valid references"
            )
        if check.status == "checked" and (
            not check.refs
            or (
                source_only and not any(ref.startswith("source:") for ref in check.refs)
            )
        ):
            raise ContractError(
                f"{rule}: applicable control review needs {'source' if source_only else 'source or runtime'} evidence"
            )


# IDs, categories and weights follow the archived v3 hygiene.py, not its pipeline.
# Each contrast is audit material, not a claim of measured model recall.
_RULES = [
    (
        "LP001",
        "least_privilege",
        3,
        "PE1",
        "Unnecessary broad privilege",
        "A read-only inventory requests root and all-host write access.",
        "A narrowly scoped read-only account suffices.",
        "No privileged action.",
    ),
    (
        "LP002",
        "least_privilege",
        3,
        "R1",
        "Unconstrained code evaluation",
        "An operator-configurable environment/config value is evaluated as command text without an execution bound. Operator authorization alone is not a bound; this is a control gap, not proof of an attacker-controlled exploit.",
        "A fixed command with bounded arguments replaces evaluation.",
        "Quoted eval examples are never executed.",
    ),
    (
        "UI001",
        "untrusted_input_boundary",
        3,
        "R1",
        "Command/data separation",
        "A value supplied as DATA (filename, text, config or query parameter) is interpolated into shell, Python, SQL or other interpreter SOURCE without enforcing data-only syntax. Inspect every interpreter invocation, including fallbacks. Operator permission to supply a filename is not permission to execute code through that filename. Shell quoting does not protect a second interpreter's syntax.",
        "Fixed source consumes data through argv/stdin or bound parameters. A safe invocation in another branch does not protect an interpolating invocation.",
        "Only a fixed literal command executes.",
    ),
    (
        "UI002",
        "untrusted_input_boundary",
        2,
        "R1",
        "Path containment",
        "A file operation (including chmod/chown metadata changes) accepts input/config paths without checking its permitted root. Joining root + relative path does not contain '..' or symlinks. Inspect each actual mutation, not only reads or shell injection.",
        "Resolved paths are checked under an explicit allowed root.",
        "Only a fixed bundled file is accessed.",
    ),
    (
        "UI003",
        "untrusted_input_boundary",
        2,
        "P1",
        "External text trust boundary",
        "Fetched instructions are explicitly delegated task authority without a trust check.",
        "Retrieved instructions stay quoted data and cannot change the task.",
        "No external text is consumed.",
    ),
    (
        "SD001",
        "sensitive_data_handling",
        3,
        "PE3",
        "Sensitive output redaction",
        "An active credential diagnostic prints secret-bearing configuration without redaction.",
        "Only redacted credential metadata is printed.",
        "A placeholder token or in-memory authentication is not output.",
    ),
    (
        "SD002",
        "sensitive_data_handling",
        2,
        "E2",
        "Credential environment scope",
        "A helper forwards the complete credential-bearing environment to child tools.",
        "Only named credentials needed by the child are passed.",
        "No credentials are accessed or forwarded.",
    ),
    (
        "DI001",
        "dependency_integrity",
        3,
        "SC2",
        "Remote executable verification",
        "An installer pipes a downloaded script directly to an interpreter.",
        "A trusted signature is checked before executing the pinned artifact.",
        "Downloaded content is only parsed as data.",
    ),
    (
        "DI002",
        "dependency_integrity",
        2,
        "SC1",
        "Dependency pinning and integrity",
        "Executable dependencies resolve mutable releases without a pin or integrity check.",
        "A trusted lockfile and verified digests constrain resolution.",
        "A fixed bundled dependency is used without downloading code.",
    ),
    (
        "DA001",
        "destructive_action_safety",
        3,
        "R1",
        "Destructive action authorization",
        "A material deletion runs without explicit target scope, preview or approval.",
        "The user approves the exact previewed destructive target.",
        "Disposable temporary cleanup is already explicitly authorized.",
    ),
    (
        "DA002",
        "destructive_action_safety",
        3,
        "R1",
        "Destructive target bounds",
        "Recursive mutation accepts broad roots or wildcard expansion without limits.",
        "Exact resolved targets reject broad roots and preserve recovery.",
        "A fixed disposable test artifact is removed.",
    ),
    (
        "DA003",
        "destructive_action_safety",
        1,
        "R2",
        "Mutation recovery",
        "A material configuration change offers no dry-run, backup, transaction or rollback.",
        "A tested transaction or backup restores the changed resource.",
        "Only read-only or trivial disposable work occurs.",
    ),
    (
        "EG001",
        "egress_destination_control",
        3,
        "E1",
        "Destination authorization",
        "An externally supplied endpoint is used without a destination policy or approval.",
        "An approved allowlist bounds destinations.",
        "No outbound request occurs.",
    ),
    (
        "EG002",
        "egress_destination_control",
        3,
        "E1",
        "Outbound payload minimization",
        "A private diagnostic upload sends unrestricted fields without a purpose/data boundary.",
        "Only approved, minimized and redacted fields are transmitted.",
        "The payload contains only designated public data.",
    ),
    (
        "EG003",
        "egress_destination_control",
        2,
        "E1",
        "External transport encryption",
        "An active external HTTP endpoint has no authenticated encrypted transport.",
        "The external service uses authenticated TLS.",
        "Loopback-only mock HTTP is used.",
    ),
    (
        "UI004",
        "untrusted_input_boundary",
        2,
        "R3",
        "Untrusted processing bounds",
        "External archives, recursive or buffered data have no relevant size/count/depth/time bound.",
        "Input and processing limits are enforced at the consuming path.",
        "Only bounded constants or platform-limited inputs are processed.",
    ),
    (
        "SD003",
        "sensitive_data_handling",
        3,
        "PE3",
        "Secret storage lifecycle",
        "Persisted credentials have neither protected storage nor restricted access and retention.",
        "A managed secret store or access-restricted, expiring storage protects credentials.",
        "Named environment or in-memory use does not persist credentials.",
    ),
    (
        "DA004",
        "destructive_action_safety",
        2,
        "R2",
        "Retry duplicate suppression",
        "A retryable non-idempotent write lacks an idempotency key or completion check.",
        "A stable idempotency key or atomic completion check prevents duplicate effects.",
        "The action is read-only or naturally idempotent.",
    ),
    (
        "EG004",
        "egress_destination_control",
        2,
        "E1",
        "TLS and SSH peer validation",
        "External TLS/SSH explicitly disables certificate, hostname or host-key validation.",
        "Effective certificate or host-key validation/pinning is enabled.",
        "An inactive example or loopback-only fixture does not contact an external peer.",
    ),
    (
        "LP003",
        "least_privilege",
        3,
        "PE1",
        "External execution isolation",
        "External repository code or hooks execute without bounded filesystem, network and credential scope.",
        "An effective sandbox isolates externally supplied code.",
        "Fixed code shipped in the Skill or data-only parsing is used.",
    ),
    (
        "LP004",
        "least_privilege",
        2,
        "PE1",
        "Write identity and context",
        "An external account/cluster/repository write trusts unverified default identity/context.",
        "The intended account and resource context are checked before writing.",
        "Only local disposable work or read-only queries occur.",
    ),
    (
        "UI005",
        "untrusted_input_boundary",
        2,
        "R4",
        "Shared temporary path safety",
        "Predictable shared temporary paths carry secrets, scripts, locks or privileged handoffs.",
        "Private directories and exclusive unpredictable creation protect the handoff.",
        "Public disposable output carries no sensitive handoff.",
    ),
    (
        "FS001",
        "failure_safety",
        3,
        "R2",
        "Security failures stop unsafe work",
        "A failed/unknown security check or isolation setup continues privileged mutation or egress.",
        "Failure stops mutation, becomes read-only, or uses an equally safe fallback.",
        "Best-effort telemetry does not decide security authority.",
    ),
]

RULES = {
    row[0]: dict(
        zip(
            (
                "category",
                "weight",
                "topic",
                "control",
                "gap_example",
                "safe_example",
                "not_applicable",
            ),
            row[1:],
        )
    )
    for row in _RULES
}
CATEGORY_CAP = Decimal(5)
TOTAL_CAP = Decimal(25)


def validate_gaps(
    gaps: list[ControlGap], catalog: dict[str, str], *, source_only: bool = False
) -> None:
    ids: set[str] = set()
    for gap in gaps:
        if gap.id in ids:
            raise ContractError(
                f"duplicate control gap ID {gap.id!r}; give independent assets distinct IDs, or merge repeated evidence"
            )
        if gap.rule_id not in RULES:
            raise ContractError(
                f"{gap.id}: unknown rule_id {gap.rule_id!r}; choose one of {', '.join(RULES)}"
            )
        ids.add(gap.id)
        if set(gap.refs + gap.control_refs) - catalog.keys():
            raise ContractError(
                f"{gap.id}: unknown control evidence {sorted(set(gap.refs + gap.control_refs) - catalog.keys())}"
            )
        if source_only and any(
            not ref.startswith("source:") for ref in gap.refs + gap.control_refs
        ):
            raise ContractError("planning control gaps require source evidence only")


def identity(item: Finding | ControlGap) -> tuple[str, ...]:
    return tuple(
        " ".join(value.casefold().split())
        for value in (item.entry, item.operation, item.boundary)
    )


def score_gaps(gaps: list[ControlGap], findings: list[Finding]) -> dict[str, Any]:
    risks = [
        f
        for f in findings
        if f.scope == "skill" and f.status in {"material_risk", "unresolved"}
    ]
    promoted = {f.control_gap_id for f in risks if f.control_gap_id}
    risk_keys = {identity(f) for f in risks}
    seen: set[tuple[str, ...]] = set()
    seen_ids: set[str] = set()
    categories: dict[str, Decimal] = {}
    counts: dict[str, int] = {}
    contributions = []

    def weight(gap: ControlGap) -> Decimal:
        return Decimal(RULES[gap.rule_id]["weight"]) * {
            "confirmed": Decimal(1),
            "uncertain": Decimal("0.5"),
        }.get(gap.status, Decimal(0))

    total = Decimal(0)
    for gap in sorted(gaps, key=lambda g: (-weight(g), g.rule_id, g.id)):
        key, category = identity(gap), RULES[gap.rule_id]["category"]
        reason, deduction = "scored", Decimal(0)
        if gap.id in promoted or key in risk_keys:
            reason = "included in risk root"
        elif gap.id in seen_ids or key in seen:
            reason = "duplicate control gap"
        elif not weight(gap):
            reason = gap.status
        else:
            seen.add(key)
            seen_ids.add(gap.id)
            counts[category] = counts.get(category, 0) + 1
            deduction = min(
                weight(gap) / counts[category],
                CATEGORY_CAP - categories.get(category, Decimal(0)),
                TOTAL_CAP - total,
            )
            categories[category] = categories.get(category, Decimal(0)) + deduction
            total += deduction
        contributions.append(
            {
                "id": gap.id,
                "rule_id": gap.rule_id,
                "category": category,
                "status": gap.status,
                "deduction": float(deduction),
                "reason": reason,
            }
        )
    return {
        "penalty": float(total),
        "contributions": contributions,
        "categories": {k: float(v) for k, v in categories.items()},
        "category_cap": 5,
        "total_cap": 25,
        "decay": "harmonic",
        "uncertain_multiplier": 0.5,
    }
