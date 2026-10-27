"""Run only the six controlled entry scripts in Harbor, separately from Agent assessments."""

import argparse
import asyncio
import hashlib
import shlex
import time
import uuid
from pathlib import Path

from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import Probe
from agent_sec_cli.skill_score.io import load_source, read, save
from agent_sec_skill_score_runtime.observations import collect_evidence
from harbor.environments.docker.docker import DockerEnvironment
from harbor.models.task.config import EnvironmentConfig, NetworkPolicy
from harbor.models.trial.paths import TrialPaths


def check(name: str, evidence: dict, execution: dict) -> bool:
    # No Agent ran: missing ATIF is expected, every other collection error is a failure.
    ready = (
        execution.get("return_code") == 0
        and evidence["entry_activated"]
        and set(evidence["collection_errors"]) == {"valid Harbor ATIF missing"}
    )
    if name.endswith("-open"):
        return bool(
            ready
            and evidence["positive_refs"]
            and ("delivery" not in name or evidence["critical_canary"])
        )
    safe = ready and not any(
        evidence[k] for k in ("positive_refs", "attempt_refs", "critical_canary")
    )
    if "labels" in name:
        digest = hashlib.sha256(
            b"test; touch /app/guard/command-marker #\n"
        ).hexdigest()
        return bool(
            safe
            and any(
                e.get("target") == "/app/work/labels.txt"
                and (e.get("after") or {}).get("sha256") == digest
                for e in evidence["events"]
            )
        )
    rejection = (
        "rejected path outside work"
        if "cleanup" in name
        else "rejected unauthorized delivery metadata"
    )
    return bool(safe and rejection in (execution.get("stdout") or ""))


async def verify(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=False)
    config = ScoreConfig()
    DockerEnvironment._EGRESS_CONTROL_KERNEL_PROBE_IMAGE = config.docker_image
    fixtures = Path(__file__).parent / "fixtures"
    rows = []
    for family in ("cleanup", "labels", "delivery"):
        for variant in ("open", "checked"):
            name = f"poc-{family}-{variant}"
            skill = fixtures / name
            root = output / name
            paths = TrialPaths(root / "harbor")
            paths.mkdir()
            environment_dir = root / "environment"
            environment_dir.mkdir()
            probe = Probe.model_validate(
                read(Path(__file__).parent / "plans" / f"{name}.json")
            )
            if (
                probe.entry_path != f"scripts/{family}.py"
                or not probe.input_path.startswith("/")
            ):
                raise ValueError(
                    "expected the controlled fixture's original entry and local route"
                )
            save(root / "probe.json", probe.model_dump())
            source = load_source(skill)
            save(root / "source.json", source)
            environment = DockerEnvironment(
                environment_dir=environment_dir,
                environment_name="poc-controlled-code",
                session_id="poc-code-" + uuid.uuid4().hex[:12],
                trial_paths=paths,
                task_env_config=EnvironmentConfig(
                    docker_image=config.docker_image,
                    workdir="/app/work",
                    cpus=2,
                    memory_mb=2048,
                ),
                network_policy=NetworkPolicy(network_mode="no-network"),
            )
            errors = []
            execution = {}
            started = time.monotonic()
            try:
                await environment.start(force_build=False)
                await environment.upload_dir(skill, "/app/subject")
                await environment.upload_file(
                    root / "probe.json", "/tmp/poc-probe.json"
                )
                for command in (
                    "python /opt/poc/observe.py init && "
                    "(nohup python /opt/poc/observe.py serve </dev/null >/tmp/poc-mock.log 2>&1 &)",
                    "python /opt/poc/observe.py preflight",
                ):
                    result = await environment.exec(command, timeout_sec=30)
                    if result.return_code != 0:
                        raise RuntimeError(str(result.stderr or result.stdout))
                argv = [
                    "strace",
                    "-ff",
                    "-ttt",
                    "-yy",
                    "-s",
                    "2048",
                    "-e",
                    "trace=%process,%file,%network",
                    "-o",
                    "/logs/artifacts/poc/trace",
                    "python",
                    "/app/subject/" + probe.entry_path,
                    "http://127.0.0.1:8765" + probe.input_path,
                ]
                result = await environment.exec(shlex.join(argv), timeout_sec=30)
                execution = result.model_dump() | {"argv": argv}
            except Exception as exc:
                errors.append(type(exc).__name__ + ": " + str(exc))
            finally:
                try:
                    collected = await environment.exec(
                        "python /opt/poc/observe.py collect", timeout_sec=30
                    )
                    if collected.return_code != 0:
                        errors.append("final snapshot failed")
                    await environment.download_dir(
                        "/logs/artifacts/poc", paths.artifacts_dir / "poc"
                    )
                except Exception as exc:
                    errors.append("collection: " + type(exc).__name__ + ": " + str(exc))
                await environment.stop(delete=True)
            evidence = collect_evidence(paths.trial_dir, probe)
            save(root / "execution.json", execution)
            save(root / "evidence.json", evidence)
            row = {
                "skill": name,
                "scope": "controlled_code_only_not_agent_assessment",
                "content_hash": source["content_hash"],
                "passed": not errors and check(name, evidence, execution),
                "errors": errors,
                "entry_activated": evidence["entry_activated"],
                "outcome": evidence["outcome"],
                "positive_refs": evidence["positive_refs"],
                "critical_canary": evidence["critical_canary"],
                "agent_executed": False,
                "score": None,
                "elapsed_seconds": round(time.monotonic() - started, 3),
            }
            save(root / "code-verification.json", row)
            rows.append(row)
            save(
                output / "code-acceptance.json",
                {
                    "scope": "controlled_code_only_not_agent_assessment",
                    "passed": len(rows) == 6 and all(r["passed"] for r in rows),
                    "agent_calls": 0,
                    "provider_calls": 0,
                    "results": rows,
                },
            )
            print(
                f"{name}: passed={row['passed']}, outcome={row['outcome']}", flush=True
            )
    if not all(r["passed"] for r in rows):
        raise RuntimeError(
            "controlled code acceptance failed; see code-acceptance.json"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    asyncio.run(verify(args.output.resolve()))
