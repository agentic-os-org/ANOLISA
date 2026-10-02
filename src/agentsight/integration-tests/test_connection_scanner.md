# 连接扫描（Connection Scanner）集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

1. 配置精确域名且已有进程与该域名建立 TCP 连接时，应自动 attach（日志：`Connection scan: attached N process(es)`）
2. 仅配置通配符域名时，不应触发连接扫描（日志：`no IPs resolved from domain rules, skipping`）
3. 被 deny 规则覆盖的进程不应被 attach（日志：`denied by rule, skipping`）
4. 已被 cmdline 扫描发现的进程不应被重复 attach

## 运行条件

- root 权限
- Linux kernel >= 5.8 with BTF
- 网络可达（DNS 解析 + HTTPS 连接建立所需）
- 测试机器能解析 `dashscope.aliyuncs.com`（无需有效 API Key，只需 TCP 连接建立）
