# TCP Plain-Text Capture (tcpsniff) 集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

### 探针加载与内核适配

1. 配置含 `http` 规则时，tcpsniff BPF 探针应被加载并 attach（日志含 "TcpSniff: attached 3 BPF programs"）
2. 配置 `http` 为空或不存在时，tcpsniff 探针不应被加载（日志含 "TcpSniff probe disabled"）
3. 在 kernel 5.18+ 上，应使用新签名（日志含 "loaded with new tcp_recvmsg signature (5.18+)"）
4. 在 kernel 5.8–5.17 上，应自动回退到旧签名（日志含 "loaded with old tcp_recvmsg signature (5.8-5.17)"）

### 请求捕获 (tcp_sendmsg)

5. 向目标 IP/端口发送 HTTP 请求时，应捕获并解析为 Request 事件（日志含 "Aggregating parsed results(1): Request"）
6. 捕获的 Request 应包含完整 HTTP headers + body（判定：GenAI 事件中 `input_messages` 非空，`raw_body` 不含 `\x00\x00` 前缀乱码）
7. 非目标的 TCP 流量不应被捕获（配置 `http: [{"rule": [":8080"]}]`，向其他端口发请求不应产生事件）

### 响应捕获 (tcp_recvmsg)

8. 目标 IP/端口的 HTTP 响应应被捕获并解析为 Response 事件（日志含 "Aggregating parsed results(1): Response"）
9. SSE 流式响应应被正确拆分为多个 SseEvent（日志含 "SseEvent, SseEvent, SseEvent"）
10. 响应内容不应出现乱码/重复/交错（判定：GenAI 事件中 `output_messages` 内容完整且无重复字符）

### 端到端数据正确性

11. 捕获的 LLM 调用应提取到正确的 token 用量（判定：`/api/sessions` 返回 `total_input_tokens > 0` 且 `total_output_tokens > 0`）
12. 不同用户请求应分配不同的 `conversation_id`（判定：`/api/sessions/{id}/traces` 返回多条 trace，每条有独立 `conversation_id`）
13. `user_query` 应从请求 body 的 messages 数组中正确提取（判定：`/api/sessions/{id}/traces` 中 `user_query` 字段与实际发送内容匹配）

### 配置

14. JSON 配置文件中 `"http": [{"rule": [":8080", "10.0.0.1:9090"]}]` 应正确设置目标（支持仅端口、仅 IP、IP+端口、域名四种格式）
15. `http` 规则支持自动识别：IP/端口格式直接写入 BPF map，域名格式通过 DNS 解析后写入

## 运行条件

- root 权限
- Linux kernel >= 5.8 with BTF（`/sys/kernel/btf/vmlinux` 存在）
- 目标 IP/端口上有可接收 HTTP 请求的服务运行（如 Higress gateway 或简单 HTTP echo server）
