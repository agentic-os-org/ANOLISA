# SSE 行结束

[English](sse-line-endings.md)

两个 SSE parser 均按 [SSE 格式](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)
识别 LF、CRLF 和独立 CR。CRLF 是一个结束符，连续 CR 则是独立的行结束。

共享分行迭代器读取原始字节，因此零拷贝 parser 的 data 偏移及 legacy
parser 的 consumed-byte 计数都包含实际结束符长度，多字节文本也不改变
坐标。未终止的行仍不完整，调用方继续用已有 continuation buffer 保留
跨读取字节；不引入 parser 所有的连接状态，也不改变多行 data 的归属。

读取末尾的 CR 立即终止该行。若下次读取以 LF 开始，该空行不会产生没有
载荷的幻影事件。零拷贝 parser 在缓冲区结束时派发已完整字段的既有行为
保留，本次不重新设计 EOF 事件派发。

回归比较相同 Anthropic 用量在三种换行下经过解析、聚合、分析和语义构造
的结果，也覆盖混合换行、跨读取拆开的 CRLF、原始偏移及未终止的 UTF-8 尾部。
