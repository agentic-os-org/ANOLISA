# Policy CLI 使用指南

[English](../../en/agent-sec-core/policy-cli.md)

使用 `agent-sec-cli` 经 `asc-daemon` 管理带 revision 的 Policy Template 和不可变的
Scope 分配。Scope 保存所选 Policy revision 的完整内容；daemon 发现匹配的进程实例，
自动创建 Binding 并协调下发。Binding 仅支持查询。

这些命令由 V2 CLI 提供，已发布的 Python CLI 尚未包含。Policy、Scope 和 Binding 状态保存在 SQLite 中，daemon 重启后恢复；
Scope 受理不代表保护已生效。CLI 和 daemon 需一起升级：Scope update、Scope revision
参数和手动 Binding mutation 已移除。

## 连接 daemon

使用部署管理员提供的绝对 socket 路径。以下示例假设 `SOCKET` 已设置为该路径，
且当前用户已获得策略管理权限。未授权用户会收到 `permission_denied`；CLI 不负责
启动 daemon 或授予权限。

```bash
agent-sec-cli --socket "$SOCKET" policy list
```

## Policy 数据库位置

未设置非空 `AGENT_SEC_DATA_DIR` 时，daemon 检查两处 Policy 数据库：
A 为 `/var/log/agent-sec/policy-state.db`，B 为
`/var/lib/agent-sec/policy/policy-state.db`。A 的目录安全时选择 A，
仅在 A 的祖先不安全且 B 的目录安全时选择 B。这兼容 Ubuntu `/var/log` 等组可写祖先，
不修改系统目录权限。仅 Policy 存储可回退，安全事件和可观测存储路径保持原状。

创建任何文件前，daemon 检查两处是否已有数据库：

- 两处都有数据库时拒绝启动，不自动选择或合并。
- 目录检查的选址会绕过另一处已有数据库时拒绝启动，包括使用 B 后 A 又恢复安全的情况。
- 其余情况打开所选位置的已有数据库或初始化新库。坏库、未知 schema、锁占用和 I/O 错误
  不会触发换库。

祖先检查通过后，daemon 会先把自己拥有的已有叶目录权限修复为 `0700`，再完成选址。
例如 `/var/log/agent-sec` 为 `0755` 时修复并继续使用 A；`/var/log` 为 `0775` 时才允许回退 B。
不修改祖先目录权限。叶目录属主错误或权限修复失败均报错，不作为回退理由。

目录诊断包含失败路径、实际 UID/GID/mode 和权限要求；冲突诊断列出两处数据库路径，
要求恢复原目录配置或显式执行离线迁移。不自动移动、删除或重建已有状态。
路径不可读取或仅剩 SQLite sidecar 均报错，不能当成数据库不存在。

在 daemon 环境中显式设置非空 `AGENT_SEC_DATA_DIR` 时，使用该固定数据目录，
不参与默认 A/B 发现或回退。数据目录必须由 daemon UID 拥有且为 `0700`，
数据库及辅助文件为 `0600`。祖先必须归 root 或 daemon UID 所有且不可组写或其他用户写，
root 所有的 sticky 目录除外。路径中的目录不能是 symlink。

## 关联本地日志

V2 使用原生 OpenTelemetry 关联本地请求日志。`--trace-context` 保留原有扁平 Agent
metadata 输入，应放在命令名及其它选项的非选项值之前。可选的 `--otel-context` 接收
version 1 JSON carrier，包含 `traceparent`、`tracestate`、`baggage` 可选字段。
两者同时提供时，显式扁平 Agent 字段优先。

```bash
RUST_LOG=info agent-sec-cli --trace-context '{"session_id":"session-123","agent_name":"openclaw"}' \
  --otel-context '{"version":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}' \
  --socket "$SOCKET" policy list
```

`RUST_LOG=info` 开启 stderr 上的有界 JSON 关联诊断；daemon 需要单独设置其进程环境。
默认 warn 不产生这些记录。背压下允许丢诊断；命令结果、错误及退出码保持原有语义。
当前不提供公开 OTLP exporter 或导出/采样/batch 配置；`OTEL_*` 设置不能开启导出或
改变固定的本地采样策略。`--otel-context` 是入站上下文，不是导出目标。
CLI 要求支持 carrier 的新 daemon，应先升级 daemon。

## Intent：从任务生成待审候选

`agent-sec-cli intent propose` 将任务转换为可组合的待审 Cedar policy，供人工
核对精确路径、目录子树、动作和条件。目前仅输出候选策略，不写入策略管理记录、不绑定执行
进程，也不下发执行。人工审阅后可将候选 Cedar 原文保存为文件，按下文示例使用
`policy create --file` 提交；Policy 准入仍会校验源码和 schema。

LLM 只接收任务文本和统一 `seccore.cedarschema`，直接返回
`{"policy":"...","unresolved":[]}`。Rust 解析原始 Cedar、检查绝对路径表达式
并执行官方 Strict 校验，不追加任何规则。
`asc.intent.draft.v1` 草稿保存原始响应及任务来源。校验通过或 `unresolved` 为空均不证明
理解正确或完整；未解决要求由模型自报。CLI 使用内嵌 schema 从草稿重建预期产物，
同时复核版本、来源、TTL 和哈希。CLI 与 daemon 须同步升级。
生成规范位于
[`policy-generation.md`](../../../../src/agent-sec-core/v2/crates/asc-intent/prompts/policy-generation.md)，
随二进制编译嵌入，标记为 `cedar-policy@1`；候选仍须人工核对。

管理员须在 daemon 启动环境中设置 `ASC_INTENT_ENABLED=true` 与 `DASHSCOPE_API_KEY`，
配置变更重启后生效。保留 `deepseek-v4.1-flash` 为默认模型，它响应较快，无需显式设置
`ASC_INTENT_MODEL`；实际延迟随机型、任务和服务而变化。Intent 不绑定 DeepSeek，
可通过 `ASC_INTENT_MODEL` 选择其它可用模型，例如 `qwen-plus`，不必修改响应协议或
Cedar 检查。默认 API base 为 `https://dashscope.aliyuncs.com/compatible-mode/v1`。
`ASC_INTENT_BASE_URL` 可切换其它 OpenAI-compatible HTTPS 服务；虽然变量沿用历史名称，
`DASHSCOPE_API_KEY` 此时应配置为所选服务的 API key。服务须接受 JSON chat-completions
请求参数，并在输出预算和期限内返回完整 policy/unresolved 响应。

`ASC_INTENT_THINKING` 接受 `disabled`（默认，发送 DashScope 扩展
`enable_thinking: false`）或 `provider`（省略该扩展）。服务不支持此扩展时应使用
`provider`。例如，在 daemon 启动环境中选择其它模型：

```bash
export ASC_INTENT_MODEL=qwen-plus
export ASC_INTENT_THINKING=provider
```

配置无效或模型响应不合法时返回不可用，不影响其它 daemon 功能。
自动选择模型和本地模型回退尚未实现；后续计划评估本地小模型以降低云端调用费用。

此功能只把任务正文发送给云模型，可能产生费用。
不采集文件正文、目录列表或 Git 变更，daemon 不打开调用方路径。
任务中的绝对路径和敏感文本会发送，请先检查任务内容。

```bash
agent-sec-cli intent propose \
  --task "Allow reading and writing the src directory tree; keep config/important.txt unchanged. Do not execute programs or use the network."
```

只需要 `--task`：连接默认 daemon 端点并输出 text。
模型须把任务目标表达为规范绝对路径，可引用尚未创建的文件；相对路径或无法确定的
目标写入 `unresolved`，不接受用户另外补充路径或保护规则。
`--socket` 或 `AGENT_SEC_DAEMON_SOCKET` 可覆盖端点，连接失败不回退本地模型。

### Policy 和安全边界

- 只使用统一 schema：`file.read`、`file.write`、`file.exec`、`network.use`。
  修改请求通过 `WriteOperation` 区分 `write/create/delete/rename/setattr`；禁删不等于不可变。
  `file.read` 表示内容读取，不等于仅列目录名或读取元数据。
- 模型输出就是全部授权：程序不追加 permit/forbid，也不覆盖模型写出的规则。
  task 要求“不删除 a.txt”时由模型自行写出对应 forbid，而不是让用户提供保护列表。
- 路径使用 `resource.path`、`context.input.program/cwd`，不使用 File UID。
  前端接受规范绝对字符串的 `==`/`!=`，或仅在目录末尾含 `/*` 通配符的 `like`。
  动态或非规范路径、未绑定模板、模型自报 Subject/File 身份及可选 `@id` 之外的注解均被拒绝。
  路径属性只能从上述接收者直接读取，`{r: resource}.r.path` 这类 record 投影会被拒绝；
  与资源类型不匹配的 action 也会校验失败，与 Policy admission 保持一致。
- 提示词要求将无法表达的必要身份、条件、审批或义务保留于 `unresolved`，不能改成
  无条件 permit。工具调用不能替换为程序启动，裸程序名不能通过 PATH 猜测。
  这是模型指令，不是确定性的完整性或冲突检查。
- 条件可直接使用 schema 的 `program`、`cwd`、带索引的 `args` 与 `arg_count`，以及可选
  `mode/ownerUid/ownerGid`；可选字段须先用 `has` 判断。参数 index 0 对应 `argv[1]`，不含 `argv[0]`。
- Cedar 无匹配 permit 默认 Deny，forbid 优先。普通 permit 不限制其它策略的允许。
  排他要求须用原生补集 forbid 表达并包含条件；write 补集须限定所请求的 operation。
- 纯禁止不应补 permit；程序不自动添加敏感路径黑名单、程序或网络禁令。
  请结合任务核对实际 Cedar；Strict 和模型自报 `unresolved` 均不证明每项要求已经落实。

### 查看结果

`--format text` 为默认值，输出原任务、模型 Cedar 草稿、`unresolved`、已校验 Cedar、
安全提示及有效期。`--format cedar` 输出待审 Cedar，注释中附未解决要求；
仅当 stdout 是终端时转义 Cedar 中的控制字符，重定向或管道输出保留模型原文；
`--format json` 输出完整结果。
模型 policy 为空时，`artifact.text` 为空。任何模式都不会读取键盘输入，也不发起审批。
默认通信预算为 45 秒；模型调用最多 30 秒，调大 `--timeout-ms` 不会延长模型预算。
`--ttl` 为 `1..=1800` 秒，默认 `1800`，从 daemon 收到请求时计时，不自动续期。

模型 JSON 响应最多 16 KiB；`unresolved` 最多 128 项，每项非空且最多
4096 字节。最终 Cedar 最多 1024 条策略、256 KiB；完整结果
最多 512 KiB。任一预算超限均整项失败，不截断输出。

`unresolved` 非空时随候选一并展示，没有可继续确认的动作。
任务被模型或 schema 判定无法表达属于正常结果，不是失败：CLI 打印模型草稿和每一条
`reasons`，不输出 Cedar，退出码为 `0`。Ctrl-C 退出 `130`。任何一次运行都不会安装、
记录或请求审批。

请求使用 `asc.intent.request.v1`，候选使用 `asc.intent.candidate.v1`，恒为
`status: "CANDIDATE"` / `authorization_required: true`。候选保留 `draft`（原始 `policy`、
`unresolved` 和任务来源）、TTL、哈希和 `notes`。
`artifact` 包含 `language`、`text` 和必填 `schema_sha256`。版本不匹配直接失败；已有候选须用
匹配版本的 CLI 和 daemon 重新生成。

`candidate_hash` 覆盖除自身外的完整候选，但不是签名或审批凭据。CLI 还从草稿重建最终产物，
即使重算哈希也能发现模型输出被替换。官方 Strict 仅证明 schema 一致性，
不证明任务完整性、可信身份或真实文件操作已被拦截。

| Intent 结果 | 退出码 |
|-------------|--------|
| `candidate`（`unresolved` 不论是否为空） | `0` |
| `needs_clarification`（草稿和原因，无候选） | `0` |
| 输入、传输、配置、模型或 Cedar 校验错误 | `1` |
| 收到时候选已过期 | `2` |
| 被中断（Ctrl-C） | `130` |

退出码只说明命令是否跑完，不说明任务能否表达；后一点看 `outcome` 和 `reasons`。

此 Cedar schema 只有 `file.read`、`file.write`、`file.exec`、`network.use`，没有 TTL
或进程身份。可信审批、具体进程绑定与到期撤销仍须由后段实现。执行端还须规范化路径、
处理符号链接/硬链接和命名空间，分别裁决 rename 的源与目标，并保证参数索引和
`arg_count` 一致、属性值有效。本命令不证明这些条件
已经实现，因此不能声称 `rm` 已被拦截。

### 单次命令候选（hook 集成）

`agent-sec-cli intent propose-command --command "<命令文本>" [--cwd <目录>]` 面向
hook 集成：只发送 hook 已有的 `command` 与 `cwd`，返回该条命令的未审批 Cedar。
输入是任意可执行命令行，不只 shell 内建：`python3 train.py`、`node app.js` 与
`rm -rf build` 同等处理，差别只在命令本身。
入口只有 `PreToolUse` 中带命令行文本的 shell 类工具：cosh-ng 的
`run_shell_command`/`shell`（`cosh-extension.json` 中 `sandbox-guard` 的 matcher
`^(run_shell_command|shell)$`）、Qoder 与 Codex 的 `Bash`、Qwen Code 的
`run_shell_command`；Python 脚本没有独立的工具入口，也经它提交。payload 提供
`tool_input.command`；cosh-ng 另提供 `tool_name` 与会话 `cwd`，未提供 `cwd` 的宿主
需显式传 `--cwd`。

以 cosh-ng 为例，该 hook 点目前能拿到的字段里，只有 `command` 与 `cwd` 会进入候选：

| 字段 | 用途 |
|------|------|
| `tool_input.command` | 命令文本，唯一的策略来源；不含 argv、环境变量或 `PATH` 解析结果 |
| `cwd` | 解释相对路径并解析为规范绝对路径，不是授权范围 |
| `tool_name` | 识别 shell 类工具，不写入候选 |
| `session_id`、`run_id`、`tool_use_id` | 会话与调用关联，供日志和审计使用 |
| `hook_event_name`、`timestamp`、`transcript_path`、`skill_context` | 协议字段；cosh-ng 的 `transcript_path` 仅为占位 |

payload 没有 argv、进程环境、`PATH` 解析结果、真实可执行文件绝对路径、重定向目标和网络
端点，所以候选只能表达命令字面量能确定的部分。需要这些信息时必须修改 hook 实现（在 hook
内解析命令行、读取环境或解析链接后传入新字段）；本接口不做补齐，CLI 也不在本地推断。

其它 `PreToolUse` 调用不适用：`Read`/`Write`/`Edit`/`Grep`/`Glob`/`Skill`/`Task`/MCP
等传入的是结构化参数，没有命令行，需要“工具参数 → 请求权限”的单独映射，本接口未提供，
后续会提供；用户输入、工具执行后、会话结束等 hook 点也没有命令文本。
若同一链路上在前的 hook 已改写 `tool_input.command`（例如 sandbox 包装），
应传入改写前的原始命令。
`--cwd` 默认为 CLI 进程的当前工作目录，并解析为规范绝对路径；它只用于解释相对路径，
不是授权范围。命令不会被执行，也不读取文件正文。

```bash
agent-sec-cli intent propose-command \
  --command 'read -r line < report.csv' \
  --cwd /work
```

因为调用方解析该输出，默认格式为 JSON：
`{"outcome":"candidate","policy":"Cedar 文本","caveats":["..."]}` 或
`{"outcome":"needs_clarification","reasons":["..."]}`。`caveats` 是不阻断候选的
已知缺口，例如 PATH 未解析或脚本内部行为未知，审批时必须一并查看；没有缺口时为空数组。
`--format text` 把候选按 read/write/exec/spawn/network 分组列出路径，未出现的类别显示
`none`，并在末尾单列 `Not covered by this candidate`；策略含无法摘要的条件时回退为原始
Cedar。`--format cedar` 把 `caveats` 写成 `// Not covered:` 注释；澄清时只输出注释，
不会把 permit 误当成结果。
只有命令语义本身无法表达时才返回澄清，不输出部分策略；命令很短也应有策略，只有完全没有
文件、进程、网络动作（如 `pwd`、`echo`、`date`）时才为空。空策略是合法候选：授权为空、
`caveats` 随候选返回，沙箱只差运行依赖；CLI 在渲染前重新执行一次 Cedar 校验。
两种结果退出码均为 `0`；daemon 会拒绝的输入在本地退出 `2`，不发出请求。

`--format cedar` 输出未审批候选原文，审批与执行不在本子命令范围内。候选只含命令自身需求，
program、cwd 与 loader 路径不在其中，需执行侧显式补齐；CLI 不做隐式补齐。实际生效仍由 PDP
与发布的 `environment_permissions` 和策略层求交：
`effective = 请求 ∩ environment_permissions ∩ 策略`。

本路径不读取或改写 base policy，不在本地计算 `effective`，也不执行命令。

## 管理 Policy

准备 JSON 模板文件，例如 `policy.json`：

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/workspace/important/**"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect important files from deletion"
    }
  ]
}
```

PolicyTemplate 是可被多个 Scope 按 ID/revision 选用的可复用策略。`rules` 表达动作、
带类型的目标、决策、条件和可选理由。当前资源格式为 `{"type":"file","path":"..."}`，
每条规则一个路径。合法的文件 read/write/exec 规则、逻辑条件、历史条件以及
block/allow/require_confirmation 决策均可保存；Network 和 AgentHook 的目标格式尚未定义，
目前不能保存。

对于原生 JSON，PEP Translator 只支持不带历史条件的 block + file/write + operation=delete 规则。
任意规则不支持，整个 Binding 失败，例如返回有界错误码 `RULE_1_UNSUPPORTED_EFFECT`，
不发送受支持的规则子集。目标 DSL 不支持转义语法，because 包含引号、反斜杠或控制字符时
转换失败，普通 Unicode 文本保持原样；省略理由时使用 Translator 默认值。
策略创建成功不代表目标 PEP（策略执行点）支持这些规则。

生成的 DSL 表达禁止删除；真实内核是否仅阻止删除仍需单独验证，当前 ActPlane 的
unlink/write 共用底层操作映射。旧 kind JSON 和包含该格式的开发期数据库不提供迁移，
也不自动重建；已有部署应先用兼容的旧二进制完成清理，再安排新库。

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --language json --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --language json --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

`--language` 决定 `--file` 的读取方式：`cedar`（缺省）读取 Cedar 原文，`json` 读取原生模板
或 JSON 源码封装。

**兼容性变更（Breaking change）**：除非传入 `--language json`，`policy create`、
`policy update` 现在都把 `--file` 当作 Cedar 原文读取。未加该参数而传入 JSON 文件的脚本
会被 daemon 以 `invalid_argument` 拒绝，Policy 不会变化；加上 `--language json` 即可保持原有行为。

ID 和 revision 使用成功命令返回的值。这些是参考示例，并非顺序执行脚本；应在目标
Policy revision 仍为 current 时创建 Scope。create/update 均要求名称和完整模板，
不是部分更新。名称或模板变化才增加 revision，相同内容不增版。只保留 current 完整
记录；get/delete 要求精确的当前 revision，旧版本返回 `not_found`。文件相对路径按
CLI 工作目录解析。

更新或删除 Policy 不改变已有 Scope 和 Binding。撤销分配应删除 Scope。已删除的已知
Policy ID 保留 revision 计数器；update 可按下一 revision 重建内容，未知 ID 不可 update。

## Cedar Policy 源码

同一组 `policy create`、`policy update` 命令可直接读取 Cedar 文件。将以下示例保存为
`cedar-policy.cedar`：

```cedar
forbid(principal, action == SecCore::Action::"file.write", resource)
when {
    context.input.operation == SecCore::WriteOperation::"delete" &&
    resource.path == "/workspace/important"
};
```

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "Cedar protection" --file cedar-policy.cedar
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "Cedar protection v2" --schema-revision 1 --file cedar-policy-v2.cedar
```

文件必须是 UTF-8，按原文保存。`--schema-id` 缺省为 `seccore.system`，`--schema-revision`
缺省为 CLI 内置 schema 的 revision，两者仅用于 `--language cedar`。解析得到的引用随
Policy 一起保存，之后升级不会改变已有 Policy 的 schema。`agent-sec-cli policy default`
以 Cedar 文件形式输出内置基线，可直接用上述命令导入；`policy default --json` 改为输出
JSON 源码封装。

JSON 源码封装在文件内指定 schema，使用 `--language json` 导入：

```json
{
  "language": "cedar",
  "content": {
    "schema": {"schemaId": "seccore.system", "revision": 1},
    "text": "forbid(principal, action == SecCore::Action::\"file.write\", resource) when { context.input.operation == SecCore::WriteOperation::\"delete\" && resource.path == \"/workspace/important\" };"
  }
}
```

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "Cedar protection" --language json --file cedar-policy.json
```

Get/list 返回保存的原文及精确 schema 引用。语法、schema 或公共子集校验失败时返回
`invalid_argument`，不修改已保存的策略内容及 revision。未知 schema revision 直接拒绝，
不回退到其他版本。原生模板继续使用原有形状。

策略创建成功不代表目标 PEP 支持该策略。当前 PEP Translator 支持限制删除、普通写入和精确文件禁读的
Cedar `forbid` 子集。禁读使用 `file.read` 和精确文件路径，要求 AgentSight 报告
`file_read_protection` 能力；不支持目录树禁读。参见 [AgentSight 禁读边界](../agentsight/integrations.md#禁止读取密钥文件)。
不支持的语义使整个翻译失败。
Policy 更新或删除后，已有 Scope/Binding 快照仍保留原
Policy revision 和 schema。源码可重新加载为内存 PST，SQLite 不保存 AST/PST。
旧二进制不能读取新增 Cedar 记录，需要回滚时应保留升级前的数据库备份。
深层嵌套源码导致 SDK 栈溢出的已知限制仍未解决，应使用管理员审阅的策略，避免不可信生成输入。

Sandbox 运行时授权复用同一组 Policy 对象，使用
`schema: {schemaId: "seccore.system", revision: 1}`。OS 授权 PDP 支持 Agent 主体、`process.spawn` 及 permit 上约定的 advice。
按 [Sandbox 指南](sandbox.md#接入策略授权)在 daemon 授权配置中选择基线和可选任务 Policy ID。
每次求值一起读取所选策略的当前版本；更新成功后影响后续请求，策略缺失或删除时停止新授权。
已有 Scope/Binding 快照和已经接收的决定继续保留各自绑定的版本。

## 管理 Scope

创建时提供 Policy ID、精确 revision 和一个 selector：`--process-name` 精确匹配 Linux
进程名（1–15 bytes），`--executable` 精确匹配规范化绝对可执行路径，`--cmdline-arg`
精确匹配作为一个完整命令行参数出现的规范化绝对路径，或 `--pid` 选择一个进程实例。
名称、可执行路径和命令行 selector 持续发现已有和后续进程；PID 固定第一次观察到的实例，
不跟随 PID 复用。目前拒绝 cgroup assignment。

`--cmdline-arg` 用于在 node、python 等共享解释器下运行的脚本型 Agent：这类进程的
进程名和可执行路径只能标识解释器。传入用户实际运行的命令路径，即 `which` 的输出。
`/proc/PID/cmdline` 中任一参数与该路径逐字节相等即命中；不解析符号链接，子串或前缀
不匹配。例如在 shell 中运行 `qodercli` 得到 `node /usr/local/bin/qodercli ...`，
`--cmdline-arg /usr/local/bin/qodercli` 可以选中它；通过其他路径启动脚本（如解析后的
`.js` 文件或相对路径）不会命中。同一用户下以该路径为参数的其他进程（如用编辑器打开它）
也会被选中。命令行参数由进程自身设置，因此该 selector 用于在 owner UID 内识别进程，
不用于认证。

**兼容性变更（Breaking change）**：移除 `scope create --cgroup-id`，此前不支持 cgroup
assignment。继续传入该参数的脚本会在本地报错，退出码为 2；请改用 `--pid`、
`--process-name` 或 `--executable` 选择目标进程。

```bash
agent-sec-cli --socket "$SOCKET" scope create --process-name openclaw --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope create --cmdline-arg "$(which qodercli)" --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope retry --scope-id "$SCOPE_ID"
```

Scope 没有 revision，不可 update；`policySnapshots` 保存所选 Policy 的完整内容。
若并发 Policy 更新/删除先于 Scope 准入提交，创建返回 `not_found`，不会静默替换版本。
准入成功后，后续匹配的新实例也使用该快照。CLI 每次分配一个 Policy，RPC 支持每个
Scope 1–32 个不同 Policy。

替换分配时，先创建新 Scope，查询所需实例的 Binding 达到 `READY` 后再删除旧 Scope；
这不是原子切换。

删除先关闭发现准入并停止 worker，再异步清理所属部署。未完成时返回
`{"scopeId":"...","completed":false}`，完成后返回 `completed:true`。
清理期间 Scope 保留 `DELETING` 状态，失败 Binding 仍可查询。重复 delete 不重置重试
预算；排除故障后使用 `scope retry` 重试终态失败，不修改分配或重启正在运行的工作。
已完成的删除及未知 Scope ID 的删除均返回 `completed:true`，daemon 重启后也可重复。
其它 Scope 和来源 Policy 不受影响。

## 查询 Binding

daemon 为每个 Scope、Policy 和具体进程实例创建一条 Binding，包含策略快照、Scope ID
和 selector、固定的进程身份及当前状态。通过 `spec.scope.scopeId` 识别所属分配。

```bash
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
```

应用从 `PENDING_APPLY` 经 `APPLYING` 到 `READY`。永久或预算耗尽的失败显示为
`APPLY_FAILED`/`DELETE_FAILED`，原因在 `status.error`；查询命令仍成功退出。
进程退出或 selector 失配与 Scope 删除使用同一清理路径。部署结果不确定时，责任保留到
确认目标不存在。自动重试有界，`scope retry` 重试所属失败工作。目前没有 `--wait`。

## 公共参数

| 参数 | 用途 |
|------|------|
| `--socket PATH` | 必填的 daemon 绝对 socket 路径，可放在子命令前后 |
| `--timeout-ms N` | 正 32 位无符号整数，默认 `5000`；连接、发送、接收共用一次时间预算 |
| `--limit N` | 列表每页条数，`1..=1000`，默认 `100` |
| `--offset N` | 列表偏移量，32 位无符号整数，默认 `0` |
| `--help` | 顶层、命令组和具体操作的帮助，不连接 daemon |
| `--version` | 顶层版本信息，不连接 daemon |

支持 `--key value` 和 `--key=value`。带空格的名称、路径需使用引号；
以 `--` 开头的值使用 `--key=--value`。重复参数会被拒绝。
列表命令每次返回一页 `{items,total}`，`total` 是分页前总数。后续页需显式请求，
offset 按实际返回条数推进；达到 3 MiB 编码条目预算时可能提前结束一页。

## 输出与错误

| 结果 | 输出 | 退出码 |
|------|------|--------|
| 成功 | stdout 输出格式化的结果 JSON，stderr 为空 | `0` |
| 本地 `policy default` | stdout 输出 Cedar 原文；`--json` 改为 JSON 源码封装；不请求 daemon | `0` |
| daemon 拒绝 | stderr 输出 JSON `{requestId,error:{code,message}}`，stdout 为空 | `1` |
| 文件、连接、响应或输出失败 | stderr 输出错误说明 | `1` |
| 命令行参数错误 | stderr 输出用法错误 | `2` |

Policy 源码文件最大为 4 MiB。`--language json` 要求有效 JSON，不能包含未知或重复字段；
`--language cedar` 要求 UTF-8 Cedar 原文。
完整编码后的请求和响应各自另有 4 MiB 上限，包含行结束符。文件大小检查通过不保证
组装后的请求符合上限；请求超限时不会发送。daemon 另在保存前拒绝编码后超过
1 MiB 的 Policy 或完整 Scope。

超时预算覆盖 daemon 通信，包括等待 daemon 处理请求；不包含文件读取、请求编码、
响应解码和结果输出。CLI 不自动重试。发送后超时、响应缺失或非法，不代表请求没有
执行；再次提交变更前应先查询当前状态。
