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

"""
OpenClawExternalAgent - Host-side OpenClaw agent adapter for Harbor.

Architecture:
  OpenClaw (host LLM) <-> OpenClawExternalAgent (router) <-> Harbor Docker container

  1. Harbor calls ``setup()`` to detect the Docker container and validate OpenClaw.
  2. Harbor calls ``run()`` which enters an agent loop:
     a. OpenClaw (host subprocess) generates bash commands via LLM inference.
     b. Router extracts commands from OpenClaw's JSON/text output.
     c. Router executes commands via ``environment.exec()`` inside the container.
     d. Results are fed back to OpenClaw for the next iteration.
  3. On completion, ``AgentContext`` is populated with execution metadata.

This is *not* an installed agent (``BaseInstalledAgent``).  It extends
``BaseAgent`` directly and is loaded at runtime via Harbor's
``--agent-import-path`` mechanism:

    harbor run --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent \
               -p dataset/my-task -m openai/my-model ...

Environment variables
--------------------
.. list-table::
   :header-rows: 1

   * - Variable
     - Default
     - Description
   * - ``OPENCLAW_VERSION``
     - ``"2026.4.14"``
     - Version string reported by ``version()``.
   * - ``OPENCLAW_AGENT_ID``
     - ``"main"``
     - OpenClaw agent-id used in the ``--agent`` flag.
   * - ``OPENCLAW_TIMEOUT``
     - ``"600"``
     - Per-call timeout (seconds) passed as ``--timeout`` to OpenClaw.
       Also used as the floor for the subprocess total-timeout guard
       (the guard is ``max(OPENCLAW_TIMEOUT, 600)``).
   * - ``OPENCLAW_NO_OUTPUT_TIMEOUT``
     - ``"500"``
     - Kill the OpenClaw subprocess after this many seconds with zero
       stdout (API-hang detection).
   * - ``OPENCLAW_THINKING``
     - ``"off"``
     - OpenClaw thinking mode (``"off"`` / ``"low"`` / ``"high"``).
   * - ``OPENCLAW_MAX_ITERATIONS``
     - ``"0"``
     - Max agent-loop iterations (0 = unlimited).
   * - ``OPENCLAW_MAX_STDOUT_BYTES``
     - ``"102400"``
     - Kill OpenClaw subprocess if stdout exceeds this (infinite-generation guard).
   * - ``DOCKER_EXEC_TIMEOUT``
     - ``"600"``
     - Per-command ``environment.exec()`` timeout in seconds.
   * - ``DATASET_DIR``
     - ``"dataset"``
     - Path to the task dataset directory (used to locate ``skill.md`` /
       ``solution/solve.sh``).  Relative to the working directory from
       which ``harbor run`` is invoked (the repo root by default).
   * - ``SKILL_FROM_SOLUTION``
     - ``"0"``
     - Set to ``"1"`` to enable auto-generated skill hints from
       ``solution/solve.sh`` (disabled by default for fair benchmark
       evaluation).
"""

from __future__ import annotations

import asyncio
import glob
import json
import logging
import os
import re
import select
import shutil
import subprocess
import time
import uuid
from functools import partial
from typing import Any

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

__all__ = ["OpenClawExternalAgent"]

_log = logging.getLogger(__name__)
_LOG_PREFIX = "OpenClaw"


class OpenClawExternalAgent(BaseAgent):
    """Route between an OpenClaw process on the **host** and a Harbor Docker container.

    OpenClaw runs on the host for LLM inference.  Commands are extracted from
    its output and relayed into the Docker container via
    ``environment.exec()``.  This keeps the agent CLI outside the container.

    The adapter implements three guardrails against hung/stalled subprocesses:

    * **total-timeout** – kill the OpenClaw subprocess after
      ``_OPENCLAW_TIMEOUT_SEC``.
    * **no-output timeout** – kill after ``_OPENCLAW_NO_OUTPUT_SEC`` with zero
      stdout (API hang).
    * **stdout cap** – kill when stdout exceeds ``_MAX_STDOUT_BYTES``
      (infinite-generation loop).
    """

    # ------------------------------------------------------------------
    # Truncation / sizing constants
    # ------------------------------------------------------------------
    _MAX_SKILL_HINT_CHARS: int = 3000
    _MAX_HEREDOC_BODY_CHARS: int = 800
    _MAX_STEP_CHARS: int = 200
    _MAX_EXEC_STDOUT_CHARS: int = 5000
    _MAX_EXEC_STDERR_CHARS: int = 1000
    _MAX_CMD_OUTPUT_CHARS: int = 2000
    _MAX_ERROR_OUTPUT_CHARS: int = 500
    _DEBUG_OUTPUT_CHARS: int = 500
    _DEBUG_TEXT_CHARS: int = 300

    # ------------------------------------------------------------------
    # Timeout constants
    # ------------------------------------------------------------------
    _OPENCLAW_TIMEOUT_SEC: int = 600
    _OPENCLAW_NO_OUTPUT_SEC: int = 500
    _DEFAULT_DOCKER_EXEC_TIMEOUT: int = 600
    _HEARTBEAT_INTERVAL_SEC: int = 30
    _SLOW_EXEC_THRESHOLD_SEC: int = 60

    # ------------------------------------------------------------------
    # Path / file constants
    # ------------------------------------------------------------------
    _DEFAULT_DATASET_DIR: str = "dataset"
    _DEFAULT_MAX_STDOUT_BYTES: int = 102400

    # ------------------------------------------------------------------
    # Command-format rules appended to auto-generated skill hints
    # ------------------------------------------------------------------
    _SKILL_COMMAND_RULES: str = (
        "\n## Command Format Rules\n"
        "- Do NOT use the exec tool — it runs on the HOST, not in the container. "
        "Put commands in ```bash``` blocks only.\n"
        "- Do NOT use `cd /app && command` — use absolute paths instead.\n"
        "- Each command must be a single simple command, not chained with `&&` "
        "or `;`.\n"
        "- You MUST execute ALL commands in the Approach section before saying "
        "TASK_COMPLETE. Do NOT claim completion without executing commands.\n"
    )

    # ------------------------------------------------------------------
    # Instance state (declared in __init__, populated during setup / run)
    # ------------------------------------------------------------------

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        # Backward compat: _extra_env was added to BaseAgent after v0.15.0.
        # In v0.15.0 BaseAgent silently drops extra_env via **kwargs, so we
        # extract it ourselves before calling super().
        _env = kwargs.pop("extra_env", None)
        super().__init__(*args, **kwargs)
        if not hasattr(self, "_extra_env") or not self._extra_env:
            self._extra_env: dict[str, str] = dict(_env) if _env else {}

        # Set during setup()
        self._task_name: str = ""
        self._skill_hint: str = ""
        self._profile_name: str = ""
        self._harbor_container_id: str = ""
        self._harbor_image: str | None = None
        self._harbor_workdir: str = "/app"

    # ------------------------------------------------------------------
    # Harbor agent contract
    # ------------------------------------------------------------------

    @staticmethod
    def name() -> str:
        return "openclaw-external"

    def version(self) -> str | None:
        return os.environ.get("OPENCLAW_VERSION", "2026.4.14")

    async def setup(self, environment: BaseEnvironment) -> None:
        # Extract task name (e.g. "crack-7z-hash__abc123" -> "crack-7z-hash")
        env_name = getattr(environment, "environment_name", "")
        self._task_name = env_name.split("__")[0] if "__" in env_name else env_name

        self._skill_hint = self._load_skill_hint()

        if hasattr(environment, "session_id"):
            self._profile_name = re.sub(
                r"[^a-zA-Z0-9_-]", "_", environment.session_id
            )
        else:
            self._profile_name = f"task_{uuid.uuid4().hex[:8]}"

        container_id = await self._detect_harbor_container(environment)
        if not container_id:
            raise RuntimeError(
                "No Harbor container found. Ensure the Harbor environment is running."
            )

        self._harbor_container_id = container_id

        returncode, stdout, stderr = await asyncio.get_event_loop().run_in_executor(
            None, self._sync_run_openclaw, ["openclaw", "--version"]
        )
        if returncode != 0:
            raise RuntimeError(
                "openclaw not available on host\n"
                f"stdout={stdout}\nstderr={stderr}"
            )

        await self._validate_docker_container(container_id)
        await self._setup_container_info(container_id)

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        execution = await self._run_agent_loop(instruction, environment)
        self._populate_context(context, execution)

        if execution["returncode"] != 0:
            raise RuntimeError(
                "OpenClaw execution failed\n"
                f"returncode={execution['returncode']}\n"
                f"stdout:\n{execution['stdout']}\n\n"
                f"stderr:\n{execution['stderr']}"
            )

    # ==================================================================
    # Core routing loop
    # ==================================================================

    async def _run_agent_loop(
        self,
        instruction: str,
        environment: BaseEnvironment,
    ) -> dict[str, Any]:
        container_id = self._harbor_container_id
        profile_name = self._profile_name
        agent_id = os.environ.get("OPENCLAW_AGENT_ID", "main")

        profile_config_dir = os.path.expanduser(f"~/.openclaw-{profile_name}")
        profile_config_path = os.path.join(profile_config_dir, "openclaw.json")

        main_config_path = os.path.expanduser("~/.openclaw/openclaw.json")
        main_config: dict[str, Any] = {}
        if os.path.exists(main_config_path):
            with open(main_config_path) as f:
                main_config = json.load(f)

        all_executions: list[dict[str, Any]] = []
        final_result: dict[str, Any] | None = None

        # Clean up old profile for a fresh start (stale models.json can
        # contain unreachable providers like codex -> chatgpt.com).
        if os.path.exists(profile_config_dir):
            shutil.rmtree(profile_config_dir)
        os.makedirs(profile_config_dir)
        profile_agents_dir = os.path.join(
            profile_config_dir, "agents", agent_id, "agent"
        )
        os.makedirs(profile_agents_dir)

        agents_config = main_config.get("agents", {})
        # Sandbox mode must be "off": we only need OpenClaw for LLM
        # inference, not command execution (done via _env_exec in the
        # Harbor container).  "all" would spawn a redundant sandbox.
        defaults = agents_config.setdefault("defaults", {})
        defaults["sandbox"] = {"mode": "off"}

        # Update the agent's default model and visibility whitelist to
        # match the model selected via ``-m``.  openclaw enforces a
        # visibility policy: if ``agents.defaults.models`` is set, only
        # keys listed there are accepted as ``--model`` overrides.
        # Stale entries from a previous run (e.g. ``custom/...``) would
        # cause ``Model override ... is not allowed for agent "main"``.
        if self.model_name:
            defaults.setdefault("model", {})["primary"] = self.model_name
            allowed = defaults.setdefault("models", {})
            allowed[self.model_name] = allowed.get(self.model_name, {})

        # Filter out providers with empty apiKey to avoid API hangs.
        raw_models = main_config.get("models", {})
        if "providers" in raw_models:
            filtered_providers = {}
            for name, provider in raw_models["providers"].items():
                if provider.get("apiKey"):
                    filtered_providers[name] = provider
                else:
                    self.logger.info(
                        "%s: skipping provider '%s' (empty apiKey)",
                        _LOG_PREFIX, name,
                    )
            raw_models = {**raw_models, "providers": filtered_providers}

        # Inject provider config from harbor --agent-env.
        # Env var names follow the ``<PROVIDER>_BASE_URL`` / ``<PROVIDER>_API_KEY``
        # convention (matching harbor's installed agent behavior), so both
        # modes use identical env vars for the same model/provider pair.
        provider_name = ""
        model_id = "default"
        if self.model_name and "/" in self.model_name:
            provider_name, model_id = self.model_name.split("/", 1)
        elif self.model_name:
            model_id = self.model_name

        extra = self._extra_env or {}
        env_prefix = provider_name.upper().replace("-", "_")
        base_url_env = f"{env_prefix}_BASE_URL"
        api_key_env = f"{env_prefix}_API_KEY"

        provider_base_url = (
            extra.get(base_url_env) or os.environ.get(base_url_env)
        )
        provider_api_key = (
            extra.get(api_key_env) or os.environ.get(api_key_env)
        )
        if provider_base_url and provider_name:
            providers = raw_models.setdefault("providers", {})
            prov = providers.get(provider_name, {})
            prov["baseUrl"] = provider_base_url
            if provider_api_key:
                prov["apiKey"] = provider_api_key

            # Declare the model explicitly.  Non-bundled providers
            # (e.g. ``custom``) require this; bundled providers (e.g.
            # ``openai``) tolerate it and benefit from the explicit
            # ``api`` field for OpenAI-compatible endpoints.
            existing_models = prov.get("models", [])
            existing_ids = {
                m.get("id") for m in existing_models
                if isinstance(m, dict)
            }
            if model_id not in existing_ids:
                existing_models.append({
                    "id": model_id,
                    "name": model_id,
                    "api": "openai-completions",
                })
            prov["models"] = existing_models

            providers[provider_name] = prov
            self.logger.info(
                "%s: injected provider '%s' (baseUrl=%s, model=%s)",
                _LOG_PREFIX, provider_name, provider_base_url, model_id,
            )

        hybrid_config: dict[str, Any] = {
            "models": raw_models,
            "agents": agents_config,
        }
        with open(profile_config_path, "w") as f:
            json.dump(hybrid_config, f, indent=2)

        main_models_path = os.path.expanduser(
            f"~/.openclaw/agents/{agent_id}/agent/models.json"
        )
        profile_models_path = os.path.join(profile_agents_dir, "models.json")
        if os.path.exists(main_models_path):
            with open(main_models_path) as f:
                models_data = json.load(f)
            # Filter unreachable providers from models.json as well.
            if "providers" in models_data:
                models_data["providers"] = {
                    k: v
                    for k, v in models_data["providers"].items()
                    if v.get("apiKey") and v.get("apiKey") != "codex-app-server"
                }
            with open(profile_models_path, "w") as f:
                json.dump(models_data, f, indent=2)

        timeout = os.environ.get("OPENCLAW_TIMEOUT", "600")
        thinking = os.environ.get("OPENCLAW_THINKING", "off")
        max_iterations = int(os.environ.get("OPENCLAW_MAX_ITERATIONS", "0"))

        current_instruction = self._build_initial_instruction(instruction, container_id)
        iteration = 0
        oc_session_id: str | None = None

        while not max_iterations or iteration < max_iterations:
            iteration += 1

            cmd = [
                "openclaw", "--profile", self._profile_name,
                "agent", "--local", "--json",
                "--agent", agent_id,
                "--timeout", timeout,
                "--thinking", thinking,
            ]
            # Pass the model from harbor's -m flag to OpenClaw.
            if self.model_name:
                cmd.extend(["--model", self.model_name])

            # Resume existing session for context continuity.
            if oc_session_id:
                cmd.extend(["--session-id", oc_session_id])

            cmd.extend(["--message", current_instruction])

            self.logger.info(
                "%s: iteration %d | prompt=%d chars | session=%s",
                _LOG_PREFIX, iteration, len(current_instruction),
                "new" if not oc_session_id else oc_session_id[:8],
            )

            # Run OpenClaw via subprocess in thread pool (avoids asyncio pipe
            # issues).
            loop = asyncio.get_event_loop()
            returncode, stdout_text, stderr_text = await loop.run_in_executor(
                None, partial(self._sync_run_openclaw, cmd, self._extra_env)
            )

            parsed = self._try_parse_json(stderr_text) or self._try_parse_json(stdout_text)
            parsed_label = "dict" if isinstance(parsed, dict) else type(parsed).__name__
            self.logger.info(
                "%s: iteration %d | returncode=%d | parsed=%s",
                _LOG_PREFIX, iteration, returncode, parsed_label,
            )

            # Extract session_id from first call for context continuity.
            if not oc_session_id and isinstance(parsed, dict):
                agent_meta = (parsed.get("meta") or {}).get("agentMeta") or {}
                oc_session_id = agent_meta.get("sessionId")
                if oc_session_id:
                    self.logger.info(
                        "%s: session established (%s...)",
                        _LOG_PREFIX, oc_session_id[:12],
                    )

            if not parsed:
                self.logger.debug(
                    "%s: stdout(first %d): %s",
                    _LOG_PREFIX, self._DEBUG_OUTPUT_CHARS,
                    stdout_text[:self._DEBUG_OUTPUT_CHARS],
                )
                self.logger.debug(
                    "%s: stderr(first %d): %s",
                    _LOG_PREFIX, self._DEBUG_OUTPUT_CHARS,
                    stderr_text[:self._DEBUG_OUTPUT_CHARS],
                )

            if returncode != 0:
                # API hang guard: retry once with the same session.
                is_api_hang = (
                    "no stdout for" in stderr_text
                    and "likely API hang" in stderr_text
                )
                if is_api_hang:
                    self.logger.warning(
                        "%s: iteration %d | API hang detected, retrying",
                        _LOG_PREFIX, iteration,
                    )
                    returncode, stdout_text, stderr_text = await loop.run_in_executor(
                        None, partial(self._sync_run_openclaw, cmd, self._extra_env)
                    )
                    parsed = self._try_parse_json(stderr_text) or self._try_parse_json(stdout_text)

                if returncode != 0:
                    final_result = self._build_result(
                        returncode, stdout_text, stderr_text,
                        parsed, container_id, iteration, all_executions,
                    )
                    break

            # Extract commands from parsed response.
            commands = self._extract_commands(parsed)
            extracted_text = self._extract_final_text(parsed) or ""
            tool_cmds = self._extract_toolcall_commands(parsed) if parsed else []
            bash_cmds = (
                self._extract_commands_from_text(extracted_text)
                if extracted_text else []
            )
            self.logger.info(
                "%s: iteration %d | commands=%d text=%d [toolCall=%d bash=%d]",
                _LOG_PREFIX, iteration, len(commands), len(extracted_text),
                len(tool_cmds), len(bash_cmds),
            )
            if extracted_text and not commands:
                self.logger.debug(
                    "%s: text(first %d): %s",
                    _LOG_PREFIX, self._DEBUG_TEXT_CHARS,
                    extracted_text[:self._DEBUG_TEXT_CHARS],
                )

            if commands:
                cmd_outputs: list[str] = []
                for i, cmd_str in enumerate(commands):
                    self.logger.info(
                        "%s: exec[%d] %s", _LOG_PREFIX, i, cmd_str[:200],
                    )
                    result = await self._env_exec(environment, cmd_str)
                    result_code = self._result_exit_code(result)
                    result_stdout = self._result_stdout(result)
                    result_stderr = self._result_stderr(result)
                    self.logger.info(
                        "%s: exec[%d] exit=%d stdout=%s",
                        _LOG_PREFIX, i, result_code, result_stdout[:300],
                    )

                    all_executions.append({
                        "command": cmd_str,
                        "returncode": result_code,
                        "stdout": result_stdout[:self._MAX_EXEC_STDOUT_CHARS],
                        "stderr": result_stderr[:self._MAX_EXEC_STDERR_CHARS],
                    })
                    cmd_outputs.append(
                        f"Command: {cmd_str}\n"
                        f"Exit code: {result_code}\n"
                        f"Output:\n{result_stdout[:self._MAX_CMD_OUTPUT_CHARS]}\n"
                        f"Errors:\n{result_stderr[:self._MAX_ERROR_OUTPUT_CHARS]}"
                    )

                current_instruction = self._build_followup_instruction(
                    cmd_outputs, instruction,
                )
                continue

            # No commands extracted – check if task is complete.
            oc_text = self._extract_final_text(parsed) or ""
            if "TASK_COMPLETE" in oc_text.upper() and all_executions:
                self.logger.info(
                    "%s: iteration %d | TASK_COMPLETE received",
                    _LOG_PREFIX, iteration,
                )
                final_result = self._build_result(
                    0, stdout_text, stderr_text,
                    parsed, container_id, iteration, all_executions,
                )
                break
            elif "TASK_COMPLETE" in oc_text.upper() and not all_executions:
                self.logger.warning(
                    "%s: iteration %d | TASK_COMPLETE but NO commands yet, "
                    "re-prompting", _LOG_PREFIX, iteration,
                )
                current_instruction = (
                    "You said TASK_COMPLETE but you have NOT executed any "
                    "commands yet. You MUST execute bash commands first "
                    "before claiming completion. Output bash commands in "
                    "```bash``` code blocks to actually perform the task."
                )
                continue

            # No commands and not complete – prompt OpenClaw again.
            self.logger.warning(
                "%s: iteration %d | no commands found, re-prompting",
                _LOG_PREFIX, iteration,
            )
            current_instruction = (
                "Your previous response did not contain bash commands in "
                "```bash``` code blocks. You MUST output bash commands to "
                "be executed in the container. Do NOT just analyze – "
                "directly provide the commands. Output bash commands, or "
                "say TASK_COMPLETE if the task is truly finished."
            )

        if not final_result:
            final_result = {
                "mode": "hybrid-docker",
                "returncode": 1,
                "stdout": "",
                "stderr": "Max iterations reached",
                "parsed_json": None,
                "container_id": container_id,
                "iterations": iteration,
                "harbor_executions": all_executions,
            }

        # Collect per-round token usage from OpenClaw session JSONL.
        session_token_report = self._collect_session_tokens(profile_config_dir, agent_id)
        if session_token_report:
            final_result["session_tokens"] = session_token_report

        # Record profile directory for traceability.
        final_result["openclaw_profile_dir"] = profile_config_dir

        return final_result

    # ==================================================================
    # Session token collection
    # ==================================================================

    def _collect_session_tokens(self, profile_dir: str, agent_id: str) -> dict[str, Any] | None:
        """Read OpenClaw session JSONL and extract per-round token usage."""
        sessions_dir = os.path.join(profile_dir, "agents", agent_id, "sessions")
        jsonl_files = sorted(glob.glob(os.path.join(sessions_dir, "*.jsonl")))
        if not jsonl_files:
            return None

        # Use the most recent session file (by mtime).
        jsonl_path = max(jsonl_files, key=os.path.getmtime)

        rounds: list[dict[str, Any]] = []
        total_input = 0
        total_output = 0
        prev_input = 0

        try:
            with open(jsonl_path) as f:
                for line in f:
                    try:
                        d = json.loads(line.strip())
                        if d.get("type") != "message":
                            continue
                        usage = (d.get("message") or {}).get("usage") or {}
                        if usage.get("totalTokens", 0) <= 0:
                            continue
                        inp: int = usage.get("input", 0)
                        out: int = usage.get("output", 0)
                        delta = inp - prev_input
                        total_input += inp
                        total_output += out
                        prev_input = inp
                        rounds.append({
                            "round": len(rounds) + 1,
                            "input": inp,
                            "input_delta": delta,
                            "output": out,
                            "total": inp + out,
                            "cumulative": total_input + total_output,
                        })
                    except (json.JSONDecodeError, KeyError, TypeError):
                        continue
        except Exception:
            self.logger.warning(
                "%s: failed to read session tokens from %s",
                _LOG_PREFIX, jsonl_path, exc_info=True,
            )
            return None

        if not rounds:
            return None

        return {
            "session_file": os.path.basename(jsonl_path),
            "num_rounds": len(rounds),
            "total_input": total_input,
            "total_output": total_output,
            "total_tokens": total_input + total_output,
            "final_input": rounds[-1]["input"],
            "avg_input_delta": (
                (rounds[-1]["input"] - rounds[0]["input"])
                // max(len(rounds) - 1, 1)
            ),
            "rounds": rounds,
        }

    # ==================================================================
    # Instruction builders
    # ==================================================================

    def _build_initial_instruction(self, instruction: str, container_id: str) -> str:
        parts = [f"Task: {instruction}\n\n"]

        if self._skill_hint:
            parts.append(
                f"Suggested approach for this task:\n{self._skill_hint}\n\n"
            )

        parts.append(
            f"Container: {container_id} | "
            f"Image: {self._harbor_image} | "
            f"Workdir: {self._harbor_workdir or '/app'}\n\n"
            "CRITICAL: Do NOT use the exec tool or any other tool to run "
            "commands. The exec tool runs on the HOST machine, NOT in the "
            "container. You MUST output bash commands ONLY in ```bash``` "
            "code blocks. Another process will execute them inside the "
            "Docker container via docker exec.\n\n"
            "RULES: Keep reasoning under 2 sentences. Use absolute paths, "
            "NOT `cd && command`. Each bash block = one simple command. "
            "Say TASK_COMPLETE when done."
        )
        return "".join(parts)

    def _build_followup_instruction(
        self, cmd_outputs: list[str], original_task: str,
    ) -> str:
        return (
            f"Results of your commands:\n\n{chr(10).join(cmd_outputs)}\n\n"
            "Continue with more bash commands in ```bash``` code blocks, "
            "or say TASK_COMPLETE if done."
        )

    # ==================================================================
    # Skill hint loading (per-task hints for the agent)
    # ==================================================================

    def _load_skill_hint(self) -> str:
        """Load per-task skill hint.

        Priority:
        1. ``dataset/<task_name>/skill.md`` (manual override, if exists).
        2. Auto-generate from ``dataset/<task_name>/solution/solve.sh``
           (disabled by default; set ``SKILL_FROM_SOLUTION=1`` to enable).
        """
        if not self._task_name:
            return ""

        dataset_dir = os.environ.get("DATASET_DIR", self._DEFAULT_DATASET_DIR)
        task_dir = os.path.join(dataset_dir, self._task_name)

        # 1. Try manual skill.md first.
        skill_path = os.path.join(task_dir, "skill.md")
        if os.path.isfile(skill_path):
            try:
                with open(skill_path) as f:
                    content = f.read().strip()
                if content:
                    if len(content) > self._MAX_SKILL_HINT_CHARS:
                        content = (
                            content[:self._MAX_SKILL_HINT_CHARS]
                            + "\n... (truncated, see full skill.md)"
                        )
                    self.logger.info(
                        "%s: loaded skill hint from %s (%d chars)",
                        _LOG_PREFIX, skill_path, len(content),
                    )
                    return content
            except OSError:
                pass

        # 2. Auto-generate from solution/solve.sh (opt-in via SKILL_FROM_SOLUTION=1).
        if os.environ.get("SKILL_FROM_SOLUTION", "0") != "1":
            return ""

        solve_path = os.path.join(task_dir, "solution", "solve.sh")
        if os.path.isfile(solve_path):
            try:
                content = self._auto_skill_from_solve(solve_path)
                if content:
                    self.logger.info(
                        "%s: auto-generated skill from %s (%d chars)",
                        _LOG_PREFIX, solve_path, len(content),
                    )
                    return content
            except Exception:
                self.logger.warning(
                    "%s: failed to auto-generate skill from %s",
                    _LOG_PREFIX, solve_path, exc_info=True,
                )

        return ""

    @staticmethod
    def _auto_skill_from_solve(solve_path: str) -> str:
        """Auto-generate a skill hint from a solve.sh file."""
        with open(solve_path) as f:
            raw = f.read()

        lines = raw.split("\n")
        steps: list[str] = []
        i = 0
        while i < len(lines):
            line = lines[i].strip()

            # Skip empty lines, comments, shebang, canary.
            # ("#!" is already covered by startswith("#").)
            if not line or line.startswith("#"):
                i += 1
                continue
            if "BENCHMARK DATA" in line or "canary" in line.lower():
                i += 1
                continue

            # Detect heredoc: "<cmd> <<'MARKER' > <file>"
            # \S+ matches any command (cat, tee, etc.) — single pattern suffices.
            m = re.match(r"(\S+)\s*<<\s*'(\w+)'\s*>\s*(\S+)", line)
            marker: str | None = None
            target_file: str | None = None
            if m:
                marker = m.group(2)
                target_file = m.group(3)

            if marker and target_file:
                block = [f"Write {target_file} with heredoc:"]
                i += 1
                body_lines: list[str] = []
                while i < len(lines):
                    if lines[i].strip() == marker:
                        i += 1
                        break
                    body_lines.append(lines[i])
                    i += 1
                body = "\n".join(body_lines)
                if len(body) > OpenClawExternalAgent._MAX_HEREDOC_BODY_CHARS:
                    body = (
                        body[:OpenClawExternalAgent._MAX_HEREDOC_BODY_CHARS]
                        + "\n... (truncated)"
                    )
                block.append(body)
                steps.append("\n".join(block))
                continue

            # Regular command line – strip trailing comments.
            if " #" in line and not line.startswith("#"):
                line = line[:line.index(" #")].strip()
            if line:
                steps.append(line)
            i += 1

        if not steps:
            return ""

        # Build skill text.
        task_name = os.path.basename(os.path.dirname(os.path.dirname(solve_path)))
        parts = [f"# {task_name}\n\n## Approach\n"]
        for idx, step in enumerate(steps, 1):
            if "\n" in step:
                # Multi-line step (heredoc).
                parts.append(f"{idx}. {step}\n")
            else:
                if len(step) > OpenClawExternalAgent._MAX_STEP_CHARS:
                    step = step[:OpenClawExternalAgent._MAX_STEP_CHARS] + "..."
                parts.append(f"{idx}. `{step}`\n")

        parts.append(OpenClawExternalAgent._SKILL_COMMAND_RULES)

        content = "".join(parts)
        if len(content) > OpenClawExternalAgent._MAX_SKILL_HINT_CHARS:
            content = (
                content[:OpenClawExternalAgent._MAX_SKILL_HINT_CHARS]
                + "\n... (truncated)"
            )
        return content

    # ==================================================================
    # OpenClaw subprocess runner
    # ==================================================================

    @staticmethod
    def _sync_run_openclaw(
        cmd: list[str], extra_env: dict[str, str] | None = None,
    ) -> tuple[int, str, str]:
        """Run OpenClaw with real-time output streaming and safety guards.

        Uses ``Popen`` + ``select`` to read output progressively, reporting
        heartbeat status so long-running calls remain visible.  Three safety
        guards prevent runaway processes:

        * **Total timeout** – kills after ``_OPENCLAW_TIMEOUT_SEC``.
        * **No-output timeout** – kills after ``_OPENCLAW_NO_OUTPUT_SEC``
          with zero stdout (API hang detection).
        * **Stdout cap** – kills when stdout exceeds
          ``OPENCLAW_MAX_STDOUT_BYTES`` (infinite-generation loop).
        """
        cls = OpenClawExternalAgent
        max_stdout_bytes = int(
            os.environ.get("OPENCLAW_MAX_STDOUT_BYTES", str(cls._DEFAULT_MAX_STDOUT_BYTES))
        )
        # The total-timeout guard follows the configured OPENCLAW_TIMEOUT so
        # that raising the per-call timeout also extends the hard kill guard.
        # _OPENCLAW_TIMEOUT_SEC acts as a floor so the guard is never lower
        # than the compiled-in default.
        configured_timeout = int(
            os.environ.get("OPENCLAW_TIMEOUT", str(cls._OPENCLAW_TIMEOUT_SEC))
        )
        total_timeout_sec = max(configured_timeout, cls._OPENCLAW_TIMEOUT_SEC)
        no_output_timeout_sec = int(
            os.environ.get("OPENCLAW_NO_OUTPUT_TIMEOUT", str(cls._OPENCLAW_NO_OUTPUT_SEC))
        )

        try:
            run_env = {**os.environ, **(extra_env or {})}
            proc = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=run_env,
            )
        except Exception as e:
            return 1, "", str(e)

        stdout_chunks: list[bytes] = []
        stderr_chunks: list[bytes] = []
        last_log_time = time.monotonic()
        last_output_time = time.monotonic()
        total_stdout_bytes = 0
        total_stderr_bytes = 0

        try:
            start = time.monotonic()
            while True:
                elapsed = time.monotonic() - start

                # Guard 1: total timeout.
                if elapsed >= total_timeout_sec:
                    proc.kill()
                    _log.warning(
                        "%s: subprocess timeout after %ds (limit %ds), killing process",
                        _LOG_PREFIX, int(elapsed), total_timeout_sec,
                    )
                    break

                # Guard 2: stdout cap (infinite generation).
                if total_stdout_bytes > max_stdout_bytes:
                    proc.kill()
                    _log.warning(
                        "%s: stdout exceeded %dB after %ds, "
                        "killing process (likely infinite generation)",
                        _LOG_PREFIX, max_stdout_bytes, int(elapsed),
                    )
                    break

                # Guard 3: no-output timeout (API hang).
                if (
                    total_stdout_bytes == 0
                    and (time.monotonic() - last_output_time)
                    >= no_output_timeout_sec
                ):
                    proc.kill()
                    _log.warning(
                        "%s: no stdout for %ds after %ds elapsed, "
                        "killing process (likely API hang)",
                        _LOG_PREFIX, no_output_timeout_sec, int(elapsed),
                    )
                    break

                ready, _, _ = select.select(
                    [proc.stdout, proc.stderr], [], [], 0.5,
                )
                if proc.stdout in ready:
                    chunk = (
                        proc.stdout.read1(8192)
                        if hasattr(proc.stdout, "read1")
                        else proc.stdout.read(8192)
                    )
                    if chunk:
                        stdout_chunks.append(chunk)
                        total_stdout_bytes += len(chunk)
                        last_output_time = time.monotonic()
                if proc.stderr in ready:
                    chunk = (
                        proc.stderr.read1(8192)
                        if hasattr(proc.stderr, "read1")
                        else proc.stderr.read(8192)
                    )
                    if chunk:
                        stderr_chunks.append(chunk)
                        total_stderr_bytes += len(chunk)

                if proc.poll() is not None:
                    # Drain remaining output.
                    for _ in range(10):
                        chunk = proc.stdout.read(65536) if proc.stdout else None
                        if chunk:
                            stdout_chunks.append(chunk)
                            total_stdout_bytes += len(chunk)
                        else:
                            break
                    for _ in range(10):
                        chunk = proc.stderr.read(65536) if proc.stderr else None
                        if chunk:
                            stderr_chunks.append(chunk)
                            total_stderr_bytes += len(chunk)
                        else:
                            break
                    break

                now = time.monotonic()
                if now - last_log_time >= cls._HEARTBEAT_INTERVAL_SEC:
                    _log.debug(
                        "%s: heartbeat: %ds elapsed, stdout=%dB, stderr=%dB",
                        _LOG_PREFIX, int(elapsed),
                        total_stdout_bytes, total_stderr_bytes,
                    )
                    last_log_time = now

            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            _log.warning(
                "%s: process killed after wait timeout", _LOG_PREFIX,
            )
        except Exception:
            _log.warning(
                "%s: error during streaming", _LOG_PREFIX, exc_info=True,
            )
            try:
                proc.kill()
            except Exception:
                pass

        returncode = proc.returncode if proc.returncode is not None else -1
        stdout_text = b"".join(stdout_chunks).decode("utf-8", errors="replace")
        stderr_text = b"".join(stderr_chunks).decode("utf-8", errors="replace")
        return returncode, stdout_text, stderr_text

    # ==================================================================
    # Result builder
    # ==================================================================

    def _build_result(
        self,
        returncode: int,
        stdout: str,
        stderr: str,
        parsed: dict[str, Any] | None,
        container_id: str,
        iteration: int,
        executions: list[dict[str, Any]],
    ) -> dict[str, Any]:
        return {
            "mode": "hybrid-docker",
            "returncode": returncode,
            "stdout": stdout,
            "stderr": stderr,
            "parsed_json": parsed,
            "container_id": container_id,
            "harbor_image": self._harbor_image,
            "iterations": iteration,
            "harbor_executions": executions,
        }

    # ==================================================================
    # Container detection and setup
    # ==================================================================

    async def _detect_harbor_container(
        self, environment: BaseEnvironment,
    ) -> str | None:
        """Discover the Harbor Docker container.

        Tries two strategies:

        1. Filter by ``session_id`` (preferred).
        2. Fallback: scan ``docker ps`` for names containing ``"__"`` and
           ``"-main-"``.  **Note**: this heuristic depends on Harbor's
           internal container naming convention and may break if that
           convention changes.
        """
        try:
            if hasattr(environment, "session_id"):
                proc = await asyncio.create_subprocess_exec(
                    "docker", "ps", "-q",
                    "--filter", f"name={environment.session_id}",
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                stdout, _ = await proc.communicate()
                if stdout.decode().strip():
                    return stdout.decode().strip().split("\n")[0]

            # Fallback: heuristic container name matching, verified via inspect.
            proc = await asyncio.create_subprocess_exec(
                "docker", "ps", "--format", "{{.ID}} {{.Names}}",
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, _ = await proc.communicate()
            for line in stdout.decode().strip().split("\n"):
                if "__" in line and "-main-" in line:
                    candidate_id = line.split()[0]
                    # Verify the candidate actually exists.
                    chk = await asyncio.create_subprocess_exec(
                        "docker", "inspect", candidate_id,
                        stdout=asyncio.subprocess.DEVNULL,
                        stderr=asyncio.subprocess.DEVNULL,
                    )
                    await chk.communicate()
                    if chk.returncode == 0:
                        return candidate_id
        except Exception:
            _log.warning(
                "%s: failed to detect Harbor container",
                _LOG_PREFIX, exc_info=True,
            )
        return None

    async def _validate_docker_container(self, container_id: str) -> None:
        proc = await asyncio.create_subprocess_exec(
            "docker", "inspect", container_id,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await proc.communicate()
        if proc.returncode != 0:
            raise RuntimeError(
                f"Cannot access Docker container {container_id}\n"
                f"stderr={stderr.decode(errors='replace')}"
            )

    async def _setup_container_info(self, container_id: str) -> None:
        # Image name.
        proc = await asyncio.create_subprocess_exec(
            "docker", "inspect", "--format",
            "{{.Config.Image}}", container_id,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, _ = await proc.communicate()
        self._harbor_image = (
            stdout.decode().strip() if proc.returncode == 0 else None
        )

        # Working directory.
        proc = await asyncio.create_subprocess_exec(
            "docker", "inspect", "--format",
            "{{.Config.WorkingDir}}", container_id,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, _ = await proc.communicate()
        self._harbor_workdir = (
            stdout.decode().strip() if proc.returncode == 0 else "/app"
        )
        if not self._harbor_workdir:
            self._harbor_workdir = "/app"

        self._harbor_container_id = container_id

    # ==================================================================
    # Command execution in container
    # ==================================================================

    async def _env_exec(
        self, environment: BaseEnvironment, command: str,
    ) -> Any:
        """Execute *command* inside the Docker container with a safety timeout.

        Uses ``asyncio.wait_for`` to prevent infinite hangs.  Timeout is
        configurable via ``DOCKER_EXEC_TIMEOUT`` env var (default 600 s).
        """
        timeout = int(
            os.environ.get("DOCKER_EXEC_TIMEOUT", str(self._DEFAULT_DOCKER_EXEC_TIMEOUT))
        )
        start = time.monotonic()

        try:
            result = await asyncio.wait_for(
                environment.exec(command),
                timeout=timeout,
            )
        except asyncio.TimeoutError:
            self.logger.warning(
                "%s: docker exec stuck for %ds, returning timeout result: %s",
                _LOG_PREFIX, timeout, command[:100],
            )
            return self._make_timeout_result(command, timeout)

        elapsed = time.monotonic() - start
        if elapsed > self._SLOW_EXEC_THRESHOLD_SEC:
            self.logger.info(
                "%s: slow docker exec (%ds): %s",
                _LOG_PREFIX, int(elapsed), command[:100],
            )
        return result

    @staticmethod
    def _make_timeout_result(command: str, timeout: int) -> Any:
        """Return a synthetic result object for timed-out commands."""

        class _TimeoutResult:
            exit_code: int = -1
            stdout: str = f"ERROR: command timed out after {timeout}s"
            stderr: str = ""
            returncode: int = -1

        return _TimeoutResult()

    @staticmethod
    def _result_stdout(result: Any) -> str:
        raw = getattr(result, "stdout", "") or ""
        # Strip null bytes – they cause ValueError in subprocess.Popen and
        # are meaningless in text output from container commands.
        return raw.replace("\x00", "")

    @staticmethod
    def _result_stderr(result: Any) -> str:
        raw = getattr(result, "stderr", "") or ""
        return raw.replace("\x00", "")

    @staticmethod
    def _result_exit_code(result: Any) -> int:
        if hasattr(result, "exit_code"):
            return int(getattr(result, "exit_code"))
        if hasattr(result, "returncode"):
            return int(getattr(result, "returncode"))
        return 0

    # ==================================================================
    # OpenClaw output parsing
    # ==================================================================

    def _extract_commands(self, parsed: dict[str, Any] | None) -> list[str]:
        if not parsed:
            return []
        # Prefer toolCall commands; fall back to ```bash``` blocks in text.
        tool_commands = self._extract_toolcall_commands(parsed)
        if tool_commands:
            return tool_commands
        text = self._extract_final_text(parsed) or ""
        return self._extract_commands_from_text(text)

    @staticmethod
    def _extract_toolcall_commands(parsed: dict[str, Any]) -> list[str]:
        """Extract exec commands from OpenClaw toolCall responses."""
        commands: list[str] = []

        # Check payloads for toolCall entries.
        payloads = parsed.get("payloads") or []
        for item in payloads:
            if not isinstance(item, dict):
                continue
            content = item.get("content", [])
            if isinstance(content, list):
                for c in content:
                    if (
                        isinstance(c, dict)
                        and c.get("type") == "toolCall"
                        and c.get("name") == "exec"
                    ):
                        args = c.get("arguments", {})
                        if isinstance(args, dict) and args.get("command"):
                            commands.append(args["command"])

        # Also check meta-level toolCalls.
        meta = parsed.get("meta") or {}
        for key in ("toolCalls", "tool_calls"):
            calls = meta.get(key, [])
            if isinstance(calls, list):
                for call in calls:
                    if (
                        isinstance(call, dict)
                        and call.get("name") == "exec"
                    ):
                        args = call.get("arguments", {})
                        if isinstance(args, dict) and args.get("command"):
                            commands.append(args["command"])
        return commands

    @staticmethod
    def _extract_commands_from_text(text: str) -> list[str]:
        blocks = re.findall(
            r"```(?:bash|sh|shell)\s*\n(.*?)\n```", text, re.DOTALL,
        )
        return [b.strip() for b in blocks if b.strip()]

    @staticmethod
    def _try_parse_json(text: str) -> dict[str, Any] | None:
        """Attempt to parse *text* as JSON, with ANSI-stripping and
        substring fallback."""
        cleaned = OpenClawExternalAgent._strip_ansi(text).strip()
        if not cleaned:
            return None

        # Direct parse.
        try:
            parsed = json.loads(cleaned, strict=False)
            if isinstance(parsed, dict):
                return parsed
        except json.JSONDecodeError:
            pass

        # Substring fallback: find outermost {...}.
        start, end = cleaned.find("{"), cleaned.rfind("}")
        if start != -1 and end > start:
            try:
                parsed = json.loads(cleaned[start:end + 1], strict=False)
                if isinstance(parsed, dict):
                    return parsed
            except json.JSONDecodeError:
                pass
        return None

    @staticmethod
    def _extract_final_text(data: dict[str, Any]) -> str | None:
        """Extract the final visible/raw text from OpenClaw response dict."""
        if not data:
            return None

        meta = data.get("meta") or {}
        for key in ("finalAssistantVisibleText", "finalAssistantRawText"):
            value = meta.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()

        payloads = data.get("payloads")
        if isinstance(payloads, list):
            texts = []
            for item in payloads:
                if isinstance(item, dict):
                    text = item.get("text")
                    if isinstance(text, str) and text.strip():
                        texts.append(text.strip())
            if texts:
                return "\n".join(texts)
        return None

    @staticmethod
    def _strip_ansi(text: str) -> str:
        return re.sub(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])", "", text)

    # ==================================================================
    # Context population
    # ==================================================================

    def _populate_context(
        self, context: AgentContext, execution: dict[str, Any],
    ) -> None:
        metadata: dict[str, Any] = {
            "openclaw_execution_mode": execution.get("mode"),
            "harbor_executions": execution.get("harbor_executions", []),
            "iterations": execution.get("iterations", 0),
            "openclaw_profile_dir": execution.get("openclaw_profile_dir"),
            "stdout": execution.get("stdout", ""),
            "stderr": execution.get("stderr", ""),
            "exit_code": execution.get("returncode"),
        }

        # Include per-round token usage from session JSONL.
        session_tokens = execution.get("session_tokens")
        if session_tokens:
            metadata["session_tokens"] = session_tokens

        parsed = execution.get("parsed_json")
        if isinstance(parsed, dict):
            meta = parsed.get("meta") or {}
            agent_meta = meta.get("agentMeta") or {}
            prompt_report = meta.get("systemPromptReport") or {}

            metadata["openclaw"] = {
                "provider": agent_meta.get("provider"),
                "model": agent_meta.get("model"),
                "session_id": agent_meta.get("sessionId"),
                "stop_reason": meta.get("stopReason"),
                "workspace_dir": prompt_report.get("workspaceDir"),
            }

            final_message = self._extract_final_text(parsed)
            if final_message:
                metadata["final_message"] = final_message

        self._safe_setattr(context, "metadata", metadata)

    @staticmethod
    def _safe_setattr(obj: Any, name: str, value: Any) -> None:
        """Set attribute *name* on *obj*, logging a warning on failure."""
        try:
            setattr(obj, name, value)
        except Exception:
            _log.warning(
                "%s: failed to set %s on context object",
                _LOG_PREFIX, name, exc_info=True,
            )
