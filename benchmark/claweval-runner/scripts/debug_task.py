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

"""Debug mode: setup task environment without running agent.

Builds the full task environment (MCP config, mock services, gateway, sandbox
container) then pauses for manual interaction. User can connect via CLI or
HTTP API to inspect tools, send messages, and debug agent behavior.

Usage:
    python scripts/debug_task.py T001zh_email_triage

    # Custom config / sandbox image
    python scripts/debug_task.py T001zh_email_triage --config claw-eval/config.yaml
    python scripts/debug_task.py T001zh_email_triage --sandbox-image claw-eval-agent:latest
"""

import argparse
import json
import os
import signal
import sys
import time
from pathlib import Path

_SCRIPT_DIR = Path(__file__).resolve().parent
_REPO_ROOT = _SCRIPT_DIR.parent
sys.path.insert(0, str(_REPO_ROOT / "src"))

# claw-eval source for sandbox mode (SandboxConfig / SandboxRunner / TaskDefinition)
_CLAW_EVAL_SRC = _REPO_ROOT / "claw-eval" / "src"
if _CLAW_EVAL_SRC.is_dir():
    sys.path.insert(0, str(_CLAW_EVAL_SRC))

from ce_runner._common import (OPENCLAW_CONFIG, load_config,
                                load_task_yaml, log, _REPO_DIR, _PYTHON)
from ce_runner.infra import (configure_tools, cleanup_mock_services,
                               reset_services, start_mock_services,
                               restart_gateway, cleanup_config,
                               check_gateway)
from ce_runner.run_task import get_model_config

# ── MCP tool verification ────────────────────────────────────────────────────

def verify_mcp_tools(task_yaml: str) -> list[str] | None:
    """Start MCP server via stdio and verify tool discovery.

    Returns list of tool names on success, None on failure.
    """
    try:
        from mcp.client.session import ClientSession
        from mcp.client.stdio import StdioServerParameters, stdio_client
        import asyncio
    except ImportError:
        log("  [WARN] mcp package not installed, skipping tool verification")
        log("  [WARN] Install: pip install mcp")
        return None

    async def _check():
        params = StdioServerParameters(
            command=_PYTHON,
            args=[
                "-m", "ce_runner.mcp_mock_services",
                "--task-yaml", task_yaml,
                "--mcp-only",
                "--no-start-services",
            ],
        )
        try:
            async with stdio_client(params) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    result = await session.list_tools()
                    return [t.name for t in result.tools]
        except Exception as e:
            log(f"  [ERROR] MCP tool verification failed: {e}")
            return None

    try:
        return asyncio.run(_check())
    except Exception as e:
        log(f"  [ERROR] MCP verification error: {e}")
        return None


# ── Debug mode ───────────────────────────────────────────────────────────────

def run_debug(args):
    """Setup task environment and wait for manual interaction."""
    import threading

    task_path = args.task
    config_path = getattr(args, "config", None)
    sandbox_image = getattr(args, "sandbox_image", None)

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

    cfg = load_config(config_path) if config_path else {}
    model_config = get_model_config(cfg)

    log("=" * 60)
    log(f"  Debug Mode: {task_id}")
    log(f"  YAML:       {task_yaml}")
    log(f"  Mode:       sandbox (always)")
    log("=" * 60)

    # Phase 1: Check gateway
    gateway_port = check_gateway(OPENCLAW_CONFIG)
    if not gateway_port:
        log("[ERROR] openclaw gateway is not running")
        sys.exit(1)
    log(f"  Gateway:    running on port {gateway_port}")

    # Phase 2: Start Docker container and inject sandbox files (no-op for empty file_list)
    sandbox_url = None
    runner = None
    handle = None
    try:
        from claw_eval.config import SandboxConfig
        from claw_eval.models.task import TaskDefinition
        from claw_eval.runner.sandbox_runner import SandboxRunner
    except ImportError as e:
        log(f"[ERROR] claw_eval not importable for sandbox mode: {e}")
        sys.exit(1)

    try:
        image = sandbox_image or "claw-eval-agent:latest"
        runner = SandboxRunner(SandboxConfig(image=image))
        run_id = f"debug-{task_id}-{int(time.time())}-{os.getpid()}"
        log(f"  [sandbox] Starting container claw-agent-{run_id}...")
        handle = runner.start_container(run_id=run_id)
        sandbox_url = handle.sandbox_url

        task_def = TaskDefinition.from_yaml(task_yaml)
        n_injected = runner.inject_files(handle, task_def, task_dir=task_dir)
        expected = len(task_def.sandbox_files) if task_def.sandbox_files else 0
        log(f"  Container:  claw-agent-{handle.run_id} → {sandbox_url}")
        log(f"  Files:      {n_injected}/{expected} injected")
        if expected and n_injected < expected:
            log(f"  [WARN] only {n_injected}/{expected} files injected")
    except Exception as e:
        log(f"[ERROR] Sandbox container startup failed: {e}")
        if runner is not None and handle is not None:
            try:
                runner.stop_container(handle)
            except Exception:
                pass
        sys.exit(1)

    # Phase 3: Configure MCP (sandbox_url=None → gateway-only registration)
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
        # Stop container first so MCP unregister doesn't race a dead bridge
        if runner is not None and handle is not None:
            try:
                runner.stop_container(handle)
            except Exception as e:
                log(f"  [warn] stop_container during failure: {e}")
        cleanup_config(context=ctx)
        cleanup_mock_services()
        sys.exit(1)
    log("  Gateway:      restarted")

    # Phase 6: Verify MCP tools (mock MCP only — sandbox MCP tools are fixed)
    log("\n  Verifying MCP tool discovery...")
    tools = verify_mcp_tools(task_yaml)
    if tools is None:
        log("  [WARN] Could not verify MCP tools")
    elif tools:
        log(f"  Found {len(tools)} mock MCP tools:")
        for t in tools:
            log(f"    ✓ mcp:{ctx.mcp_name}:{t}")
    else:
        log("  No mock MCP tools registered for this task (tools: [])")

    if ctx.sandbox_mcp_name:
        log(f"  Sandbox MCP tools (bridged to container):")
        for t in ("Bash", "Read", "Write", "Edit", "Glob", "Grep"):
            log(f"    ✓ mcp:{ctx.sandbox_mcp_name}:{t}")

    # Phase 7: Print connection info
    session_id = f"debug-{task_id}-{int(time.time())}-{os.getpid()}"
    model_id = model_config.get("model_id", "")
    prompt_text = task.get("prompt", {})
    if isinstance(prompt_text, dict):
        prompt_text = prompt_text.get("text", "")

    print(f"\n{'=' * 60}")
    print(f"  Debug Session Ready")
    print(f"{'=' * 60}")
    print(f"  Task:      {task_id}")
    print(f"  Gateway:   http://127.0.0.1:{gateway_port}")
    print(f"  Agent ID:  {agent_id}")
    print(f"  Session:   {session_id}")
    if model_id:
        print(f"  Model:     {model_id}")
    if sandbox_url:
        print(f"  Sandbox:   {sandbox_url}")
        if handle is not None:
            print(f"  Container: claw-agent-{handle.run_id}")
    print()
    print(f"  Connect via CLI:")
    print(f"    openclaw agent --session-id {session_id} --agent {agent_id}")
    print()
    if model_id:
        print(f"  Or send messages via API:")
        print(f"    curl -s -X POST http://127.0.0.1:{gateway_port}/v1/chat/completions \\")
        print(f"      -H 'Content-Type: application/json' \\")
        print(f"      -d '{{")
        print(f"        \"model\": \"{model_id}\",")
        print(f"        \"messages\": [{{\"role\": \"user\", \"content\": \"{prompt_text[:50]}\"}}]")
        print(f"      }}'")
        print()
    if sandbox_url:
        print(f"  Test container directly:")
        print(f"    curl -s --noproxy 127.0.0.1 -X POST \\")
        print(f"      -H 'Content-Type: application/json' \\")
        print(f"      -d '{{\"command\": \"hostname\", \"timeout_seconds\": 5}}' \\")
        print(f"      {sandbox_url}/exec")
        print()
    print(f"  Check sessions:")
    print(f"    ls ~/.openclaw/agents/{agent_id}/sessions/")
    print(f"{'=' * 60}")
    print(f"  Press Ctrl+C to exit and cleanup")
    print(f"{'=' * 60}\n")

    # Wait for interrupt
    stop_event = threading.Event()

    def handler(sig, frame):
        log("\n  Exiting debug mode...")
        stop_event.set()

    signal.signal(signal.SIGINT, handler)
    signal.signal(signal.SIGTERM, handler)

    try:
        while not stop_event.is_set():
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass

    # Cleanup: stop container BEFORE config cleanup so MCP bridge teardown is clean
    log("\n  Cleaning up...")
    if runner is not None and handle is not None:
        try:
            runner.stop_container(handle)
            log("  Container: stopped")
        except Exception as e:
            log(f"  [warn] stop_container: {e}")
    cleanup_config(context=ctx)
    cleanup_mock_services()
    log("  Done.")


def main():
    parser = argparse.ArgumentParser(
        description="Debug mode: setup task environment without running agent",
    )
    parser.add_argument("task", help="Task ID (e.g. T001zh_email_triage)")
    parser.add_argument("--sandbox-image", default=None,
                        help="Docker image for sandbox container (default: claw-eval-agent:latest)")
    parser.add_argument("--config", default=None,
                        help="Path to config.yaml")

    args = parser.parse_args()
    run_debug(args)


if __name__ == "__main__":
    main()
