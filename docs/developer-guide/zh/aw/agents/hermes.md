# Hermes

[English](../../../en/aw/agents/hermes.md)

## 准备

已验证基线为提交 `952c941e741e922a9be8fc403c8944c6e96318bb`。AW 记录实际检出的修订，
其他具备原生 profile、插件和 shell-hook 接口的修订也可接入。已跟踪文件保持干净，
保留安装路径与加载环境检查。安装 Python console script，使用绝对 Python shebang；入口为本地 `chat`。
按[教程](../integration.md)构建 AW，复制[Hermes 配置](../../../../../src/aw/crates/aw-service/examples/aw.hermes.yaml)，把 argv 改为该安装的绝对路径。
先用 Hermes 初始化 profile 和模型账号。

## 安装插件并启动

```bash
cd src/aw
target/debug/aw agent connect --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

connect 备份配置后安装 AW 插件，报告 `backup` 和 `displaced` 路径。保留这两份文件。
运行时使用同一 profile，等待 Hook 就绪后按[原生验收](../validation.md)验证工具。

## 部署约束

| 项目 | 要求 |
| --- | --- |
| 入口 | Python console script 的本地 chat；不支持 shell wrapper、`python -m`、TUI、Gateway、ACP 或会话/profile/worktree 切换 |
| 路径 | 工作目录、profile、venv、checkout 和 AW 路径须为 UTF-8；profile 与 `.env` 归当前用户且不可被他人写入 |
| Git 环境 | 取消 `GIT_DIR`、`GIT_COMMON_DIR`；HOME/XDG 和 Git 配置路径使用绝对路径；拒绝 filter 与 fsmonitor helper |
| 原生选项 | safe mode 和 chat 帮助参数会在接入前被拒绝；直接用 `hermes chat --help` 查看帮助 |
| 工具参数 | 启用 AW before 时禁止 Hook/middleware 改写输入，包含 `execute_code` 内部调用 |
| 预算 | 事件最多 55000 ms，并须满足原生回调期限 |

before 阻断后，其他原生回调仍可运行；after 可观察失败或被阻断的尝试。
配置 AW 回调时，工具执行中的强制插件重载会被拒绝。空策略仍须安装插件，运行时注册零个工具回调。

## 升级与清理

插件不匹配时，先停止 Hermes 与配置写入者，把 `<profile>/plugins/aw-native-hooks` 移到 plugins 目录之外备份，再运行 connect。
回退时核对 `backup` 和 `displaced`，恢复所需配置并保留安装后有意修改。AW 没有 Hermes 插件卸载命令。
退出 Hermes 后停止 AW 服务；配置发布或清理失败按报告路径核对，不覆盖未知文件。
详细探测、原子发布与冲突处理见[Hermes 合同](../../../../../src/aw/docs/design/zh/adapters/hermes-adapter.md)。

## Skill 信任检查

通过[通用安全 Provider](../providers/sec-core.md#加载前校验-skill)，在 `tool.before`
为 `native:hermes:skill_view` 启用 `check_skill`。AW 使用原生工具注册项与本地目录
选择结果，检查规范化的 Skill 目录，再将受检 manifest 交给原生渲染。原生禁用、
平台限制和隔离决定继续生效。

支持目录型 `skill_view` 的主 `SKILL.md`。插件限定来源、旧式单文件和 `file_path`
引用返回解析错误，`on_error: block` 会阻止加载。斜杠命令、预加载、直接读文件与
Skill 内联命令需要各自的策略接点。

## 检查用户提交

使用[输入配置](../../../../../src/aw/crates/aw-service/examples/aw.hermes-submission.yaml)，填写 Hermes、sec-core Provider、CLI 和 socket 路径，然后在同一 profile 安装插件。`scan_prompt` 和 `scan_user_input` 会在 quiet 单查询 CLI 构造 Agent 前完成：

```bash
aw run --config /absolute/aw.hermes-submission.yaml --agent hermes \
  --native-profile /absolute/hermes-profile -- --query 'Summarize the current task.'
```

配置的 argv 已包含 `chat --oneshot --quiet`。可用 `--query-file /absolute/prompt.txt` 替换 `--query`。策略拒绝以状态码 2 退出 CLI，模型调用为零，受检文本不会进入原生会话。回调故障按 `on_error` 处理，派发或拒绝前关闭待完成的扫描。输入须为非空 UTF-8 文本，编码后的回调数据不超过 256 KiB。

启用 `input.submit` 时要求 quiet 单查询，拒绝交互 chat 和 `--image`。受检对象为原生 query-file 解析后、图片路由前的文本；其后的原生上下文与 Skill 展开沿用各自合同，原生 API request 插件自行负责改写行为。未配置输入步骤时继续保留原有 chat 选项。Python 工厂、gateway 会话和交互 `/steer` 不属于此 CLI 入口。

## 准备原生启动并更新策略

完成插件连接后，将启动模块安装到明确选择的专用 Hermes 虚拟环境。下面以 Python 3.14 为例，需使用该环境实际已有的 `site-packages` 路径。AW 创建三个新文件，遇到已有启动资产时拒绝覆盖。

```bash
aw instance setup --config /absolute/aw.yaml --agent hermes \
  --directory /absolute/hermes-venv/lib/python3.14/site-packages \
  --native-profile /absolute/hermes-profile > hermes-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("hermes-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
/absolute/hermes-venv/bin/hermes chat --cli
```

配置中的目标使用相同的原生参数；参数不一致时在原生分发前终止。准备完成后重启一次。启动模块保留原生 main，在同一进程复用 AW 的登记、参数完整性和完成检查。版本查询和管理命令保留原生行为。准备失败会阻止聊天分发。

```bash
aw instance update --config /absolute/aw.yaml --agent hermes
```

更新在确认结束的聊天轮次之间生效。请求占用包括模型调用、工具执行和回调清理。取消后没有回复、异常或仍在执行的回调会保留占用状态，处理原因后需重启进程。这条更新路径的事件预算上限为 55,000 ms。准备前将原生 `plugins.hook_callback_timeout` 设为至少 60 秒，或明确设为无限制，为完整事件预算和回调清理留出时间。AW 校验这项前提，保留用户的原生配置。

工具观察、阻断和结果替换使用稳定路由。安静单次文本查询还会在 `input.submit` 前登记请求，并将同一占用保留到聊天结束；交互目标拒绝新增输入检查策略。可在轮次之间启用或移除 `check_skill`，复用 [Skill 接入](#skill-信任检查) 的目录解析、源码完整性检查和受检内容渲染；未启用检查的轮次沿用原生加载行为。

移除原生启动准备前，先停止该专用环境的 Hermes 进程，再仅删除 `setup` 列出的三个文件：`aw-native-bootstrap.pth`、`aw_native_startup.py` 和 `aw-native-bootstrap.json`，保留环境和插件中的其他文件。插件回滚沿用[已有清理步骤](#升级与清理)。
