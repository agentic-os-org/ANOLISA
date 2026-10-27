"""Invoke the optional Python 3.12 worker without importing Harbor into the CLI."""

import os
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Probe
from agent_sec_cli.skill_score.evidence.redact import redact_text
from agent_sec_cli.skill_score.io import read, save, scrub_configured_key
from agent_sec_cli.skill_score.provider.bailian import resolve_payg_auth


def worker_python(prefix: Path) -> Path:
    """Resolve the sibling raw runtime; preserve the optional RPM layout."""
    if prefix.name == "cli" and prefix.parent.name == "skill-score":
        return prefix.parent / "runtime/bin/python3.12"
    return Path("/opt/agent-sec/skill-score-runtime/bin/python3.12")


WORKER_PYTHON = worker_python(Path(sys.prefix))


class Runtime:
    def __init__(self, config: ScoreConfig) -> None:
        self.config = config

    def _invoke(
        self, request: dict[str, Any], root: Path, timeout: float
    ) -> dict[str, Any]:
        if not WORKER_PYTHON.is_file():
            raise ContractError(
                "install the assessment-enabled sec-core package and prepare Docker and its images first"
            )
        auth = resolve_payg_auth()
        request["config"] = self.config.model_dump()
        input_path, output_path = (
            root / "worker-input.json",
            root / "worker-output.json",
        )
        save(input_path, request)
        environment = {
            key: value
            for key, value in os.environ.items()
            if key
            in {
                "PATH",
                "HOME",
                "LANG",
                "LC_ALL",
                "SSL_CERT_FILE",
                "SSL_CERT_DIR",
                "DOCKER_HOST",
                "DOCKER_CONTEXT",
                "DOCKER_CONFIG",
                "DOCKER_TLS_VERIFY",
                "DOCKER_CERT_PATH",
            }
        }
        environment.update(
            auth.runtime_env(
                model=self.config.model, small_fast_model=self.config.model
            )
        )
        environment["ANTHROPIC_API_KEY"] = auth.api_key
        # The private interpreter sees only its own site-packages.
        command = [
            str(WORKER_PYTHON),
            "-m",
            "agent_sec_skill_score_runtime.worker",
            str(input_path),
            str(output_path),
        ]
        with (root / "worker.log").open("w") as log:
            process = subprocess.Popen(
                command,
                stdout=log,
                stderr=subprocess.STDOUT,
                env=environment,
                start_new_session=True,
            )
            try:
                process.wait(timeout=timeout + 90)
            except BaseException:
                process.send_signal(signal.SIGTERM)
                try:
                    process.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                raise
            finally:
                log.flush()
                scrub_configured_key(root)
        if not output_path.is_file():
            raise ContractError(
                "Runtime worker did not return evidence: "
                + redact_text((root / "worker.log").read_text()[-1200:])
            )
        response = read(output_path)
        if not isinstance(response, dict):
            raise ContractError("Runtime worker returned an invalid JSON object")
        if process.returncode and not response.get("probe_id"):
            raise ContractError(response.get("error", "Runtime preparation failed"))
        return response

    def environment(self) -> dict[str, Any]:
        with tempfile.TemporaryDirectory(
            prefix="agent-sec-score-preflight-"
        ) as temporary:
            return self._invoke({"operation": "environment"}, Path(temporary), 60)

    def run(
        self, skill: Path, probe: Probe, root: Path, timeout: float
    ) -> dict[str, Any]:
        return self._invoke(
            {
                "operation": "run",
                "skill_path": str(skill),
                "probe": probe.model_dump(),
                "root": str(root),
                "timeout": timeout,
            },
            root,
            timeout,
        )
