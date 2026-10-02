# UDP DNS 集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

1. 配置含 `https` 规则时，UDP DNS BPF 探针应被加载并 attach（日志中无 "UDP DNS probe disabled"）
2. 配置不含 `https` 规则时，UDP DNS BPF 探针不应被加载（日志中出现 "UDP DNS probe disabled"）
3. DNS 查询匹配 `https` 规则的域名时，应触发 SSL 探针 attach 到该进程
4. DNS 查询不匹配 `https` 规则的域名时，不应触发 attach
5. 被 `cmdline deny` 规则匹配的进程，即使域名匹配也不应 attach

## 运行条件

- root 权限
- Linux kernel >= 5.8 with BTF
- 网络可达（需发起外部 DNS 查询）
