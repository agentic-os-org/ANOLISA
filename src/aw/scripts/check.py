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

if sys.version_info < (3, 9):
    raise SystemExit("AW CI control jobs require Python 3.9 or newer")

sys.dont_write_bytecode = True

AW = Path(__file__).resolve().parents[1]
REPO = AW.parents[1]
# Also support importlib-based gate tests without loading full-tree modules in scope jobs.
sys.path.insert(0, str(Path(__file__).resolve().parent))


def run(
    command: list[str], cwd: Path, timeout: float = 600, capture: bool = False,
    umask: int = -1,
) -> str:
    """Run a bounded command and reap its owned process group on interruption."""
    permissions = f"umask={umask:04o} " if umask >= 0 else ""
    print("+ " + permissions + " ".join(command), flush=True)
    with subprocess.Popen(
        command,
        cwd=cwd,
        text=True,
        start_new_session=True,
        umask=umask,
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


def tokenless_input(path: str) -> bool:
    """Select real backend contracts only for their source and build inputs."""
    return path.startswith((
        "providers/tokenless/src/", "providers/tokenless/tests/",
        "src/tokenless/crates/tokenless-cli/src/", "src/tokenless/crates/tokenless-protocol/src/",
        "src/tokenless/crates/tokenless-runtime/src/", "src/tokenless/crates/tokenless-stats/src/",
        "src/tokenless/third_party/patches/", "src/aw/crates/aw-provider/src/",
        "src/aw/crates/aw-provider/schemas/", "src/aw/crates/aw-exec/src/",
        "src/aw/crates/aw-package/src/package/", "src/aw/crates/aw-package/src/manifest/",
        "src/aw/crates/aw-package/src/bin/support/",
    )) or path in {
        "providers/tokenless/Cargo.toml", "providers/tokenless/Cargo.lock",
        "providers/tokenless/rust-toolchain.toml", "src/tokenless/rust-toolchain.toml",
        "src/tokenless/Cargo.toml", "src/tokenless/Cargo.lock",
        "src/tokenless/crates/tokenless-cli/Cargo.toml",
        "src/tokenless/crates/tokenless-protocol/Cargo.toml",
        "src/tokenless/crates/tokenless-runtime/Cargo.toml",
        "src/tokenless/crates/tokenless-stats/Cargo.toml",
        "src/tokenless/scripts/setup-rtk.sh", "src/aw/Cargo.toml", "src/aw/Cargo.lock",
        "src/aw/rust-toolchain.toml", "src/aw/crates/aw-provider/Cargo.toml",
        "src/aw/crates/aw-exec/Cargo.toml", "src/aw/crates/aw-package/Cargo.toml",
        "src/aw/crates/aw-package/src/lib.rs", "src/aw/crates/aw-package/src/package.rs",
        "src/aw/crates/aw-package/src/manifest.rs", "src/aw/crates/aw-package/src/bin/aw-build.rs",
        "src/aw/scripts/check_tokenless.py", "src/aw/scripts/check.py",
        "src/aw/tests/test_ci_tokenless.py", "src/aw/tests/test_ci_daily_workflow.py",
        ".github/workflows/aw-ci.yml",
    }


def scope(event_name: str, event: dict, actual: str, repo: Path = REPO,
          *, compatibility: bool = False) -> bool:
    """Select the complete AW gate for relevant event changes."""
    comparisons = []
    if event_name == "pull_request":
        base = sha(event["pull_request"]["base"]["sha"])
        head = sha(event["pull_request"]["head"]["sha"])
        # Base-branch AW updates must not select an unrelated PR.
        comparisons = [f"{base}...{head}"]
    elif event_name == "push":
        before, after = sha(event["before"]), sha(event["after"])
        if after != actual:
            raise ValueError("push after SHA does not match checkout")
        # Rewritten history may no longer contain the previous push head.
        if before == "0" * 40 or event.get("forced") is True:
            return True
        comparisons = [f"{before}..{after}"]
    elif event_name == "merge_group":
        base = sha(event["merge_group"]["base_sha"])
        head = sha(event["merge_group"]["head_sha"])
        if head != actual:
            raise ValueError("merge group head SHA does not match checkout")
        comparisons = [f"{base}..{head}"]
    elif event_name == "workflow_dispatch":
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
        tokenless_input(path) if compatibility else
        (path.startswith(("src/aw/", "providers/")) or tokenless_input(path)
         or path == ".github/workflows/aw-preview.yml")
        for path in changed
    )


def check_inventory(metadata: dict) -> None:
    """Build once, then validate the runnable matrix against real test executables."""
    from check_inventory import check_inventory as check_tests

    check_tests(AW, metadata, run)


def structure(metadata: dict, root: Path) -> None:
    """Load layout checks only after sparse scope selection."""
    from check_layout import structure as check_structure

    check_structure(metadata, root)


def selftest(scope_only: bool = False) -> None:
    """Run the gate's behavior tests, rejecting missing or empty discovery."""
    run(
        [
            sys.executable,
            "-B",
            "-c",
            """
import sys
import unittest
suite = unittest.defaultTestLoader.discover('tests', pattern='test_ci_*.py')
if sys.argv[1] == "scope":
    suite = unittest.defaultTestLoader.loadTestsFromName("test_ci_checks.ScopeTests")
if suite.countTestCases() == 0:
    raise SystemExit('AW gate self-tests are missing or empty')
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.testsRun == len(result.skipped):
    raise SystemExit('AW gate self-tests were all skipped')
sys.exit(not result.wasSuccessful())
""",
            "scope" if scope_only else "all",
        ],
        AW,
        timeout=120,
    )


def package_permissions() -> None:
    """Exercise package readers, transactions and leases under child-local umasks."""
    commands = [
        ["cargo", "test", "--locked", "-p", "aw-package", "--test", "database"],
        ["cargo", "test", "--locked", "-p", "aw-cli", "--test", "inventory"],
        ["cargo", "test", "--locked", "-p", "aw-package", "--lib"],
        ["cargo", "test", "--locked", "-p", "aw-package", "--test", "management"],
        ["cargo", "test", "--locked", "-p", "aw-host", "--test", "host", "package_leases"],
        ["cargo", "test", "--locked", "-p", "aw-service", "--lib", "inspect::package_leases"],
        ["cargo", "test", "--locked", "-p", "aw-exec", "--test", "packages"],
        ["cargo", "test", "--locked", "-p", "aw-cli", "--test", "packages"],
    ]
    for mask in (0o022, 0o002):
        for command in commands:
            run(command, AW, timeout=120, umask=mask)


def check() -> None:
    """Run the existing contract checks and the gate's own behavior tests."""
    actual = candidate()
    selftest()
    run(["cargo", "fmt", "--all", "--", "--check"], AW)
    from check_native import load

    load(AW)
    metadata = json.loads(
        run(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            AW,
            capture=True,
        )
    )
    provider_roots = [REPO / "providers" / name for name in ("agent-sec-core", "tokenless")]
    metadata["provider_workspaces"] = []
    for root in provider_roots:
        run(["cargo", "fmt", "--all", "--", "--check"], root)
        metadata["provider_workspaces"].append(json.loads(run(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            root, capture=True,
        )))
    structure(metadata, AW)
    for root in [AW, *provider_roots]:
        run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], root)
    from check_legacy_cli import prepare_legacy_cli

    os.environ["AW_LEGACY_PACKAGE_CLI"] = str(prepare_legacy_cli(AW, REPO, run))
    check_inventory(metadata)
    for root in [AW, *provider_roots]:
        run(["cargo", "test", "--workspace", "--locked", "--no-fail-fast"], root)
    package_permissions()
    run([sys.executable, "tests/check_canonical.py"], AW, timeout=30)
    for root in [AW, *provider_roots]:
        run(["cargo", "doc", "--workspace", "--no-deps", "--locked"], root)
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
    selected = env.get("TOKENLESS_SELECTED")
    if selected == "true":
        if (env.get("TOKENLESS_RESULT") != "success"
                or env.get("TOKENLESS_TESTED_SHA") != actual):
            raise ValueError("Tokenless compatibility missing, unsuccessful, or stale")
    elif selected != "false" or env.get("TOKENLESS_RESULT") != "skipped":
        raise ValueError("Tokenless compatibility selection/result is invalid")


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
            selftest(scope_only=True)
            event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
            selected = scope(os.environ["GITHUB_EVENT_NAME"], event, actual)
            tokenless = scope(os.environ["GITHUB_EVENT_NAME"], event, actual,
                              compatibility=True)
            output(selected=str(selected).lower(), tokenless=str(tokenless).lower(),
                   candidate_sha=actual)
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
