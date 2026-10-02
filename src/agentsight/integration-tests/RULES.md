# 集成测试通用规则

## 用户变量

以下变量因人而异，执行测试前需确认或由用户提供：

| 变量 | 说明 | 示例 |
|------|------|------|
| `TEST_HOST` | 测试机器地址 | `local` 或 `root@<your-test-ip>` |

`TEST_HOST` 支持两种模式：

- **`local`**: 在本机直接执行测试，无需 SSH
- **SSH 地址**（如 `root@10.0.0.1`）：通过 SSH 连接远程机器执行测试

## 测试环境

- **测试机器**: `$TEST_HOST`（`local` 为本机，否则为远程 SSH 地址）
- **OS**: Alibaba Cloud Linux 3 (kernel 5.10.134, x86_64)
- **部署方式**: `local` 时直接本地构建运行；远程时本地构建后 scp 上传
- **二进制路径**: `/root/agentsight`

## 部署流程

- **本地模式** (`TEST_HOST=local`):
  1. 直接构建: `cargo build --release`
  2. 二进制即 `target/release/agentsight`

- **远程模式** (`TEST_HOST=<ssh-address>`):
  1. 本地构建: `cargo build --release`
  2. 上传到测试机: `scp target/release/agentsight $TEST_HOST:/root/agentsight`
  3. 后续命令通过 `ssh $TEST_HOST` 执行

## 执行前准备

执行测试前，agent 需根据测试目标阅读相关代码，了解对应模块的 CLI 参数、配置格式、日志关键字等。不要假设接口细节，以代码为准。

## 通用规则

- 所有测试需要 **root 权限**（eBPF 要求）
- 测试前确认 `agentsight --version` 能正常输出
- 测试产生的临时文件放 `/tmp/agentsight-test-*`，测试结束后清理
- 验证方式优先使用日志输出（`RUST_LOG=debug`），其次使用 API 接口查询
- 测试过程不修改代码，通过则通过，失败则失败；在测试报告中给出有助于定位和修复的分析

## 日志保存规则

测试日志**不保存原始完整输出**，只保存与测试判定相关的关键信息：

- 使用 `grep` 过滤出与测试目标直接相关的日志行（如包含特定关键字的行）
- 每个测试用例只保留能证明 PASS/FAIL 的最小日志集合（通常 5-15 行）
- 保存内容应包含：时间戳、日志级别、模块名、关键事件描述
- 示例：测试 SNI attach 时，只需保存含 `[TLS-SNI]`、`Attaching to`、`denied` 等关键字的行
- 禁止保存：证书内容、TLS 握手详情、无关进程的 attach 日志、libbpf 加载细节等

## 日志级别

- 正常运行: `RUST_LOG=info`
- 调试测试: `RUST_LOG=debug`（会输出 SNI 匹配、进程 attach 等详细信息）

## 判定标准

- **PASS**: 实际行为与测试目标描述一致
- **FAIL**: 实际行为与预期不符，需输出相关日志辅助定位
- **SKIP**: 环境不满足前提条件（如网络不通、内核版本不足）

## 测试报告

测试执行完毕后，输出一份测试报告，包含：

- 测试名称和执行时间
- 每条测试目标的结果（PASS / FAIL / SKIP）
- 每项附带**关键日志证据**（grep 过滤后的 3-10 行），而非原始输出
- 失败项额外附带分析和可能的根因
- 总结：通过数 / 失败数 / 跳过数
- 补充发现（如竞态条件、兼容性问题等）
