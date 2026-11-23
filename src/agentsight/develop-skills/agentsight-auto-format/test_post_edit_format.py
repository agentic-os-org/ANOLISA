"""Regression tests for post-edit-format's stdin path extraction."""

import importlib.util
from pathlib import Path

SCRIPT = Path(__file__).with_name("post-edit-format.py")
_spec = importlib.util.spec_from_file_location("post_edit_format", SCRIPT)
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
extract_paths = _module.extract_paths


def test_hook_payload_with_null_edits_keeps_tool_input_path():
    # An optional "edits" serialized as null must not discard the path that
    # was already parsed from tool_input.
    raw = '{"tool_name":"Edit","tool_input":{"file_path":"/repo/a.py"},"edits":null}'
    assert extract_paths(raw) == ["/repo/a.py"]


def test_hook_payload_with_scalar_edits_keeps_tool_input_path():
    raw = '{"tool_input":{"file_path":"/repo/a.py"},"edits":5}'
    assert extract_paths(raw) == ["/repo/a.py"]


def test_multiedit_payload_collects_all_paths():
    raw = (
        '{"tool_input":{"file_path":"/repo/a.py"},'
        '"edits":[{"file_path":"/repo/b.py"}]}'
    )
    assert extract_paths(raw) == ["/repo/a.py", "/repo/b.py"]


def test_top_level_file_path():
    assert extract_paths('{"file_path":"/repo/a.py"}') == ["/repo/a.py"]


def test_plain_text_still_falls_back_to_lines():
    assert extract_paths("/repo/a.py\n\n/repo/b.py\n") == ["/repo/a.py", "/repo/b.py"]
