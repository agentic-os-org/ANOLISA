"""OpenClaw JSONL trace reading tolerates invalid-UTF-8 session files.

Agent transcripts routinely embed arbitrary bytes; a single bad byte in one
session file used to abort the entire analyze-traces run (every trace in the
window lost), while the sibling module agents/openclaw/tokenless_evidence.py
deliberately reads with errors="replace".
"""

import json

from swe_runner.trace_extraction.openclaw_jsonl import (
    iter_openclaw_jsonl_traces,
    reconstruct_openclaw_jsonl_session,
)

_WINDOW = (0, 9_000_000_000_000_000_000)


def _session_lines(session_id: str) -> list[bytes]:
    return [
        json.dumps(
            {"type": "session", "id": session_id, "timestamp": "2026-04-24T15:22:05.720Z"}
        ).encode(),
        json.dumps(
            {
                "type": "message",
                "id": "user-1",
                "timestamp": "2026-04-24T15:22:05.726Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": "hello"}],
                },
            }
        ).encode(),
        json.dumps(
            {
                "type": "message",
                "id": "assistant-1",
                "timestamp": "2026-04-24T15:22:06.000Z",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "hi there"}],
                    "usage": {"input": 10, "output": 5, "cacheRead": 0},
                    "model": "test-model",
                    "provider": "test-provider",
                },
            }
        ).encode(),
    ]


def _write_session(path, lines: list[bytes]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"\n".join(lines) + b"\n")


def test_session_with_invalid_utf8_byte_is_reconstructed(tmp_path) -> None:
    """一个坏字节只损坏它所在的会话文件，其余照常收集。

    修复前：path.read_text(encoding="utf-8") 对 0xff 抛
    UnicodeDecodeError，向上穿透到 CLI（只捕 ExtractionError）——
    窗口内全部 trace 丢失。
    """
    good_dir = tmp_path / "good" / "agents" / "a" / "sessions"
    _write_session(good_dir / "session-1.jsonl", _session_lines("s-good"))

    bad_dir = tmp_path / "bad" / "agents" / "a" / "sessions"
    # 0xff 永远不是合法 UTF-8 序列的一部分
    _write_session(
        bad_dir / "session-2.jsonl", _session_lines("s-bad") + [b'{"payload": "\xff"}']
    )

    traces = iter_openclaw_jsonl_traces(
        profile_dirs=[tmp_path / "good", tmp_path / "bad"],
        start_ns=_WINDOW[0],
        end_ns=_WINDOW[1],
    )
    session_ids = {t.get("session_id") for t in traces}
    assert "s-good" in session_ids


def test_single_bad_session_file_returns_none_not_crash(tmp_path) -> None:
    bad_line = b'{"payload": "\xff\xfe"}'
    _write_session(tmp_path / "only-bad.jsonl", [bad_line])

    # 坏文件被替换字符修复后仍无有效条目 → None（跳过语义），不抛异常
    assert reconstruct_openclaw_jsonl_session(tmp_path / "only-bad.jsonl") is None
