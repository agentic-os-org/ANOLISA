# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""OpenClaw local CLI client."""

from __future__ import annotations

import subprocess
import time
from dataclasses import dataclass

from swe_runner.agents import AgentNotFoundError, AgentTimeoutError
from swe_runner.common.commands import run_command

# The OpenClaw CLI enforces its own ``--timeout`` budget and exits gracefully
# when the budget is exhausted. The subprocess watchdog must be strictly
# larger than that budget: it starts counting at process spawn while the CLI
# starts counting later (after interpreter startup and session loading), so an
# equal-value watchdog always fires first and discards the CLI's structured
# timeout output. The grace margin lets a well-behaved CLI finish its own
# budget and report its result; the watchdog only catches a CLI that ignores
# its budget or hangs outright.
_TIMEOUT_GRACE_SECONDS = 60


@dataclass(frozen=True)
class OpenClawRunOutcome:
    raw_output: str
    duration_seconds: float
    returncode: int
    error: str | None = None


class OpenClawClient:
    """Run one OpenClaw embedded-agent turn through ``openclaw agent --local``."""

    def __init__(
        self,
        *,
        profile: str,
        agent_id: str,
        cli_path: str = "openclaw",
    ) -> None:
        self._profile = profile
        self._agent_id = agent_id
        self._cli_path = cli_path

    def run_prompt(
        self,
        prompt: str,
        *,
        session_id: str,
        timeout: int,
        max_steps: int,
    ) -> OpenClawRunOutcome:
        del max_steps
        cmd = [
            self._cli_path,
            "--profile",
            self._profile,
            "agent",
            "--local",
            "--json",
            "--agent",
            self._agent_id,
            "--session-id",
            session_id,
            "--message",
            prompt,
            "--timeout",
            str(timeout),
        ]

        start_time = time.monotonic()
        try:
            result = run_command(
                cmd,
                # Watchdog = CLI budget + grace so the CLI's own timeout handling
                # wins the race and its output is preserved (see constant above).
                timeout=timeout + _TIMEOUT_GRACE_SECONDS,
                encoding="utf-8",
                errors="replace",
            )
        except subprocess.TimeoutExpired as exc:
            raise AgentTimeoutError(
                f"OpenClaw local agent timed out after {timeout}s"
                f" (+{_TIMEOUT_GRACE_SECONDS}s watchdog grace); partial output: {_partial_output(exc)}"
            ) from None
        except FileNotFoundError:
            raise AgentNotFoundError(f"'{self._cli_path}' not found in PATH") from None

        duration = round(time.monotonic() - start_time, 2)
        combined_output = result.output
        error = combined_output if result.returncode != 0 else None
        return OpenClawRunOutcome(
            raw_output=combined_output,
            duration_seconds=duration,
            returncode=result.returncode,
            error=error,
        )


def _partial_output(exc: subprocess.TimeoutExpired) -> str:
    """Return the output captured before the watchdog killed the CLI."""
    parts: list[str] = []
    for stream_name in ("stdout", "stderr"):
        captured = getattr(exc, stream_name, None)
        if isinstance(captured, bytes):
            captured = captured.decode("utf-8", errors="replace")
        if isinstance(captured, str) and captured.strip():
            parts.append(captured.strip())
    return "\n".join(parts) or "<none captured>"
