# SSE 流首前缀

[English](sse-stream-prefix.md)

[SSE 格式](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)
只忽略流首一个 UTF-8 BOM，保留后续字段及载荷 U+FEFF。此前首个 `data:` 前的标记
会遮蔽该事件；命名 `event:` 前的标记仍可能留下可读 JSON。不假定服务商默认带 BOM。

旧完整流解析器的消费字节计数包含被忽略字节。任意 TLS 读取保留零拷贝原行为；只有
已知首段或解压完整正文使用内部流首入口。实时 HTTP/1 的私有 helper 从保留读取中
重建首块，去重比较原始来源坐标，保留后续相同事件及元数据修复时的原始 Rc。沿用
有界读取状态缓存和全部清理入口，达到既有 1 MiB 上限后停止恢复，超大不完整前缀仍
为尽力处理。不改既有 EOF/多行行为。HTTP/2 继续使用旧完整流解析器。

合成回归覆盖偏移、BOM/字段分片、正文与头同读或分读、压缩正文、仅前缀终止、后续
相同事件、双重或后续标记、载荷 U+FEFF。不代表 BPF 采集或服务商流量验证。
