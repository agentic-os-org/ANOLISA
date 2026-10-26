"""Two stages around the existing evidence rules and fixed scoring function."""

import json
import sys
import time
from pathlib import Path
from typing import Any, Callable

from agent_sec_cli.skill_score.audit import Judge
from agent_sec_cli.skill_score.catalog import judge_catalog
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import (
    Adjudication,
    Audit,
    ContractError,
)
from agent_sec_cli.skill_score.evidence.redact import redact_text
from agent_sec_cli.skill_score.evidence_rules import (
    apply_dynamic,
    baseline_eligibility,
    canary_findings,
    merge_attempts,
)
from agent_sec_cli.skill_score.hygiene import RULES, validate_gaps
from agent_sec_cli.skill_score.io import (
    load_source,
    missing_scripts,
    read,
    save,
    source_catalog,
    validate_refs,
)
from agent_sec_cli.skill_score.probes import plan_probes
from agent_sec_cli.skill_score.risk_catalog import CATALOG, REVISION, UPSTREAM
from agent_sec_cli.skill_score.runtime import Runtime
from agent_sec_cli.skill_score.scanners import (
    redact_source,
    scan_content,
    scan_skill,
)
from agent_sec_cli.skill_score.score.engine import score_findings
from agent_sec_cli.skill_score.task import (
    Task,
    parse_task,
    task_json,
    validate_task,
)


def progress(output: Path, stage: str) -> None:
    print(f"[skill-score] {stage}", file=sys.stderr, flush=True)


def runtime_scan_items(catalog: dict[str, str]) -> list[dict[str, str]]:
    """Scan content, not file/process metadata; retain every observation in the catalog."""
    items = []
    for ref, text in catalog.items():
        if ref.startswith("source:"):
            continue
        if ref.startswith("event:"):
            try:
                event = json.loads(text)
            except ValueError:
                event = None
            if isinstance(event, dict) and event.get("kind") in {"file", "process_exit"}:
                # Paths/modes/exit status still feed activation, effect matching and the Judge;
                # unlike ATIF, commands and network/mock payloads, they are not file contents.
                continue
        items.append(
            {
                "ref": ref,
                "text": text,
                "language": (
                    "bash" if ":code-bash-" in ref else "python" if ":code-python-" in ref else ""
                ),
            }
        )
    return items


def plan_skill(
    skill: Path,
    output: Path,
    config: ScoreConfig,
    *,
    judge: Any = None,
    scanner: Callable = scan_skill,
) -> dict[str, Any]:
    source = load_source(skill)
    progress(output, "static scan")
    static = scanner(skill, source, config)
    source["files"].update(static.get("cleaned", {}))
    catalog = source_catalog(source)
    environment = read(Path(__file__).parent / "tool-inventory.json")
    missing = missing_scripts(source)
    if missing:
        progress(output, "input material: referenced scripts missing: " + ", ".join(missing))
    if environment.get("code_scanner_available"):
        environment["scanner_api"] = (
            "from agent_sec_cli.code_scanner.scanner import scan; from agent_sec_cli.code_scanner.models import Language; scan(code, Language.BASH, mode='regex')"
        )
    progress(output, "semantic audit")
    judge = judge or Judge(config)
    audit = Audit.model_validate(
        judge.call(
            "audit",
            {
                "catalog": catalog,
                "static": static,
                "omitted": source["omitted"],
                "missing_script_references": missing,
                "risk_catalog": CATALOG,
                "hygiene_checks": RULES,
                "environment": environment,
            },
            Audit,
            output / "audit.json",
        )
    )
    validate_refs(audit.findings, catalog)
    validate_gaps(audit.control_gaps, catalog, source_only=True)
    audit.omitted_sources = source["omitted"]
    audit.limitations = list(
        dict.fromkeys(
            audit.limitations
            + [
                f"{topic}: {check.reason}"
                for topic, check in audit.pattern_checks.items()
                if check.status == "insufficient_evidence"
            ]
        )
    )
    for finding in audit.findings:
        finding.critical_confirmed = False
        if finding.dynamic != "unsupported":
            finding.dynamic = "no_proof"
    audit.probes = plan_probes(audit, source)
    value = task_json(skill, source["content_hash"], audit)
    validate_task(parse_task(value, skill.parent), source, catalog)
    return value


def run_task(
    task: Task,
    output: Path,
    config: ScoreConfig,
    *,
    judge: Any = None,
    runtime: Any = None,
    content_scanner: Callable = scan_content,
    source_redactor: Callable = redact_source,
) -> dict[str, Any]:
    skill = Path(task.skill_path)
    source = load_source(skill)
    if source["content_hash"] != task.content_hash:
        raise ContractError("Skill content changed after planning; create a new task")
    source = source_redactor(source)
    catalog = source_catalog(source)
    validate_task(task, source, catalog)
    output.mkdir(parents=True, exist_ok=True)
    save(output / "task.json", task_json(skill, task.content_hash, task.audit))
    start = time.monotonic()
    judge = judge or Judge(config)
    runtime = runtime or Runtime(config)
    audit, probes = task.audit, task.audit.probes
    findings = [finding.model_copy(deep=True) for finding in audit.findings]
    gaps = [gap.model_copy(deep=True) for gap in audit.control_gaps]
    failures = [
        f"missing implementation: {path} (required by {', '.join(refs)})"
        for path, refs in audit.missing_implementation.items()
    ]
    evidence, corrections, task_results = [], [], []
    task_checks, hygiene_review = {}, {}
    limitations = [
        f"planning/{identifier}: {reason}" for identifier, reason in audit.planning_errors.items()
    ]
    final_available = False
    environment = {}
    baseline_task = {"completed": False, "failure_kind": "unknown", "assessable": False, "refs": []}
    execution_count = 0
    try:
        try:
            environment = runtime.environment() if probes else {}
        except Exception as exc:
            environment = {"error": redact_text(f"{type(exc).__name__}: {exc}")}
            limitations.append("environment: " + environment["error"])
        save(output / "environment.json", environment)
        for probe in ([] if environment.get("error") else probes):
            attempts = []
            item = {"probe_id": probe.id, "complete": False, "error": "execution budget exhausted"}
            for attempt in range(2 if probe.required else 1):
                remaining = config.deadline_minutes * 60 - (time.monotonic() - start)
                if remaining <= 0 or execution_count >= config.hard_run_limit:
                    break
                attempt_dir = output / "probes" / probe.id / f"attempt-{attempt + 1}"
                attempt_dir.mkdir(parents=True, exist_ok=True)
                execution_count += 1
                progress(output, f"Harbor {probe.id}, attempt {attempt + 1}")
                try:
                    item = runtime.run(
                        skill,
                        probe,
                        attempt_dir,
                        min(remaining, config.timeout_seconds * len(probe.turns) + 180),
                    )
                except Exception as exc:
                    item = {
                        "probe_id": probe.id,
                        "complete": False,
                        "error": redact_text(f"{type(exc).__name__}: {exc}"),
                    }
                save(attempt_dir / "evidence.json", item)
                attempts.append(item)
                if (
                    item.get("complete")
                    or item.get("critical_canary")
                    or item.get("failure_kind") == "setup"
                ):
                    break
                if any(
                    word in str(item.get("error", "")).lower()
                    for word in ("authentication", "api key", "modelnotfound")
                ):
                    break
            if attempts:
                item = merge_attempts(attempts)
            evidence.append(item)
            catalog.update(item.get("catalog", {}))
            if not item.get("complete") and probe.required:
                reason = (
                    item.get("error")
                    or "; ".join(item.get("collection_errors", []))
                    or "missing required trajectory/observations"
                )
                limitations.append(
                    f"runtime/{probe.id}: {reason}; attempts={len(attempts)}; dynamic path remains unverified"
                )
                if probe.id == "baseline":
                    baseline_task.update(
                        failure_kind=(
                            "environment"
                            if item.get("failure_kind") == "setup"
                            else "observation_missing"
                        ),
                        reason=reason,
                    )
            if item.get("critical_canary"):
                progress(output, "Critical confirmed; stopping subsequent Runtime")
                break
        progress(output, "runtime signal scan and final adjudication")
        if (output / "runtime-signals.json").exists():
            dynamic_signals = read(output / "runtime-signals.json")
        else:
            items = runtime_scan_items(catalog)
            progress(
                output,
                f"runtime content scan: {len(items)} items; metadata retained as observations",
            )
            try:
                dynamic_signals = content_scanner(items, config)
            except Exception as exc:
                dynamic_signals = {"signals": [], "cleaned": {}}
                limitations.append(
                    redact_text(
                        f"runtime scanner: {type(exc).__name__}: {exc}; existing observations still sent to final Judge"
                    )
                )
            save(output / "runtime-signals.json", dynamic_signals)
        catalog.update(dynamic_signals.get("cleaned", {}))
        save(output / "evidence-catalog.json", catalog)
        required_refs = {s["ref"] for s in dynamic_signals.get("signals", []) if s.get("ref")}
        required_refs.update(ref for f in audit.findings for ref in f.refs + f.control_refs)
        required_refs.update(ref for gap in gaps for ref in gap.refs + gap.control_refs)
        required_refs.update(
            ref
            for item in evidence
            for ref in item.get("entry_exit_refs", item.get("entry_failure_refs", []))
        )
        model_catalog, catalog_stats = judge_catalog(catalog, required_refs)
        save(output / "judge-catalog.json", {"catalog": model_catalog, "stats": catalog_stats})
        final = Adjudication.model_validate(
            judge.call(
                "final",
                {
                    "catalog": model_catalog,
                    "audit": audit.model_dump(),
                    "runtime_signals": dynamic_signals.get("signals", []),
                    "runtime_tools": {
                        "python_version": environment.get("python"),
                        "available_executables": [
                            name
                            for name, path in environment.get("executables", {}).items()
                            if path
                        ],
                    },
                    "hygiene_checks": RULES,
                    "coverage": [
                        {k: v for k, v in item.items() if k not in {"catalog", "events"}}
                        for item in evidence
                    ],
                },
                Adjudication,
                output / "adjudication.json",
            )
        )
        validate_refs(final.findings, catalog)
        validate_gaps(final.control_gaps, catalog)
        if {finding.id for finding in findings} - {finding.id for finding in final.findings}:
            raise ContractError("final Judge silently dropped prior findings")
        if {gap.id for gap in gaps} - {gap.id for gap in final.control_gaps}:
            raise ContractError("final Judge silently dropped prior control gaps")
        findings = final.findings
        gaps = final.control_gaps
        task_checks = {
            key: [check.model_dump() for check in checks]
            for key, checks in final.task_checks.items()
        }
        hygiene_review = {key: check.model_dump() for key, check in final.hygiene_review.items()}
        task_results = [item.model_dump() for item in final.task_results]
        corrections.extend(final.corrections)
        baseline = next((item for item in evidence if item["probe_id"] == "baseline"), {})
        assessable, baseline_reason = (
            baseline_eligibility(final, probes[0], baseline, catalog)
            if probes
            else (False, "baseline planning failed; no normal task was executed")
        )
        baseline_task = {
            "completed": final.baseline_completed,
            "failure_kind": "none" if final.baseline_completed else final.baseline_failure,
            "reason": final.baseline_reason,
            "refs": final.baseline_refs,
            "assessable": assessable,
            "eligibility_reason": baseline_reason,
        }
        if not assessable:
            limitations.append("baseline: " + baseline_reason)
        apply_dynamic(findings, probes, evidence)
        for probe in probes[1:]:
            active = any(
                finding.id in probe.finding_ids
                and finding.scope == "skill"
                and finding.severity in {"Critical", "High"}
                and finding.status in {"material_risk", "unresolved"}
                for finding in findings
            )
            observed = next((item for item in evidence if item["probe_id"] == probe.id), {})
            if (
                probe.required
                and active
                and any(
                    f.id in probe.finding_ids
                    and f.dynamic in {"no_proof", "not_activated", "unsupported"}
                    for f in findings
                )
            ):
                limitations.append(
                    f"runtime/{probe.id}: {observed.get('unverified_reason') or 'risk path/control not dynamically established'}; static finding retained"
                )
        critical_ids = {
            finding.id
            for finding in audit.findings
            if finding.severity == "Critical" and finding.scope == "skill"
        }
        critical_ids.update(
            finding.id
            for finding in findings
            if finding.severity == "Critical" and finding.scope == "skill"
        )
        critical_candidates = [finding for finding in findings if finding.id in critical_ids]
        if critical_candidates and not canary_findings(evidence):
            progress(output, "independent Critical adjudication")
            try:
                independent = Adjudication.model_validate(
                    judge.call(
                        "critical",
                        {
                            "catalog": model_catalog,
                            "claims": [
                                {
                                    key: finding.model_dump()[key]
                                    for key in (
                                        "id",
                                        "entry",
                                        "operation",
                                        "boundary",
                                        "refs",
                                    )
                                }
                                for finding in critical_candidates
                            ],
                        },
                        Adjudication,
                        output / "critical.json",
                    )
                )
            except Exception:
                failures.append("Independent Critical adjudication did not complete")
                raise
            for finding in critical_candidates:
                reviewed = next(
                    (
                        other
                        for other in independent.findings
                        if other.id == finding.id
                        and other.scope == "skill"
                        and other.severity == "Critical"
                        and other.status in {"material_risk", "unresolved"}
                    ),
                    None,
                )
                finding.critical_confirmed = (
                    reviewed is not None
                    and reviewed.status == "material_risk"
                    and finding.severity == "Critical"
                    and finding.scope == "skill"
                    and finding.status == "material_risk"
                    and bool(set(finding.refs).intersection(reviewed.refs))
                )
                if reviewed is not None and not finding.critical_confirmed:
                    finding.reason = (
                        f"Primary verdict ({finding.scope}/{finding.severity}/{finding.status}): "
                        f"{finding.reason} Independent Skill Critical verdict: {reviewed.reason}"
                    )
                    finding.refs = list(dict.fromkeys([*finding.refs, *reviewed.refs]))
                    finding.scope, finding.severity, finding.status = (
                        "skill",
                        "Critical",
                        "unresolved",
                    )
        final_available = True
        save(output / "evidence-catalog.json", catalog)
    except Exception as exc:
        limitations.append(
            redact_text(
                f"assessment: {type(exc).__name__}: {getattr(exc, 'code', str(exc))}; retained validated source findings and available positive observations"
            )
        )
        apply_dynamic(findings, probes, evidence)
    deterministic = canary_findings(evidence)
    if deterministic:
        findings = [
            finding for finding in findings if finding.id != "critical-canary"
        ] + deterministic
    cited = {
        ref
        for finding in findings
        if finding.scope == "skill" and finding.status in {"material_risk", "unresolved"}
        for ref in finding.refs
    }
    if not final_available and any(
        set(item.get("positive_refs", []) + item.get("attempt_refs", [])) - cited
        for item in evidence
    ):
        failures.append(
            "New boundary evidence could not be adjudicated; a source-only score would omit an observed consequence"
        )
    data = {
        "schema_version": "agent-sec-skill-score/report-v1",
        "skill": skill.name,
        "content_hash": task.content_hash,
        "model": config.model,
        "judge_model": config.judge_model,
        "findings": [f.model_dump() for f in findings],
        "control_gaps": [gap.model_dump() for gap in gaps],
        "failures": failures,
        "planning_notes": list(audit.limitations),
        "limitations": limitations,
        "audit_omitted_sources": audit.omitted_sources,
        "final_available": final_available,
        "corrections": corrections,
        "probes": [p.model_dump() for p in probes],
        "coverage": [
            {k: v for k, v in item.items() if k not in {"catalog", "events"}} for item in evidence
        ],
        "baseline_task": baseline_task,
        "task_results": task_results,
        "task_checks": task_checks,
        "hygiene_review": hygiene_review,
        "environment": environment,
        "elapsed_seconds": round(time.monotonic() - start, 3),
        "risk_catalog": {"revision": REVISION, "upstream": UPSTREAM},
    }
    data.update(score_findings(findings, failures, gaps))
    neutral = [f.model_copy(update={"dynamic": "no_proof"}) for f in findings]
    neutral_score = score_findings(neutral, failures, gaps)["noncritical_score"]
    data["dynamic_diagnostic"] = {
        "scope": "Final roots, severity and existence fixed; noncritical dynamic factor only. Not a causal experiment.",
        "neutral_noncritical_score": neutral_score,
        "observed_noncritical_score": data["noncritical_score"],
        "delta": round(data["noncritical_score"] - neutral_score, 1),
        "critical_gate": data["decision"] in {"blocked", "needs_review"},
    }
    save(output / "evidence-catalog.json", catalog)
    save(output / "events.json", [event for item in evidence for event in item.get("events", [])])
    calls = []
    for stage in ("adjudication", "critical"):
        path = output / f"{stage}.json"
        if path.exists():
            calls.extend(read(path).get("calls", []))
        elif path.with_suffix(".attempts.json").exists():
            calls.extend(read(path.with_suffix(".attempts.json")))
    data["usage"] = {
        "judge_calls": sum("seconds" in call or "usage" in call for call in calls),
        "judge_tokens": sum(call.get("usage", {}).get("total_tokens", 0) for call in calls),
        "runtime_attempts": execution_count,
        "runtime_prompt_tokens": sum(
            (item.get("usage") or {}).get("total_prompt_tokens") or 0
            for ev in evidence
            for item in ev.get("attempts", [ev])
        ),
        "runtime_completion_tokens": sum(
            (item.get("usage") or {}).get("total_completion_tokens") or 0
            for ev in evidence
            for item in ev.get("attempts", [ev])
        ),
    }
    save(output / "internal-report.json", data)
    return public_report(data, catalog)


def finding_excerpt_refs(finding: dict[str, Any], coverage: list[dict]) -> list[str]:
    """Keep a source and the matched outcome visible when the public excerpt is bounded."""
    refs = list(
        dict.fromkeys(finding["refs"] + finding["control_refs"] + finding["control_test_refs"])
    )
    outcomes = [
        ref
        for item in coverage
        if finding["id"] in item.get("matched_finding_ids", [])
        for ref in item.get("positive_refs", []) + item.get("attempt_refs", [])
        if ref in refs
    ]
    priority = (
        [ref for ref in refs if ref.startswith("source:")][:1]
        + outcomes[:1]
        + finding["control_refs"][:1]
        + finding["control_test_refs"][:1]
    )
    return list(dict.fromkeys(priority + refs))[:8]


def public_report(data: dict[str, Any], catalog: dict[str, str]) -> dict[str, Any]:
    deductions = {item["id"]: item["deduction"] for item in data["contributions"]}
    findings = [
        {key: item[key] for key in ("id", "title", "severity", "status", "dynamic", "reason")}
        | {
            "deduction": deductions.get(item["id"], 0.0),
            "evidence": [
                f"{ref}: {catalog.get(ref, 'evidence unavailable')[:500]}"
                for ref in finding_excerpt_refs(item, data["coverage"])
            ],
        }
        for item in data["findings"]
        if item["scope"] == "skill"
        and item["status"] in {"material_risk", "unresolved"}
        and (deductions.get(item["id"], 0) > 0 or item["severity"] == "Critical")
    ]
    judgments = {item["task_id"]: item for item in data["task_results"]}
    baseline = data["baseline_task"]
    judgments["baseline"] = {
        "completed": baseline["completed"] and baseline.get("assessable", False),
        "reason": baseline.get("reason")
        or baseline.get("eligibility_reason")
        or "normal task could not be verified",
    }
    observed = {item["probe_id"]: item for item in data["coverage"]}
    execution = []
    for probe in data["probes"]:
        item = judgments.get(probe["id"])
        ev = observed.get(probe["id"], {})
        if probe["id"] not in observed:
            item = {
                "completed": False,
                "reason": (
                    "not executed: confirmed Critical stopped Runtime"
                    if data["decision"] == "blocked"
                    else "not executed: "
                    + "; ".join(data["failures"] + data.get("limitations", []))
                ),
            }
        elif item is None:
            item = {
                "completed": False,
                "reason": ev.get("error")
                or "task completion was not established by the final Judge",
            }
        execution.append(
            {"task_id": probe["id"], "completed": item["completed"], "reason": item["reason"]}
        )
    return {
        "schema_version": "agent-sec-skill-score/report-v1",
        "content_hash": data["content_hash"],
        "decision": data["decision"],
        "score": data["score"],
        "reason": "; ".join(data["reasons"])
        or (
            "Source-based safety score with partial/unverified dynamic coverage; not a passed dynamic validation."
            if data.get("limitations")
            else "Evidence-qualified relative safety score; not a safety certification."
        ),
        "findings": findings,
        "execution": execution,
        **({"planning_notes": data["planning_notes"]} if data.get("planning_notes") else {}),
        **({"limitations": data["limitations"]} if data.get("limitations") else {}),
        **(
            {
                "control_gaps": [
                    gap
                    | {
                        "deduction": next(
                            (
                                item["deduction"]
                                for item in data["hygiene"]["contributions"]
                                if item["id"] == gap["id"]
                            ),
                            0.0,
                        ),
                        "evidence": [
                            f"{ref}: {catalog.get(ref, 'evidence unavailable')[:500]}"
                            for ref in dict.fromkeys(gap["refs"] + gap["control_refs"])
                        ][:8],
                    }
                    for gap in data["control_gaps"]
                ]
            }
            if data.get("control_gaps")
            else {}
        ),
    }
