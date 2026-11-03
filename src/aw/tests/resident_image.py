"""Execute the existing native resident inventory inside the isolated image."""

import importlib.metadata
import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from native_acceptance import Commands, verify_bundle
from native_reports import verify_case
from resident_contract import BINARIES, shards, suites


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shard", required=True)
    args = parser.parse_args()
    os.umask(0o077)
    root = Path(__file__).resolve().parents[1]
    binaries, evidence, work = Path("/opt/aw/bin"), Path("/evidence"), Path("/work")
    expected = (root / "source-sha.txt").read_text().strip()
    verify_bundle(binaries, expected, BINARIES)
    manifest = json.loads((root / "tests/native-matrix.json").read_text())
    selected = suites(manifest)
    plan = shards(manifest)[args.shard]
    from claude_agent_sdk._internal.transport.subprocess_cli import (
        SubprocessCLITransport,
    )
    from claude_agent_sdk import ClaudeAgentOptions

    cli = SubprocessCLITransport(prompt="", options=ClaudeAgentOptions())._find_cli()
    runtime = {
        "python": platform.python_version(),
        "python_packages": {
            name: importlib.metadata.version(name)
            for name in selected["claude-sdk"]["versions"]
        },
        "node": subprocess.check_output(
            ["node", "--version"], text=True, timeout=10
        ).strip(),
        "rtk": subprocess.check_output(
            [str(binaries / "rtk"), "--version"], text=True, timeout=10
        ).strip(),
        "claude_cli": subprocess.check_output(
            [cli, "--version"], text=True, timeout=15
        ).strip(),
        "pi": json.loads(
            Path(
                "/opt/pi/node_modules/@earendil-works/pi-coding-agent/package.json"
            ).read_text()
        )["version"],
    }
    report = {
        "candidate_sha": expected,
        "shard": args.shard,
        "runtime": runtime,
        "passed": False,
        "suites": [],
    }
    try:
        for agent, row in selected.items():
            if agent != plan["agent"]:
                continue
            agent_work = work / agent
            agent_work.mkdir()
            fixture_output = agent_work / "fixtures"
            fixture_output.mkdir()
            agent_evidence = evidence / agent
            agent_evidence.mkdir()
            environment = {
                **os.environ,
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
                "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
            }
            commands = Commands(agent_evidence, environment)
            values = {
                "python": sys.executable,
                "bin": str(binaries),
                "output": str(fixture_output),
                "npm": "/opt/pi",
            }
            for case in plan["cases"]:
                try:
                    log = commands.run(
                        [arg.format_map(values) for arg in case["argv"]],
                        root,
                        case["timeout_seconds"],
                    )
                    verify_case(case, log, agent_work)
                    report["suites"].append({"agent": agent, "name": case["name"]})
                finally:
                    # Publish only the declared result files; SDK profiles and task data
                    # remain in the disposable container, including on failure.
                    for result in case["results"]:
                        source = agent_work / result["file"]
                        if source.is_file():
                            destination = agent_evidence / result["file"]
                            destination.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(source, destination)
        report["passed"] = True
    finally:
        (evidence / "result.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":

    def interrupted(signum: int, _frame: object) -> None:
        raise InterruptedError(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    main()
