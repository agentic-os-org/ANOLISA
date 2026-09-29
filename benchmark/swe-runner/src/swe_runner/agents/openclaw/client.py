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
                timeout=timeout,
                encoding="utf-8",
                errors="replace",
            )
        except subprocess.TimeoutExpired:
            raise AgentTimeoutError(f"OpenClaw local agent timed out after {timeout}s") from None
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
