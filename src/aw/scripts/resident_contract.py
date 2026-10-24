"""Share container suite selection and evidence checks with nightly aggregation."""

import json
import math
from pathlib import Path

from native_reports import validate_case, verify_case

BINARIES = {"aw", "aw-provider-tokenless", "tokenless", "rtk"}


def shards(manifest: dict) -> dict:
    """Isolate long sessions while deriving every deadline from the inventory."""
    result = {}
    for agent, row in suites(manifest).items():
        for lane in ("regular", "long"):
            cases = [
                case for case in row["cases"]
                if (case["timeout_seconds"] > 1200) == (lane == "long")
            ]
            if cases:
                seconds = sum(case["timeout_seconds"] + 20 for case in cases) + 120
                result[f"{agent}-{lane}"] = {
                    "agent": agent, "cases": cases, "timeout_seconds": seconds,
                    # Image load, evidence copy, cleanup and artifact upload.
                    "job_minutes": math.ceil((seconds + 900) / 60),
                }
    return result


def suites(manifest: dict) -> dict:
    """Reuse every resident command and its exact report from the native inventory."""
    selected = {}
    for agent in ("claude-sdk", "pi"):
        row = manifest["agents"][agent]
        cases = [
            case for case in row["cases"] if "tests/sdk_resident.py" in case["argv"]
        ]
        if not cases or len({case["name"] for case in cases}) != len(cases):
            raise ValueError(f"{agent}: missing or duplicate resident suites")
        for case in cases:
            if case["argv"][:2] != ["{python}", "tests/sdk_resident.py"]:
                raise ValueError("resident suite must execute the shared harness")
            if case["report"] != "json":
                raise ValueError("resident container requires named JSON evidence")
            validate_case(case)
        selected[agent] = {**row, "cases": cases}
    if len({row["python"] for row in selected.values()}) != 1:
        raise ValueError("resident image requires a shared pinned Python runtime")
    return selected


def verify_shard(directory: Path, manifest: dict, expected: str, shard: str) -> dict:
    """Recheck individual native reports instead of trusting the container exit code."""
    report = json.loads((directory / "result.json").read_text())
    if report.get("candidate_sha") != expected or report.get("passed") is not True:
        raise ValueError("resident container failed or belongs to another candidate")
    plan = shards(manifest)[shard]
    selected = {plan["agent"]: {**manifest["agents"][plan["agent"]], "cases": plan["cases"]}}
    if report.get("shard") != shard:
        raise ValueError("resident report belongs to a different shard")
    keys = {
        (agent, case["name"])
        for agent, row in selected.items()
        for case in row["cases"]
    }
    completed = report.get("suites", [])
    if (
        len(completed) != len(keys)
        or {(item["agent"], item["name"]) for item in completed} != keys
    ):
        raise ValueError("resident container has missing, duplicate or unknown suites")
    for agent, row in selected.items():
        for case in row["cases"]:
            verify_case(case, "", directory / agent)
    runtime = report["runtime"]
    if runtime["python"] != next(iter(selected.values()))["python"]:
        raise ValueError("resident Python version differs from the native inventory")
    if (
        runtime["node"] != "v24.16.0"
        or runtime["claude_cli"] != "2.1.161 (Claude Code)"
        or runtime["rtk"] != "rtk 0.49.0"
    ):
        raise ValueError("resident native runtime version changed")
    for name, version in manifest["agents"]["claude-sdk"]["versions"].items():
        if runtime["python_packages"].get(name) != version:
            raise ValueError(f"resident dependency version mismatch: {name}")
    if runtime["pi"] != "0.84.2":
        raise ValueError("resident Pi version changed")
    cleanup = json.loads((directory / "container.json").read_text())
    if (
        cleanup.get("candidate_sha") != expected
        or cleanup.get("shard") != shard
        or cleanup.get("network") != "none"
        or cleanup.get("mounts") != []
        or cleanup.get("removed") is not True
        or cleanup.get("image_removed") is not True
    ):
        raise ValueError(
            "resident container isolation or cleanup evidence is incomplete"
        )
    return report


def verify(directory: Path, manifest: dict, expected: str) -> dict:
    """Require every planned shard from the same cleaned-up image build."""
    build = json.loads((directory / "image/image.json").read_text())
    if build.get("candidate_sha") != expected or build.get("image_removed") is not True:
        raise ValueError("resident image build identity or cleanup is incomplete")
    reports = {}
    for path in directory.glob("*/result.json"):
        shard = json.loads(path.read_text())["shard"]
        if shard in reports or shard not in shards(manifest):
            raise ValueError("duplicate or unknown resident shard")
        reports[shard] = path.parent
    if set(reports) != set(shards(manifest)):
        raise ValueError("missing resident shard")
    completed = []
    for shard, path in reports.items():
        report = verify_shard(path, manifest, expected, shard)
        container = json.loads((path / "container.json").read_text())
        if container.get("image") != build["image"]:
            raise ValueError("resident shards used different image builds")
        completed.extend(report["suites"])
    return {"suites": completed}
