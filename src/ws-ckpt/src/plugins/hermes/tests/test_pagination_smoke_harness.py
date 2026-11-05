"""The native acceptance harness must accept valid timing and short-page differences."""

import importlib.util
import json
from pathlib import Path
from unittest.mock import Mock

import pytest

_path = Path(__file__).resolve().parents[4] / "tests" / "hermes_pagination_smoke.py"
_spec = importlib.util.spec_from_file_location("pagination_smoke", _path)
smoke = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(smoke)


def test_table_ignores_only_final_timing_footer():
    assert smoke.stable_table("snapshot A\nCompleted in 0.003s") == smoke.stable_table(
        "snapshot A\nCompleted in 0.005s"
    )
    assert smoke.stable_table("snapshot A\nCompleted in 0.003s") != smoke.stable_table(
        "snapshot B\nCompleted in 0.003s"
    )
    assert smoke.stable_table("Completed in 0.003s\nsnapshot A") == (
        "Completed in 0.003s\nsnapshot A"
    )
    assert smoke.stable_table("snapshot A\nCompleted in unknown") == (
        "snapshot A\nCompleted in unknown"
    )


def test_continuation_accepts_byte_limited_page_and_preserves_cursor():
    page = {
        "snapshots": [{"id": "second", "metadata": "x" * (600 * 1024)}],
        "next_cursor": "still-more",
    }
    call = Mock(return_value={"success": True, **page})
    cli = Mock(return_value=json.dumps({**page, "elapsed_secs": 0.005}))
    smoke.check_continuation(call, cli, Path("/workspace"), " opaque cursor ")
    call.assert_called_once_with({"cursor": " opaque cursor "})
    cli.assert_called_once_with(
        "list", "-w", "/workspace", "--format", "json", "--cursor", " opaque cursor "
    )


@pytest.mark.parametrize("field,value", [("snapshots", []), ("next_cursor", None)])
def test_continuation_rejects_different_content_or_cursor(field, value):
    page = {"snapshots": [{"id": "second"}], "next_cursor": "more"}
    call = Mock(return_value={"success": True, **page, field: value})
    cli = Mock(return_value=json.dumps(page))
    with pytest.raises(AssertionError):
        smoke.check_continuation(call, cli, Path("/workspace"), "cursor")
