# SSE line endings

[中文版](sse-line-endings_zh.md)

Both SSE parsers recognize LF, CRLF, and standalone CR as line endings,
following the [SSE wire format](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream).
CRLF is one terminator; consecutive CR characters are separate line endings.

The shared line iterator reads original bytes. The zero-copy parser's data
offsets and the legacy parser's consumed-byte count therefore include the
actual terminator length, even around multibyte text. An unterminated line
remains incomplete; callers still retain cross-read bytes in their existing
continuation buffers. This does not introduce parser-owned connection state
or change multiline-data ownership.

A CR at the end of a read terminates that line immediately. If the next read
begins with LF, that empty line cannot create a payload-less phantom event.
The existing zero-copy behavior of emitting complete fields at end of a
buffer is retained; this change does not redesign event dispatch at EOF.

Regression coverage compares identical Anthropic usage under all three
endings through parsing, aggregation, analysis, and semantic construction.
It also covers mixed endings, CRLF split across reads, raw offsets, and
unterminated UTF-8 tails.
