# Qoder CLI 网关观测

[English](qoder-gateway.md)

Qoder CLI 的 HTTP/SSE 聊天端点使用自定义 Base64 字符表、美元符号填充和
首尾三段交换来编码请求体。这是可逆编码，仅捕获 TLS 明文还不能得到 JSON。

现有静态 SSL 挂载流程处理 Bun 打包的 Qoder 二进制，无需新增 BPF 程序、
认证实现或 API 网关。消息适配器解码请求，将独立的 system prompt、
model configuration 和 parameters 转换为 OpenAI 请求模型。解码后的原始请求
上下文保留原生 session ID，供 GenAI 归属判定使用。

响应可能将每个 OpenAI 形状的 chunk 封装在 JSON 对象中，实际 chunk 位于
字符串 `body` 内。仅在存在 `statusCode` 或 `statusCodeValue` 封装标记时拆包。
共享 parser helper 使原始 SSE、压缩归并、Token 提取和所有权交付读取相同的
payload。对象形式的工具参数在类型化消息解析前规范化，不改变其他供应商的参数处理。

集成测试回放合成采集记录，覆盖跨记录的封装、文本、thinking、对象工具参数及
最终仅含 usage 的 chunk。Input tokens 已包含缓存命中；缓存单独报告，不再增加总量。

两种模式复用有界组帧：原始模式保留完整事件，压缩模式归并增量。
HTTP 分块结束无需 SSE `[DONE]`，两种模式保留帧完成时的时间戳。
超出预算的流被显式拒绝，Qoder 配置订阅不记为 LLM 调用。

覆盖范围限于 `/algo/api/v2/service/pro/sse/agent_chat_generation`。
Qoder IDE、QoderWork、gRPC 和 HTTP/3 需要单独的兼容证据。
进程分类复用现有 SSL 解析器；去符号二进制仍需可用符号、可识别模式或配置的 offset。
删除 Qoder 发现规则可停用采集，也可以回滚本次变更。
