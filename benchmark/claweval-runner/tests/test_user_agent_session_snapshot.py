"""UserAgent turn state and conversation share one recorded session snapshot."""

import builtins
import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType

import pytest


@pytest.fixture
def agent(monkeypatch: pytest.MonkeyPatch):
    common = ModuleType("ce_runner._common")
    common.OPENCLAW_CONFIG = "unused"
    common.SESSIONS_DIR = "unused"
    common._agent_sessions_dir = lambda *args: "unused"
    common.load_task_yaml = lambda *args: {
        "user_agent": {"persona": "test", "max_rounds": 3}
    }
    warnings = []
    common.log = warnings.append
    monkeypatch.setitem(sys.modules, "ce_runner", ModuleType("ce_runner"))
    monkeypatch.setitem(sys.modules, "ce_runner._common", common)
    path = Path(__file__).parents[1] / "src/ce_runner/agent.py"
    spec = importlib.util.spec_from_file_location("ce_runner.agent", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, warnings


def message(role, content):
    return {"type": "message", "message": {"role": role, "content": content}}


def write_session(path: Path, rows: list) -> None:
    path.write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows),
        encoding="utf-8",
    )


@pytest.mark.parametrize(
    "invalid",
    [
        None,
        [],
        7,
        {"type": "message", "message": None},
        message("assistant", [None, "bad", {"type": "text", "text": 42}]),
    ],
)
def test_bad_record_does_not_discard_later_conversation(agent, tmp_path: Path, invalid):
    module, warnings = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path, [message("user", "before"), invalid, message("assistant", "after")]
    )
    assert module._build_conversation_for_user_agent(str(path)) == [
        {"role": "user", "text": "before"},
        {"role": "assistant", "text": "after"},
    ]
    assert warnings


def test_latest_plain_text_assistant_clears_previous_tool_state(agent, tmp_path: Path):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path,
        [
            message("assistant", [{"type": "toolCall", "name": "read"}]),
            message("assistant", "done"),
        ],
    )
    assert module._get_last_assistant_has_tool_calls(str(path)) is False


def test_bad_record_does_not_hide_later_tool_state(agent, tmp_path: Path):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path,
        [
            message("assistant", "before"),
            None,
            message("assistant", [{"type": "toolCall", "name": "read"}]),
        ],
    )
    assert module._get_last_assistant_has_tool_calls(str(path)) is True


def test_valid_text_blocks_and_tools_preserve_existing_projection(
    agent, tmp_path: Path
):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path,
        [
            message("user", [{"type": "text", "text": "first"}, {"type": "image"}]),
            message("toolResult", [{"type": "text", "text": "hidden"}]),
            message(
                "assistant",
                [
                    {"type": "text", "text": "one"},
                    {"type": "toolCall", "name": "read"},
                    {"type": "text", "text": "two"},
                ],
            ),
        ],
    )
    assert module._get_last_assistant_has_tool_calls(str(path)) is True
    assert module._build_conversation_for_user_agent(str(path)) == [
        {"role": "user", "text": "first"},
        {"role": "assistant", "text": "one\ntwo"},
    ]


def test_each_dialogue_round_reads_one_snapshot(agent, tmp_path: Path, monkeypatch):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(path, [message("user", "question"), message("assistant", "answer")])
    original_open = builtins.open
    reads = []

    def opened(file, *args, **kwargs):
        if Path(file) == path:
            reads.append(file)
        return original_open(file, *args, **kwargs)

    monkeypatch.setattr(builtins, "open", opened)
    monkeypatch.setattr(module, "run_agent", lambda *args, **kwargs: str(path))
    conversations = []
    monkeypatch.setattr(
        module,
        "_call_user_agent_llm",
        lambda cfg, persona, history: conversations.append(history),
    )
    assert module.run_agent_with_user_agent("id", "unused", 10, {}) == str(path)
    assert len(reads) == 1
    assert conversations == [
        [{"role": "user", "text": "question"}, {"role": "assistant", "text": "answer"}]
    ]


def test_no_user_agent_call_for_in_progress_snapshot(
    agent, tmp_path: Path, monkeypatch
):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(path, [message("assistant", [{"type": "toolCall", "name": "read"}])])
    monkeypatch.setattr(module, "run_agent", lambda *args, **kwargs: str(path))
    monkeypatch.setattr(
        module,
        "_call_user_agent_llm",
        lambda *args: pytest.fail("tool work is still in progress"),
    )
    assert module.run_agent_with_user_agent("id", "unused", 10, {}) == str(path)


def test_unreadable_session_keeps_empty_defaults_with_diagnostic(agent, tmp_path: Path):
    module, warnings = agent
    path = tmp_path / "missing.jsonl"
    assert module._build_conversation_for_user_agent(str(path)) == []
    assert module._get_last_assistant_has_tool_calls(str(path)) is False
    assert any("missing.jsonl" in item for item in warnings)


def test_malformed_blocks_retain_valid_text_and_tool_evidence(agent, tmp_path: Path):
    module, warnings = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path,
        [
            message(
                "assistant",
                [
                    None,
                    {"type": "text", "text": "usable"},
                    {"type": "text", "text": 42},
                    {"type": "toolCall", "name": "read"},
                ],
            )
        ],
    )
    assert module._get_last_assistant_has_tool_calls(str(path)) is True
    assert module._build_conversation_for_user_agent(str(path)) == [
        {"role": "assistant", "text": "usable"}
    ]
    assert len(warnings) == 2  # One warning per read, rather than per malformed block.


def test_invalid_content_does_not_clear_known_in_progress_state(agent, tmp_path: Path):
    module, _ = agent
    path = tmp_path / "session.jsonl"
    write_session(
        path,
        [
            message("assistant", [{"type": "toolCall", "name": "read"}]),
            message("assistant", [None]),
        ],
    )
    assert module._get_last_assistant_has_tool_calls(str(path)) is True
