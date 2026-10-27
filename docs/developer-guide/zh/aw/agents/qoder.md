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

CLI 要求 SemVer ≥1.1.64，版本输出使用裸版本号。接线依赖 `--settings`、字面 `args` 命令 Hook、`PreToolUse`/`PostToolUse`、回调身份和退出码 2 阻断。

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
