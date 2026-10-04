"""Codex hooks must fail open on parseable-but-wrong-shaped stdin payloads.

The Codex plugin documents "all errors fail open" (codex-plugin/README.md):
every hook is a subprocess whose non-zero exit is reported as a hook failure
and whose stdout must be either empty (allow) or a HookOutput object.

``json.load`` happily returns non-object JSON (``null``, arrays, scalars), so
each hook must reject those shapes explicitly before calling ``.get``.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

_HOOKS_DIR = (
    Path(__file__).resolve().parents[3] / "codex-plugin" / "hooks-plugin" / "hooks"
)

_ALL_HOOKS = (
    "code_scanner_hook.py",
    "prompt_scanner_hook.py",
    "pii_checker_hook.py",
    "skill_ledger_hook.py",
)

# Each payload decodes as JSON but is not a hook-input object.
_NON_OBJECT_PAYLOADS = ("null", "[]", "42", '"text"', "true")

# Prompt-scanner / PII hooks need an object payload that reaches the mapping
# logic; the sandboxed-cwd case exercises the path resolver in the
# skill-ledger hook (Path(cwd) with a non-string value).
_WRONG_FIELD_TYPE_PAYLOADS = (
    '{"prompt": "$some-skill", "cwd": 5}',
    '{"prompt": "$some-skill", "cwd": null}',
    '{"prompt": "$some-skill", "cwd": ["/tmp"]}',
)


def _run_hook(hook_name: str, payload: str) -> subprocess.CompletedProcess:
    env = os.environ.copy()
    env.setdefault("AGENT_SEC_DATA_DIR", os.environ.get("AGENT_SEC_DATA_DIR", ""))
    return subprocess.run(
        [sys.executable, str(_HOOKS_DIR / hook_name)],
        input=payload,
        capture_output=True,
        check=False,
        text=True,
        timeout=30,
        env=env,
    )


def _assert_fail_open(proc: subprocess.CompletedProcess) -> None:
    assert (
        proc.returncode == 0
    ), f"hook exited {proc.returncode} instead of failing open:\n{proc.stderr}"
    stdout = proc.stdout.strip()
    if stdout:
        # Empty stdout is Codex's allow signal; anything else must be a JSON
        # object so the host can parse a decision.
        decoded = json.loads(stdout)
        assert isinstance(decoded, dict)


@pytest.mark.parametrize("hook_name", _ALL_HOOKS)
@pytest.mark.parametrize("payload", _NON_OBJECT_PAYLOADS)
def test_non_object_stdin_payload_fails_open(hook_name, payload):
    _assert_fail_open(_run_hook(hook_name, payload))


@pytest.mark.parametrize("hook_name", ["skill_ledger_hook.py"])
@pytest.mark.parametrize("payload", _WRONG_FIELD_TYPE_PAYLOADS)
def test_non_string_cwd_fails_open(hook_name, payload):
    """A non-string cwd must not raise TypeError from Path(cwd)."""
    _assert_fail_open(_run_hook(hook_name, payload))
