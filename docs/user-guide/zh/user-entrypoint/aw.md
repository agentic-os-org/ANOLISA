# AW 使用指南

[English](../../en/user-entrypoint/aw.md)

AW 的目标是让一份策略配置用于不同的 Agent。用户继续使用 Agent 原有的交互界面，
AW 将它的工具 Hook 接到选定的规则和处理程序。首批面向 QwenPaw、Qoder CLI、
OpenClaw 和 Hermes。

计划中的交付物是 AW 安装包和一份 `aw.yaml`。切换 Agent 时复用这份策略，部署状态
和审计记录由同一个服务管理。当前版本已经可以检查配置，并从源码运行本地 Provider
Host 示例。Agent 启动和原生策略接入仍在开发中。

## 当前可用范围

✅ 表示当前版本已提供。❌ 表示计划交付，尚不能通过这份配置使用。早期实验中的
效果不计入当前版本的支持范围。

| 希望完成的操作 | 状态 | 当前可以得到什么 |
| --- | --- | --- |
| 从模板开始写配置 | ✅ 已支持 | 提供起步模板和完整示例 |
| 检查字段、类型与 Provider 引用 | ✅ 已支持 | 离线校验器报告配置错误 |
| 声明全部 16 个事件名 | ✅ 已支持 | 识别名称，不代表原生 Hook 已接通 |
| 用合成工具事件试运行本地 Provider | ✅ 源码示例 | Host 执行发现、私有配置校验和有界调用，不启动 Agent |
| 通过 AW 启动或接入 Agent | ❌ 待交付 | 独立服务和产品 CLI 尚未提供 |
| 在原生工具执行前后调用 Provider | ❌ 待交付 | 各框架还需完成适配和效果验证 |
| 用 sec-core 规则阻断工具或隐藏敏感结果 | ❌ 待交付 | 需要 sec-core Provider、受支持的效果，并验证 Agent 确实采用响应 |
| 查看已应用策略与持久审计记录 | ❌ 待交付 | 由后续服务保存和查询 |
| 安装 AW 并生成默认配置 | ❌ 待交付 | 当前手动复制起步模板 |
| 主动请求人工审批或在原生 Hook 之外强制执行策略 | ❌ 后续范围 | 当前拒绝启用的 ask 步骤，尚不提供 OS 层执行约束 |

四个首批 Agent 的标识都能写入配置，当前版本对它们的运行接入均为 ❌。QwenPaw
与 Qwen Code 分别识别。适配交付后，再按 Agent 版本和具体操作公布实际支持情况。

开发者可以在 Linux 上运行[本地 Provider Host 示例](../../../../src/aw/docs/design/provider-host_zh.md#本地示例)。
它返回候选效果与执行失败，不安装 Hook、不激活 Agent 防护，也不持久写入审计记录。

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
都设置了 `required: true`，要求后续运行时在目标无法提供这些事件时拒绝接入。

其余设置选择产品默认的本地服务位置，并声明只记录审计元数据。事件预算为
5,000 毫秒。这些值在模板里显式写出，校验器不会启动服务、写审计或执行计时限制，
也不会自动寻找默认文件或替用户补写缺失字段。

## 现在可以执行的检查

当前校验器从源码运行。安装 rustup 后，进入 `src/aw` 会选中仓库固定的 Rust
工具链。从仓库根目录依次执行下列命令，即可检查起步模板。准备好自己的
`aw.yaml` 后，替换命令最后的文件路径；相对路径以 `src/aw` 为起点。

```bash
cd src/aw
cargo run --locked -p aw-config --example validate -- \
  crates/aw-config/examples/aw.minimal.yaml
```

检查成功后会输出以下内容。

```text
Configuration is statically valid; runtime admission has not run.
```

这表示字段结构和静态引用通过检查。校验器不要求本机已安装 Qoder，也不执行配置
里的命令。策略生效前，服务还需要检查已安装 Agent 和 Provider 的实际能力。

## 加入自己的策略程序

Provider 是检查或处理事件的程序，可以是安全引擎，也可以是团队自己的工具结果
处理程序。在 `spec.providers` 中为每个实例命名，填写入口命令，并把它自己的
设置放进 `config`。

事件步骤通过 `provider` 引用这个名字，再用 `operation` 选择操作。
[完整示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.yaml)
中的 `business-before` 引用了 `business`，工具前末尾检查引用了 `security`。
原生步骤调度仍属于后续 Agent 接入工作。本地调用可通过独立的 Host 示例运行；
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
