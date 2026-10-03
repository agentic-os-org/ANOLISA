# AW 使用指南

[English](../../en/user-entrypoint/aw.md)

AW 将工具策略和 Hook 命令接入 Agent，同时保留它原有的交互界面。在 `aw.yaml`
中声明程序和事件后，AW 启动或复用本地服务，接通受支持的原生 Hook，并在 Agent
会话结束后继续保存执行记录。

当前 Linux 源码版本支持 Qoder CLI 1.1.64 和 Hermes 官方提交 `952c941e`。
其他首批 Adapter 独立交付。AW 不安装 Agent，也不配置模型账号；继续使用框架
原有的模型与认证配置。

## 当前支持范围

| 能力 | 状态 |
| --- | --- |
| 校验一份包含全部 16 个事件名的配置 | ✅ 识别事件不代表安装 Hook |
| 通过 AW 启动 Qoder CLI 1.1.64 | ✅ 交互和 print 入口 |
| 工具前运行结构化 Provider | ✅ `observe`、`block` |
| 工具后运行结构化 Provider | ✅ `observe`；成功、失败及阻断尝试的覆盖随框架而异 |
| 工具前后执行原生脚本和命令 | ✅ 回调输入保持不变，转交字节输出与退出状态 |
| 保留 Qoder 已有 Hook 及其调度 | ✅ 默认配置和显式传入的附加配置文件 |
| 复用共享服务并持久保存执行元数据 | ✅ 按需启动或外部启动服务 |
| 通过 AW 启动 Hermes | ✅ 显式安装原生插件后的本地 chat |
| 启动其他首批框架 | ❌ 相应 Adapter 独立交付；QwenPaw 与 Qwen Code 分别识别 |
| 其他事件、跨框架 `ask`、结果替换或 OS 执行约束 | ❌ 当前结构化 Provider 路径不予准入 |
| 安装已发布的 AW 包或生成默认配置 | ❌ 当前手动复制示例 |

对于 Qoder，`tool.after` 映射到成功调用后的 `PostToolUse`。Qoder 的 `PostToolUseFailure`
是独立事件，本 Adapter 尚未接入。原生 Hook 命令仍按 Qoder 的响应语义处理。
透传原生审批响应不代表 AW 已提供跨框架审批支持；交互式审批不在本期验收范围内。

## 构建并启动 Qoder

AW 尚未通过 `anolisa install` 或 RPM 发布。开发者可以在 Linux 上用 rustup 和
仓库固定的工具链构建。另行安装 Qoder CLI 1.1.64 并核对版本。从仓库根目录执行：

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

[Qoder 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)使用
`argv: [qodercli]`。如果 `PATH` 中是其他版本，将该项改为受支持可执行文件的绝对
路径。`--agent qoder` 选择 `spec.agents` 下的命名对象；名称可以自定，
`adapter: qoder` 才是框架类型。

示例在工具前后消费 Hook 输入并返回 `{}`，不增加限制，也不是安全策略。可以让
Qoder 执行一个无副作用的只读工具，触发前后两个回调；没有工具调用的回答不会
触发它们。`--` 后的参数传给 Qoder，例如：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder -- -p 'Read the current directory name with a tool.'
```

AW 打开 Qoder 的原生终端界面，Qoder 退出后回到原 shell，并释放本次会话的绑定
与未完成工作。共享 daemon 继续运行，供使用同一配置的后续会话复用。

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

## 启动 Hermes

使用官方 Hermes 提交 `952c941e741e922a9be8fc403c8944c6e96318bb`。示例为
`argv: [hermes, chat]`，请指向该安装的可执行文件。先通过 Hermes 初始化 profile
与模型账号。`aw install` 是显式的一次性原生 AW 插件安装：启用前备份原始配置，
保留未知配置字段，拒绝覆盖同名非 AW 插件。原生 YAML 写入器修改私有候选文件，
AW 再原子替换真实配置；需要逐字节回退时
使用命令报告的备份。安装不会热加载或重启既有 Hermes 服务。

后续 `aw run` 保留该 profile 的认证、历史与工作目录。原生 before/after 按 Hermes
注册顺序执行，before 阻断不会跳过后续已注册的回调；post 也可观察被阻断和失败的
尝试。AW 保留原生 Hook 返回解析和 Agent 自身审批，尚不支持通用 AW `ask`。
当前入口为以 `--cli` 固定的本地 `chat`，不含原生 TUI、Gateway 或 ACP。
请使用完整 chat 参数；不支持 profile/worktree/恢复会话切换与选项缩写。事件预算最多 55,000 ms，
同时必须留在原生回调超时范围内。

```bash
target/debug/aw install --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

[Hermes 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.hermes.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

## 接入自己的程序

`spec.providers` 中的每个命名对象描述一个程序，事件步骤通过 `provider` 引用
它。根据现有程序选择协议：

| 协议 | 输入与结果 | 步骤字段 |
| --- | --- | --- |
| `aw-provider/v1alpha1` | AW 执行 `describe`、`validate_config`、`invoke`，响应包含经校验的候选效果 | `operation`、`effects`、`on_error` |
| `native-hook/v1alpha1` | 命令收到所选框架的原始回调 stdin，stdout、stderr 和退出状态按该 Adapter 合同处理 | `native: {}`、`on_error`；不填 `operation`、`effects` |

接入原生 Hook 时，将示例的 `transport.argv` 换成脚本的可执行文件及字面量参数。
AW 不插入 shell；使用 shell 语法时明确指定 `/bin/sh -c ...`。这种协议保留
`config: {}`，不执行 Provider 配置交互。Qoder 专用的原生输出不会自动转换成其他
框架的响应。

原生命令使用回调进程的实际环境，包括框架加载的 profile 变量；结构化 Provider
使用绑定时固定的启动环境。环境内容不写入事件或审计。

统一策略使用 `aw-provider/v1alpha1`，Provider 自己的设置写在 `config` 中。
可运行的[策略示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.yaml)展示工具前
`observe`/`block` 和工具后 `observe`。先执行
`cargo build --locked -p aw-provider --example policy`，再从 `src/aw` 运行，
因为示例命令使用相对路径。其中的阻断工具名仅供演示；测试阻断时，替换成 Agent
实际使用的工具名。样例 Provider 不是 sec-core。

`timeout_ms` 限制单次命令，`default_event_budget_ms` 限制整个事件，输出上限
约束返回字节数。Qoder 启动器接受 1 到 55,000 毫秒的事件预算。`on_error` 处理
执行故障，与 Provider 成功返回的策略阻断分开。
对原生命令，正常获得的非零退出仍是原生结果，不算 AW 传输故障。Qoder 在支持
阻断的点位将退出码 2 解释为阻断，其他非零退出是非阻断错误。observe 结果不授予
新权限，也不覆盖 Qoder 自身的工具权限。

## 保留已有 Hook 与调度

Qoder 继续加载原有 user、project 和 local 配置。AW 为本次会话生成回调条目，
不修改这些文件。如果原来通过 Qoder 的 `--settings` 传入额外 JSON 文件，改用：

```bash
target/debug/aw run --config ./aw.yaml --agent qoder --native-settings ./qoder.settings.json
```

AW 保留该文件的其他字段和 Hook 条目，加入自己的回调，再将合并配置传给 Qoder。
每个 AW 步骤成为独立的同步原生 Hook，串行或并行执行由 Qoder 决定，包括与
其他匹配 Hook 的共同调度。

设置 `spec.agents.qoder.qoder.sequential: true`，可将生成的组标记为顺序执行。
任一匹配的同步 Qoder 组要求 sequential 时，全部匹配的同步 Hook 都按顺序执行。
省略该字段或设为 false，不会覆盖已有组的 true。属于同一原生事件的步骤共享 AW
预算，后续回调不会重新计时。

当前支持 AW 步骤之间输入保持不变的脚本，不支持顺序输入重写链。若命令返回
`updatedInput`，后续回调输入变化会被拒绝，并按该步骤的 `on_error` 处理。
转交原生字节不代表已支持全部原生效果组合或审批流程。

启动器会拒绝禁用或替换接线的设置和参数，不会覆盖用户关闭 Hook 的选择，包括
直接传入的 `--settings`、`--setting-sources`、`--headless-fast-hooks` 和冲突的
原生配置。自定义配置根目录、其他 Qoder 配置目录模式、恢复会话、远程会话和 worktree 启动
尚不在当前 Adapter 范围内。短选项须分别传入，不能合并；请从所需工作目录启动。已有原生 Hook 的行为与审计
仍由它们自身负责。

## 服务生命周期与记录

`spec.daemon.startup: on_demand` 在选定端点没有服务时启动一个；`external`
要求服务已由外部启动。已有服务的协议和完整配置版本匹配时才会复用。
`endpoint: auto`、`state_dir: auto` 会在 `XDG_RUNTIME_DIR` 下选择按配置区分的
私有目录；该变量未设置时使用 `/tmp/aw-UID` 下的目录。显式路径必须是绝对路径，
并且指向同一个 `aw.sock` 位置。

服务持有固定配置快照，编辑 `aw.yaml` 不会热更新它。停用某份配置前，用原文件或
socket 停止相应服务；修改配置后的 auto 路径可能指向另一个服务。退出 Agent 不会
停止其他会话或共享 daemon。本地端点是同用户访问边界，不提供 sandbox。

| 命令 | 用途 |
| --- | --- |
| `aw validate --config FILE` | 检查语法和静态引用，不执行程序 |
| `aw run --config FILE --agent TARGET [ADAPTER_OPTIONS] -- ARGS` | 启动配置的 Agent 并接通受支持 Hook |
| `aw install --config FILE --agent TARGET --native-profile ABSOLUTE_DIR` | 向已有 Hermes profile 显式安装原生插件 |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | 在前台运行服务 |
| `aw status --config FILE` 或 `aw status --socket ABSOLUTE_PATH` | 查看选定服务，不启动它 |
| `aw stop --config FILE` 或 `aw stop --socket ABSOLUTE_PATH` | 请求正常关闭服务 |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | 从 stdin 读取一个开发者操作 JSON 对象，默认 5,000 毫秒 |

源码构建时将 `aw` 替换为 `target/debug/aw`。`aw hook` 是启动器生成的内部回调，
无需用户手写。服务记录包含执行元数据，不包含工具输入或结果、Provider 私有配置
和原始 stdout/stderr。服务运行或调用完成不能证明原生策略已采用。原生回调被杀
或禁用时可能缺失检查，本版本不提供 final/protected 执行或 OS 兜底。

## 运行本地服务演示

该演示无需 Agent 账号或模型请求。从 `src/aw` 执行：

```bash
cargo build --locked -p aw-provider --example policy
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-service/examples/aw.yaml \
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

[配置参考](../../../developer-guide/zh/aw/configuration.md)列出完整字段和事件名。
[本地服务合同](../../../../src/aw/docs/design/local-service_zh.md)面向 Adapter 开发者，
说明操作 JSON、截止时间和生命周期。
