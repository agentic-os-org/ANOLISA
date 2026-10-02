"""Runner for the adapter contract suite (roadmap §5.6).

Executes one hook invocation against the mock protocol binary
(mock_tokenless.py) under a chosen behavior class and returns the emitted
envelope plus the recorded tokenless spawns, so tests can assert both the
envelope contract and the one-subprocess gate.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
import tempfile

import corpus

CONTRACT_DIR = os.path.dirname(os.path.abspath(__file__))
MOCK_TOKENLESS = os.path.join(CONTRACT_DIR, "mock_tokenless.py")


class ContractResult:
    def __init__(self, proc, spawns: list[str], requests: list[dict]):
        self.proc = proc
        self.spawns = spawns
        self.requests = requests
        self.envelope = json.loads(proc.stdout) if proc.stdout.strip() else None


def run_case(
    hook_script: str,
    stdin_text: str,
    agent_env: dict,
    behavior: str | None,
    *,
    tokenless_on_path: bool = True,
) -> ContractResult:
    """Run one (hook, agent, behavior) case hermetically.

    `behavior=None` runs with no `tokenless` binary at all (the
    binary-missing class). The mock's spawn log lives outside the hermetic
    temp tree so it survives the run.
    """
    with tempfile.TemporaryDirectory() as tmp:
        spawn_log = os.path.join(tmp, "spawn_log")
        request_log = os.path.join(tmp, "request_log")
        mock_bin = None
        if behavior is not None:
            mock_bin = os.path.join(tmp, "tokenless")
            shutil.copy(MOCK_TOKENLESS, mock_bin)
            os.chmod(mock_bin, os.stat(mock_bin).st_mode | stat.S_IEXEC)
        proc = corpus.run_hook(
            hook_script,
            stdin_text,
            agent_env,
            tokenless_bin=mock_bin,
            extra_env={
                "TOKENLESS_MOCK_BEHAVIOR": behavior or "",
                "TOKENLESS_MOCK_LOG": spawn_log,
                "TOKENLESS_MOCK_REQUEST_LOG": request_log,
            },
            tokenless_on_path=tokenless_on_path,
        )
        try:
            with open(spawn_log) as f:
                spawns = [line.strip() for line in f if line.strip()]
        except OSError:
            spawns = []
        try:
            with open(request_log) as f:
                requests = [json.loads(line) for line in f if line.strip()]
        except OSError:
            requests = []
    return ContractResult(proc, spawns, requests)
