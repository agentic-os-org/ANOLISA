# sec-core CLI Provider 边界

[English](sec-core-provider.md)

`aw-provider-sec-core` crate 是一个具体的 AW Provider 二进制，与 sec-core 的依赖
边界是公开进程接口，不是 Rust crate 或 daemon 协议。sec-core 无需增加 CLI 子命令、
源码包内容或 AW 依赖。

```text
AW Host → aw-provider-sec-core → agent-sec-cli scan-code → sec-core daemon
        ← AW 效果/错误        ← 扫描 JSON + 退出状态    ← 规则/判定
```

AW 协议处理 describe、私有配置校验、请求与摘要绑定以及效果准入。桥接负责精确工具名、
JSON Pointer 选取及显式 observe/block 阈值；sec-core 负责代码扫描、规则和 verdict
含义。Core 与 Host 无需理解扫描器字段。`observe_tool` 仅确认工具后观察，不扫描或
变换结果。

桥接读取有类型的 `ok` 和 `verdict` 字段，允许 CLI 增加其他输出字段。CLI 成功退出
只是判定有效的必要条件：`ok=true/pass` 表示通过，`ok=true/warn|deny` 表示风险。
非法或未成功的响应归为执行故障。stdout/stderr 与扫描详情均有大小上限，不会作为
AW 响应数据返回。Host 独立处理 `on_error`。

调用使用字面 argv、绝对 CLI 路径与 daemon 端点、选定语言、`--mode regex` 及剩余
`--timeout-ms`。不插入 shell、本地扫描器、服务启动、重试或旧 CLI 回退路径。
首个实现面向公开的 Rust V2 CLI。未来若需扩展字段，应表达通用扫描能力，不向 CLI
引入 AW 请求 ID 或事件类型。

`aw-exec` 管理内层 CLI 的期限、输出上限和进程组清理，CLI 继承宿主为 Provider
选定的环境。执行预算从请求解析前开始计算，上限 60 秒，在 EOF 到达后检查；
它不能中断任意阻塞的 `Read`/`Write` 实现。所有方法均要求调用方限制整个进程生命周期
及 stdio。`aw-host` 通过 `aw-exec` 提供外层期限，直接调用方必须关闭 stdin 并自行
终止停滞进程。Provider 不承诺独立调用的总耗时上限。清理使用执行器已有的一秒
额外预算。Linux 父进程死亡信号保证 Host 强制终止 Provider 时，直属 CLI
也会终止。这不构成所有后代的 OS 强制执行保证：逃逸进程组、权限切换以及 owner
被杀后无法核实清理仍是边界。已验收的 Rust scan CLI 不另起扫描进程，而是调用
已有 daemon；已派发的 daemon 工作由 sec-core 约束，终止客户端不会回滚这些工作。

私有配置与可运行的 Host 步骤见[用户指南](../../../../docs/user-guide/zh/user-entrypoint/aw-sec-core.md)。
示例提供完整 AWConfiguration，无需修改统一配置 schema。部署管理、原生 Hook
调度、效果采用与持久化 AW 审计仍由 daemon 和适配器负责。使用早期实验命令
`agent-sec-cli aw-provider` 的组合分支需要将 Provider argv 改为独立桥接。

## 验收边界

`cargo test --locked -p aw-provider-sec-core` 覆盖公开 CLI fixture、请求绑定、离线
操作、配置拒绝、精确 argv、判定与故障映射、预算和输出上限，以及 Host 取消时
内层 CLI 的终止，以及 stdin 保持打开、Provider 等待 EOF 时的外层期限清理。
常规 AW gate 同时运行这些测试与既有 Provider/Host/Core 测试。
真实 CLI/daemon 验收复用 Host 示例，使用隔离 daemon 路径与内置规则，独立于 fixture
测试和 Agent 原生验收。返回 block 候选效果不代表已经阻止工具执行。
