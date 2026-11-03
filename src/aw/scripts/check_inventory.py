"""Compile the workspace once and inventory its actual libtest executables."""

import json
import re
from pathlib import Path
from typing import Callable


def inventory(text: str) -> set[str]:
    """Reject truncated, empty-format or unrecognized libtest listings."""
    tests = set()
    summary = None
    for line in text.splitlines():
        if line.endswith(": test"):
            tests.add(line[:-6])
        elif match := re.fullmatch(r"(\d+) tests?, (\d+) benchmarks?", line):
            if summary is not None:
                raise ValueError("duplicate test inventory summary")
            summary = int(match[1])
        elif line.strip():
            raise ValueError(f"unexpected test inventory line: {line!r}")
    if summary is None or summary != len(tests):
        raise ValueError("missing or inconsistent test inventory summary")
    return tests


def collect(
    root: Path, metadata: dict, run: Callable, opt_in: frozenset[str] = frozenset()
) -> dict[str, set[str]]:
    """Enumerate every harness; only explicit native acceptance targets may be ignored."""
    packages = {
        p["id"]: p for p in metadata["packages"] if p["id"] in metadata["workspace_members"]
    }
    expected = {
        f"{p['name']}/test/{target['name']}"
        for p in packages.values()
        for target in p["targets"]
        if target["kind"] == ["test"]
    }
    if not opt_in <= expected:
        raise ValueError(f"unknown opt-in test targets: {sorted(opt_in - expected)}")
    built = run(
        [
            "cargo",
            "test",
            "--workspace",
            "--locked",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ],
        root,
        capture=True,
    )
    executables = {}
    finished = False
    for line in built.splitlines():
        artifact = json.loads(line)
        if artifact["reason"] == "build-finished":
            finished = artifact["success"] is True
        if (
            artifact["reason"] != "compiler-artifact"
            or artifact["package_id"] not in packages
            or not artifact["profile"]["test"]
            or artifact["executable"] is None
        ):
            continue
        target = artifact["target"]
        key = f"{packages[artifact['package_id']]['name']}/{target['kind'][0]}/{target['name']}"
        if key in executables:
            raise ValueError(f"duplicate test artifact: {key}")
        executables[key] = artifact["executable"]
    if not finished or not executables or not expected <= executables.keys():
        raise ValueError(f"missing compiled test targets: {sorted(expected - executables.keys())}")
    runnable = {}
    for key, executable in sorted(executables.items()):
        tests = inventory(run([executable, "--list"], root, timeout=30, capture=True))
        ignored = inventory(
            run([executable, "--list", "--ignored"], root, timeout=30, capture=True)
        )
        if key in opt_in and (not tests or tests != ignored):
            raise ValueError(f"{key}: opt-in targets must contain only ignored acceptance tests")
        if not ignored <= tests or (key in expected - opt_in and not tests - ignored):
            raise ValueError(f"{key}: no runnable tests or inconsistent ignored inventory")
        runnable[key] = tests - ignored
        print(f"{key}: {len(runnable[key])} runnable tests", flush=True)
    return runnable


def check_inventory(root: Path, metadata: dict, run: Callable) -> None:
    """Bind every required matrix case to a non-ignored, compiled test."""
    from check_matrix import validate

    matrix = json.loads((root / "tests/regression-matrix.json").read_text(encoding="utf-8"))
    opt_in = matrix["opt_in_targets"]
    if not isinstance(opt_in, dict) or any(
        not isinstance(reason, str) or not reason.strip() for reason in opt_in.values()
    ):
        raise ValueError("opt-in test targets require an explicit acceptance rationale")
    workspaces = [(root, metadata)] + [
        (Path(provider["workspace_root"]), provider)
        for provider in metadata.get("provider_workspaces", [])
    ]
    targets = set()
    runnable = {}
    for directory, workspace in workspaces:
        names = {
            p["name"] for p in workspace["packages"]
            if p["id"] in workspace["workspace_members"]
        }
        selected = frozenset(key for key in opt_in if key.split("/")[0] in names)
        inventory = collect(directory, workspace, run, selected)
        if runnable.keys() & inventory.keys():
            raise ValueError("test target belongs to multiple workspaces")
        runnable.update(inventory)
        targets.update(selected)
    if targets != opt_in.keys():
        raise ValueError(f"unknown opt-in test targets: {sorted(opt_in.keys() - targets)}")
    validate(matrix, runnable, root)
