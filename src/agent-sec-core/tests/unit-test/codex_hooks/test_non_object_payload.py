"""Non-object stdin payloads must fail open in the codex hooks.

Valid JSON that is not an object (array/scalar/null) has no hook fields to
read; the hooks document "empty stdout = allow in Codex", so these payloads
must be treated exactly like undecodable input. Issue #4989 reported the
crashes; these tests pin the pii_checker and code_scanner variants (the
prompt_scanner and skill_ledger variants are deferred pending open PRs that
touch those files' main() regions).
"""

import json
import subprocess
import sys
from pathlib import Path

import pytest

_HOOKS_DIR = (
    Path(__file__).resolve().parents[3] / "codex-plugin" / "hooks-plugin" / "hooks"
)

_HOOKS = [
    "pii_checker_hook.py",
    "code_scanner_hook.py",
]


@pytest.mark.parametrize("hook", _HOOKS)
@pytest.mark.parametrize(
    "payload",
    ["[1, 2, 3]", '"just a string"', "null"],
    ids=["array", "scalar", "null"],
)
def test_non_object_payload_fails_open(hook: str, payload: str) -> None:
    proc = subprocess.run(
        [sys.executable, str(_HOOKS_DIR / hook)],
        input=payload,
        capture_output=True,
        text=True,
        timeout=15,
    )
    # Empty stdout + exit 0 is the hooks' documented allow contract.
    assert proc.returncode == 0, f"stderr: {proc.stderr}"
    assert proc.stdout == ""


def test_object_payload_still_processed() -> None:
    """Sanity: an object payload reaches the normal path (an empty-ish event
    object produces the hook's normal silent allow, not the guard)."""
    proc = subprocess.run(
        [sys.executable, str(_HOOKS_DIR / "pii_checker_hook.py")],
        input=json.dumps({"hook_event_name": "UserPromptSubmit", "prompt": ""}),
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert proc.returncode == 0, f"stderr: {proc.stderr}"
