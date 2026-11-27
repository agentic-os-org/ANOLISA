"""Regression obligations and visible Hook grids share one checked manifest."""

import re
from pathlib import Path

START = "<!-- aw-regression-matrix:start -->"
END = "<!-- aw-regression-matrix:end -->"
COVERAGE_START = "<!-- aw-hook-coverage:start -->"
COVERAGE_END = "<!-- aw-hook-coverage:end -->"
CAPABILITY_DOCS = (
    "README.md",
    "README_zh.md",
    "../../docs/user-guide/en/aw/capabilities.md",
    "../../docs/user-guide/zh/aw/capabilities.md",
)
SHARED_CASES = {
    "catalog": (
        "aw-adapter/lib/aw_adapter",
        "adapter::catalog::tests::registered_capabilities_match_the_regression_matrix_and_config_schema",
    ),
    "security-retry": (
        "aw-cli/test/pretool",
        "retry_neutrality_cannot_undo_security_denials_or_fail_closed_errors",
    ),
    "final-replacement": (
        "aw-cli/test/posttool",
        "adapters_keep_observers_and_user_hooks_before_one_final_replacement",
    ),
}
HOOK_EVENTS = (
    "session.start",
    "input.submit",
    "tool.before",
    "tool.after",
    "permission.request",
    "compact.before",
    "compact.after",
    "subagent.start",
    "subagent.stop",
    "turn.stop",
    "session.end",
    "model.before_request",
    "runtime.observed",
    "runtime.exited",
    "security.violation",
    "coverage.changed",
)
EFFECT_ORDER = ("observe", "block", "replace_input", "replace_result", "ask")
AW_PRODUCED_EVENTS = ("runtime.observed", "runtime.exited", "coverage.changed")
COVERAGE_EVENTS = tuple(event for event in HOOK_EVENTS if event not in AW_PRODUCED_EVENTS)
EFFECT_ABBREVIATIONS = {
    "observe": "o",
    "block": "b",
    "replace_input": "ri",
    "replace_result": "rr",
    "ask": "a",
}
BLOCKING_EVENTS = frozenset(
    {
        "input.submit",
        "tool.before",
        "permission.request",
        "compact.before",
        "subagent.start",
        "turn.stop",
        "model.before_request",
        "security.violation",
    }
)


def event_allowed_effects(event: str) -> tuple[str, ...]:
    """Return the semantic effects permitted on one canonical Hook event."""
    allowed = ["observe"]
    if event in BLOCKING_EVENTS:
        allowed.append("block")
    if event == "tool.before":
        allowed.append("replace_input")
    if event == "tool.after":
        allowed.append("replace_result")
    if event in {"tool.before", "permission.request"}:
        allowed.append("ask")
    return tuple(allowed)


def capability_cells(matrix: dict) -> dict[str, dict[str, dict]]:
    """Project the matrix-declared default native 16 x framework event/effect grid.

    Note: regression-matrix.json only tracks default native event/effect regression
    obligations. Supported entries therefore emit static placeholders (`sources: ["native"]`,
    `mode: "control"`, `failure_actions: []`) and do not reflect runtime deployment profile
    differences (such as `withhold_result` or auxiliary observation sources); callers must
    use `aw_contracts::source::project_capability_cells` for authoritative runtime
    `CapabilityCell` contracts.
    """
    if not isinstance(matrix.get("agents"), dict) or not matrix["agents"]:
        raise ValueError("missing or unsupported regression matrix")
    cells: dict[str, dict[str, dict]] = {}
    for agent, row in sorted(matrix["agents"].items()):
        events = row.get("events")
        if not isinstance(events, dict) or not events:
            raise ValueError(f"{agent}: missing hook coverage")
        for event, declared in events.items():
            if event not in HOOK_EVENTS:
                raise ValueError(f"{agent}/{event}: unknown semantic hook")
            if not isinstance(declared, dict) or not declared:
                raise ValueError(f"{agent}/{event}: missing effect coverage")
            allowed = event_allowed_effects(event)
            if "observe" not in declared or any(effect not in allowed for effect in declared):
                raise ValueError(f"{agent}/{event}: unsupported or incomplete effect set")
        agent_cells: dict[str, dict] = {}
        for event in HOOK_EVENTS:
            declared = events.get(event)
            if declared:
                agent_cells[event] = {
                    "adapter": agent,
                    "event": event,
                    "status": "supported",
                    "sources": ["native"],
                    "mode": "control",
                    "effects": [effect for effect in EFFECT_ORDER if effect in declared],
                    "failure_actions": [],
                }
            else:
                agent_cells[event] = {
                    "adapter": agent,
                    "event": event,
                    "status": "unsupported",
                    "sources": [],
                    "mode": None,
                    "effects": [],
                    "failure_actions": [],
                }
        cells[agent] = agent_cells
    return cells


def table(matrix: dict) -> str:
    """Render runtime event/effect declarations, not native SDK certification."""
    columns = sorted(
        {
            (event, effect)
            for row in matrix["agents"].values()
            for event, effects in row["events"].items()
            for effect in effects
        },
        key=lambda cell: (
            {"tool.before": 0, "tool.after": 1}.get(cell[0], 2),
            cell[0],
            cell[1] != "observe",
            cell[1],
        ),
    )
    rows = [
        "| Agent | " + " | ".join(f"`{event}` / `{effect}`" for event, effect in columns) + " |",
        "| --- | " + " | ".join("---" for _ in columns) + " |",
    ]
    for agent, row in sorted(matrix["agents"].items()):
        cells = [
            "✓" if effect in row["events"].get(event, {}) else "×" for event, effect in columns
        ]
        rows.append(f"| `{agent}` | " + " | ".join(cells) + " |")
    return "\n".join(rows)


def coverage_table(matrix: dict) -> str:
    """Render declared default-profile coverage across all Agent-semantic Hooks."""
    cells = capability_cells(matrix)
    rows = [
        "| Agent | " + " | ".join(f"`{event}`" for event in COVERAGE_EVENTS) + " |",
        "| --- | " + " | ".join("---" for _ in COVERAGE_EVENTS) + " |",
    ]
    for agent, agent_cells in sorted(cells.items()):
        rendered = []
        for event in COVERAGE_EVENTS:
            cell = agent_cells[event]
            if cell["status"] == "supported":
                rendered.append(
                    " ".join(
                        EFFECT_ABBREVIATIONS[effect]
                        for effect in EFFECT_ORDER
                        if effect in cell["effects"]
                    )
                )
            else:
                rendered.append("×")
        rows.append(f"| `{agent}` | " + " | ".join(rendered) + " |")
    return "\n".join(rows)


_DETAILS_TAG_RE = re.compile(r"<details\b[^>]*>|</details\s*>", re.IGNORECASE)


def _has_unclosed_details(prefix: str) -> bool:
    """Return whether an open HTML <details> element surrounds the following marker."""
    cleaned = re.sub(r"<!--.*?-->", "", prefix, flags=re.DOTALL)
    depth = 0
    for match in _DETAILS_TAG_RE.finditer(cleaned):
        if match.group(0).startswith("</"):
            depth = max(0, depth - 1)
        else:
            depth += 1
    return depth > 0


def validate(matrix: dict, runnable: dict[str, set[str]], root: Path) -> None:
    """Fail on missing/ignored tests, uncovered cells, schema drift or stale guide grids."""
    if matrix.get("version") != 1 or not matrix.get("agents") or not matrix.get("cases"):
        raise ValueError("missing or unsupported regression matrix")
    for name, (target, test) in SHARED_CASES.items():
        if matrix["cases"].get(name) != {"target": target, "test": test}:
            raise ValueError(f"required shared matrix case changed: {name}")
    for target in matrix["required_targets"]:
        if not runnable.get(target):
            raise ValueError(f"required test target is missing or empty: {target}")
    for name, case in matrix["cases"].items():
        if case["test"] not in runnable.get(case["target"], set()):
            raise ValueError(f"matrix case {name} is missing or ignored: {case}")

    def covered(cases: list, label: str) -> None:
        if not isinstance(cases, list) or not cases:
            raise ValueError(f"missing regression coverage: {label}")
        if any(case not in matrix["cases"] for case in cases):
            raise ValueError(f"unknown regression case: {label}")

    # These two tests iterate every matrix agent using explicit native fixtures.
    shared = {"security": "security-retry", "tokenless": "final-replacement"}
    for agent, row in matrix["agents"].items():
        for obligation in ("lifecycle", "security", "tokenless"):
            covered(row.get(obligation), f"{agent}/{obligation}")
        for obligation, case in shared.items():
            if obligation == "tokenless" and "replace_result" not in row["events"].get("tool.after", {}):
                continue
            if case not in row[obligation]:
                raise ValueError(f"{agent}: shared {obligation} regression is required")
        if not row.get("events"):
            raise ValueError(f"{agent}: missing hook coverage")
        for event, effects in row["events"].items():
            if not effects:
                raise ValueError(f"{agent}/{event}: missing effect coverage")
            for effect, cases in effects.items():
                covered(cases, f"{agent}/{event}/{effect}")
    cells = capability_cells(matrix)
    if any(len(row) != len(HOOK_EVENTS) for row in cells.values()):
        raise ValueError("incomplete 16 x framework capability cell grid")
    if set(matrix["providers"]) != {"sec-core", "tokenless", "semantic-arbiter"}:
        raise ValueError("sec-core, Tokenless and semantic-arbiter provider contracts are required")
    for provider, cases in matrix["providers"].items():
        covered(cases, provider)
    expected_grid = table(matrix)
    expected_coverage = coverage_table(matrix)
    for document in CAPABILITY_DOCS:
        text = (root / document).read_text(encoding="utf-8")
        if text.count(START) != 1 or text.count(END) != 1 or text.index(START) > text.index(END):
            raise ValueError(f"{document}: missing unique matrix markers")
        if _has_unclosed_details(text.split(START, 1)[0]):
            raise ValueError(f"{document}: keep the Hook grid expanded")
        if text.split(START)[1].split(END)[0].strip() != expected_grid:
            raise ValueError(f"{document}: update the grid from tests/regression-matrix.json")
        if (
            text.count(COVERAGE_START) != 1
            or text.count(COVERAGE_END) != 1
            or text.index(COVERAGE_START) > text.index(COVERAGE_END)
        ):
            raise ValueError(f"{document}: missing unique coverage markers")
        if _has_unclosed_details(text.split(COVERAGE_START, 1)[0]):
            raise ValueError(f"{document}: keep the Hook coverage grid expanded")
        if text.split(COVERAGE_START)[1].split(COVERAGE_END)[0].strip() != expected_coverage:
            raise ValueError(
                f"{document}: update the coverage grid from tests/regression-matrix.json"
            )


def _replace_block(
    document: str, text: str, start: str, end: str, content: str, label: str
) -> str:
    if text.count(start) != 1 or text.count(end) != 1 or text.index(start) > text.index(end):
        raise ValueError(f"{document}: missing unique {label} markers")
    before, rest = text.split(start, 1)
    _, after = rest.split(end, 1)
    return f"{before}{start}\n{content}\n{end}{after}"


def main() -> None:
    """Explicit local regeneration; the CI gate only checks and never rewrites docs."""
    import argparse
    import json

    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write-docs", action="store_true")
    mode.add_argument(
        "--coverage",
        action="store_true",
        help="print the declared default-profile coverage table across Agent-semantic Hooks",
    )
    mode.add_argument(
        "--cells-json",
        action="store_true",
        help=(
            "print static matrix-declared native 16 x framework event/effect cells as JSON "
            "(developer projection with placeholder mode/failure_actions, not runtime CapabilityCell)"
        ),
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    matrix = json.loads((root / "tests/regression-matrix.json").read_text(encoding="utf-8"))
    if args.cells_json:
        print(json.dumps(capability_cells(matrix), indent=2, sort_keys=True))
        return
    if args.coverage:
        print(coverage_table(matrix))
        return
    grid = table(matrix)
    if not args.write_docs:
        print(grid)
        return
    coverage = coverage_table(matrix)
    for name in CAPABILITY_DOCS:
        path = root / name
        text = path.read_text(encoding="utf-8")
        text = _replace_block(name, text, START, END, grid, "matrix")
        text = _replace_block(name, text, COVERAGE_START, COVERAGE_END, coverage, "coverage")
        path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    main()
