"""Triage calibration regressions.

The Python ``classify`` treated any trajectory with zero agent text as
``empty``, diverging from the Rust rule in ``src/reuse/triage.rs``:
``n_steps <= 1 || (max_agent_len == 0 && n_tool_calls == 0)``. Tool-only
transcripts (the 71-step / 84-call corpus documented in the Rust comment) were
therefore counted as empty and inflated the excluded percentage.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

SCRIPT = Path(__file__).parents[2] / "calibrate-triage.py"


def load_module():
    spec = importlib.util.spec_from_file_location("calibrate_triage", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


calibrate = load_module()


def tool_only_document() -> str:
    # 71 steps, no agent text anywhere, 84 tool calls.
    steps = [{"source": "agent", "message": ""} for _ in range(70)]
    steps.append(
        {"source": "agent", "message": "", "tool_calls": [{"function_name": "bash"}] * 84}
    )
    return json.dumps({"steps": steps})


def test_tool_only_document_is_not_empty():
    m = calibrate.measure(tool_only_document())
    assert m == {"n_steps": 71, "n_user_turns": 0, "n_tool_calls": 84, "max_agent_len": 0}
    assert calibrate.classify(m, 2000) != "empty"


def test_document_without_answers_or_tools_stays_empty():
    doc = {"steps": [{"source": "user", "message": "hi"}] * 3}
    m = calibrate.measure(json.dumps(doc))
    assert calibrate.classify(m, 2000) == "empty"


def test_single_step_document_stays_empty():
    m = {"n_steps": 1, "n_user_turns": 1, "n_tool_calls": 0, "max_agent_len": 0}
    assert calibrate.classify(m, 2000) == "empty"


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok {name}")
