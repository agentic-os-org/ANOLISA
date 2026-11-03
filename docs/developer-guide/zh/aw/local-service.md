# 本地服务演示

[English](../../en/aw/local-service.md)

用本地 daemon 和合成工具事件检查 socket 通信与审计记录。

## 运行本地服务演示

该演示无需 Agent 账号或模型请求。从 `src/aw` 执行：

```bash
cargo build --locked -p aw-cli --bin aw
cargo build --locked -p aw-provider --example policy
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw service serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

在另一个终端进入 `src/aw`，使用打印的绝对 socket 路径：

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- "$AW_DEMO_SOCKET"
```

示例使用合成能力和事件，检查 `read_demo`、阻断 `delete_demo`、观察工具后事件，
并输出审计键。没有启动原生 Agent 或工具。将 `AUDIT_KEY` 替换为返回的准备记录键
或事件 ID，可以查询记录：

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

`terminal: false` 表示尚无终结记录，可能仍在运行，也可能已中断。调用超时后不得
作为新步骤重新执行。停止响应仅确认收到请求；等前台服务退出后，在原终端删除
本次演示目录：

```bash
rm -r -- "$AW_DEMO_ROOT"
```

正常关闭保留审计历史，删除所属 socket。强制终止后，AW 会报告遗留 socket，
不会自动删除。确认旧服务已停止后，再移除其所属 `aw.sock`；保留服务目录时，
保留其中的锁和 Journal。重启产生新的服务身份，历史记录仍可查询，但不会恢复
或重放旧事件。

[配置参考](configuration.md)列出完整字段和事件名。
[本地服务合同](../../../../src/aw/docs/design/zh/runtime/local-service.md)面向 Adapter 开发者，
说明操作 JSON、截止时间和生命周期。

sec-core 扫描通过独立的 `aw-provider-sec-core` 接入，见[配置与使用](../../../user-guide/zh/aw/security.md)。

模型可见的 PostTool 压缩使用可选的 [Tokenless Provider](../../../user-guide/zh/aw/tokenless.md)。
