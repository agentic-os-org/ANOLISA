# 将组件接入 AW

[English](../../en/aw/integration.md)

本文帮助组件开发者从 Linux 源码构建开始，跑通工具策略，再将同一份策略接入
Qoder、OpenClaw、QwenPaw 和 Hermes。简单规则优先使用布尔判断命令；组件需要
自有操作或结构化效果时，再实现完整 Provider。

AW 当前为 Preview，尚未提供 `anolisa install aw` 或 RPM 分发。使用预编译二进制请
参阅 [Preview 包指南](../../../user-guide/zh/user-entrypoint/aw-preview.md)。下述源码流程在
选择 sec-core 或原生 Agent 步骤之前，无需安装 sec-core 或配置模型账号。

## 1. 从源码构建

准备 Linux、Git、rustup 和 C 编译器/链接器。仓库通过
`src/aw/rust-toolchain.toml` 固定 Rust、rustfmt 和 Clippy 版本。可选的示例策略
使用 Python 3；AW 开发门禁还需要 Node.js 和 Python 3。框架运行时与模型凭据
仍由各框架管理。打包构建的额外依赖见 Preview 指南。

在所需已评审提交的 checkout 中执行：

```bash
cd src/aw
cargo build --locked -p aw-service -p aw-package -p aw-provider -p aw-host --bins --examples
export AW_SOURCE="$PWD"
export AW_BIN="$AW_SOURCE/target/debug/aw"
export AW_PACKAGE="$AW_SOURCE/target/debug/aw-package"
export AW_HOST="$AW_SOURCE/target/debug/examples/host"
"$AW_BIN" --help
"$AW_PACKAGE" --help
```

debug 二进制位于 `src/aw/target/debug`，此步骤不安装系统服务。需要可分发的
release 包时，按[打包构建说明](../../../user-guide/zh/user-entrypoint/aw-preview.md#构建产物开发者)
使用 `aw-build`。core 可独立于 sec-core 构建。不要混装来自不同源码提交或包版本
的 core 与 Provider 包。

## 2. 准备四框架共用配置

运行任何 `aw-package config add-*` 命令前，必须先有初始 `aw.yaml`。已安装 core、
使用 Qoder/OpenClaw 时，通过 `aw-package configure` 创建；不传 `--provider` 就生成
空策略配置。按[初始化与编辑流程](../../../user-guide/zh/user-entrypoint/aw-preview.md#编辑已有-provider-与-hook-配置)
依次执行 `configure` → `add-provider` → `add-event` → `add-hook` → `validate`。
`configure` 会校验已安装的包前缀，仅构建源码二进制还不能提供这个前缀。它只创建
新文件，拒绝覆盖已有文件。

本源码教程及 QwenPaw/Hermes 使用下方完整四框架示例作为初始配置。生成器目前只
支持 Qoder/OpenClaw，编辑器不能添加 Agent 条目。示例已声明两个工具事件；只有
事件不存在时，才需要先 `add-event` 再 `add-hook`。

在同一终端保留这些变量。另开终端时，重新 export 打印出的绝对路径，避免再次
创建一个新的演示目录。

```bash
export AW_DEMO="$(mktemp -d "$AW_SOURCE/target/aw-onboarding.XXXXXX")"
export AW_CONFIG="$AW_DEMO/aw.yaml"
install -d -m 700 "$AW_DEMO/workspace" "$AW_DEMO/state"
cp "$AW_SOURCE/crates/aw-service/examples/aw.shared.yaml" "$AW_CONFIG"
cp "$AW_SOURCE/crates/aw-service/examples/check-tool.py" "$AW_DEMO/check-tool.py"
chmod 600 "$AW_CONFIG" "$AW_DEMO/check-tool.py"
printf 'AW_BIN=%s\nAW_CONFIG=%s\nAW_DEMO=%s\n' "$AW_BIN" "$AW_CONFIG" "$AW_DEMO"
```

编辑复制后的 YAML，将占位符替换为下表中的**展开后的绝对路径**。AW 不会对 YAML
中的 `$AW_BIN`、`$HOME` 等 shell 变量进行插值。路径含 YAML 标点时应加引号。

| 字段 | 本流程中的值 |
| --- | --- |
| `spec.daemon.state_dir` | 打印出的 `AW_DEMO` 路径加 `/state` |
| `providers.check-tool.transport.argv[0]` | Python 3 可执行文件的绝对路径；存在时保留 `/usr/bin/python3` |
| `providers.check-tool.transport.argv[1]` | 打印出的 `AW_DEMO` 路径加 `/check-tool.py` |
| `spec.agents.<id>.argv` | 已安装且受支持的框架可执行文件及其入口参数 |

下述合成检查不执行 Agent，可以暂时保留示例的 Agent 条目。原生启动前，确认所选
命令解析到[受支持版本](../../../user-guide/zh/user-entrypoint/aw.md#当前支持范围)。Hermes
须使用已安装的 Python console script，并带绝对 Python shebang。保留示例中的
`chat`、`app` 和 `gateway run` 参数。

`endpoint: auto` 使用 `<state_dir>/aw.sock`。`agents` 命名启动目标，`providers`
命名程序，`events` 将有序步骤挂到工具执行边界。四个目标共用一个布尔 Hook 命令，各步骤分别声明命中后的效果。示例在工具前输入包含 `AW_DENY_TEST` 时请求阻断，在工具完成后请求
观察。它用于演示接线，不是生产安全规则。

[配置参考](configuration.md)解释全部字段。当前 `aw-package configure` 只生成
Qoder/OpenClaw 条目；四框架配置使用本 YAML 副本。
[配置编辑器](../../../user-guide/zh/user-entrypoint/aw-preview.md#编辑已有-provider-与-hook-配置)
可以增删 Provider 和 Hook 步骤，不修改 Agent 条目。

```bash
"$AW_BIN" validate --config "$AW_CONFIG"
"$AW_PACKAGE" config show --config "$AW_CONFIG" --event tool.before
"$AW_PACKAGE" config validate --config "$AW_CONFIG"
```

校验检查文档结构与引用，不执行策略、不检查凭据，也不证明框架采纳了阻断。

## 3. 不依赖模型验证策略

已有 Host 示例使用合成能力绑定 `qoder` 目标，运行真实布尔 Hook 命令，发送标准化
事件并输出候选效果。示例不会执行事件数据中的命令。

```bash
cd "$AW_DEMO/workspace"
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_OK"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_DENY_TEST"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.after demo_shell '{"command":"printf AW_OK"}'
```

依次应得到 `candidate_effects: []`、`block/demo_marker_denied` 和
`observe/demo_tool_observed`，每次 `failure` 均为 null。`adoption: not_tested`
是预期标记：这里验证 Hook，不执行原生工具，也不产生服务的持久审计。
[本地服务演示](../../../user-guide/zh/user-entrypoint/aw.md#运行本地服务演示)使用另附的配置，
单独演示 socket、服务客户端、审计查询和停止流程，同样无需模型。

验证故障处置时，在 `AW_DEMO` 中将工作配置复制为 `aw.failure.yaml`，仅将
`check-tool` 的脚本路径改为不存在的文件，再用该配置重复第一条 Host 命令。
预期进程非零退出、failure 非 null 且 `failure_action: block`，而不是成功返回空
效果。策略阻断和策略进程失败是不同结果。检查后删除故障配置副本，不要修改正在
运行的服务所用配置。

## 4. 接入自己的组件

选择符合组件需求的最小合同：

| 接入方式 | 需要实现 | 如何配置 |
| --- | --- | --- |
| 布尔判断命令 | 从 stdin 读取一个标准化事件，stdout 输出 JSON `true`/`false` 并退出 0；诊断写 stderr | 省略 `protocol` 和 `operation`，默认使用 `aw-hook/v1alpha1` / `invoke`；`effects` 和可选 `reason_code` 放在各步骤中，与共用示例一致 |
| 已有原生 Hook | 遵守所选框架的原始输入输出及退出状态合同 | `native-hook/v1alpha1`、空 `config`、步骤中使用 `native: {}`；输出格式依赖框架 |
| 结构化 Provider | 实现 `describe`、`validate_config` 和 `invoke`，每个进程处理一份 JSON 请求并返回一份响应 | `aw-provider/v1alpha1`、自己的程序 argv 与私有 `config`；步骤指定 `operation`、`effects` 和 `on_error` |

共用脚本在 `tool.after` 时返回 true，after 步骤请求 `observe`；before 步骤只在
标记命中时请求 `block`。AW 在本地准入两个步骤，不调用脚本的 `describe` 或
`validate_config`。脚本只收到标准化事件，不接收 method、request ID 或摘要，
也不用构造 response 对象。已有显式 `aw policy` 桥接配置仍受支持；新增简单规则
可使用这里的缺省 Hook 形式。false 不施加额外限制，也不授予原生权限。非零退出、
格式错误和超时均属于执行失败，由步骤的 `on_error` 处理。stdout 不应混入日志。

示例要求标准化事件包含 `tool.input`，其值可以为 null。AW 在启动脚本前校验调用
结构。若直接测试脚本时传入缺少 `tool.input` 的工具前 JSON，脚本会非零退出；
缺少字段应报错，不能当作规则未命中而返回 false。

完整 Provider 可参考第 1 步已构建的
[Rust 示例](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-provider/examples/policy.rs)和
[协议参考](../../../../src/aw/docs/design/provider-protocol_zh.md)。以下两次调用验证它的
发现与私有配置合同：

```bash
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"demo-describe"}' | \
  timeout 5s "$AW_SOURCE/target/debug/examples/policy"
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"demo-config","config":{"blocked_tools":["delete_demo"]}}' | \
  timeout 5s "$AW_SOURCE/target/debug/examples/policy"
```

每份响应必须回传原 `api_version` 和 `request_id`；`invoke` 还须回传请求的
`input_digest`，只返回准入范围内的效果。成功的策略阻断使用退出码 0。新增
Provider 无需修改 AW 或把组件 crate 引入 AW。可执行文件及其运行依赖须安装在
Agent 运行的位置。AW 为每个方法启动新进程，长期运行的后端由组件自己管理。

[sec-core 接入流程](../../../user-guide/zh/user-entrypoint/aw-sec-core.md)展示了如何桥接已有
CLI/daemon。需要为每个框架映射**实际工具名称和输入 JSON Pointer**；标准化事件
结构不代表所有工具参数相同。Preview 生成的 sec-core 映射覆盖 Qoder `Bash` 和
OpenClaw `exec`。扩充并验证映射后，才能声明覆盖 QwenPaw 或 Hermes 扫描。
未映射工具只产生观察，不执行扫描。

## 5. 验证原生效果并清理

沿用本终端的 `AW_BIN`、`AW_CONFIG` 和 `AW_DEMO`，执行
[共用配置启动与验收步骤](../../../user-guide/zh/user-entrypoint/aw.md#四框架使用同一份配置)。
保留各框架独立的模型 profile，起步时按受支持入口逐个测试。被拒绝的工具应同时
有 AW 拒绝结果且没有工具副作用；模型拒绝或原生权限拒绝不能证明 AW 已执行。

组件改动可从 `AW_SOURCE` 执行相应检查：

```bash
cd "$AW_SOURCE"
cargo test --locked -p aw-host -p aw-service
```

提交 AW 实现改动前，在仓库根目录运行完整 Linux 门禁：
`python3 src/aw/scripts/check.py`。它覆盖合同与 fixture，原生模型会话仍需单独
验收。在 PR 中记录提交、框架版本、命令与实际结果，不将仅配置校验或合成事件
检查写成四框架验收通过。

原生会话结束后，按使用指南停止服务。只执行 Host 命令时不会创建 AW 服务。
确认所有使用此演示目录的进程已退出后，目录内的配置、脚本副本、标记文件和审计
记录均属于本次演示。保留所需审计证据，再执行：

```bash
cd "$AW_SOURCE"
rm -r -- "$AW_DEMO"
test ! -e "$AW_DEMO"
unset AW_CONFIG AW_DEMO AW_HOST AW_PACKAGE AW_BIN AW_SOURCE
```

该清理不影响框架安装与 profile；Hermes 插件安装的回退步骤另见使用指南。
Cargo 构建产物保留在 `src/aw/target/`。仅当需要丢弃该组件构建缓存时，才在
`src/aw` 执行 `cargo clean`。
