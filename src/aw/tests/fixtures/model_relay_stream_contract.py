"""SSE framing, recovery, and diagnostic contract tests for ModelRelay."""

import asyncio
from contextlib import redirect_stderr
import io
import json
from unittest.mock import patch

import httpx

from aw_invocations import digest
from aw_model import MAX_FRAME


START = b'data: {"type":"message_start","message":{"content":[]}}\n\n'
STOP = b'data: {"type":"message_stop"}\n\n'


def tool_frames(partial, index=0):
    events = [
        {"type": "content_block_start", "index": index, "content_block": {
            "type": "tool_use", "id": "native", "name": "tool", "input": {"initial": 1}}},
        {"type": "content_block_delta", "index": index, "delta": {
            "type": "input_json_delta", "partial_json": partial}},
        {"type": "content_block_stop", "index": index},
    ]
    return [b"data: " + json.dumps(event).encode() for event in events]


class StreamContractMixin:
    async def test_partial_sse_never_registers_an_incomplete_tool(self):
        self.content_type = "text/event-stream"
        self.body = b'data: {"type":"message_start","message":{"content":[]}}\n\n'
        await self.post()
        await asyncio.wait_for(self.stopped.wait(), 1)
        self.assertFalse(self.bridge.invocations.active)

    async def test_mixed_sse_line_endings_register_tool_with_split_crlf(self):
        start, delta, stop = tool_frames('{"value":1}')
        chunks = [
            b'\xef\xbb\xbfdata: {"type":"message_start",\r', b'\ndata: "message":{"content":[]}}\r',
            b'\n\n: heartbeat\r\rdata:\ndata:\n\ndata\r\r',
            start + b"\n\r", b"\n" + delta + b"\r\r" + stop + b"\r\n\n" + STOP,
        ]

        class Chunks(httpx.AsyncByteStream):
            async def __aiter__(self):
                for chunk in chunks:
                    yield chunk

        await self.relay.http.aclose()
        self.relay.http = httpx.AsyncClient(transport=httpx.MockTransport(
            lambda _: httpx.Response(200, headers={"content-type": " Text/Event-Stream ; charset=utf-8"},
                                     stream=Chunks())))
        response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        self.assertEqual([event["type"] for event in events], [
            "message_start", "content_block_start", "content_block_delta", "content_block_stop", "message_stop"])
        entry = self.bridge.invocations.active[events[1]["content_block"]["id"]]
        self.assertNotEqual(entry.call_id, "native")
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest({"value": 1})))
        self.assertIsNone(self.bridge.failure)

    async def test_post_stop_marker_and_heartbeats_are_discarded(self):
        self.content_type = "Text/Event-Stream ; charset=utf-8"
        for tail in (b"data: [DONE]\n\n", b"data: [DONE]", b": heartbeat", b"data: [DONE]\r\r: heartbeat\n\n"):
            with self.subTest(tail=tail):
                self.body = b": heartbeat\n\ndata:\n\ndata:   \ndata:\n\n" + START + STOP + tail
                response = await self.post()
                self.assertEqual(response.content.count(b"event: "), 2)
                self.assertNotIn(b"[DONE]", response.content)
                self.assertNotIn(b"heartbeat", response.content)
                self.assertIsNone(self.bridge.failure)

    async def test_compatibility_warnings_are_content_free_and_per_request(self):
        self.content_type = "text/event-stream; token=private-header"
        blank = b"event: private-model\ndata:  \ndata:\n\n"
        done = b": private-prompt\ndata: [DONE]\n\n"
        tools = [line.replace(b'"initial": 1', b'"initial": "private-tool"')
                 for index in range(2) for line in tool_frames("   ", index)]
        self.body = START + blank * 2 + b"".join(line + b"\n\n" for line in tools) + STOP + done * 2
        for has_tools in (True, False):
            log = io.StringIO()
            with redirect_stderr(log):
                await self.post(b'{"messages":[{"role":"user","content":"private-prompt"}]}')
            expected = [
                "AW managed model response warning: reason=blank_data data_bytes=2 data_lines=2",
            ]
            if has_tools:
                expected.append("AW managed model response warning: reason=blank_tool_input partial_bytes=3")
            expected.append(f"AW managed model response warning: reason=trailing_done frame_bytes={len(done)-1}")
            self.assertEqual(log.getvalue().splitlines(), expected)
            self.assertIsNone(self.bridge.failure)
            self.body = START + blank + STOP + done

    async def test_standard_stream_is_quiet(self):
        self.content_type = "text/event-stream"
        self.body = b": keep-alive\r\n\r\n" + START + b"".join(
            line + b"\r\n\r\n" for line in tool_frames("")) + STOP + b": heartbeat"
        log = io.StringIO()
        with redirect_stderr(log):
            await self.post()
        self.assertEqual(log.getvalue(), "")
        self.assertIsNone(self.bridge.failure)

    async def test_blank_tool_input_uses_initial_object(self):
        self.content_type = "text/event-stream"
        self.body = START + b"".join(line + b"\n\n" for line in tool_frames(" \t\r\n")) + STOP
        response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        start, *deltas, stop = events[1:-1]
        self.assertEqual(start["content_block"]["input"], {})
        self.assertEqual(stop["type"], "content_block_stop")
        self.assertEqual(events[-1]["type"], "message_stop")
        arguments = json.loads("".join(event["delta"]["partial_json"] for event in deltas))
        self.assertEqual(arguments, {"initial": 1})
        entry = self.bridge.invocations.active[start["content_block"]["id"]]
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest(arguments)))
        self.assertEqual(self.bridge.scope.registered, 1)

    async def failed_request(self, *fields, content=b'{"messages":[]}', before_headers=False):
        log = io.StringIO()
        with redirect_stderr(log):
            if before_headers:
                with self.assertRaises(httpx.RemoteProtocolError):
                    await self.post(content)
                body = b""
            else:
                body = (await self.post(content)).content
            await asyncio.wait_for(self.stopped.wait(), 1)
        line = log.getvalue()
        self.assertIn("AW managed model response failed: ", line)
        for field in fields:
            self.assertIn(field, line)
        for private in ("fixture-only", "private-prompt", "private-model", "private-tool",
                        "private-header", self.relay.upstream, self.endpoint, self.relay.secret):
            self.assertNotIn(private, line)
        self.assertIsNotNone(self.bridge.failure)
        self.assertEqual(self.bridge.scope.registered, 0)
        return body

    async def test_request_json_diagnostics(self):
        await self.failed_request("JSONDecodeError", "stage=request_json", "json_err=Extra data",
                                  "json_pos=3", content=b'{} private-prompt', before_headers=True)
        self.assertFalse(self.requests)

    async def test_response_json_diagnostics(self):
        self.body = b'{"private-model":'
        self.content_type = "application/x-private-header; token=fixture-only"
        body = await self.failed_request("JSONDecodeError", "stage=response_json", "status=200",
                                         "content_type=other", "json_err=Expecting value", "json_pos=17")
        self.assertEqual(body, b"")

    async def test_sse_frame_diagnostics(self):
        self.content_type = "text/event-stream; token=fixture-only"
        self.body = START + b"event: private-header\ndata:   {}\ndata: private-model\n\n" + STOP
        body = await self.failed_request("JSONDecodeError", "stage=sse_frame", "sse_event=other",
                                         "status=200", "content_type=text/event-stream", "data_lines=2",
                                         "data_bytes=18", "frame_bytes=", "json_err=Extra data", "json_pos=5",
                                         "payload_kind=non_json", "stream_started=1", "stream_stopped=0")
        self.assertNotIn(b"private-model", body)
        self.assertNotIn(b"message_stop", body)

    async def test_vertical_tab_is_invalid_sse_data(self):
        self.content_type = "text/event-stream"
        self.body = START + b"data: \x0b\n\n" + STOP
        body = await self.failed_request("JSONDecodeError", "stage=sse_frame", "payload_kind=non_json")
        self.assertNotIn(b"message_stop", body)

    async def test_nonbreaking_space_tool_input_recovers_with_empty_arguments(self):
        self.content_type = "text/event-stream"
        self.body = START + b"".join(line + b"\n\n" for line in tool_frames("\u00a0")) + STOP
        log = io.StringIO()
        with redirect_stderr(log):
            response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        self.assertEqual([event["type"] for event in events], [
            "message_start", "content_block_start", "content_block_delta",
            "content_block_stop", "message_stop",
        ])
        entry = self.bridge.invocations.active[events[1]["content_block"]["id"]]
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest({})))
        self.assertEqual(self.bridge.scope.registered, 1)
        self.assertEqual(log.getvalue().splitlines(), [
            "AW managed model response warning: reason=invalid_tool_input "
            "partial_bytes=2 json_err=Expecting value json_pos=0",
        ])
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())

    async def test_premature_done_fails_closed(self):
        self.content_type, self.body = "text/event-stream", START + b"data: [DONE]\n\n" + STOP
        body = await self.failed_request("stage=sse_frame", "payload_kind=done")
        self.assertNotIn(b"[DONE]", body)
        self.assertNotIn(b"message_stop", body)

    async def test_event_after_stop_fails_closed(self):
        self.content_type, self.body = "text/event-stream", START + STOP + b'data: {"type":"ping"}\n\n'
        body = await self.failed_request("stage=stream_event", "sse_event=ping", "stream_stopped=1")
        self.assertNotIn(b"ping", body)

    async def test_malformed_eof_after_stop_fails_closed(self):
        self.content_type, self.body = "text/event-stream", START + STOP + b"data: private-model"
        body = await self.failed_request("stage=sse_frame", "payload_kind=non_json", "stream_stopped=1")
        self.assertNotIn(b"private-model", body)

    async def test_truncated_tool_input_recovers_with_content_free_warning(self):
        self.content_type = "text/event-stream"
        self.body = START + b"".join(line + b"\n\n" for line in tool_frames('{"private-tool":')) + STOP
        log = io.StringIO()
        with redirect_stderr(log):
            response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        self.assertEqual([event["type"] for event in events], [
            "message_start", "content_block_start", "content_block_delta",
            "content_block_stop", "message_stop",
        ])
        entry = self.bridge.invocations.active[events[1]["content_block"]["id"]]
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest({})))
        self.assertEqual(self.bridge.scope.registered, 1)
        self.assertEqual(log.getvalue().splitlines(), [
            "AW managed model response warning: reason=invalid_tool_input "
            "partial_bytes=16 json_err=Expecting value json_pos=16",
        ])
        self.assertNotIn("private-tool", log.getvalue())
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())

    async def test_extra_data_tool_input_recovers_and_retires_on_error_result(self):
        self.content_type = "text/event-stream"
        payload = '{"command":"echo hi"}{"extra":"private-tool"}'
        self.body = START + b"".join(line + b"\n\n" for line in tool_frames(payload)) + STOP
        log = io.StringIO()
        with redirect_stderr(log):
            response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        call_id = events[1]["content_block"]["id"]
        entry = self.bridge.invocations.active[call_id]
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest({})))
        self.assertEqual(log.getvalue().splitlines(), [
            f"AW managed model response warning: reason=invalid_tool_input "
            f"partial_bytes={len(payload.encode())} json_err=Extra data json_pos=21",
        ])
        self.assertNotIn("private-tool", log.getvalue())
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())
        # Native CLI validation failure returns is_error: true and retires cleanly on the next turn.
        self.body = START + STOP
        followup = json.dumps({
            "messages": [{
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": call_id, "is_error": True}],
            }]
        }).encode()
        self.assertEqual((await self.post(followup)).status_code, 200)
        self.assertFalse(self.bridge.invocations.active)
        self.assertIsNone(self.bridge.failure)

    async def test_non_object_tool_input_recovers_with_empty_arguments(self):
        self.content_type = "text/event-stream"
        for payload in ("[1, 2, 3]", '"private-tool"', "123", "null"):
            with self.subTest(payload=payload):
                self.body = START + b"".join(line + b"\n\n" for line in tool_frames(payload)) + STOP
                log = io.StringIO()
                with redirect_stderr(log):
                    response = await self.post()
                events = [json.loads(line[6:]) for line in response.content.splitlines()
                          if line.startswith(b"data: ")]
                call_id = events[1]["content_block"]["id"]
                entry = self.bridge.invocations.active[call_id]
                self.assertEqual((entry.phase, entry.arguments), ("issued", digest({})))
                self.assertEqual(log.getvalue().splitlines(), [
                    "AW managed model response warning: reason=non_object_tool_input "
                    f"partial_bytes={len(payload.encode())}",
                ])
                self.assertNotIn("private-tool", log.getvalue())
                self.assertIsNone(self.bridge.failure)
                self.assertFalse(self.stopped.is_set())
                self.bridge.invocations.active.pop(call_id)

    async def test_upstream_stream_error_with_and_without_open_tool_reuses_relay(self):
        self.content_type = "text/event-stream"
        err_event = b'data: {"type":"error","error":{"type":"overloaded_error","message":"private-model"}}\n\n'
        # Mid-stream error without open blocks.
        self.body = START + err_event
        log = io.StringIO()
        with redirect_stderr(log):
            response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        self.assertEqual([event["type"] for event in events], ["message_start", "error"])
        self.assertEqual(log.getvalue().splitlines(), [
            "AW managed model response warning: reason=upstream_stream_error",
        ])
        self.assertNotIn("private-model", log.getvalue())
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())

        # Mid-stream error with an open unregistered tool_use block drops it without leaking.
        start, delta, _ = tool_frames('{"partial":')
        self.body = START + start + b"\n\n" + delta + b"\n\n" + err_event
        log = io.StringIO()
        with redirect_stderr(log):
            response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        self.assertEqual([event["type"] for event in events], [
            "message_start", "content_block_start", "content_block_delta", "error",
        ])
        self.assertFalse(self.bridge.invocations.active)
        self.assertEqual(self.bridge.scope.registered, 0)
        self.assertEqual(log.getvalue().splitlines(), [
            "AW managed model response warning: reason=upstream_stream_error",
        ])
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())

        # The same relay remains usable for a subsequent tool turn.
        self.body = START + b"".join(line + b"\n\n" for line in tool_frames('{"ok":1}')) + STOP
        response = await self.post()
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.bridge.scope.registered, 1)
        self.assertIsNone(self.bridge.failure)

    async def test_event_after_stream_error_fails_closed(self):
        self.content_type = "text/event-stream"
        err_event = b'data: {"type":"error","error":{"type":"overloaded_error","message":"private-model"}}\n\n'
        self.body = START + err_event + b'data: {"type":"ping"}\n\n'
        body = await self.failed_request("stage=stream_event", "sse_event=ping", "stream_stopped=1")
        self.assertNotIn(b"ping", body)

    async def test_non_contiguous_block_index_allowed_while_reopen_fails_closed(self):
        self.content_type = "text/event-stream"
        text_events = [
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_stop", "index": 0},
        ]
        frames = [b"data: " + json.dumps(ev).encode() for ev in text_events] + tool_frames('{"v":1}', index=2)
        self.body = START + b"".join(line + b"\n\n" for line in frames) + STOP
        response = await self.post()
        events = [json.loads(line[6:]) for line in response.content.splitlines()
                  if line.startswith(b"data: ")]
        tool_start = [ev for ev in events if ev["type"] == "content_block_start" and ev["index"] == 2][0]
        entry = self.bridge.invocations.active[tool_start["content_block"]["id"]]
        self.assertEqual((entry.phase, entry.arguments), ("issued", digest({"v": 1})))
        self.assertIsNone(self.bridge.failure)

        # Reopening an earlier index (1 after 2) still fails closed.
        self.bridge.scope.registered = 0
        reopen = [
            b'data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}',
            b'data: {"type":"content_block_stop","index":2}',
            b'data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}',
        ]
        self.body = START + b"".join(line + b"\n\n" for line in reopen) + STOP
        await self.failed_request("stage=stream_event", "sse_event=content_block_start")

    async def test_missing_message_start_content_allowed_while_non_empty_fails_closed(self):
        self.content_type = "text/event-stream"
        self.body = b'data: {"type":"message_start","message":{}}\n\n' + STOP
        response = await self.post()
        self.assertEqual(response.status_code, 200)
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())

        # Non-empty pre-populated content still fails closed without leaking message text.
        self.body = (
            b'data: {"type":"message_start","message":{"content":[{"type":"text","text":"private-model"}]}}\n\n'
            + STOP
        )
        await self.failed_request("stage=stream_event", "sse_event=message_start")

    async def test_unterminated_oversized_line_fails_closed(self):
        self.content_type, self.body = "text/event-stream", START + b"x" * (MAX_FRAME + 1)
        body = await self.failed_request("stage=sse_frame", "frame_bytes=")
        self.assertNotIn(b"xxxx", body)

