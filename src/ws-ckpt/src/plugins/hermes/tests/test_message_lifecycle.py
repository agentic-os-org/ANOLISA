"""Checkpoint prompt ownership through the registered Hermes hook lifecycle."""

from unittest.mock import MagicMock, PropertyMock, patch

import pytest

import hermes
from hermes.checkpoint_manager import CheckpointManager, CheckpointResult
from hermes.config import HermesPluginConfig


@pytest.fixture
def lifecycle():
    hooks = {}
    context = MagicMock()
    context.register_hook.side_effect = lambda name, hook: hooks.__setitem__(name, hook)
    hermes.register(context)
    config = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
    manager = MagicMock(spec=CheckpointManager)
    type(manager).config = PropertyMock(return_value=config)
    manager.skip_next_auto_checkpoint = False
    manager.advance_turn.return_value = 1
    manager.create_checkpoint.return_value = CheckpointResult(success=True, message="ok", snapshot="snapshot")
    with patch("hermes.get_manager", return_value=manager):
        yield hooks, manager, config


def messages(manager):
    return [call.kwargs["message"] for call in manager.create_checkpoint.call_args_list]


def test_interleaved_sessions_keep_their_own_checkpoint_messages(lifecycle):
    hooks, manager, _ = lifecycle
    hooks["pre_llm_call"](session_id="interleaved-a", user_message="question from A")
    hooks["pre_llm_call"](session_id="interleaved-b", user_message="question from B")
    hooks["on_session_end"](session_id="interleaved-a")
    hooks["on_session_end"](session_id="interleaved-b")
    assert messages(manager) == ["question from A", "question from B"]


def test_uncaptured_session_does_not_consume_another_sessions_prompt(lifecycle):
    hooks, manager, _ = lifecycle
    hooks["pre_llm_call"](session_id="captured", user_message="captured question")
    hooks["on_session_end"](session_id="uncaptured")
    hooks["on_session_end"](session_id="captured")
    assert messages(manager) == ["agent turn", "captured question"]


def test_turn_end_consumes_the_prompt_including_legacy_no_session_id(lifecycle):
    hooks, manager, _ = lifecycle
    hooks["pre_llm_call"](user_message="legacy question")
    hooks["on_session_end"]()
    hooks["on_session_end"]()
    assert messages(manager) == ["legacy question", "agent turn"]


@pytest.mark.parametrize("reason", ["disabled", "skipped"])
def test_skipped_checkpoint_cannot_leak_its_prompt_into_a_later_turn(lifecycle, reason):
    hooks, manager, config = lifecycle
    hooks["pre_llm_call"](session_id=reason, user_message="old prompt")
    if reason == "disabled":
        config.auto_checkpoint = False
    else:
        manager.skip_next_auto_checkpoint = True
    hooks["on_session_end"](session_id=reason)
    manager.create_checkpoint.assert_not_called()
    config.auto_checkpoint = True
    hooks["on_session_end"](session_id=reason)
    assert messages(manager) == ["agent turn"]


def test_session_start_discards_pending_text_for_that_session_only(lifecycle):
    hooks, manager, config = lifecycle
    hooks["pre_llm_call"](session_id="restarted", user_message="stale question")
    hooks["pre_llm_call"](session_id="ongoing", user_message="ongoing question")
    config.auto_checkpoint = False
    hooks["on_session_start"](session_id="restarted")
    config.auto_checkpoint = True
    hooks["on_session_end"](session_id="restarted")
    hooks["on_session_end"](session_id="ongoing")
    assert messages(manager) == ["agent turn", "ongoing question"]
