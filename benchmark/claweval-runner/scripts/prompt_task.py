#!/usr/bin/env python3

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

"""Send a prompt to an agent in a fully-setup task environment and print the response.

Replicates the environment setup of debug_task.py (gateway, sandbox, MCP,
mock services) but instead of pausing for interactive use, sends a single
prompt, waits for the agent to finish, prints the assistant's reply, then
cleans up.

Usage:
    python scripts/prompt_task.py
    python scripts/prompt_task.py T001zh_email_triage
    python scripts/prompt_task.py T001zh_email_triage --prompt "查看邮件列表"
    python scripts/prompt_task.py T002_calendar_query --prompt "今天的日程是什么"
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

_SCRIPT_DIR = Path(__file__).resolve().parent
_REPO_ROOT = _SCRIPT_DIR.parent
sys.path.insert(0, str(_REPO_ROOT / "src"))

# claw-eval source for sandbox mode
_CLAW_EVAL_SRC = _REPO_ROOT / "claw-eval" / "src"
if _CLAW_EVAL_SRC.is_dir():
    sys.path.insert(0, str(_CLAW_EVAL_SRC))

from ce_runner._common import (OPENCLAW_CONFIG, load_task_yaml, log, _REPO_DIR)
from ce_runner.infra import (configure_tools, cleanup_mock_services,
                              reset_services, start_mock_services,
                              restart_gateway, cleanup_config,
                              check_gateway)

DEFAULT_TASK = "T001zh_email_triage"
DEFAULT_PROMPT = "查看邮件列表"
DEFAULT_TIMEOUT = 300


def _parse_assistant_response(session_file: str) -> str:
    """Extract the final assistant text from an openclaw session JSONL file."""
    last_text = ""
    try:
        with open(session_file) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("type") != "message":
                    continue
                msg = event.get("message", {})
                if msg.get("role") != "assistant":
                    continue
                content = msg.get("content", [])
                if isinstance(content, str):
                    last_text = content
                elif isinstance(content, list):
                    texts = [
                        c.get("text", "") for c in content
                        if c.get("type") == "text"
                    ]
                    text = "\n".join(t for t in texts if t)
                    if text:
                        last_text = text
    except Exception as e:
        log(f"  [ERROR] Failed to parse session file: {e}")
    return last_text


def _send_prompt(prompt: str, agent_id: str, timeout: int) -> str:
    """Send a prompt via HTTP API and return the session file path."""
    import httpx

    with open(OPENCLAW_CONFIG) as f:
        oc_config = json.load(f)

    gateway_port = oc_config["gateway"]["port"]
    auth_token = oc_config["gateway"]["auth"]["token"]

    model = f"openclaw/{agent_id}"
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
    }

    api_timeout = timeout + 120
    log(f"  Sending prompt: {prompt}")
    log(f"  Model: {model}  Timeout: {api_timeout}s")

    try:
        resp = httpx.post(
            f"http://127.0.0.1:{gateway_port}/v1/chat/completions",
            json=body,
            headers={
                "Authorization": f"Bearer {auth_token}",
                "Content-Type": "application/json",
            },
            timeout=api_timeout,
        )
        resp.raise_for_status()
    except httpx.HTTPError as exc:
        log(f"  [ERROR] HTTP API call failed: {exc}")
        return ""
    except httpx.TimeoutException:
        log(f"  [ERROR] HTTP API call timed out after {api_timeout}s")
        return ""

    # Locate the session file created by this request
    from ce_runner._common import _agent_sessions_dir
    sessions_dir = _agent_sessions_dir(agent_id)
    sdir = Path(sessions_dir)
    sessions = [p for p in sdir.glob("*.jsonl")
                if not p.name.endswith(".trajectory.jsonl")]
    if sessions:
        return str(max(sessions, key=lambda p: p.stat().st_mtime))
    return ""


def main():
    parser = argparse.ArgumentParser(
        description="Send a prompt to an agent in a task environment and print the response",
    )
    parser.add_argument("task", nargs="?", default=DEFAULT_TASK,
                        help=f"Task ID (default: {DEFAULT_TASK})")
    parser.add_argument("--prompt", default=DEFAULT_PROMPT,
                        help=f"Prompt to send (default: {DEFAULT_PROMPT!r})")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT,
                        help=f"Agent timeout in seconds (default: {DEFAULT_TIMEOUT})")
    parser.add_argument("--sandbox-image", default=None,
                        help="Docker image for sandbox container (default: claw-eval-agent:latest)")

    args = parser.parse_args()

    task_path = args.task
    sandbox_image = args.sandbox_image
    prompt = args.prompt
    timeout = args.timeout

    # Resolve task
    task_yaml = str(Path(task_path))
    if not Path(task_yaml).exists():
        task_yaml = str(_REPO_DIR / "claw-eval" / "tasks" / task_path / "task.yaml")
    if not Path(task_yaml).exists():
        task_yaml = str(_REPO_DIR / "claw-eval" / "tasks" / task_path)
    if not Path(task_yaml).exists():
        log(f"[ERROR] Task not found: {task_path}")
        sys.exit(1)
    if not task_yaml.endswith(".yaml"):
        task_yaml = os.path.join(task_yaml, "task.yaml")

    task = load_task_yaml(task_yaml)
    task_id = task["task_id"]
    task_dir = str(Path(task_yaml).parent)

    log("=" * 60)
    log(f"  Prompt Task: {task_id}")
    log(f"  YAML:        {task_yaml}")
    log(f"  Prompt:      {prompt}")
    log("=" * 60)

    # Phase 1: Check gateway
    gateway_port = check_gateway(OPENCLAW_CONFIG)
    if not gateway_port:
        log("[ERROR] openclaw gateway is not running")
        sys.exit(1)
    log(f"  Gateway:    running on port {gateway_port}")

    # Phase 2: Start Docker container
    sandbox_url = None
    runner = None
    handle = None
    ctx = None
    session_file = ""
    try:
        from claw_eval.config import SandboxConfig
        from claw_eval.models.task import TaskDefinition
        from claw_eval.runner.sandbox_runner import SandboxRunner

        image = sandbox_image or "claw-eval-agent:latest"
        runner = SandboxRunner(SandboxConfig(image=image))
        run_id = f"prompt-{task_id}-{int(time.time())}-{os.getpid()}"
        log(f"  [sandbox] Starting container claw-agent-{run_id}...")
        handle = runner.start_container(run_id=run_id)
        sandbox_url = handle.sandbox_url

        task_def = TaskDefinition.from_yaml(task_yaml)
        n_injected = runner.inject_files(handle, task_def, task_dir=task_dir)
        expected = len(task_def.sandbox_files) if task_def.sandbox_files else 0
        log(f"  Container:  claw-agent-{handle.run_id} -> {sandbox_url}")
        log(f"  Files:      {n_injected}/{expected} injected")

        # Phase 3: Configure MCP
        agent_id, _injector, ctx = configure_tools(task_yaml, sandbox_url=sandbox_url)
        log(f"  Agent ID:   {agent_id}")

        # Phase 4: Start mock services
        cleanup_mock_services()
        reset_services(task_yaml)
        start_mock_services(task_yaml, task_dir)
        log("  Mock services: started")

        # Phase 5: Restart gateway
        if not restart_gateway(OPENCLAW_CONFIG, gateway_port):
            log("[ERROR] Gateway restart timed out")
            sys.exit(1)
        log("  Gateway:      restarted")

        # Phase 6: Send prompt and collect response
        log("\n  Sending prompt to agent...")
        session_file = _send_prompt(prompt, agent_id, timeout)

        response = ""
        if session_file:
            response = _parse_assistant_response(session_file)

    except ImportError as e:
        log(f"[ERROR] claw_eval not importable for sandbox mode: {e}")
        sys.exit(1)
    except Exception as e:
        log(f"[ERROR] {e}")
        sys.exit(1)
    finally:
        # Phase 7: Cleanup — guaranteed regardless of exception / sys.exit
        if runner is not None and handle is not None:
            try:
                runner.stop_container(handle)
                log("  Container: stopped")
            except Exception as e:
                log(f"  [warn] stop_container: {e}")
        if ctx is not None:
            try:
                cleanup_config(context=ctx)
            except Exception as e:
                log(f"  [warn] cleanup_config: {e}")
        try:
            cleanup_mock_services()
        except Exception as e:
            log(f"  [warn] cleanup_mock_services: {e}")

    # Print result
    print("\n" + "=" * 60)
    print("  RESPONSE")
    print("=" * 60)
    if response:
        print(response)
    else:
        print("  [no response]")
    print("=" * 60)

    if session_file:
        print(f"\n  Session file: {session_file}")

    sys.exit(0 if response else 1)


if __name__ == "__main__":
    main()
