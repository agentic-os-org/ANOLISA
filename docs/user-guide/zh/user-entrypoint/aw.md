# AW 使用指南

[English](../../en/user-entrypoint/aw.md)

AW 的目标是让一份策略配置用于不同的 Agent。用户继续使用 Agent 原有的交互界面，
AW 将它的工具 Hook 接到选定的规则和处理程序。首批面向 QwenPaw、Qoder CLI、
OpenClaw 和 Hermes。

计划中的交付物是 AW 安装包和一份 `aw.yaml`。切换 Agent 时复用这份策略，部署状态
和审计记录由同一个服务管理。当前版本已提供可从源码构建的独立 Linux 服务、本地
客户端和持久执行记录。Agent 启动和原生策略接入仍在开发中。

## 当前可用范围

✅ 表示当前版本已提供。❌ 表示计划交付，尚不能通过这份配置使用。早期实验中的
效果不计入当前版本的支持范围。

| 希望完成的操作 | 状态 | 当前可以得到什么 |
| --- | --- | --- |
| 从模板开始写配置 | ✅ 已支持 | 提供起步模板和完整示例 |
| 检查字段、类型与 Provider 引用 | ✅ 已支持 | 离线校验器报告配置错误 |
| 声明全部 16 个事件名 | ✅ 已支持 | 识别名称，不代表原生 Hook 已接通 |
| 用合成工具事件试运行本地 Provider | ✅ 源码示例 | 服务准备并调用 Provider，不启动 Agent |
| 独立于 shell 或 Agent 运行 AW | ✅ 源码构建 | 启动前台服务，使用本地客户端访问 |
| 通过 AW 启动或接入 Agent | ❌ 待交付 | 原生 Adapter 和 `aw run` 尚未实现 |
| 在原生工具执行前后调用 Provider | ❌ 待交付 | 各框架还需完成适配和效果验证 |
| 用 sec-core 规则阻断工具或隐藏敏感结果 | ❌ 待交付 | 需要 sec-core Provider、受支持的效果，并验证 Agent 确实采用响应 |
| 查询持久 Provider 执行记录 | ✅ 已支持 | 通过本地服务读取准备阶段和事件元数据 |
| 验证原生 Agent 已采用策略 | ❌ 待交付 | 服务运行和 Provider 调用成功不证明效果已采用 |
| 安装 AW 并生成默认配置 | ❌ 待交付 | 当前手动复制起步模板 |
| 主动请求人工审批或在原生 Hook 之外强制执行策略 | ❌ 后续范围 | 当前拒绝启用的 ask 步骤，尚不提供 OS 层执行约束 |

四个首批 Agent 的标识都能写入配置，当前版本对它们的运行接入均为 ❌。QwenPaw
与 Qwen Code 分别识别。适配交付后，再按 Agent 版本和具体操作公布实际支持情况。

## 运行本地服务演示

AW 尚未通过 `anolisa install` 或 RPM 发布。开发者可以在 Linux 上通过 rustup
使用仓库固定的 Rust 工具链构建。从仓库根目录构建 CLI 和样例 Provider，再启动
前台服务：

```bash
cd src/aw
cargo build --locked -p aw-provider --example policy
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-service/examples/aw.yaml
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

在另一个终端进入同一份代码的 `src/aw` 目录，将 `AW_DEMO_SOCKET` 替换为第一个
终端打印的绝对 socket 路径：

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- \
  "$AW_DEMO_SOCKET"
```

示例使用合成 Qoder 能力和工具事件，准备样例策略，检查不增加限制的 `read_demo`
工具前事件和被阻断的 `delete_demo` 工具前事件，再观察一次工具后事件。输出包含
结果和审计键。这些是本地 Provider 结果，不会启动 Qoder 或执行原生工具；样例
策略也不是 sec-core。

[演示配置](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.yaml)使用
`./target/debug/examples/policy`。运行示例时保持工作目录为 `src/aw`；Provider
路径按照客户端提供的上下文解析。后面的起步模板没有 Provider，不能产生这组
演示效果。

## 命令与记录

| 命令 | 用途 |
| --- | --- |
| `aw validate --config FILE` | 检查语法和静态引用，不执行命令 |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | 在前台运行一份不可变配置 |
| `aw status --socket ABSOLUTE_PATH` | 查看服务身份、资源数量及审计健康状态 |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | 从 stdin 读取一个操作 JSON 对象，默认超时为 5,000 毫秒 |
| `aw stop --socket ABSOLUTE_PATH` | 请求取消并正常关闭服务 |

源码构建时，将 `aw` 替换为 `target/debug/aw`。配置校验成功输出
`configuration valid`，控制命令输出 JSON，错误返回非零退出码。
`request` 是绑定、事件和审计操作的开发者接口。输入只包含操作对象，不是完整
协议信封；客户端负责补充服务身份和截止时间。全部操作字段见
[本地服务合同](../../../../src/aw/docs/design/local-service_zh.md#本地协议)。

将下面的 `AUDIT_KEY` 替换为示例返回的准备记录键或事件 ID。已记录的准备失败
也会报告审计键。

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
```

结果包含已验证的记录和 `terminal`。记录保留执行元数据，不包含工具输入或结果、
Provider 私有配置及原始 stdout/stderr。`terminal: false` 可能表示仍在运行或
曾被中断，不能直接推断进程崩溃。终结结果也不证明 Agent 已使用策略。调用超时时，
应使用已知审计键查询，不要重新执行步骤。

## 停止与重启

```bash
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

等待前台 `serve` 命令退出；停止响应仅表示请求已被接收。关闭时会取消未完成
调用，删除所属 socket，状态目录中的 `service.lock` 和 `journal/` 保留供后续
检查。

状态目录必须使用绝对路径，其已存在的父目录需要属于当前用户，且组和其他用户
不可写。示例通过 `mktemp` 创建私有父目录，不修改已有构建目录的权限。AW 以 0700 创建
该目录，已有目录也必须具备这一权限。显式 `spec.daemon.state_dir` 和 `endpoint`
必须匹配选定目录及其 `aw.sock`；`auto` 将位置选择交给调用方。按需启动和
supervisor 安装尚未提供。

强制终止后，AW 不会覆盖遗留 socket。先确认之前的服务进程已退出，再仅删除
这次演示的 socket：

```bash
rm -- "$AW_DEMO_SOCKET"
```

保留锁文件和 Journal。沿用第一个终端的 `AW_DEMO_ROOT`，使用相同的 `serve`
命令重启可保留审计历史。每次重启都
产生新的服务身份，旧客户端、绑定和事件句柄不能复用。旧记录可以查询，但不会
自动恢复或重放执行。全部服务进程退出后，若不再需要审计历史，可在第一个终端中仅删除本次
创建的演示目录：

```bash
rm -r -- "$AW_DEMO_ROOT"
```

## 从起步模板开始

把[起步文件](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.minimal.yaml)
复制到选定的 `aw.yaml` 位置。它声明 Qoder 和工具前后两个事件，没有配置策略程序，
也没有启用安全规则。

```yaml
# Starter configuration for offline validation.
# No policy program is configured; runtime integration is still being built.
apiVersion: aw/v1alpha1
kind: AWConfiguration
metadata:
  name: local-agent
spec:
  daemon:
    startup: on_demand
    endpoint: auto
    state_dir: auto
  execution:
    guarantee: native_hook
    default_event_budget_ms: 5000
  audit:
    enabled: true
    payload: metadata_only
  agents:
    qoder:
      adapter: qoder
      argv: [qodercli]
  providers: {}
  events:
    tool.before:
      enabled: true
      required: true
      steps: []
    tool.after:
      enabled: true
      required: true
      steps: []
```

熟悉 Kubernetes 的用户可以沿用对资源配置的理解。`apiVersion` 选择文件格式，
`kind` 表示 AW 配置类型，`metadata.name` 为这份配置命名。期望使用的内容写在
`spec` 中。AW 按独立组件设计，检查这份文件不需要 Kubernetes 集群或 CRD。

`agents` 中的 `qoder` 是用户为目标选择的名字，`adapter` 指定框架，`argv` 指定
程序和参数。增加另一个 Agent 对象，就能声明它与现有目标共用 Provider 和事件
配置。完整示例列出了 Qoder 与 OpenClaw，QwenPaw、Hermes 的启动细节会随适配核实。

空的 `providers` 表示尚未配置策略程序，空的 `steps` 不调用 Provider。两个事件
都设置了 `required: true`，要求运行时在调用方提供的目标能力无法覆盖这些事件时拒绝接入。

其余设置选择产品默认的本地服务位置，并声明只记录审计元数据。事件预算为
5,000 毫秒。这些值在模板里显式写出，校验器不会启动服务、写审计或执行计时限制，
也不会自动寻找默认文件或替用户补写缺失字段。

## 检查起步文件

按上面的步骤构建 CLI 后，在 `src/aw` 中执行。准备好自己的 `aw.yaml` 后，替换
命令最后的路径；相对路径以当前目录为起点。

```bash
target/debug/aw validate --config crates/aw-config/examples/aw.minimal.yaml
```

检查成功输出 `configuration valid`。

这表示字段结构和静态引用通过检查。校验器不要求本机已安装 Qoder，也不执行配置
里的命令。策略生效前，服务还需要检查已安装 Agent 和 Provider 的实际能力。

## 加入自己的策略程序

Provider 是检查或处理事件的程序，可以是安全引擎，也可以是团队自己的工具结果
处理程序。在 `spec.providers` 中为每个实例命名，填写入口命令，并把它自己的
设置放进 `config`。

事件步骤通过 `provider` 引用这个名字，再用 `operation` 选择操作。
[完整示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.yaml)
中的 `business-before` 引用了 `business`，工具前末尾检查引用了 `security`。
原生步骤调度仍属于后续 Agent 接入工作。本地调用可通过服务演示运行；
围绕真实 Agent 工具执行 Provider 在当前版本仍标为 ❌。

完整示例列出全部 16 个事件，另有一个默认关闭的结果隐藏步骤。其中的业务程序
路径和 sec-core 命令仅作示意，接入 Agent 时需要换成真实实现。完整示例包含超出当前
Host 支持的工具事件范围的能力，不是它的可运行模板。修改 `enabled` 会改变待校验的
配置，不会安装 Hook 或开启防护。

## 使用配置启动 Agent

计划中的流程从 AW 读取配置开始。服务先检查选定的 Agent 是否能执行所需动作，
再安装属于 AW 的原生 Hook 或插件配置，打开 Agent 原有的交互界面。Provider
在受支持的点位执行规则，AW 服务记录部署状态与处理结果。

完整示例中的 Qoder 和 OpenClaw 目标拟通过下列命令使用。这两个命令仍为 ❌
待交付接口，当前版本无法执行。

```bash
aw run qoder --config ./aw.yaml
aw run openclaw --config ./aw.yaml
```

Agent 无法执行必需的安全动作时，应拒绝绑定；可选观察来源缺失时，应明确展示。
服务计划在一次 Agent 交互结束后继续运行，供其他会话复用配置与记录。

字段限制、省略字段的处理方式及完整事件词汇见[配置参考](../../../developer-guide/zh/aw/configuration.md)。
