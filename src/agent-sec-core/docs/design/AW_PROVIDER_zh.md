# AW Provider 边界

[English](AW_PROVIDER.md)

`agent-sec-cli aw-provider` 是已有 V2 `action.code_scan` 方法的新版本化调用入口。
不改变 V1 兼容接口、daemon 方法、扫描规则、Principal、审计 schema、服务拓扑
或部署合同；该路径的产品运行时全部为 Rust。

```text
AW 配置 + 标准化事件
  -> AW Host：准入、进程/输出限制、审计
  -> agent-sec-cli aw-provider：aw-provider/v1alpha1 + 私有配置
  -> 已有 asc-daemon-client -> action.code_scan
  -> 已有 Action Runtime -> Code Scanner + 扫描审计
  -> Provider 候选效果 -> AW 适配器 -> Agent 原生决策
```

AW 负责 Agent 适配、配置版本绑定和 Provider 调用历史；sec daemon 负责安全
执行及既有审计生命周期。AW 不替代 sec 引擎，也不将其数据库作为部署注册表。
本阶段编译和测试不依赖 AW daemon、Provider Host 实现或 Agent 适配器；真实
Hook 采用留待后续联合验收。

## 协议与策略

运行时通过路径依赖复用已合入的 `aw-provider` 离线合同 crate，校验请求大小
（1 MiB）、深度（32）、重复键、未知字段、效果范围、身份及输入摘要。这属于
协议依赖，不依赖 `aw-host`、AW daemon 或具体 Agent。成功响应原样回传摘要，
不会生成采用凭据。

CLI 读取一个以 EOF 结束的请求并写出一个响应。AW 必须关闭 stdin 并限制整个
进程执行时限；Provider 限制输入字节数，将剩余调用预算传给已有 daemon 客户端，
覆盖连接、写入和读取。CLI 超时可进一步缩短预算。超时后的结果被丢弃，通信
超时不会触发重试。客户端取消不能撤销 sec daemon 扫描，也不能保证服务端工作
已停止；服务端继续使用自身执行控制与审计。

`describe` 声明 `tool.before` 的 `scan_code`（observe/block）和 `tool.after`
的 `observe_tool`（observe）。`validate_config` 只校验私有配置，不探测或证明
后端可用性；`invoke` 再次校验配置。版本 1 要求显式模式，并用映射将标准化
工具名关联到 Bash/Python 语言和输入 JSON Pointer。`tool_unmapped` 表示缺少
扫描覆盖，不是放行判断；已匹配映射输入非法时返回故障，不跳过。

阻断需显式启用，沿用已有 Code Scanner Hook `enable_block` 的 `warn/deny`
阈值，不改变规则或 verdict。`observe_tool` 只确认收到工具后事件，不调用扫描器。
策略阻断以零退出码和 `status: ok` 返回；扫描、daemon、通信故障以零退出码和
`status: error` 返回，不携带效果，由 AW 显式执行 `on_error` 策略。非法协议
输入或 stdio 故障以非零退出码结束。响应与 Provider 诊断均不包含原始证据或输入。

Source0 将 AW 已跟踪源码打包到 `third_party/aw`，sec-core 打包辅助程序只修改
暂存副本中的路径依赖。Source1 仍通过已有 V2 lockfile 打包 registry 依赖，
使源码包可脱离 monorepo 构建，无需在版本库维护另一份协议实现。

## 验收与兼容性

| 范围 | 可执行证据 |
|---|---|
| 共享 AW 协议 | `asc-cli/tests/aw_provider.rs`：describe/配置校验、摘要篡改、重复键/大小/深度拒绝、响应校验。 |
| 私有配置 | 非法与未知字段、显式模式、精确名称、嵌套/转义/根 JSON Pointer、覆盖缺口和输入错误。 |
| 后端投影 | 有界模拟 daemon 请求；pass/warn/deny/error、非法 verdict、方法/通信故障、超时和效果准入。 |
| 真实调用方 | `asc-cli/tests/pap_process.rs::aw_provider_uses_real_daemon_rules_and_preserves_protocol_outcomes`：真实 CLI → daemon → 内置规则，覆盖干净输入、风险阻断、观察与扫描失败。 |
| 打包源码 | `tests/packaging/test-aw-source.py`：生成并解包 Source0，确认所有本地 Cargo 包均在归档内，使用 `--offline --locked` 构建，再执行 Provider discovery。离线检查前需准备 registry 缓存。 |
| 既有 CLI 兼容 | 现有 `asc-cli` 命令、上下文、输出、进程测试语义保持一致。daemon wire 方法未改，无需新增其协议 fixture。 |

在 `src/agent-sec-core/v2` 使用仓库 CI Rust 工具链运行：

```sh
cargo +1.93.0 fmt --all -- --check
cargo +1.93.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.93.0 test --workspace --locked
cargo +1.93.0 doc --workspace --no-deps --locked
```

[用户指南](../../../../docs/user-guide/zh/agent-security/agent-sec-core/aw-provider.md)
和[配置示例](../../v2/examples/aw.yaml)定义当前调用接口。自定义策略脚本、PII、
最终安全检查及 OS 强制执行不在本阶段范围。回退只需删除 AW Provider steps
和配置；已有 sec daemon 状态与原生 sec-core Hook 无需迁移。
