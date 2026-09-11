#!/usr/bin/env python3
"""Run the Linux AW contract gate locally and in GitHub Actions."""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
from pathlib import Path

AW = Path(__file__).resolve().parents[1]
REPO = AW.parents[1]


def run(command: list[str], cwd: Path, timeout: float = 600, capture: bool = False) -> str:
    """Run a bounded command and reap its owned process group on interruption."""
    print("+ " + " ".join(command), flush=True)
    with subprocess.Popen(
        command,
        cwd=cwd,
        text=True,
        start_new_session=True,
        stdout=subprocess.PIPE if capture else None,
    ) as process:
        try:
            output, _ = process.communicate(timeout=timeout)
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                pass
            raise
        finally:
            # A child can outlive its parent or ignore SIGTERM.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command)
        return output or ""


def git(*args: str, repo: Path = REPO) -> str:
    """Keep Git failures visible, including missing comparison objects."""
    return run(["git", *args], repo, timeout=30, capture=True).rstrip("\n")


def sha(value: str) -> str:
    """Accept full Git SHA-1 object IDs only, never revision expressions."""
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError(f"invalid commit SHA: {value!r}")
    return value


def candidate(repo: Path = REPO) -> str:
    """Bind the checkout to the Actions event when running in CI."""
    actual = sha(git("rev-parse", "HEAD", repo=repo))
    if "GITHUB_SHA" in os.environ and sha(os.environ["GITHUB_SHA"]) != actual:
        raise ValueError("checkout HEAD does not match GITHUB_SHA")
    return actual


def output(**values: str) -> None:
    """Publish small, validated results using native Actions outputs."""
    lines = "".join(f"{key}={value}\n" for key, value in values.items())
    print(lines, end="", flush=True)
    if "GITHUB_OUTPUT" in os.environ:
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
            stream.write(lines)


def scope(event_name: str, event: dict, actual: str, repo: Path = REPO) -> bool:
    """Select the complete AW gate for relevant event changes."""
    comparisons = []
    if event_name == "pull_request":
        base = sha(event["pull_request"]["base"]["sha"])
        head = sha(event["pull_request"]["head"]["sha"])
        ancestor = sha(git("merge-base", base, head, repo=repo))
        comparisons = [f"{base}...{head}", f"{ancestor}..{actual}"]
    elif event_name == "push":
        before, after = sha(event["before"]), sha(event["after"])
        if after != actual:
            raise ValueError("push after SHA does not match checkout")
        # Rewritten history may no longer contain the previous push head.
        if before == "0" * 40 or event.get("forced") is True:
            return True
        comparisons = [f"{before}..{after}"]
    elif event_name in ("workflow_dispatch", "merge_group"):
        return True
    else:
        raise ValueError(f"unsupported event: {event_name}")
    changed = set()
    for comparison in comparisons:
        print(f"AW scope comparison: {comparison}", flush=True)
        changed.update(
            git("diff", "--name-only", "-z", "--no-renames", comparison, "--", repo=repo).split(
                "\0"
            )
        )
    return any(
        path.startswith("src/aw/") or path == ".github/workflows/aw-ci.yml" for path in changed
    )


def inventory(text: str) -> set[str]:
    """Parse libtest's list output and reject truncated or unknown formats."""
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


def check_inventory() -> None:
    """Require runnable tests in every contract integration target."""
    for target in ("canonical", "schemas", "contracts", "orchestration"):
        command = ["cargo", "test", "--locked", "--test", target, "--", "--list"]
        tests = inventory(run(command, AW, capture=True))
        ignored = inventory(run([*command, "--ignored"], AW, capture=True))
        if not ignored <= tests or not tests - ignored:
            raise ValueError(f"{target}: no runnable tests or inconsistent ignored inventory")
        print(f"{target}: {len(tests - ignored)} runnable tests", flush=True)


def selftest() -> None:
    """Run the gate's behavior tests, rejecting missing or empty discovery."""
    run(
        [
            sys.executable,
            "-B",
            "-c",
            """
import sys
import unittest
suite = unittest.defaultTestLoader.discover('tests', pattern='test_ci_checks.py')
if suite.countTestCases() == 0:
    raise SystemExit('AW gate self-tests are missing or empty')
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.testsRun == len(result.skipped):
    raise SystemExit('AW gate self-tests were all skipped')
sys.exit(not result.wasSuccessful())
""",
        ],
        AW,
        timeout=120,
    )


def check() -> None:
    """Run the existing contract checks and the gate's own behavior tests."""
    actual = candidate()
    selftest()
    run(["cargo", "fmt", "--all", "--", "--check"], AW)
    run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], AW)
    check_inventory()
    run(["cargo", "test", "--workspace", "--locked"], AW)
    run([sys.executable, "tests/check_canonical.py"], AW, timeout=30)
    run(["cargo", "doc", "--workspace", "--no-deps", "--locked"], AW)
    if candidate() != actual:
        raise ValueError("checkout changed during validation")
    output(tested_sha=actual)


def required(env: dict[str, str]) -> None:
    """Reject incomplete or mismatched job results; allow explicit no-op only."""
    actual = sha(env.get("GITHUB_SHA", ""))
    if env.get("SCOPE_RESULT") != "success" or env.get("CANDIDATE_SHA") != actual:
        raise ValueError("scope failed or candidate SHA does not match the event")
    selected, checks = env.get("SELECTED"), env.get("CHECKS_RESULT")
    if selected == "false" and checks == "skipped":
        print("AW gate not applicable: no AW paths changed")
    elif selected == "true" and checks == "success" and env.get("TESTED_SHA") == actual:
        print(f"AW gate passed for {actual}")
    else:
        raise ValueError("AW checks missing, unsuccessful, or tested a different SHA")


def interrupted(signum: int, _frame: object) -> None:
    """Unwind active command cleanup when Actions terminates the runner."""
    raise InterruptedError(f"received signal {signum}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command", nargs="?", default="check", choices=("scope", "check", "required")
    )
    command = parser.parse_args().command
    signal.signal(signal.SIGTERM, interrupted)
    try:
        if command == "scope":
            actual = candidate()
            selftest()
            event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
            selected = scope(os.environ["GITHUB_EVENT_NAME"], event, actual)
            output(selected=str(selected).lower(), candidate_sha=actual)
        elif command == "check":
            check()
        else:
            required(dict(os.environ))
    except subprocess.CalledProcessError as error:
        print(f"AW gate failed: {error}", file=sys.stderr)
        return error.returncode if error.returncode > 0 else 1
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"AW gate failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
