---
name: policy-authoring
description: 把自然语言授权意图（允许、禁止、只允许某个范围）写成 agent-sec-core Scope/Binding 部署路径使用的 SecCore Cedar policy，不生成 sandbox 请求授权策略。Agent 按随本 Skill 分发的 schema、语法参考、示例和自检清单逐步完成：先问清歧义，再写规则，再自检，最后回译确认，全程不需要命令行工具。当用户要求"让 Agent 可以读写某个目录""禁止删除某个文件""只允许某个程序执行""把这段授权需求写成策略"时使用。只负责生成和自检策略，不代为批准、绑定或下发；查询已发生的安全事件用 security-observability，检查代码或 prompt 用 code-scanner、prompt-scanner。
---

# Policy Authoring（SecCore Cedar）

本 Skill 的规则、示例和自检仅适用于 **Scope/Binding 部署路径**。生成的裸 principal
策略即使成功保存到 PAP，也不参与 sandbox 的请求授权。若用户需要请求授权策略，说明此边界，
引导至[请求授权指南](https://github.com/agentic-os-org/ANOLISA/blob/main/docs/user-guide/zh/agent-sec-core/sandbox.md#接入策略授权)，不要将本 Skill 的输出当作已生效的请求限制。

你的任务：把自然语言授权意图写成一份 agent-sec-core 能加载的 Cedar policy，并且**表达出用户真正
想要的边界**。这是形式化任务：语法不是难点（`references/` 里有），难点是把含糊意图钉到固定
词汇表上的具体动作、路径和条件。

- **这是一次对话，不是一次性生成。** 先问清会改变输出的缺口，再写规则，再自检，最后回译。
  不要跳过消歧直接给一份"看起来合理"的 policy。
- **你写规则，不写 schema。** SecCore schema 由 agent-sec-core 固定提供，动作、资源、字段和
  上下文都不能新增或改写。
- **全程离线。** schema、语法、示例、自检清单都随本 Skill 分发，不调用命令、不依赖服务、
  不读仓库源码。

按顺序执行，每步都有产出：

0. **读契约**：先过"基准事实"和"固定词汇表"，再动笔。
1. **消歧**：把影响输出的缺口问清，每个结论记成一句假设。
2. **形式化**：按 2a 写单条、2b 配补集、2c 判断能不能表达、2d 收尾。
3. **自检**：逐项过清单，把会被拒的写法改到全部通过。
4. **回译**：读回自然语言，说明哪些仍然被拒绝，然后交付。

## 什么时候触发

- 触发：用户想把"Agent 能做什么、不能做什么"落成策略，例如"让 Agent 只能读 `/work/repo`"、
  "禁止它删 `build` 目录"、"只允许 `git` 在 `/work` 下执行"、"把这段授权需求写成 policy"、
  "这条 policy 为什么被拒，帮我改到能过"。
- 不触发：查历史拦截记录用 security-observability；扫代码或 prompt 用 code-scanner、
  prompt-scanner；只问 Cedar 语法常识且不产出策略；要求批准、绑定或下发。

## 自己跑一遍（验收本 Skill）

1. 确认宿主已加载本 Skill（安装时已整个目录落盘，自包含，不读外部文件）。
2. 发一句触发句，例如"禁止 Agent 删除 `/work/repo` 下的文件，读取也只限这个目录，写成 policy"。
   七个示例各自的触发句和预期产出在 `examples/README.md` 的"每个示例怎么触发"，可直接拿来发。
3. 期待行为：先按 Step 1 问出至少两个关键歧义（operation 集合、子树还是单文件），再给 Cedar，
   最后给自检段落和 unresolved。没按步骤走就对照文末"不要"收一收。

## 基准事实（写之前先读）

以这些为准；不在这些里的动作、字段、写法一律当作不存在，不要凭记忆或别的 Cedar 教程补。

- `references/seccore.cedarschema`：schema 唯一来源（schemaId `seccore.system`，revision `1`），
  动作、资源、字段的类型契约。
- `references/cedar-policy-language.md`：可用语法、路径形态、能解析但会被拒的清单与下游提交格式。
- `references/self-check.md`：生成后必过的自检清单和交付标注要求。
- `examples/README.md` 与 `examples/*.cedar`：可整份复用的 Cedar 示例。

## 固定词汇表

每个请求是 `(principal, action, resource, context)`。本 Skill 的部署策略中，`principal` 写裸 `principal`：主体由
Scope/Binding 选择，策略里不写主体条件。文件类动作的 `resource` 是 `SecCore::File`，用
`resource.path` 读目标路径；所有可读事实都在 `context.input` 里。

| action | resource | `context.input` 字段 |
|---|---|---|
| `file.read` | `SecCore::File` 的 `resource.path` | `program`（发起读取的可执行文件） |
| `file.write` | `SecCore::File` 的 `resource.path` | `program`、`operation`、`mode?`、`ownerUid?`、`ownerGid?` |
| `file.exec` | `SecCore::File`，`resource.path` 是要执行的程序 | `cwd`、`arg_count`、`args`（`{index, value}` 集合） |
| `network.use` | `SecCore::Network::"all"` | 无（`context {}`） |

`WriteOperation` 取值：`write`、`create`、`delete`、`rename`、`setattr`。

会咬人的规则：

- **动作分别授权。** 本 Skill 每条规则只写一个动作；限定目录的"只读审查"需 read permit、
  目录外 read forbid，再加 write / exec / network 三条 forbid。
- **资源必须和动作对得上。** 给 `file.read` 配 `Network` 资源这类组合，SDK 只报 warning，
  但提交策略时会被拒。文件类写 `resource is SecCore::File`，网络写
  `resource == SecCore::Network::"all"`。
- **注解一律不能写。** 本 Skill 不生成任何注解（含 `@id`、`@advice`）。给读者看的说明
  写成 `//` 注释；注解不能用来给 Agent 留提示。
- **`file.exec` 的 `resource.path` 是程序本身。** 锁定参数用 `arg_count` + `args` 全量精确匹配：
  `context.input.arg_count == 1 && context.input.args == [{index:0,value:"test"}]`，
  索引 `0` 是 `argv[1]`，`argv[0]` 不在 `args` 里。
- **可选字段先 `has` 再读。** `mode`、`ownerUid`、`ownerGid` 直接取是校验错误。
- **`network.use` 只有全网络开关**，没有 host、port、协议维度。
- **`file.read` 是内容读取**，不等于列目录名或读元数据。
- **规模上限**：本 Skill 生成的策略集不超过 1024 条、文本不超过 256 KiB，属于写作约束。
- **历史、时间窗、计数、审批、主体身份**在当前 schema 里都不存在。

## 不能改变的地板

一份 policy 只能决定自己写到的许可；最终 policy set 中没有匹配 permit 的组合默认拒绝。有些事任何规则都改不了：

- **默认拒绝，forbid 优先。** 没有匹配的 permit 就是拒绝；forbid 始终压过 permit，包括压过
  别的策略给的 permit。
- **permit 只增加许可。** 普通 permit 不能收窄另一条 permit，也不能收窄别的策略；"只允许 X"
  必须用同动作的补集 forbid 表达。
- **主体不在策略里。** 写裸 `principal`，不写 `principal == ...`，不在条件里引用 `principal`。
- **没人替你补规则。** 写出的文本就是最终策略：要求"不删除 `a.txt`"，就必须自己写出对应的
  forbid，不要指望下游追加或改写。
- **不猜身份、路径和存在性。** 裸程序名（`git`、`python3`）不是绝对路径；PATH 解析、DNS、
  符号链接和文件是否存在都不归策略管。
- **路径边界是词法的。** 规则只按字面量比较请求里的路径字符串，不折叠 `..`、`.`，也不解析
  符号链接：`/work/project/../../etc/shadow` 仍匹配 `like "/work/project/*"`，
  `/work/config/./important.txt` 不等于 `/work/config/important.txt`。要求物理路径边界时，
  规范化与 symlink 解析列入 unresolved，必须由可信请求构造层先做。
- **工具调用不等于程序启动。** 不要把"禁止调用某个工具"写成 `file.exec`，agent hook 动作尚未定义。
- **通过自检不等于理解正确，也不等于已生效。** 自检不证明实际校验通过；候选仍需人工批准、
  绑定和下发。

## Step 1 —— 消歧

只把用户**说出来的授权**写成规则，不从目标反推权限。"把故障修好""帮我做完这个需求"都不是授权
描述：它们可能涉及读日志、查资源、发通知，但不能据此推出"可以读所有日志、可以改所有资源"。
缺什么问什么，问出来的才写。

含糊说法到具体条件的对照：

| 用户说法 | 写规则前必须确定 |
|---|---|
| "可以读取日志" | 哪个绝对路径？单文件还是整棵目录？ |
| "改一下配置" | 哪个路径？`operation` 包含哪些：只 `write`，还是也含 `create`、`rename`、`setattr`？ |
| "跑测试" / "执行命令" | 程序绝对路径、`cwd`、是否锁定 `arg_count` 与 `args` |
| "最近访问过的文件" | 时间窗在 schema 里不存在；改用路径或程序条件，否则进 unresolved |
| "同一个文件" | 用同一 `resource.path` 字面量，每个动作各写自己的条件，不存在跨动作自动关联 |
| "最多三次" / "每小时 6 次" | 计数与频率不可表达 → unresolved，不要用近似规则冒充 |
| "只有我能删" | 主体不在策略里，由 Scope/Binding 选定 → 说明后进 unresolved |
| "禁止删除" | `forbid` + `operation == "delete"`；不能靠一条窄 permit 盖住宽许可 |
| "只允许访问 api.example.com:443" | 只有 `network.use` 全开/全关 → unresolved；全开须用户明确接受全部目的地 |
| "禁止调用某个工具" | 没有 hook 动作 → unresolved，不要写成 `file.exec` |

逐项确认，每问一项都说清它对应哪条规则：

1. **允许还是禁止。** "可以 X" → `permit`；"永不 X""X 时阻断" → `forbid` 挖洞；"只允许 X"
   → permit 加补集 forbid。
2. **哪些动作。** 把用户的名词映射到四个动作，一条规则一个动作。
3. **写操作的 operation 集合。** "写文件"通常指 `write` + `create`；`delete`、`rename`、
   `setattr` 要不要一起给？范围不清就问，写宽了就是扩权。
4. **路径精确到哪。** 单文件还是子树？子树需要两个子条件：`resource.path == "/work/project"` 或
   `resource.path like "/work/project/*"`。只给相对路径（`src/`、`./config`）时必须问出规范
   绝对路径，拿不到就写入 unresolved。真实 `*` 在等式中原样写，仅在 `like` 中写成 `\*`。
   "只能看代码"先问审查根目录，不授权读取全部文件。
5. **执行条件。** 程序的绝对路径是什么？要不要限定 `cwd`？要不要锁定完整参数？拿不到绝对
   路径就把这条要求交回用户。
6. **网络。** "只允许访问 api.example.com:443"无法表达：只有 `network.use` 全开或全关。
   把它记进 unresolved，不要用全网络许可冒充。
7. **身份、时间、频率、审批。** "只允许张三""每小时最多 6 次""需二次确认"都无法表达 → unresolved。
8. **边界。** 路径比较是精确相等，子路径和同前缀邻居都不匹配；`like` 只允许末尾 `/*`，且
   不匹配目录自身；比较的是字符串本身，`..`、`.` 和符号链接都不会被规范化。属性值要请求侧
   就带齐，缺字段时规则不匹配。

**别漏了 Agent 自身跑起来的权限。** 业务权限写对、Agent 自己却启动不了，是常见结果：确认它用
哪个模型端点、凭据放在哪个文件、需要执行哪些工具，再判断这些是否已被现有 Scope/Binding 覆盖。
没覆盖或无法确认就列入 unresolved，先问明确授权，不自动补权或绕过只读、禁网 forbid。
凭据与配置是 `file.read`，工具是 `file.exec`；仅用户明确接受全部目的地才写 `network.use` permit，
否则模型端点保持 unresolved。已有全网许可也不表示端点限制已落实。

提问示例："'写 `src` 目录'要不要包含删除和改权限？包含我加 `delete`、`rename`、`setattr`，
不包含就只给 `write`、`create`。"

## Step 2 —— 形式化

### 2a 单条规则：动作 → 资源 → 路径 → 条件

骨架（官方校验接受的子集）：

```cedar
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/work/project" || resource.path like "/work/project/*" };
```

一条规则只决定四件事，按顺序定下来：

1. **动作**：四个动作里选一个，一条规则一个动作。
2. **资源**：文件类写 `resource is SecCore::File`；`network.use` 写
   `resource == SecCore::Network::"all"`。
3. **路径**：单文件用 `==`；子树用 `resource.path == "/dir" || resource.path like "/dir/*"`。
4. **条件**：只用该动作声明的 `context.input` 字段，可选字段先 `has`。

书写清单：

- 头部只写 `principal, action, resource`，条件全部放 `when` / `unless`。
- 不写注解、不写模板；交付的文本至少一条规则。没有任何可表达要求时不交付 policy（见 2c），
  而不是交一段空文本。
- 路径只和规范绝对字面量比较：`==`、`!=`，或末尾 `/*` 的 `like`。
- 每条规则以 `;` 结尾；同一文件里的规则按动作分组，便于和交付说明对应。

### 2b 组合：permit 加补集 forbid

"只允许 X"永远是 permit + 同动作补集 forbid，两步都要写。补集用 `unless { 完整许可条件 }`，
连同路径、operation 或执行条件一起否定，不能只否定路径而保留 operation 正向条件。

```cedar
// 保留写权限，单独封掉删除；operation 条件不能省
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { context.input.operation == SecCore::WriteOperation::"delete" };

// 只允许某程序在该目录下执行，其余执行一律拒绝
permit(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
when { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };
forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
unless { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };
```

同一个动作有多个允许范围时，先并成一条联合条件，再写一组 permit + 补集 forbid。两份各自带
补集 forbid 的"只允许"不能直接拼进同一份 policy set：范围 A 的请求会命中范围 B 的补集，
最终两个范围都被拒绝。

```cedar
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when {
  (resource.path == "/work/project" || resource.path like "/work/project/*") ||
  (resource.path == "/work/vendor" || resource.path like "/work/vendor/*")
};
forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
unless {
  (resource.path == "/work/project" || resource.path like "/work/project/*") ||
  (resource.path == "/work/vendor" || resource.path like "/work/vendor/*")
};
```

### 2c 表达不了的事实

不是所有要求都能落成规则。历史（"最近""连续三次"）、计数与频率、计算事实（正则、风险评分、
语义分类）、主体身份、审批和精确到 host/port 的网络限制，schema 里都没有对应字段。遇到就写进
unresolved 并说明缺什么，**不要编一条近似规则充数**——那会让用户以为约束已经生效。

- **必要限制不可表达时，不生成那条 permit。** 限制是许可成立的前提，就不能把限制留在
  unresolved 而交付一条更宽的许可。例："这次会话、接下来一小时可以读 `/secret`"不写成永久
  read permit，而是整条记进 unresolved。
- **全部要求都不可表达时，只返回 unresolved。** 不交付任何 policy 原文；"不交付"不等于
  "交付空策略集"，空文本仍然不是交付物。
- **停下来等用户回答是合法终点。** 缺关键信息时把问题交回用户，等回答再继续，不要先给一份
  带假设的 policy 兜底。

### 2d 收尾

- 按动作与语义拆规则；排他意图需要 permit 与补集 forbid，不重复写语义相同的 permit。
- 不为"看起来更安全"加用户没要求的规则；一般不需要兜底 forbid，但明确的排他要求必须写补集。
- 写完整份通读一遍：每条规则能不能说清"谁在什么条件下做什么"，说不清就是还没想清。

## 常见形状

这七种形在 `examples/` 里各有一份可整份复用的文件，用法和逐条触发句见 `examples/README.md`。

| 形状 | 文件 | 要点 |
|---|---|---|
| 只读审查 | `readonly-review.cedar` | 已确认目录的 read permit + 目录外 read forbid + write / exec / network 三条 forbid |
| 只在某子树内读 | `allow-read-subtree.cedar` | 两个路径子条件用 `\|\|` 连接，permit 加完整补集 forbid |
| 可写但绝不删 | `forbid-delete.cedar` | 已有写许可的补充：forbid write 且 `operation == "delete"` |
| 单文件保护 | `protect-file.cedar` | 只在那个绝对路径上 forbid，不牵动别处 |
| 只允许某程序执行 | `allow-program-exec.cedar` | permit 加 `unless` 补集，条件含程序路径与 `cwd` |
| 排他写 | `exclusive-write.cedar` | permit 一个路径一个 operation，unless 否定完整条件，拒绝其余全部写操作 |
| 全网络关闭 | `deny-network.cedar` | forbid `network.use`，资源写 `Network::"all"` |

组合时按动作叠加：先给明确授权的 permit，再用 forbid 挖洞，最后按最终 policy set 确认边界。

## Step 3 —— 自检（必做）

生成后逐项过 [`references/self-check.md`](references/self-check.md) 的五段：结构、主体与资源、
动作与字段、路径、语义。清单覆盖语法、类型、写作约束与语义，命中就改到不命中为止。

这是本 Skill 的自检路径，不需要任何命令，也不依赖任何服务。交付时要写"已按清单自检"并列出过的
段落；它不证明实际解析、校验或语义正确，语义仍靠 Step 4 回译确认。

## Step 4 —— 回译并交付

自检过完再做意图回译，按顺序给出：

1. 一句话复述你的理解。
2. 你做的假设：路径、operation 集合、子树还是单文件、`cwd` 与参数。
3. 校验状态：已过自检清单的哪几段，以及仍然不确定、需要人工确认的判定。
4. Cedar 原文；没有可表达要求时明确说明本次不交付 policy。不包装为 JSON，包装和提交由下游
   完成。
5. 每条规则的中文说明，以及**哪些操作仍然被拒绝**。
6. 无法表达、需要交回用户和下游的清单（unresolved）。
7. Agent 自身运行所需的权限（模型端点、凭据、工具）是否被这份 policy 或现有 Binding 覆盖；
   没覆盖就说清缺哪一条，未确认或未明确授权的缺口列入 unresolved，不自动补权。

如果下游仍然拒绝，先弄清是哪条 forbid 或哪个 Binding 范围在起作用；再加 permit 通常解决不了
问题，只会让边界更难读。

不要把"自检通过"写成"已经保护"，不要声称已批准、已绑定或已拦截。

## 无法表达的要求（记进 unresolved，不要编）

- 精确到 host、port、协议的网络限制（只有 `network.use` 全网络开关）
- 时间窗、频率、速率、累计次数
- 主体身份（用户名、UID、agent 身份）
- 审批、二次确认、义务
- 工具调用限制（没有对应 action）
- 相对路径、运行时才能确定的路径、目录枚举和存在性判断
- 内容级判断（正则、风险评分、语义分类）

## 不要

- 不要发明动作、字段、类型；只有 `file.read`、`file.write`、`file.exec`、`network.use`。
- 不要写 principal 约束或条件，不要把 `context` 放进策略头部。
- 不要写注解（含 `@id`、`@description`、`@advice`）或模板，也不要交出空策略集充当交付物。
- 不要在 `like` 里用中间通配（`/a/*/b`）、裸前缀（`/a*`）、相对路径、`..`、重复 `/`、
  尾部 `/` 或路径中的真实反斜杠；`like` 模式中的 `\*` 是转义，不是路径反斜杠。
- 不要把资源类型和动作配错，也不要靠 SDK 的动作适用性 warning 过关——策略校验会拒。
- 不要把"禁止删除"扩大成"禁止全部修改"，也不要用全网络许可冒充 host 级限制。
- 不要交出没过自检清单的 policy。
- 不要把自检说成"已校验"或"已生效"；自检不证明实际校验通过或语义正确。
- 不要把不可表达的必要限制留在 unresolved 而交付更宽的 permit，也不要先交一版带假设的 policy
  再改；等用户确认是合法终点。
- 不要把同动作的多个排他范围写成两组 permit 加补集 forbid；先合并成联合条件。
