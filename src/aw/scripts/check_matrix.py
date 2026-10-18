"""Regression obligations and the README capability grid share one checked manifest."""

from pathlib import Path

START = "<!-- aw-regression-matrix:start -->"
END = "<!-- aw-regression-matrix:end -->"
SHARED_CASES = {
    "catalog": (
        "aw-adapter/lib/aw_adapter",
        "adapter::catalog::tests::registered_capabilities_match_the_regression_matrix_and_config_schema",
    ),
    "security-retry": (
        "aw-service/test/pretool",
        "retry_neutrality_cannot_undo_security_denials_or_fail_closed_errors",
    ),
    "final-replacement": (
        "aw-service/test/posttool",
        "adapters_keep_observers_and_user_hooks_before_one_final_replacement",
    ),
    "capability-rejection": (
        "aw-adapter/lib/aw_adapter",
        "adapter::catalog::tests::unsupported_capabilities_are_rejected_before_provider_discovery",
    ),
}

# The runtime surface, not roadmap vocabulary. Expanding it requires every row
# to declare support or tested rejection for each new cell.
SURFACE = {
    "tool.before": {"observe", "block"},
    "tool.after": {"observe", "replace_result"},
}


def table(matrix: dict) -> str:
    """Render runtime event/effect declarations, not native SDK certification."""
    columns = sorted(
        {(event, effect) for event, effects in SURFACE.items() for effect in effects},
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
            "✓" if effect in row["events"].get(event, {}) else "—" for event, effect in columns
        ]
        rows.append(f"| `{agent}` | " + " | ".join(cells) + " |")
    return "\n".join(rows)


def validate(matrix: dict, runnable: dict[str, set[str]], root: Path) -> None:
    """Fail on missing/ignored tests, uncovered cells, schema drift or stale README grids."""
    if matrix.get("version") != 2 or not matrix.get("agents") or not matrix.get("cases"):
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

    for agent, row in matrix["agents"].items():
        for obligation in ("lifecycle", "security", "tokenless", "admission"):
            covered(row.get(obligation), f"{agent}/{obligation}")
        if "capability-rejection" not in row["admission"]:
            raise ValueError(f"{agent}: shared capability rejection is required")
        if not row.get("events"):
            raise ValueError(f"{agent}: missing hook coverage")
        if not isinstance(row.get("unsupported"), dict):
            raise ValueError(f"{agent}: missing unsupported capability declaration")
        declared = set()
        for section in ("events", "unsupported"):
            for event, effects in row[section].items():
                if not effects:
                    raise ValueError(f"{agent}/{event}: missing effect coverage")
                for effect, cases in effects.items():
                    cell = (event, effect)
                    if effect not in SURFACE.get(event, set()) or cell in declared:
                        raise ValueError(f"{agent}: unknown or overlapping capability {cell}")
                    declared.add(cell)
                    covered(cases, f"{agent}/{section}/{event}/{effect}")
                    if section == "unsupported" and "capability-rejection" not in cases:
                        raise ValueError(f"{agent}/{cell}: rejection coverage is required")
        expected = {(event, effect) for event, effects in SURFACE.items() for effect in effects}
        if declared != expected:
            raise ValueError(f"{agent}: every capability needs support or rejection coverage")
        before = row["events"].get("tool.before", {})
        after = row["events"].get("tool.after", {})
        security = "security-retry" if "block" in before else "capability-rejection"
        tokenless = {"final-replacement"} if "replace_result" in after else set()
        if "block" in before:
            tokenless.add("security-retry")
        if security not in row["security"] or not (tokenless or {"capability-rejection"}) <= set(
            row["tokenless"]
        ):
            raise ValueError(
                f"{agent}: applicable shared security/Tokenless regression is required"
            )
    if set(matrix["providers"]) != {"sec-core", "tokenless"}:
        raise ValueError("sec-core and Tokenless provider contracts are required")
    for provider, cases in matrix["providers"].items():
        covered(cases, provider)
    for readme in ("README.md", "README_zh.md"):
        text = (root / readme).read_text(encoding="utf-8")
        if text.count(START) != 1 or text.count(END) != 1:
            raise ValueError(f"{readme}: missing unique matrix markers")
        if text.split(START)[1].split(END)[0].strip() != table(matrix):
            raise ValueError(f"{readme}: update the grid from tests/regression-matrix.json")


def main() -> None:
    """Explicit local regeneration; the CI gate only checks and never rewrites docs."""
    import argparse
    import json

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-readmes", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    matrix = json.loads((root / "tests/regression-matrix.json").read_text(encoding="utf-8"))
    grid = table(matrix)
    if not args.write_readmes:
        print(grid)
        return
    for name in ("README.md", "README_zh.md"):
        path = root / name
        text = path.read_text(encoding="utf-8")
        if text.count(START) != 1 or text.count(END) != 1:
            raise ValueError(f"{name}: missing unique matrix markers")
        before, after = text.split(START)[0], text.split(END)[1]
        path.write_text(f"{before}{START}\n{grid}\n{END}{after}", encoding="utf-8")


if __name__ == "__main__":
    main()
