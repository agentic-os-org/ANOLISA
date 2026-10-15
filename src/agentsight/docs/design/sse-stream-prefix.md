# SSE stream prefix

[中文版](sse-stream-prefix_zh.md)

The [SSE format](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)
ignores one leading UTF-8 BOM, retaining later field/payload U+FEFF. A mark
before first `data:` previously hid the event; a named `event:` could still
leave JSON readable. Ordinary provider responses are not assumed to use BOMs.

The legacy full-stream parser counts the ignored bytes in original offsets.
Arbitrary TLS reads keep the zero-copy parser's behavior; only known initial
or decompressed bodies use its internal stream-start entry point. For live
HTTP/1, a private helper reconstructs the first block across retained reads.
Dedup uses source coordinates, preserving identical later events and the
original Rc during metadata repair. It reuses the bounded read-tracking cache
and all cleanup hooks; recovery stops at the existing 1 MiB continuation cap.
Incomplete oversized prefixes remain best-effort. Existing EOF/multiline
behavior is retained. HTTP/2 continues using the legacy full-stream parser.

Synthetic regressions cover offsets, fragmented BOM/fields, inline/separate
and compressed bodies, prefix-only completion, identical later events, double
and late marks, and payload U+FEFF. They do not claim BPF or provider capture.
