# Qoder CLI

[English](../../../en/aw/agents/qoder.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 构建并启动 Qoder

AW 尚未通过 `anolisa install` 或 RPM 发布。开发者可以在 Linux 上用 rustup 和
仓库固定的工具链构建。另行安装 Qoder CLI 并核对版本。从 `src/aw` 执行。

```bash
cargo build --locked -p aw-cli --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

[Qoder 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)使用
`argv: [qodercli]`。若需选择特定安装，将该项改为对应可执行文件的绝对路径。
`--agent qoder` 选择 `spec.agents` 下的命名对象；名称可以自定，
`adapter: qoder` 才是框架类型。

已验证基线为 CLI 1.1.64。AW 记录已安装 CLI 输出的裸 SemVer 版本号。接线依赖 `--settings`、字面 `args` 命令 Hook、`PreToolUse`/`PostToolUse`、回调身份和退出码 2 阻断。

例如，将目标命名为 `qodercli`，并选择已安装的可执行文件：

```yaml
agents:
  qodercli:
    adapter: qoder
    argv: [/absolute/path/to/qodercli]
```

将该段合入完整配置的 `spec` 下，再运行
`aw run --config /absolute/aw.yaml --agent qodercli`。目标名与原生可执行文件名、
版本号分别定义。

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

## 准备原生命令启动

在 Qoder 停止时安装一次启动 Hook，之后继续使用原来的 `qoder` 或 `qodercli`
命令。配置的 `argv[0]` 须指向实际 `qodercli` 可执行文件；shell 分发器可以启动
该文件，AW 不会推测任意包装脚本的目标。`HOME/.qoder` 与所选启动目录须为当前
用户所有的规范绝对路径，权限为 `0700`。安装保留现有设置与用户 Hook。

```bash
mkdir -p "$HOME/.local/state/aw"
target/debug/aw instance setup --config aw.yaml --agent qoder \
  --directory "$HOME/.local/state/aw/qoder-startup"
qoder
# 也可使用原生 print 模式：
qoder -p 'Review this harmless prompt.'
```

原生 `SessionStart` 回调直接完成实例准备与就绪确认，初始 `aw.yaml` 策略在
首次提交前生效。每个进程分别绑定 PID、启动身份、会话与 Service 代次，多个
Qoder 进程可以共用已准备的配置目录。选中实例空闲时，编辑允许更新的策略字段后执行：

```bash
target/debug/aw instance update --config aw.yaml --instance INSTANCE_ID
```

print 请求在启动时读取初始策略，运行中的请求不能切换策略。原有参数限制继续
适用：自定义设置来源、关闭 Hook、worktree、恢复会话与远程入口需要各自受支持
的接线。启动绑定缺失或过期时，输入和工具执行前回调会阻断操作。正常
`SessionEnd` 释放运行资产；进程异常退出后，等待 Service 清理完成，再移除安装：

```bash
target/debug/aw instance setup --remove --config aw.yaml --agent qoder \
  --directory "$HOME/.local/state/aw/qoder-startup"
```

移除会拒绝仍存活的 owner，只回收确认失效的进程记录及其资产，并按完整匹配移除
安装的 Hook 组，保留用户修改。被修改过的 AW Hook 须先恢复再移除。Service Socket 缺失时，移除只确认 owner
退出并清理其私有本地资产，不代表已验证 Service 排空。尚未发布完成的空记录或
不完整记录可在 owner 退出后清理，损坏内容不会用来推测并删除其他文件。原生 print
验收覆盖模型调用前的输入阻断；工具策略更新和多 owner 隔离另有确定性 Service
测试。已有 TUI [附着入口](../deployment/independent-instances.md)继续支持其已验证
的空闲进程范围。

## 检查用户提交

普通 print 和终端启动可通过 Qoder 的 `UserPromptSubmit` 命令 Hook 接入
`input.submit`。将[提交检查](../../../../user-guide/zh/aw/security.md#检查用户提交)
中已有的 sec-core `scan_user_input` 步骤加入同一配置。Provider 收到的
`input.text` 是本次提交的 `prompt`；阻断效果或 `on_error: block` 检查失败时，
原生 Hook 以退出码 2 阻止模型请求。`on_error: report` 会记录检查失败并放行。

原生 `session_id` 和 `request_set_id` 将同次提交的串行或并行 AW 步骤关联起来。
身份缺失时按该步骤的失败策略处理。已有原生 Hook 继续启用，其后续追加的上下文
不属于本次提交文本快照。reloadable 和 attach 的 TUI 路由也在模型请求前执行此事件。
稳定回调使用当前策略，请求保持该版本直到收到原生空闲确认。
普通 CLI 绑定最多保留 1,024 次不同受检提交的防重放记录，达到该数量前重新开启
CLI 会话。

## 纳入原生终端会话

Qoder CLI 1.1.73 使用独占的私有 HOME 配置启动终端后，可在第一次请求前附着。
在 `aw.yaml` 的 `spec.agents.qoder.argv` 中配置匹配的可执行文件，
从该配置所在目录执行：

```bash
aw instance attach --agent qoder --pid "$QODER_PID"
aw instance update --agent qoder
```

先将 `QODER_PID` 设为自己启动的终端进程。附着会核验启动身份和原生配置采用结果，
完成后实例才进入 Ready。修改 `aw.yaml` 中的策略字段，在两次请求之间执行更新。
活动请求、工具调用和未确认的清理会拒绝更新。存在多个实例时，使用
`--instance INSTANCE_ID` 明确选择一个。

该路由支持 `input.submit`、`tool.before` 和 `tool.after`。print 模式、
headless fast hooks 和其他 Qoder 进程共用的配置目录会被拒绝。
自动化附着 fixture 覆盖增加及移除输入阻断、阻止模型派发。
真实 1.1.73 TUI 已验证工具阻断、空闲更新后的工具放行，以及保留原进程和用户设置的
移除与重新附着。

终端空闲后移除附着：

```bash
aw instance attach --agent qoder --pid "$QODER_PID" --remove
```

AW 只恢复自己拥有的配置条目。版本校验与中断恢复见
[策略更新](../deployment/policy-updates.md)。

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

启动器会拒绝禁用或替换接线的设置和参数，不会覆盖用户关闭 Hook 的选择，包括
直接传入的 `--settings`、`--setting-sources`、`--headless-fast-hooks` 和冲突的
原生配置。自定义配置根目录、其他 Qoder 配置目录模式、恢复会话、远程会话和 worktree 启动
尚不在当前 Adapter 范围内。短选项须分别传入，不能合并；请从所需工作目录启动。已有原生 Hook 的行为与审计
仍由它们自身负责。
