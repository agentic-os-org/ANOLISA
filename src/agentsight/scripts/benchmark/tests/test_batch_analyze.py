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
