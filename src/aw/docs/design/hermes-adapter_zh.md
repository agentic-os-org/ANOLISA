# Hermes 原生 Hook 适配器

[English](hermes-adapter.md)

Hermes 适配器通过框架原生 Plugin 和 shell Hook API，将已有本地 profile
接入 AW 服务。本期支持 Linux 上官方 `NousResearch/hermes-agent` 提交
`952c941e741e922a9be8fc403c8944c6e96318bb` 的 Python `chat` 入口。
Gateway、ACP、Desktop 和独立 TUI 入口尚未支持，使用 `.container-mode` 将 CLI
转入托管容器的 profile 也会被拒绝。
AW 加入原生 `--cli`，防止环境或配置中的 TUI 偏好切换入口。只接受完整的受支持
chat 参数，拒绝 profile 切换、worktree、恢复会话、禁用插件与原生长选项缩写。
版本探测最长允许 60 秒，因为该 Hermes 版本会在 `--version` 中执行自身更新
检查。AW 不修改这项原生设置，也不重试失败的探测。AW 根据输出的安装目录
核对完整 Git 提交，不使用可能随 `origin/main` 变化的 banner 作为版本依据。

## 安装与启动

Hermes 从 profile 目录读取 `config.yaml`。该版本没有临时配置覆盖入口，
且只加载已启用的插件，因此 AW 提供显式安装步骤：

```bash
aw install --config aw.yaml --agent hermes --native-profile /absolute/profile
aw run --config aw.yaml --agent hermes --native-profile /absolute/profile -- chat
```

Agent 的 `argv` 指定已安装的 Hermes 可执行文件。`chat` 及其参数可以放在
`argv` 中，也可以放在启动命令的 `--` 后，出现一次即可。
[示例配置](../../crates/aw-service/examples/aw.hermes.yaml)已在 `argv` 中写入 `chat`。

安装向 `plugins/aw-native-hooks` 写入随 AW 提供的插件，将名称添加到
`plugins.enabled`，并移除 `plugins.disabled` 中对应项。未知配置字段与已有
插件选择保持原值。首次更新配置会将原始字节完整保存到私有的
`config.yaml.aw-backup-<id>`。官方 `hermes config set` 只在 AW 独占的私有暂存
目录中修改需要调整的插件选择字段，其 YAML 1.1 写入器保留未知值、注释与引号。
AW 核对真实配置仍等于原始字节后，再原子替换；命令失败或出现并发编辑时，
真实配置保持不变。暂存目录只有配置候选，不复制认证与历史，安装结束后删除。
每次原生写入命令的期限为 30 秒。文件未变化时重复安装不产生修改。同名非 AW 插件、
符号链接文件、非法插件列表或已占用的安装锁都会返回明确错误。

安装不会调用 `hermes plugins enable`，因为该命令可能向已有 Gateway 或
Desktop 进程热加载插件。AW 只更新本地文件，不复制或重写 profile 的 `.env`、
`auth.json`、数据库、记忆与会话。Hermes 正常运行时仍可更新自身状态。

启动要求匹配的 AW 插件已经安装并启用。AW 保留选定的 `HERMES_HOME`，显式
指定对应的原生 `--profile`，防止 sticky `active_profile` 改变目标目录，并
保留启动工作目录。普通 Hermes 启动没有 AW 环境变量时，已安装插件不会注册
任何回调。

## Hook 语义

每个准入的 AW step 对应独立的原生 shell Hook 回调，通过
`PluginContext.register_hook` 注册。Hermes 按登记顺序串行执行回调，工具批次
的串行或并行仍由 Hermes 调度。已有配置中的 Hook 继续执行；AW 不导入或
重排其配置。该固定版本的原生 Hook 配置没有逐 Hook 的环境变量对象。

原生命令收到回调进程的实际环境，包括 profile 的 `.env` 变量，以及 Hermes
设置的 HOME/TMPDIR。结构化 Provider 保持启动绑定时固定的环境。AW 不将环境
内容写入归一化事件或审计记录。

| 原生事件 | AW 事件 | 可用行为 |
| --- | --- | --- |
| `pre_tool_call` | `tool.before` | 结构化 observe/block；原生命令保留 stdout 与退出状态语义 |
| `post_tool_call` | `tool.after` | 观察，包括被阻断、失败、取消及超时的工具结果 |

原生载荷提供 `session_id`、`extra.api_request_id` 和 `extra.tool_call_id`。
归一化 call ID 对请求与工具调用 ID 一起取哈希，允许模型在后续请求中复用同一
call ID。AW 拒绝缺少身份的回调，
保留任意工具名和对象参数，保持 `extra.result` 原始类型，包括已序列化的
JSON 字符串。缺少原生 call ID 时不会生成替代值。

Hermes 将原生 `action: block` 或退出码 2 解释为阻断。阻断不会使后续登记的
before 回调停止执行。原生 `action: approve` 表示请求人工确认，不表示自动
放行；结构化 AW `ask` 尚未支持。原生 `modify` 继续交由 Hermes 自身解析，
AW 不据此定义跨框架的参数改写合同。Post 回调不能撤销工具或替换结果。

AW 事件期限与 Hermes 回调期限共同生效。原生命令期限及清理余量超过
`plugins.hook_callback_timeout` 时，启动会拒绝该配置。插件完成注册后写入
私有就绪凭据，启动器据此显示原生 Hook 已就绪。这是同一用户下的安装证据，
不构成 final 或 protected 安全边界；插件加载失败后，Hermes 可能在启动器
检测到凭据缺失前继续执行。

## 验证

Rust 测试覆盖 profile 安装、原字节备份、幂等、未知字段保留、同名与符号链接
拒绝、事件身份及入口限制。可选原生测试在固定的官方 Hermes Python 环境中
使用临时 profile：

```bash
python src/aw/crates/aw-service/tests/fixtures/hermes/native_contract.py \
  --source /absolute/hermes-agent \
  --plugin /absolute/anolisa/src/aw/adapters/hermes
```

`live_cli.py` 使用确定性的本地 OpenAI 兼容服务、真实 AW daemon 与示例
Provider，运行官方 `chat --oneshot`。测试使用固定的 Hermes Python 环境，并以
`--source` 指定该源码目录，检查允许和阻断是否被采用、已有相对路径 Hook 共存、
before/after 命令、profile 与原生 YAML 值保留、回调环境、实例释放与清理。测试不使用真实
模型密钥，也不据此声称托管模型或交互式人工确认已经通过验收。

## 回退

先停止使用该适配器的会话，再将 `aw install` 返回的确切备份文件恢复到同一
profile 的 `config.yaml`，删除 AW 创建的 `plugins/aw-native-hooks` 和
`.aw-install.lock`。私有备份按用户的保留策略处理。普通 Hermes 启动也可以
保留插件，因为没有 AW 启动绑定时插件不会生效。
