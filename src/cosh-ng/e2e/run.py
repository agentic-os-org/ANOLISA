#!/usr/bin/env python3
"""Run staged cosh-ng acceptance against an installed cosh launcher."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parent
PTY_KINDS = {"pty", "pty_bash", "pty_zsh", "provider", "soak"}


def now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def file_sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest(path: pathlib.Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as stream:
        manifest = json.load(stream)
    validate_manifest(manifest)
    return manifest


def load_result_schema(path: pathlib.Path = ROOT / "result.schema.json") -> dict[str, Any]:
    with path.open(encoding="utf-8") as stream:
        schema = json.load(stream)
    if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
        raise ValueError("result schema must use JSON Schema draft 2020-12")
    return schema


def validate_report(report: dict[str, Any], schema: dict[str, Any]) -> None:
    required = set(schema["required"])
    if set(report) != required:
        raise ValueError(f"result keys do not match schema: {sorted(set(report) ^ required)}")
    if report["profile"] not in schema["properties"]["profile"]["enum"]:
        raise ValueError(f"invalid result profile: {report['profile']}")
    case_schema = schema["properties"]["cases"]["items"]
    statuses = set(case_schema["properties"]["status"]["enum"])
    case_required = set(case_schema["required"])
    case_properties = set(case_schema["properties"])
    for case in report["cases"]:
        invalid_keys = not case_required.issubset(case) or not set(case).issubset(case_properties)
        if invalid_keys or case["status"] not in statuses:
            raise ValueError(f"case result does not match schema: {case!r}")
    cleanup_statuses = set(schema["properties"]["cleanup"]["properties"]["status"]["enum"])
    if report["cleanup"].get("status") not in cleanup_statuses:
        raise ValueError(f"invalid cleanup result: {report['cleanup']!r}")


def validate_manifest(manifest: dict[str, Any]) -> None:
    if manifest.get("schema_version") != 1:
        raise ValueError("manifest schema_version must be 1")
    profiles, cases = manifest.get("profiles"), manifest.get("cases")
    if not isinstance(profiles, dict) or not profiles:
        raise ValueError("manifest profiles must be a non-empty object")
    if not isinstance(cases, list) or not cases:
        raise ValueError("manifest cases must be a non-empty array")
    seen: set[str] = set()
    required = {"id", "name", "profiles", "kind", "requires", "timeout_seconds", "purpose"}
    for case in cases:
        if not isinstance(case, dict) or not required.issubset(case):
            raise ValueError(f"case is missing required fields: {case!r}")
        case_id = case["id"]
        if not isinstance(case_id, str) or not case_id.startswith("E2E-") or case_id in seen:
            raise ValueError(f"invalid or duplicate case id: {case_id!r}")
        seen.add(case_id)
        unknown = set(case["profiles"]) - set(profiles)
        if unknown:
            raise ValueError(f"{case_id} references unknown profiles: {sorted(unknown)}")


def missing_requirements(
    case: dict[str, Any], cosh_bin: pathlib.Path | None = None
) -> list[str]:
    missing = []
    if cosh_bin is not None and (
        not cosh_bin.is_file() or not os.access(cosh_bin, os.X_OK)
    ):
        missing.append(f"executable:{cosh_bin}")
    if case["kind"] in PTY_KINDS and shutil.which("shell-use") is None:
        missing.append("shell-use")
    for item in case["requires"]:
        if item.startswith("COSH_E2E_"):
            if not os.environ.get(item):
                missing.append(item)
        elif shutil.which(item) is None:
            missing.append(item)
    return missing


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default=str(ROOT / "manifest.json"))
    parser.add_argument("--profile", choices=("local", "g2", "g3", "g4", "g5"), default="local")
    parser.add_argument("--case")
    parser.add_argument("--cosh-bin", default=os.environ.get("COSH_E2E_BIN", "/usr/bin/cosh"))
    parser.add_argument("--artifact-dir", default=f"e2e-results/{uuid.uuid4().hex}")
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--cleanup-only", action="store_true")
    parser.add_argument("--resume", type=pathlib.Path)
    parser.add_argument("--soak-seconds", type=int)
    return parser.parse_args(argv)


def select_cases(manifest: dict[str, Any], args: argparse.Namespace) -> list[dict[str, Any]]:
    cases = [case for case in manifest["cases"] if args.profile in case["profiles"]]
    if args.case:
        cases = [case for case in cases if case["id"] == args.case]
        if not cases:
            raise ValueError(f"case {args.case} is not part of profile {args.profile}")
    if args.resume:
        with args.resume.open(encoding="utf-8") as stream:
            previous = json.load(stream)
        retry_ids = {item["id"] for item in previous.get("cases", []) if item.get("status") != "PASS"}
        cases = [case for case in cases if case["id"] in retry_ids]
    return cases


class Runner:
    def __init__(self, args: argparse.Namespace, manifest: dict[str, Any]) -> None:
        self.args, self.manifest = args, manifest
        self.cosh = pathlib.Path(args.cosh_bin)
        self.bundle = pathlib.Path(args.artifact_dir).resolve()
        self.bundle.mkdir(parents=True, exist_ok=True)
        self.state_path = self.bundle / "cleanup-state.json"
        if args.cleanup_only:
            if not self.state_path.is_file():
                raise ValueError(f"cleanup state does not exist: {self.state_path}")
            state = json.loads(self.state_path.read_text(encoding="utf-8"))
            self.home = pathlib.Path(state["home"])
            self.sessions = list(state.get("sessions", []))
        else:
            self.home = pathlib.Path(tempfile.mkdtemp(prefix="cosh-e2e-home-"))
            self.sessions: list[str] = []
        self.cleanup_actions: list[str] = []
        self.persist_cleanup_state()

    def persist_cleanup_state(self) -> None:
        self.state_path.write_text(
            json.dumps({"home": str(self.home), "sessions": self.sessions}, indent=2) + "\n",
            encoding="utf-8",
        )

    def register_session(self, session: str) -> None:
        self.sessions.append(session)
        self.persist_cleanup_state()

    def command(self, argv: list[str], timeout: int, evidence: pathlib.Path) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["HOME"] = str(self.home)
        completed = subprocess.run(argv, check=False, capture_output=True, text=True, timeout=timeout, env=environment)
        evidence.write_text(
            f"argv={json.dumps(argv)}\nexit_code={completed.returncode}\nstdout:\n{completed.stdout}\nstderr:\n{completed.stderr}",
            encoding="utf-8",
        )
        return completed

    def shell_use(self, session: str, command: list[str], timeout: int = 30) -> subprocess.CompletedProcess[str]:
        return self.command(
            ["shell-use", "--session", session, *command], timeout,
            self.bundle / f"{session}-{command[0]}.log",
        )

    def shell_use_environment(self) -> dict[str, str]:
        environment = os.environ.copy()
        environment["HOME"] = str(self.home)
        return environment

    def pty(self, case: dict[str, Any], shell: str | None = None) -> tuple[str, str, list[str]]:
        marker = f"COSH_E2E_{case['id'].replace('-', '_')}"
        session = f"cosh-e2e-{case['id'].lower()}-{uuid.uuid4().hex[:8]}"
        self.register_session(session)
        launch = ["run", str(self.cosh)] + (["--shell", shell] if shell else [])
        results = [self.shell_use(session, launch, case["timeout_seconds"])]
        results.append(self.shell_use(session, ["submit", f"printf '{marker}\\n'"]))
        results.append(self.shell_use(session, ["wait", "text", marker, "--full", "--timeout", str(case["timeout_seconds"] * 1000)], case["timeout_seconds"] + 5))
        results.append(self.shell_use(session, ["expect", "text", marker, "--full", "--no-strict"]))
        self.shell_use(session, ["submit", "exit"])
        results.append(self.shell_use(session, ["wait", "exit", "--timeout", "30000"], 35))
        cast = self.bundle / f"{session}.cast"
        recording = subprocess.run(
            ["shell-use", "--session", session, "get-recording"], check=False,
            capture_output=True, text=True, env=self.shell_use_environment(),
        )
        cast.write_text(recording.stdout, encoding="utf-8")
        passed = all(result.returncode == 0 for result in results)
        return ("PASS" if passed else "FAIL", "" if passed else "PTY marker or clean exit assertion failed", [cast.name])

    def ssh(self, case: dict[str, Any]) -> tuple[str, str, list[str]]:
        marker, path = "COSH_E2E_SSH", self.bundle / f"{case['id']}.log"
        # No explicit cosh invocation: the remote login shell itself expands
        # $SHELL, so a bash login shell fails the assertion below.
        argv = ["ssh", "-tt", "-i", os.environ["COSH_E2E_SSH_KEY"], "-o", f"UserKnownHostsFile={os.environ['COSH_E2E_KNOWN_HOSTS']}", "-o", "BatchMode=yes", os.environ["COSH_E2E_SSH_TARGET"], f"printf '%s\\n' \"{marker}:$SHELL\""]
        completed = self.command(argv, case["timeout_seconds"], path)
        shell_line = next((line for line in completed.stdout.splitlines() if line.startswith(f"{marker}:")), "")
        passed = completed.returncode == 0 and shell_line.endswith("/cosh")
        return ("PASS" if passed else "FAIL", "" if passed else "SSH login-shell assertion failed", [path.name])

    def sudo(self, case: dict[str, Any]) -> tuple[str, str, list[str]]:
        marker, path = "COSH_E2E_SUDO", self.bundle / f"{case['id']}.log"
        argv = ["sudo", "-n", "-u", os.environ["COSH_E2E_SUDO_USER"], str(self.cosh), "-c", f"printf '{marker}\\n'"]
        completed = self.command(argv, case["timeout_seconds"], path)
        invalidated = subprocess.run(["sudo", "-K"], check=False).returncode == 0
        self.cleanup_actions.append("sudo -K")
        passed = completed.returncode == 0 and marker in completed.stdout and invalidated
        return ("PASS" if passed else "FAIL", "" if passed else "sudo execution or credential cleanup failed", [path.name])

    def provider(self, case: dict[str, Any]) -> tuple[str, str, list[str]]:
        prompt = os.environ.get("COSH_E2E_PROVIDER_PROMPT", "Reply with COSH_E2E_PROVIDER_OK")
        expected = os.environ.get("COSH_E2E_PROVIDER_EXPECT", "COSH_E2E_PROVIDER_OK")
        session = f"cosh-e2e-provider-{uuid.uuid4().hex[:8]}"
        self.register_session(session)
        results = [self.shell_use(session, ["run", str(self.cosh)], case["timeout_seconds"])]
        results.append(self.shell_use(session, ["submit", prompt]))
        results.append(self.shell_use(session, ["wait", "text", expected, "--full", "--timeout", str(case["timeout_seconds"] * 1000)], case["timeout_seconds"] + 5))
        self.shell_use(session, ["submit", "exit"])
        self.shell_use(session, ["wait", "exit", "--timeout", "30000"], 35)
        passed = all(result.returncode == 0 for result in results)
        return ("PASS" if passed else "FAIL", "" if passed else "deterministic provider response was not observed", [])

    def soak(self, case: dict[str, Any]) -> tuple[str, str, list[str]]:
        profile = self.manifest["profiles"][self.args.profile]
        duration = self.args.soak_seconds if self.args.soak_seconds is not None else profile["duration_seconds"]
        deadline, cycles, failures, pty_cycles, latencies = time.monotonic() + duration, 0, 0, 0, []
        before = self.process_snapshot()
        while cycles < profile["minimum_cycles"] or time.monotonic() < deadline:
            started = time.monotonic()
            if cycles % 20 == 0:
                # Decouple per-PTY-op timeout from the multi-hour soak budget so
                # one stuck PTY operation cannot consume the whole case.
                status, _, _ = self.pty({**case, "timeout_seconds": 300})
                failures += int(status != "PASS")
                pty_cycles += 1
            else:
                completed = self.command([str(self.cosh), "-c", "printf 'COSH_E2E_SOAK\\n'"], 30, self.bundle / f"{case['id']}-cycle-{cycles}.log")
                failures += int(completed.returncode != 0 or completed.stdout.strip() != "COSH_E2E_SOAK")
            latencies.append(time.monotonic() - started)
            cycles += 1
        after = self.process_snapshot()
        residual_growth = any(after[key] > before[key] for key in ("processes", "rss_kib", "fds"))
        ordered = sorted(latencies)
        p95_index = max(0, int(len(ordered) * 0.95) - 1)
        metrics = {
            "cycles": cycles,
            "pty_cycles": pty_cycles,
            "failures": failures,
            "max_latency_seconds": max(latencies, default=0.0),
            "p95_latency_seconds": ordered[p95_index] if ordered else 0.0,
            "processes_before": before,
            "processes_after": after,
            "residual_growth": residual_growth,
        }
        path = self.bundle / f"{case['id']}-metrics.json"
        path.write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")
        passed = failures == 0 and cycles >= profile["minimum_cycles"] and not residual_growth
        reason = "" if passed else "soak had failed cycles or residual process/resource growth"
        return ("PASS" if passed else "FAIL", reason, [path.name])

    def process_snapshot(self) -> dict[str, int]:
        completed = subprocess.run(
            ["ps", "-axo", "pid=,rss=,command="], check=False, capture_output=True, text=True
        )
        # The installed launcher execs into cosh-shell, which drives cosh-core,
        # so leaked processes never keep the launcher path in their command
        # line. Match executable basenames instead of the launcher text.
        tracked = {self.cosh.name, "cosh-shell", "cosh-core"}
        processes = rss_kib = fds = 0
        for line in completed.stdout.splitlines():
            fields = line.strip().split(maxsplit=2)
            if len(fields) != 3:
                continue
            argv0 = fields[2].split(maxsplit=1)[0]
            if pathlib.PurePath(argv0).name not in tracked:
                continue
            try:
                pid, rss = int(fields[0]), int(fields[1])
            except ValueError:
                continue
            processes += 1
            rss_kib += rss
            fd_dir = pathlib.Path("/proc") / str(pid) / "fd"
            if fd_dir.is_dir():
                try:
                    fds += sum(1 for _ in fd_dir.iterdir())
                except OSError:
                    pass
        return {"processes": processes, "rss_kib": rss_kib, "fds": fds}

    def run_case(self, case: dict[str, Any]) -> dict[str, Any]:
        started = now()
        missing = missing_requirements(case, self.cosh)
        if missing:
            return self.result(case, "BLOCKED", 0, started, f"missing requirements: {', '.join(missing)}")
        try:
            kind = case["kind"]
            if kind == "non_interactive":
                marker, path = "COSH_E2E_NON_INTERACTIVE", self.bundle / f"{case['id']}.log"
                completed = self.command([str(self.cosh), "-c", f"printf '{marker}\\n'; exit 23"], case["timeout_seconds"], path)
                passed = completed.returncode == 23 and completed.stdout.strip() == marker
                outcome = ("PASS" if passed else "FAIL", "" if passed else "installed launcher did not preserve output and exit 23", [path.name])
            elif kind in {"pty", "pty_bash", "pty_zsh"}:
                outcome = self.pty(case, {"pty_bash": "bash", "pty_zsh": "zsh"}.get(kind))
            else:
                outcome = getattr(self, kind)(case)
            return self.result(case, outcome[0], 1, started, outcome[1], outcome[2])
        except (OSError, subprocess.TimeoutExpired) as error:
            return self.result(case, "FAIL", 1, started, str(error))

    @staticmethod
    def result(case: dict[str, Any], status: str, attempts: int, started: str, reason: str, evidence: list[str] | None = None) -> dict[str, Any]:
        return {"id": case["id"], "status": status, "attempts": attempts, "started_at": started, "finished_at": now(), "evidence": evidence or [], "reason": reason}

    def cleanup(self) -> dict[str, Any]:
        failed = False
        for session in self.sessions:
            result = subprocess.run(
                ["shell-use", "--session", session, "close"], check=False,
                capture_output=True, text=True, env=self.shell_use_environment(),
            )
            failed |= result.returncode not in (0, 3)
            self.cleanup_actions.append(f"close shell-use session {session}")
        shutil.rmtree(self.home, ignore_errors=True)
        self.cleanup_actions.append(f"remove isolated HOME {self.home}")
        status = "FAIL" if failed or self.home.exists() else "PASS"
        self.state_path.write_text(
            json.dumps({"home": str(self.home), "sessions": self.sessions, "status": status}, indent=2) + "\n",
            encoding="utf-8",
        )
        return {"status": status, "actions": self.cleanup_actions}


def git_sha() -> str:
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT.parent, check=False, capture_output=True, text=True)
    return result.stdout.strip() or "unknown"


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    manifest = load_manifest(pathlib.Path(args.manifest))
    cases = select_cases(manifest, args)
    if args.plan:
        cosh_bin = pathlib.Path(args.cosh_bin)
        print(
            json.dumps(
                {
                    "profile": args.profile,
                    "cosh_bin": args.cosh_bin,
                    "cases": [
                        {
                            "id": case["id"],
                            "missing": missing_requirements(case, cosh_bin),
                        }
                        for case in cases
                    ],
                },
                indent=2,
            )
        )
        return 0
    try:
        runner = Runner(args, manifest)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(str(error), file=sys.stderr)
        return 2
    if args.cleanup_only:
        print(json.dumps(runner.cleanup(), indent=2))
        return 0
    if not runner.cosh.is_file() or not os.access(runner.cosh, os.X_OK):
        print(f"installed cosh launcher is not executable: {runner.cosh}", file=sys.stderr)
        runner.cleanup()
        return 2
    if shutil.which("shell-use") is None and any(case["kind"] in PTY_KINDS for case in cases):
        print("shell-use is required for real PTY cases", file=sys.stderr)
        runner.cleanup()
        return 2
    started = now()
    results = [runner.run_case(case) for case in cases]
    if args.resume:
        previous = json.loads(args.resume.read_text(encoding="utf-8"))
        previous_by_id = {item["id"]: item for item in previous.get("cases", [])}
        for result in results:
            prior = previous_by_id.get(result["id"])
            if prior and prior.get("status") != "PASS":
                result["attempts"] += int(prior.get("attempts", 0))
                if result["status"] == "PASS":
                    result["status"] = "FLAKY"
                    result["reason"] = "passed only during explicit resume after a prior failure"
    cleanup = runner.cleanup()
    if cleanup["status"] == "FAIL":
        for result in results:
            if result["status"] == "PASS":
                result.update(status="FAIL", reason="test cleanup failed")
    report = {"schema_version": 1, "run_id": runner.bundle.name, "profile": args.profile, "started_at": started, "finished_at": now(), "artifact": {"path": str(runner.cosh), "sha256": file_sha256(runner.cosh), "git_sha": git_sha()}, "environment": {"platform": platform.platform(), "python": platform.python_version()}, "cases": results, "cleanup": cleanup}
    validate_report(report, load_result_schema())
    path = runner.bundle / "result.json"
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(path)
    return int(cleanup["status"] == "FAIL" or any(result["status"] in {"FAIL", "BLOCKED", "FLAKY"} for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
