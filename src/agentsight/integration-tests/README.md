# AgentSight 集成测试用例

本目录存放各模块的接口级集成测试描述。每个文件用自然语言描述测试目标和断言条件，具体执行过程由测试 agent 自行分析代码确定。

**执行任何测试前，先阅读 `RULES.md` 了解环境信息和通用规则。**

## 文件列表

| 文件 | 说明 |
|------|------|
| `RULES.md` | 测试环境、部署流程、通用规则 |
| `TEMPLATE.md` | 新建测试用例的模板 |
| `test_dns.md` | UDP DNS 探针：域名→IP 解析捕获 |
| `test_hermes_dns.md` | 通过 DNS 捕获 Hermes agent（dashscope.aliyuncs.com） |
| `test_http.md` | 明文 HTTP（tcpsniff）流量捕获 |
| `test_connection_scanner.md` | 连接扫描器：活跃连接发现 |
| `test_pid_namespace.md` | 容器内目标进程的 pid 解析（DaemonSet + hostPID 形态） |
| `test_ffi_integration.md` | C FFI API 集成 |
| `test_claude_code.md` | Claude Code BoringSSL 探针、SSE thinking/tool_use 解析、msg_id 会话关联 |
