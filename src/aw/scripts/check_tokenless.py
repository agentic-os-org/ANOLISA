"""Validate AW's real Tokenless/RTK consumer contracts on the candidate SHA."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal

from check import AW, REPO, candidate, interrupted, output
from native_acceptance import Commands

CASES = {
    "provider": "bundled_cli_compresses_losslessly_without_a_recovery_entry",
    "pretool": "public_cli_pretool_and_posttool_share_the_executed_wrapper",
    "managed_rtk": "real_managed_rtk_rewrites_executes_and_preserves_attributed_after",
    "views": "bundled_cli_statistics_exclude_dry_runs_and_match_selected_candidate_totals",
}


def verify_result(log: str, name: str) -> None:
    """An ignored/renamed test or an empty Cargo selection must fail."""
    if (not re.search(r"^test " + re.escape(name) + r" \.\.\. ok$", log, re.MULTILINE)
            or not re.search(
                r"^test result: ok\. 1 passed; 0 failed; 0 ignored;", log, re.MULTILINE)):
        raise ValueError(f"Tokenless consumer contract missing or skipped: {name}")


def check(directory: Path) -> None:
    expected = candidate()
    directory.mkdir(parents=True, exist_ok=False)
    environment = {
        **os.environ, "TOKENLESS_COMPRESSION_ENABLED": "1",
        "TOKENLESS_STATS_ENABLED": "0", "TOKENLESS_SLS_ENABLED": "0",
        "AW_TOKENLESS_TEST_CLI": str(REPO / "src/tokenless/target/release/tokenless"),
        "AW_TOKENLESS_TEST_RTK": str(AW / "target/release/rtk"),
    }
    commands = Commands(directory, environment)
    report = {"candidate_sha": expected, "passed": False, "cases": []}
    try:
        commands.run(
            ["cargo", "run", "--locked", "--release", "-p", "aw-package", "--bin", "aw-build",
             "--", "--version", "0.1.0-preview.1", "--component", "tokenless",
             "--output", str(directory / "packages")], AW, 2700,
        )
        report["binaries"] = {
            name: hashlib.sha256(Path(environment[key]).read_bytes()).hexdigest()
            for name, key in (("tokenless", "AW_TOKENLESS_TEST_CLI"), ("rtk", "AW_TOKENLESS_TEST_RTK"))
        }
        for target, name in CASES.items():
            log = commands.run(
                ["cargo", "test", "--locked", "--test", target, name, "--", "--exact", "--ignored"],
                REPO / "providers/tokenless", 120,
            )
            verify_result(log, name)
            report["cases"].append(name)
        if candidate() != expected:
            raise ValueError("checkout changed during Tokenless compatibility validation")
        report["passed"] = True
        output(tested_sha=expected)
    finally:
        (directory / "result.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    check(args.output.resolve())
