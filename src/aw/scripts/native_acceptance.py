"""Run pinned native suites against one verified build with explicit credential boundaries."""

import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True

from check import AW, sha
from check_native import load, plan
from native_reports import verify_case
from native_cleanup import own_children, reap_children, remove_containers, wait_child


def redact_logs(directory: Path, secret: str) -> None:
    """Keep the explicitly supplied account token out of retained process logs."""
    if secret:
        for path in directory.rglob("*.log"):
            text = path.read_text(errors="replace")
            if secret in text:
                path.write_text(text.replace(secret, "[REDACTED]"))


def verify_bundle(directory: Path, expected: str) -> None:
    """Bind every executable to the package build and candidate commit."""
    if (directory / "source-sha.txt").read_text().strip() != sha(expected):
        raise ValueError("native binary bundle belongs to a different commit")
    required = {
        "aw",
        "aw-provider-tokenless",
        "aw-provider-sec-core",
        "tokenless",
        "rtk",
        "policy",
        "agent-sec-cli",
        "agent-sec-daemon",
    }
    entries = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        if name not in required or name in entries:
            raise ValueError(f"unexpected or duplicate native binary: {name}")
        path = directory / name
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"native binary checksum mismatch: {name}")
        if not os.access(path, os.X_OK):
            raise ValueError(f"native binary is not executable: {name}")
        entries[name] = digest
    if set(entries) != required:
        raise ValueError("native binary bundle is incomplete")


class Commands:
    """Record exact owned processes and stop their groups on timeout/interruption."""

    def __init__(self, evidence: Path, environment: dict[str, str]) -> None:
        self.evidence = evidence
        self.environment = environment
        self.number = 0

    def run(
        self, argv: list[str], cwd: Path, timeout: float = 600, *, containers: bool = False
    ) -> str:
        own_children()
        self.number += 1
        owner = uuid.uuid4().hex if containers else ""
        environment = {**self.environment, "AW_NATIVE_OWNER": owner}
        log = self.evidence / f"command-{self.number:02}.log"
        print("+ " + " ".join(argv), flush=True)
        with log.open("w") as stream, subprocess.Popen(
            argv,
            cwd=cwd,
            env=environment,
            stdout=stream,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        ) as child:
            try:
                record = {
                    "command": argv,
                    "cwd": str(cwd),
                    "pid": child.pid,
                    "pgid": child.pid,
                    "ports": "owned fixture loopback only",
                    "log": str(log),
                    "timeout_seconds": timeout,
                    "stop": f"kill -INT -- -{child.pid}",
                    "container_owner": owner,
                }
                with (self.evidence / "processes.jsonl").open("a") as ledger:
                    ledger.write(json.dumps(record) + "\n")
                code = wait_child(child, timeout)
            except BaseException:
                try:
                    # Let running fixtures unwind and reap their own process groups.
                    os.killpg(child.pid, signal.SIGINT)
                except ProcessLookupError:
                    pass
                try:
                    wait_child(child, min(timeout, 15))
                except subprocess.TimeoutExpired:
                    pass
                raise
            finally:
                # This guard also covers ledger open/write/flush failures after spawn.
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                cleanup = {"descendants": [], "containers": [], "passed": False}
                try:
                    try:
                        child.wait(timeout=5)
                        cleanup["descendants"] = reap_children()
                    finally:
                        if owner:
                            cleanup["containers"] = remove_containers(owner, environment)
                    cleanup["passed"] = True
                except Exception as error:
                    cleanup["error"] = f"{type(error).__name__}: {error}"
                    raise
                finally:
                    (self.evidence / f"cleanup-{self.number:02}.json").write_text(
                        json.dumps(cleanup, indent=2) + "\n"
                    )
        text = log.read_text(errors="replace")
        token = self.environment.get("QODER_PERSONAL_ACCESS_TOKEN")
        if token:
            text = text.replace(token, "[REDACTED]")
        if code:
            print(text[-16000:], file=sys.stderr)
            raise subprocess.CalledProcessError(code, argv)
        return text


def setup(row: dict, work: Path, commands: Commands) -> dict[str, str]:
    """Keep each framework's dependencies in its own disposable environment."""
    source, npm = work / "source", work / "npm"
    python = work / "venv/bin/python"
    if "source" in row:
        source.mkdir()
        commands.run(["git", "init", "-q"], source)
        commands.run(["git", "remote", "add", "origin", row["source"]["url"]], source)
        commands.run(["git", "fetch", "--depth=1", "origin", row["source"]["revision"]], source)
        commands.run(["git", "checkout", "--detach", "FETCH_HEAD"], source)
        if commands.run(["git", "rev-parse", "HEAD"], source).strip() != row["source"]["revision"]:
            raise ValueError("native source checkout does not match its pin")
        commands.run(
            [
                "uv",
                "sync",
                "--locked",
                "--no-dev",
                "--link-mode",
                "copy",
                "--python",
                sys.executable,
            ],
            source,
        )
        # Own the copies before tightening permissions; never chmod shared cache hardlinks.
        for directory, _children, files in os.walk(source / ".venv"):
            path = Path(directory)
            path.chmod(path.stat().st_mode & ~0o022)
            for name in files:
                path = Path(directory) / name
                if not path.is_symlink():
                    path.chmod(path.stat().st_mode & ~0o022)
        python = source / ".venv/bin/python"
    else:
        commands.run(["uv", "venv", "--python", sys.executable, str(python.parent.parent)], AW)
        if row.get("requirements"):
            commands.run(
                ["uv", "pip", "install", "--python", str(python), *row["requirements"]],
                AW,
            )
    versions = commands.run(
        [
            str(python),
            "-c",
            (
                "import importlib.metadata as m,json; "
                "print(json.dumps({d.metadata['Name']:d.version for d in m.distributions()},sort_keys=True))"
            ),
        ],
        AW,
    )
    installed = {name.lower(): value for name, value in json.loads(versions).items()}
    for name, expected in row["versions"].items():
        if installed.get(name.lower()) != expected:
            raise ValueError(f"{name}: native dependency does not match {expected}")
    (commands.evidence / "python-versions.json").write_text(versions)
    npm.mkdir()
    if "npm_fixture" in row:
        for name in ("package.json", "package-lock.json"):
            shutil.copyfile(AW / row["npm_fixture"] / name, npm / name)
        commands.run(
            [
                "npm",
                "ci",
                "--prefix",
                str(npm),
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
            ],
            AW,
        )
    elif "npm_packages" in row:
        commands.run(
            [
                "npm",
                "install",
                "--prefix",
                str(npm),
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                *row["npm_packages"],
            ],
            AW,
        )
    if (npm / "package-lock.json").exists():
        shutil.copyfile(npm / "package-lock.json", commands.evidence / "npm-lock.json")
    values = {
        "python": str(python),
        "source": str(source),
        "npm": str(npm),
        "aw_root": str(AW),
        "qwenpaw": str(python.parent / "qwenpaw"),
    }
    if "qoder_cli" in row:
        commands.run(
            [
                sys.executable,
                str(AW / "scripts/native_qoder.py"),
                "--inventory",
                str(AW / "tests/native-matrix.json"),
                "--destination",
                str(work / "qoder"),
                "--report",
                str(commands.evidence / "qoder-version.json"),
            ],
            AW,
        )
        values["qoder"] = str(work / "qoder/qodercli")
    if "container" in row:
        commands.run(["docker", "pull", row["container"]], AW, 180)
    return values


def hermes_path(work: Path, python: str, binaries: Path, environment: dict[str, str]) -> str:
    """Keep native executable discovery out of shared, writable CI tool caches."""
    directory = work / "bin"
    directory.mkdir(mode=0o700)
    uv = shutil.which("uv", path=environment["PATH"])
    if uv is None:
        raise FileNotFoundError("native Hermes acceptance requires the pinned uv executable")
    shutil.copyfile(uv, directory / "uv")
    (directory / "uv").chmod(0o700)
    return os.pathsep.join(
        [
            str(directory),
            str(Path(python).parent),
            str(Path(sys.executable).parent),
            str(binaries),
            "/usr/bin",
            "/bin",
        ]
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--bin", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    plan(AW, args.candidate_sha)
    row = load(AW)["agents"][args.agent]
    args.bin = args.bin.resolve()
    args.output = args.output.resolve()
    verify_bundle(args.bin, args.candidate_sha)
    args.output.mkdir(parents=True, exist_ok=False)
    result = {
        "agent": args.agent,
        "candidate_sha": args.candidate_sha,
        "owner": row["owner"],
        "boundary": row["boundary"],
        "runtime": row,
        "status": "failed",
        "cases": [],
        "python": platform.python_version(),
    }
    # Native fixtures resolve paths before opening UDS sockets (Linux limit: 107 bytes).
    # Keep one owned short runtime directory; retain only evidence under target/.
    with tempfile.TemporaryDirectory(prefix="awn-") as temporary:
        work = Path(temporary)
        output = work / "fixtures"
        output.mkdir()
        (work / "tmp").mkdir()
        environment = {
            **os.environ,
            "PYTHONDONTWRITEBYTECODE": "1",
            "TMPDIR": str(work / "tmp"),
            "AW_TEST_OUTPUT": str(output),
            "TOKENLESS_STATS_ENABLED": "0",
            "TOKENLESS_SLS_ENABLED": "0",
            "PATH": str(args.bin) + os.pathsep + os.environ["PATH"],
        }
        # Dependency installation and unrelated native jobs do not receive the PAT.
        token = environment.pop("QODER_PERSONAL_ACCESS_TOKEN", "")
        commands = Commands(args.output, environment)
        try:
            if args.agent == "qoder" and not token:
                raise ValueError("Qoder nightly requires the QODER_PERSONAL_ACCESS_TOKEN Secret")
            if platform.python_version() != row["python"]:
                raise ValueError(f"native Python must match {row['python']}")
            result["node"] = commands.run(["node", "--version"], AW).strip()
            result["uv"] = commands.run(["uv", "--version"], AW).strip()
            values = setup(row, work, commands)
            if args.agent == "qoder":
                environment["QODER_PERSONAL_ACCESS_TOKEN"] = token
            values.update(bin=str(args.bin), output=str(output))
            if args.agent == "hermes":
                # Hosted runners configure Git LFS filters globally; the fixture needs none.
                environment.update(
                    PATH=hermes_path(work, values["python"], args.bin, environment),
                    GIT_CONFIG_GLOBAL="/dev/null",
                    GIT_CONFIG_SYSTEM="/dev/null",
                )
            environment["OPENCLAW_PACKAGE_DIR"] = values["npm"] + "/node_modules/openclaw"
            for case in row["cases"]:
                start = time.monotonic()
                log = commands.run(
                    [arg.format_map(values) for arg in case["argv"]],
                    AW,
                    case["timeout_seconds"],
                    containers=case["name"] == "sec-core",
                )
                verify_case(case, log, work)
                result["cases"].append(
                    {
                        "name": case["name"],
                        "status": "passed",
                        "log": f"command-{commands.number:02}.log",
                        "seconds": round(time.monotonic() - start, 2),
                    }
                )
            result["status"] = "passed"
        finally:
            redact_logs(output, token)
            redact_logs(args.output, token)
            shutil.copytree(output, args.output / "fixtures", dirs_exist_ok=True)
            # Preserve the same relative result paths for aggregation after upload.
            for path in (work / "npm").glob("*.json"):
                if path.name not in {"package.json", "package-lock.json"}:
                    (args.output / "npm").mkdir(exist_ok=True)
                    shutil.copyfile(path, args.output / "npm" / path.name)
            (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"{args.agent}: {len(result['cases'])} native suites passed")


if __name__ == "__main__":

    def interrupted(signum: int, _frame: object) -> None:
        raise InterruptedError(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    # Profiles and launch paths must not inherit a shared runner's writable umask.
    os.umask(0o077)
    main()
