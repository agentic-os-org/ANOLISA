"""Batch analysis converter regressions.

``jsonl_to_atif`` assumed every parsed JSONL line is an object; ``json.loads``
also accepts scalars and arrays, so a line like ``42`` raised
``AttributeError`` and aborted the whole batch instead of being skipped like a
decode error.
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).parents[2] / "batch-analyze.py"


def load_module():
    spec = importlib.util.spec_from_file_location("batch_analyze", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


batch_analyze = load_module()


def test_scalar_line_is_skipped_like_a_decode_error():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text(
            '42\n{"type":"user","message":{"role":"user","content":"hello"}}\n',
            encoding="utf-8",
        )
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    assert [step["message"] for step in doc["steps"]] == ["hello"]


def test_array_line_is_skipped_like_a_decode_error():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text(
            '[1, 2]\n{"type":"user","message":{"role":"user","content":"hi"}}\n',
            encoding="utf-8",
        )
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    assert [step["message"] for step in doc["steps"]] == ["hi"]


def test_document_with_only_scalars_is_rejected():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("42\nnull\n", encoding="utf-8")
        assert batch_analyze.jsonl_to_atif(session) is None


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok {name}")


def test_parallel_tool_results_in_separate_records_are_all_kept():
    """Two tool_use calls answered in separate JSONL records must both reach
    the agent step's observation; the converter used to overwrite the first
    result with the second."""
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"tool_use","id":"a","name":"read","input":{}},'
        '{"type":"tool_use","id":"b","name":"read","input":{}}]}}',
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"a","content":"first result"}]}}',
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"b","content":"second result"}]}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert len(agent_steps) == 1
    results = agent_steps[0]["observation"]["results"]
    assert [r["source_call_id"] for r in results] == ["a", "b"]
    assert [r["content"] for r in results] == ["first result", "second result"]


def test_combined_tool_results_in_one_record_still_kept():
    """A single record carrying both results (the already-working shape) must
    keep working after the append fix."""
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"tool_use","id":"a","name":"read","input":{}},'
        '{"type":"tool_use","id":"b","name":"read","input":{}}]}}',
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"a","content":"first result"},'
        '{"type":"tool_result","tool_use_id":"b","content":"second result"}]}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert len(agent_steps) == 1
    results = agent_steps[0]["observation"]["results"]
    assert [r["source_call_id"] for r in results] == ["a", "b"]


def test_results_after_intervening_agent_step_attach_to_latest_step():
    """A tool result attaches to the most recent agent step: when a new agent
    step intervenes between two result records, each result stays with its
    own step rather than being merged backwards."""
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"tool_use","id":"a","name":"read","input":{}}]}}',
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"a","content":"first result"}]}}',
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"tool_use","id":"b","name":"read","input":{}}]}}',
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"b","content":"second result"}]}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert len(agent_steps) == 2
    assert [r["source_call_id"] for r in agent_steps[0]["observation"]["results"]] == ["a"]
    assert [r["source_call_id"] for r in agent_steps[1]["observation"]["results"]] == ["b"]


def test_thinking_only_assistant_record_keeps_its_step():
    """Claude Code can stream a thinking block in its own assistant record.
    Dropping text-less, call-less records discarded the reasoning entirely."""
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"thinking","thinking":"weigh the options"}]}}',
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"text","text":"the answer is 4"}]}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert len(agent_steps) == 2
    assert agent_steps[0]["reasoning_content"] == "weigh the options"
    assert agent_steps[0]["message"] is None
    assert agent_steps[1]["message"] == "the answer is 4"


def test_usage_shared_by_split_assistant_records_counts_once():
    """Claude Code splits one API message into per-block records that repeat
    the same usage. Counting each record doubled the cost input."""
    lines = [
        '{"type":"assistant","message":{"id":"msg_1","role":"assistant",'
        '"content":[{"type":"text","text":"hello"}],'
        '"usage":{"input_tokens":1000,"output_tokens":100}}}',
        '{"type":"assistant","message":{"id":"msg_1","role":"assistant",'
        '"content":[{"type":"tool_use","id":"t1","name":"read","input":{}}],'
        '"usage":{"input_tokens":1000,"output_tokens":100}}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert len(agent_steps) == 2, "both content blocks must keep their step"
    with_metrics = [s for s in agent_steps if "metrics" in s]
    assert len(with_metrics) == 1, "the shared usage must be counted once"
    assert with_metrics[0]["metrics"] == {
        "prompt_tokens": 1000,
        "completion_tokens": 100,
    }


def test_cumulative_usage_uses_the_last_snapshot():
    """A later record for the same message carries the fuller usage; the last
    snapshot is the one the collector uses."""
    lines = [
        '{"type":"assistant","message":{"id":"msg_2","role":"assistant",'
        '"content":[{"type":"text","text":"hello"}],'
        '"usage":{"input_tokens":1000,"output_tokens":10}}}',
        '{"type":"assistant","message":{"id":"msg_2","role":"assistant",'
        '"content":[{"type":"tool_use","id":"t1","name":"read","input":{}}],'
        '"usage":{"input_tokens":1000,"output_tokens":20}}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert agent_steps[0].get("metrics") is None
    assert agent_steps[1]["metrics"]["completion_tokens"] == 20


def test_usage_without_message_id_stays_with_its_step():
    """Records with no message id cannot share usage; each keeps its own."""
    lines = [
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"text","text":"one"}],'
        '"usage":{"input_tokens":10,"output_tokens":1}}}',
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"text","text":"two"}],'
        '"usage":{"input_tokens":20,"output_tokens":2}}}',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("\n".join(lines) + "\n", encoding="utf-8")
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    agent_steps = [s for s in doc["steps"] if s["source"] == "agent"]
    assert agent_steps[0]["metrics"]["prompt_tokens"] == 10
    assert agent_steps[1]["metrics"]["prompt_tokens"] == 20
