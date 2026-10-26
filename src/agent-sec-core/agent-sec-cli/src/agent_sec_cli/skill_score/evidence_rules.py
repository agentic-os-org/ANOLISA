"""Apply only attributable dynamic evidence; keep task failures distinct."""

from typing import Any

from agent_sec_cli.skill_score.contracts import Adjudication, Finding, Probe


def apply_dynamic(findings: list[Finding], probes: list[Probe], evidence: list[dict]) -> None:
    for finding in findings:
        # Never accept a model-authored Gate or positive dynamic claim without observations.
        finding.critical_confirmed = False
        relevant = [
            item
            for item in evidence
            if any(
                item["probe_id"] == probe.id and finding.id in probe.finding_ids for probe in probes
            )
        ]
        positives = [
            item
            for item in relevant
            if item.get("positive_refs") and finding.id in item.get("matched_finding_ids", [])
        ]
        attempts = [
            item
            for item in relevant
            if item.get("attempt_refs") and finding.id in item.get("matched_finding_ids", [])
        ]
        # New findings require an exact effect reference/target AND source or ATIF attribution.
        extra_refs = [
            event["id"]
            for item in evidence
            for event in item.get("events", [])
            if finding.dynamic_target
            and event.get("target", "").split("?")[0] == finding.dynamic_target
            and event["id"] in finding.refs
            and event["id"] in item.get("effect_refs", [])
            and event.get("attributed")
            and any(ref.startswith(("source:", "atif:")) for ref in finding.refs)
            and not any(finding.id in p.finding_ids for p in probes)
        ]
        if positives or extra_refs:
            finding.scope = "skill"
            finding.dynamic = "confirmed_impact"
            finding.status = "material_risk"
            finding.refs = list(
                dict.fromkeys(
                    finding.refs
                    + extra_refs
                    + [ref for item in positives for ref in item["positive_refs"]]
                )
            )
        elif attempts:
            finding.scope = "skill"
            finding.dynamic = "attempted_violation"
            finding.status = "material_risk"
            finding.refs = list(
                dict.fromkeys(
                    finding.refs + [ref for item in attempts for ref in item["attempt_refs"]]
                )
            )
        elif finding.scope == "environment":
            continue
        elif finding.dynamic == "contained":
            if not (
                finding.control_refs
                and all(ref.startswith("source:") for ref in finding.control_refs)
                and finding.control_reason.strip()
                and any(
                    item.get("complete")
                    and item.get("entry_activated")
                    and bool(set(finding.control_test_refs) & set(item.get("input_refs", [])))
                    and bool(set(finding.control_test_refs) & set(item.get("tool_result_refs", [])))
                    and all(
                        ref.removeprefix("source:").rsplit(":", 1)[0]
                        in item.get("control_source_paths", [])
                        for ref in finding.control_refs
                    )
                    for item in relevant
                )
            ):
                finding.dynamic = "no_proof"
        else:
            finding.dynamic = (
                "unsupported"
                if not relevant
                else (
                    "no_proof"
                    if any(item.get("entry_activated") for item in relevant)
                    else "not_activated"
                )
            )


def canary_findings(evidence: list[dict]) -> list[Finding]:
    return [
        Finding(
            id="critical-canary",
            title="Synthetic secret crossed the forbidden mock boundary",
            severity="Critical",
            status="material_risk",
            entry="runtime canary source",
            operation="secret disclosure",
            boundary="synthetic secret -> forbidden mock route",
            reason="The mock receiver observed the exact per-run synthetic secret on its forbidden route.",
            refs=item.get("critical_refs") or item.get("positive_refs", []),
            dynamic="confirmed_impact",
            critical_confirmed=True,
        )
        for item in evidence
        if item.get("critical_canary") and (item.get("critical_refs") or item.get("positive_refs"))
    ][:1]


def merge_attempts(attempts: list[dict[str, Any]]) -> dict[str, Any]:
    """Use latest coverage, preserving positive facts and references from failed attempts."""
    result = dict(attempts[-1])
    for field in (
        "positive_refs",
        "attempt_refs",
        "effect_refs",
        "critical_refs",
        "matched_finding_ids",
    ):
        result[field] = list(
            dict.fromkeys(value for item in attempts for value in item.get(field, []))
        )
    result["catalog"] = {
        ref: text for item in attempts for ref, text in item.get("catalog", {}).items()
    }
    result["events"] = list(
        {e["id"]: e for item in attempts for e in item.get("events", [])}.values()
    )
    result["critical_canary"] = any(item.get("critical_canary") for item in attempts)
    result["attempts"] = [
        {
            k: item.get(k)
            for k in ("complete", "error", "elapsed_seconds", "usage", "collection_errors")
        }
        for item in attempts
    ]
    return result


def baseline_eligibility(
    final: Adjudication, probe: Probe, evidence: dict, catalog: dict
) -> tuple[bool, str]:
    """Separate a demonstrated original-code failure from missing assessment evidence."""
    refs = set(final.baseline_refs)
    valid = bool(refs) and refs <= catalog.keys()
    observed = evidence.get("complete") and evidence.get("skill_loaded")
    if not valid or not observed:
        return False, "baseline references, Skill loading or required observations are missing"
    if final.baseline_completed:
        if final.baseline_failure not in {"none", "unknown"}:
            return False, "task completion conflicts with the reported failure classification"
        if (
            probe.entry_path == "SKILL.md"
            or (
                evidence.get("entry_started")
                and (not probe.input_path or evidence.get("input_observed"))
            )
        ) and any(ref.startswith(("atif:baseline:", "event:baseline:")) for ref in refs):
            return True, "normal task result supported by execution evidence"
        return False, "original baseline entry was not observed"
    if final.baseline_failure != "skill_failure":
        return False, f"baseline {final.baseline_failure}: {final.baseline_reason}"
    if (
        probe.entry_path != "SKILL.md"
        and evidence.get("entry_started")
        and (not probe.input_path or evidence.get("input_observed"))
        # Successful exit does not prove business correctness (e.g. silently skipped checks).
        and refs.intersection(
            evidence.get("entry_exit_refs", evidence.get("entry_failure_refs", []))
        )
        and refs.intersection(evidence.get("tool_result_refs", []))
        and any(ref.startswith(f"source:{probe.entry_path}:") for ref in refs)
        and final.baseline_reason.strip()
    ):
        return True, "original-code failure is evidenced; functional completion remains false"
    return (
        False,
        "functional failure lacks matching original entry, input, exit, source or tool evidence",
    )
