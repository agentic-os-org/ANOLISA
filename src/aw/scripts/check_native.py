"""Keep scheduled native acceptance aligned with the registered Agent matrix."""

import argparse
import json
import re
from pathlib import Path

from check import AW, git, output, sha
from native_reports import validate_case


def load(root: Path) -> dict:
    """Reject unaccounted Agents, empty suites and unpinned native runtimes."""
    manifest = json.loads((root / "tests/native-matrix.json").read_text())
    registered = json.loads((root / "tests/regression-matrix.json").read_text())["agents"]
    if manifest.get("version") != 1 or set(manifest["agents"]) != set(registered):
        raise ValueError("native acceptance must account for every registered Agent")
    for agent, row in manifest["agents"].items():
        if not row.get("owner"):
            raise ValueError(f"{agent}: native acceptance needs an owner")
        if row.get("mode") == "manual":
            # This existing runtime needs an account/CLI that CI does not provision.
            if agent != "qoder" or not row.get("reason") or not (root / row["guide"]).is_file():
                raise ValueError(f"{agent}: new native coverage cannot silently become manual")
            continue
        if row.get("mode") != "automated" or not row.get("boundary"):
            raise ValueError(f"{agent}: declare automated coverage and its native boundary")
        if not re.fullmatch(r"3\.\d+\.\d+", row.get("python", "")):
            raise ValueError(f"{agent}: pin the Python runtime")
        if not any(row.get(key) for key in ("versions", "source", "npm_fixture", "npm_packages")):
            raise ValueError(f"{agent}: pin the native runtime")
        for requirement in row.get("requirements", []):
            if "==" not in requirement or any(c in requirement for c in "*<>;"):
                raise ValueError(f"{agent}: use exact Python dependency versions")
        if "source" in row:
            sha(row["source"]["revision"])
        if "npm_fixture" in row:
            for name in ("package.json", "package-lock.json"):
                if not (root / row["npm_fixture"] / name).is_file():
                    raise ValueError(f"{agent}: missing locked npm fixture")
        for package in row.get("npm_packages", []):
            if not re.search(r"@\d+\.\d+\.\d+$", package):
                raise ValueError(f"{agent}: pin the npm package")
        cases = row.get("cases", [])
        if not cases or len({case["name"] for case in cases}) != len(cases):
            raise ValueError(f"{agent}: missing or duplicate native cases")
        for case in cases:
            if not 0 < case["timeout_seconds"] <= 1200:
                raise ValueError(f"{agent}: native cases need bounded deadlines")
            validate_case(case)
            scripts = [arg for arg in case["argv"] if arg.endswith((".py", ".mjs"))]
            if not scripts or any(not (root / script).is_file() for script in scripts):
                raise ValueError(f"{agent}/{case['name']}: native test entrypoint is missing")
    return manifest


def plan(root: Path, expected: str) -> dict:
    """Generate the job matrix from the tested checkout, never a second Agent list."""
    if git("rev-parse", "HEAD", repo=root.parents[1]) != sha(expected):
        raise ValueError("native plan checkout does not match candidate SHA")
    manifest = load(root)
    return {
        "include": [
            {"agent": agent, "python": row["python"]}
            for agent, row in manifest["agents"].items()
            if row["mode"] == "automated"
        ]
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-sha", required=True)
    args = parser.parse_args()
    output(matrix=json.dumps(plan(AW, args.candidate_sha), separators=(",", ":")))


if __name__ == "__main__":
    main()
