# Skill Score 主线架构与交付设计

[English](skill-score-native-design.md)

本文记录已批准并在本分支实施的架构契约，验收状态以绑定提交与制品的测试记录为准。开发从 ASC `codex/skill-score-public-compose@0f9355446ed3831b90826a1e8f21f72c2ad915a5` 和 SkillEval `codex/security-fc-integration@6cb567a7e62ab4def8703c042d42dabcaec61989` 开始；2026-10-27 已重新抓取官方主线 `23cd43a3df09aff4988528171af1b89fc5e742fb`，但本分支尚未重放到该主线。历史 Python 预览见[原运行设计](skill-score-runtime_zh.md)。

本版锁定三项交付决定：发布小型镜像构建源码包，基础 ASC 默认安装保持轻量，动态评测执行依赖必须显式安装。OCI 镜像仓库分发属于未来优化，不是首版的并行实现、主线合入条件或正式发布前置条件。

## 1. 目标与已确定的选择

将 Skill 动态安全评测纳入 AgentSecCore（ASC）产品，保持基础安装轻量：标准 `sec-core` 包包含 Rust 命令和评分核心，需要动态执行的用户显式安装可选执行组件，再通过 `agent-sec-cli skillsec score plan/run` 使用。SkillEval 只是调用这一组件的平台之一；单机 Linux 与 FC 内的评测使用同一套核心、worker 和网络实现。

| 事项 | 设计决定 |
| --- | --- |
| 公共入口 | 保留 `agent-sec-cli skillsec score plan/run`，不增加 `prepare`，不另设评分产品或服务 |
| 评分实现 | Rust 负责规划、模型裁决、证据判定、去重、计分和报告 |
| 动态执行 | 保留 Python 3.12 Harbor worker，使用未修改的 Harbor；不重写第三方执行框架 |
| 网络实现 | 统一使用 ASC 兼容侧车，普通 Linux 和 FC 相同；用户无需选择后端 |
| 安装交付 | 基础 ASC 包不自动安装评分执行依赖；自有 worker、锁文件、兼容性清单与安装 helper 放入显式安装的可选组件 |
| 镜像分发 | 只发布构建源码；显式安装时构建两个镜像，并核验与源码绑定的本地回执 |
| 运行行为 | 运行前检查；依赖缺失或不兼容则明确报错并给修复建议，不在评测过程中安装、拉取或构建 |
| 模型接入 | 扩展 ASC 共享模型客户端；评分模型与被测 Agent 共用配置解析，保留原扫描器的本地默认行为 |
| 平台责任 | SkillEval 继续负责 FC 模板、实例、凭据注入、上传下载、任务状态、超时、清理与运维 |

默认安装不新增评分专用 Python、Harbor、Docker 或镜像下载，即使宿主已经有 Docker 也不自动启用。动态执行的首版仍需要 Linux、可用的 Docker/Compose、兼容的内核能力及足够资源。安装可选组件不代表 ASC 会自动修改任意宿主机的内核或创建云权限。

## 2. 架构、进程与责任

| 所在位置 | 组件 | 责任与调用关系 |
| --- | --- | --- |
| 用户终端或 SkillEval 调度进程 | 调用方 | 通过固定 CLI 与文件契约提交 Skill，读取任务、报告和证据 |
| 执行宿主：单机 Linux 或 FC 实例 | Rust CLI + `asc-skill-score` | 创建输入快照，规划探针，调用 worker，裁决证据，输出报告；是评测控制器 |
| 同一宿主中的独立进程 | Python Harbor worker | 调用 Harbor API，创建探针环境，设置 fixture/mock，运行 Agent，收集和归一化证据，回收本次资源 |
| 每个探针的 Docker 环境 | 工作容器 | 运行被测 Agent、Skill 和采证工具，不持有 Docker socket 或网络管理权限 |
| 同一探针的独立容器 | ASC 网络侧车 | 执行固定出站策略；与工作容器共享网络命名空间，自身持有所需网络权限 |
| 评测控制器与工作容器之外 | 模型服务 | 分别接收 Rust Judge 和被测 Agent 的模型请求；两种角色的输出和费用分别记录 |

在单机场景，用户直接运行 ASC，ASC 管理内层 Docker 资源。在 SkillEval 场景，平台创建 FC 实例，实例内执行同一条 ASC 命令，ASC 管理内层 Docker，平台管理外层 FC。ASC 不包含 FC SDK、模板 ID、账号信息或云管理凭据。

FC 隔离的是整个评测实例；内层容器继续隔离不可信的被测程序与评测控制器。因此不把控制器、工作容器和网络侧车合成一个具有全部权限的进程。

### 2.1 Rust 与 Python 的边界

仅新增一个业务 crate：`src/agent-sec-core/v2/crates/asc-skill-score/`。

| 模块 | 职责 |
| --- | --- |
| `contracts.rs` | task/report 及内部 Finding、Probe 的类型与校验 |
| `source.rs` | 输入快照、完整内容摘要、路径和输出边界 |
| `scanners.rs` | 复用 V2 prompt/code/PII 能力，迁移必要的评分规则适配 |
| `fixtures.rs`、`masb.rs`、`redaction.rs` | 探针 fixture、规则分支与结构化脱敏 |
| `judge.rs` | 提示、结构化输出、重试预算与模型调用审计 |
| `evidence.rs` | 证据可信度、主体归因、动态结论和 Critical 确认门槛 |
| `score.rs` | 根因去重、扣分、精确舍入、最终报告 |
| `worker.rs` | 安装版本校验、子进程协议、超时取消与资源状态 |
| `lib.rs` | 静态信号、源码审计、探针规划、`plan`/`run` 编排和总预算 |

`asc-cli` 的现有 `skill_score.rs` 从 exec 私有 Python CLI 改为直接调用该 crate。Python worker 保留 Harbor hooks、异常后的证据恢复和 ATIF/observer 适配；首版不为追求“薄 worker”重写全部观测归一化逻辑。worker 不做模型 Judge、风险严重度决定或最终扣分。

删除的是评分专用 Python 3.11 环境，不是 ASC 所有 Python 组件；已有其他适配器不受这项决策影响。旧 Python 评分实现保留为迁移测试参考，完成等价性验证后退出发布包。

主线现有规范强调 V2 Rust 运行时和 daemon 路径。本方案需要在同一组主线 PR 中明确增加“显式调用的本地评测能力”契约：它不连接 daemon，不是 daemon 不可用时的降级路径；其 Python Harbor 适配器属于明确列出的外部执行依赖。不能仅通过改名或进程分离宣称自动符合原规范。

## 3. 用户安装与执行

以下命令对应本分支新增的 Rust 评分核心与可选组件。预览使用绑定的 fork 发布索引和安装器；正式渠道可用性以主线合入及正式发布为准。

1. 使用标准入口安装基础 ASC；不安装评分执行依赖，不要求 Docker。
2. 需要动态评测时，准备 Docker/Compose 与所需权限，然后显式安装可选 `sec-core-skill-score` 组件。该组件获取锁定的第三方依赖并从校验后的源码构建镜像；无需用户选择网络后端、操作镜像文件或分别安装第三方库。
3. 配置一次共享模型服务和凭据。
4. 生成计划并执行，结果通过报告与证据目录读取。

```bash
sudo anolisa --install-mode system install sec-core
sudo anolisa --install-mode system install sec-core-skill-score

agent-sec-cli skillsec score plan /absolute/path/to/skill \
  --output-file /absolute/path/to/task.json

agent-sec-cli skillsec score run /absolute/path/to/task.json \
  --output-file /absolute/path/to/report.json \
  --evidence-dir /absolute/path/to/evidence
```

保留 `run -` 标准输入方式、stdout 单个 JSON、stderr 进度，以及拒绝意外覆盖输出文件的行为。输出目录必须位于被测输入之外。`plan` 使用基础包中的 Rust 核心和模型配置，不要求可选执行组件或 Docker/Harbor。`run` 需要完整执行环境；未安装可选组件时非零退出，提示显式安装；已安装但损坏时提示修复。两种情况都不自动下载。

这是一个 ASC 产品的可选安装单元，不是第二套公共 CLI 或新的服务。复用 `anolisa install <component>` 的现有形式，不新增 `--with`、`score install` 或 `prepare` 命令。当前安装器没有已落实的组件依赖自动求解能力，因此首版明确先装基础包，可选组件安装 helper 校验基础版本，不能依赖声明一个 dependency 就假定会自动安装。

这里的轻量保证针对 `anolisa install sec-core`。现有 `anolisa install --all` 枚举全部组件，新可选组件注册后也会包含在内；将它作为显式安装全部组件的行为说明，不能把 `--all` 宣传成同样轻量。

### 3.1 共享模型配置

建议新增 `/etc/agent-sec/model-service.json`，支持 `AGENT_SEC_MODEL_CONFIG` 覆盖配置路径。首版只提供一个共享远程配置，不引入 provider 路由、多 profile 或新的密钥服务。

下例是目标配置格式，模型名沿用当前预览默认值，不代表对该模型长期可用性的承诺：

```json
{
  "schemaVersion": 1,
  "remote": {
    "provider": "bailian-payg-anthropic",
    "baseUrl": "https://dashscope.aliyuncs.com/apps/anthropic",
    "defaultModel": "qwen3.8-max",
    "apiKeyEnv": "AGENT_SEC_MODEL_API_KEY"
  }
}
```

配置只保存密钥引用；实际值由用户会话或平台秘密管理注入。`bailian-payg-anthropic` provider 保留 HTTPS endpoint 约束。显式选择的 `anthropic-compatible` provider 接受运维方授权的 HTTP(S) DNS/IPv4 前缀并追加 `/v1/messages`，拒绝 URL 凭据、查询参数、片段、空白和反斜线，不跟随重定向。Judge 与 Agent 共用解析后的 provider/model 和不透明凭据。平台身份生成、端点授权和计量由调用方／代理负责，不进入 ASC；平台专属 provider 名须在生成 ASC 配置前转换。Judge 向通用代理只发送 Messages 基础字段（`model`、`system`、`messages`、`max_tokens`）。百炼请求另外指定 temperature、thinking 和 effort。两者共用重试预算、usage 解析和脱敏逻辑，每个代理的真实推理仍需单独验收。

在 `asc-model-client` 增加显式远程入口和百炼实现；现有 `create_client()`、Ollama 本地地址限制与原扫描器默认行为保持不变。评分调用远程入口，不能通过放宽现有本地 factory 的校验偷偷改变所有扫描器行为。

解析一次配置后，Rust Judge 使用类型化客户端；worker 只接收 Agent 所需的内部 `ANTHROPIC_*` 环境。公共配置文件存在但无效时直接失败，不从旧环境变量拼凑配置。文件不存在时，迁移期兼容旧 `ANTHROPIC_API_KEY` 与 `ANTHROPIC_BASE_URL` 成对配置，保持当前两者必填的校验，发出不含密钥的弃用提示。后续移除兼容需单独公告。

所有日志、源码展示和模型输入脱敏使用实际解析到的密钥集合；不能继续只扫描一个旧变量。被测 Agent 获得必要的模型凭据，不继承云管理凭据、镜像仓库凭据或宿主的其他环境。模型凭据的使用额度与有效期由部署方管理；允许访问模型端点不等于能够阻止通过该端点传递任何数据。

## 4. 输入、执行与报告契约

### 4.1 同一份输入贯穿计划与执行

`plan` 先创建私有输入快照，再从该快照计算摘要和生成审计；`run` 从输入创建新的私有快照，校验摘要一致后，只把验证后的快照交给 Harbor。生成计划以后修改原目录会导致明确的重新规划错误。

内容摘要覆盖实际交付给 Agent 的全部文件、相对路径、类型和影响执行的权限位，包括依赖目录与二进制；模型审阅文本的长度限制不再等于执行内容摘要的范围。首版拒绝符号链接、特殊文件、越界路径和超出限制的输入，并指出具体路径；不静默遗漏文件后继续评估。后续若支持链接，需定义物化和摘要规则并独立测试。

源码索引最多接受 32,768 行，逐行引用键与文本合计最多 4 MiB。超限时输入校验失败，模型调用尚未开始。

模型可能只审阅其中一部分；报告分别记录完整执行输入摘要与模型审阅覆盖范围。截断、扫描失败和关键上下文缺失不得解释为“未发现风险”。

源码视图使用不碰撞的不透明占位值及 `source_redaction` 来源说明。相同的受保护源码拼写共享占位值，但不据此推断语言转义、长度、子串、编码或正则行为；原始执行字节和摘要保持不变。规划阶段拒绝把插入的占位值放进执行输入或业务预期。证据中序列化为字符串的 JSON 按深度与大小上限递归脱敏，无法支持的重复视图省略，不原样暴露。

探针通常使用外部输入。可选的 `input_mode: fixed_target` 表示原始脚本直接引用固定 guard 文件或 mock 端点，无需外部输入。其 `fixed_target_refs` 必须指向入口脚本及目标 finding 引用的源码行，并包含精确的带引号目标字面量；禁止虚构 fixture、payload、输入准备或 mock 响应。计算生成的目标暂不由此模式支持。源码绑定只是定位，动态确认仍须实际入口执行、匹配的进程树和正确时序的效果证据。调用使用绝对路径 `/skills/subject/<entry_path>`；同名副本不是原入口，未观察到工作目录的相对路径不能证明激活。不虚构输入读取。Canary 外传还须真实 canary 读取及同一来源对应的 sink 效果。所有探针的业务预期都排除保留 guard、canary 与 sink 后果；这些属于安全激活与风险判断。

保留公共 `agent-sec-skill-score/task-v1` 字段结构与 `skill_path`、`content_hash` 绑定；完整摘要实现替代当前部分内容摘要。旧计划可能无法通过新校验，必须重新 `plan`，不自动接受或重写旧摘要。此项属于明确的升级兼容变化，需要发布说明和 SkillEval 契约测试。若实施时必须新增必填字段，则升级任务 schema，不在 v1 中偷偷加入新的必填条件。

### 4.2 内部 worker 协议

继续使用短命子进程和私有请求/响应文件，不新增服务、socket 或跨语言 FFI。内部协议显式标识 `agent-sec-skill-score/worker-v1`。

请求包括 request ID、`environment|run` 操作、快照路径、证据路径、运行截止时间、必要的 Agent 配置与单个 probe；不写入密钥或 Judge 答案。响应包括相同版本与 request ID、执行结果或分类错误、证据覆盖、资源标识及清理状态。响应临时写入后原子替换；stdout/stderr 只是日志。

Rust 根据自身发布清单寻找固定 worker 与 Python，校验代码、协议、Harbor 和镜像版本；不回退系统 Python。继承环境采用明确的最小允许列表，程序路径及参数固定。文件设为私有权限，所有反序列化有类型、大小和路径约束。

### 4.3 网络与证据边界

继续维护一套 ASC 侧车实现，不修改 Harbor 源码，不提供两套可选网络策略。它通过兼容的 iptables match 路径避免已遇到的原生 nft FIB 表达式问题，但仍有其他内核和权限前提；检查能力，不凭内核版本号推断可用性。

工作容器丢弃 `NET_ADMIN/NET_RAW`，与侧车共享网络命名空间。固定策略和域名到 IPv4 映射由可信控制器生成，只读交给侧车；先使策略与侧车健康，再启动被测程序。保留当前本地目标/ICMP规则、允许 TCP 目标及其他出站拒绝语义，测试这些例外的实际范围。首版不宣称任意网络协议、IPv6 或所有云内核都已兼容。代理退出不清除阻断规则，策略失败不得降级成开放网络。

证据在送给模型之前，ASC 自身必须验证路径、文件类型、大小、总量和摘要；拒绝链接逃逸与不可信硬链接、特殊文件和声明不一致的归档。以受限目录和安全文件读取避免检查后换文件；不能仅依赖 SkillEval 下载后的校验。工作容器不能写控制器配置、最终报告或可信清单。模型判断使用带来源的证据引用，最终计分是确定性代码，不执行模型生成的任意命令。

每类证据记录生产者与可修改主体。被测容器可写的日志、快照属于待核实证据，其摘要只证明回收后的一致性，不能证明被测程序未伪造。高置信动态效果需要独立可核验的观测或交叉证据，不能仅凭自报文件升级结论；首版不因此新增外部观测服务。

实际模型密钥按字节清除，包含二进制证据；包含密钥的路径、超限或无法安全处理的证据必须拒绝，清除本请求原始目录后仅保留脱敏错误与资源坐标，不能留下可分发的原始产物。资源清理未确认时，遗留容器仍可能修改证据，因此同样拒绝并清除该请求原始目录，将恢复坐标保存在可导出目录之外；清除失败则致命报错，不产出报告。JSON 先解析再结构化脱敏、再序列化；源码只遮盖真实敏感值，不能把正常的 `token = Path(...).read_text()` 表达式改坏。原内容摘要与脱敏展示分别标识，避免脱敏改变审计语义。

### 4.4 完成、风险与清理分别表达

保留 `agent-sec-skill-score/report-v1` 现有风险与计分字段，增加以下可选字段，并同步消费者测试：

| 字段 | 语义 |
| --- | --- |
| `execution_status` | `complete / incomplete / failed / cancelled`，说明执行与评测链路状态 |
| `dynamic_complete` | 无阻断性规划错误，全部计划探针完成且必要证据齐全；零探针不按“全部满足”自动算动态成功 |
| `execution[].execution_complete` | 单个探针的执行与采证完整；与保留的业务成功字段 `completed` 分开 |
| `cleanup_status` | ASC 内层资源 `complete / pending / unknown` |
| `failure_stage`、错误明细 | 环境、Agent、采证、模型裁决、清理等失败位置及可操作信息 |

这些字段由新 Rust 评测路径输出；旧预览报告不具备同样的完成契约。完整执行仍可能得到“有风险”；Agent 拒绝执行只证明本次拒绝，不等于动态攻击已发生或沙箱已成功阻断。规划说明 `planning_notes` 与阻断性 `limitations` 继续分离。

探针完成指执行已结束且证据足够形成结论，不等于业务任务成功或动态目标已激活，两者分别记录。不能用已得到风险结论替代全部计划覆盖；即使因确认 Critical 而提前停止，未执行探针仍列出，`dynamic_complete=false` 并返回非零，以明确的提前终止原因区别于基础设施故障。

阻断性的 `planning_errors` 保留到报告并使完整性检查失败；部分有效探针不能掩盖其他必要探针的规划失败。非阻断说明仍放在 `planning_notes`。这是对历史预览完整性判定的明确收紧。

公开退出码：`0` 表示评测链路完整产出，风险 `blocked/needs_review` 本身不当作基础设施故障；`2` 表示输入/契约/内容绑定错误；`1` 表示环境、模型、执行、证据不足、取消或无法确认清理等问题。动态不完整即使已有分数也返回非零，这是相对当前行为的收紧。非零时尽量保留有效部分报告；消费者同时检查报告与退出码。

安全判定、动态完成和资源清理不能合并成一个布尔值。SkillEval 的外层 `cleanup_pending` 继续表示平台实例清理，另行消费 ASC 内层状态；外层实例删除不应把内层失败记录改成成功。

Harbor 将 Agent 日志、验证器日志及制品写入本次请求的暂存目录。转换轨迹或读取验证结果前，worker 对普通文件做有界快照，存入宿主专用目录。最终轨迹只由宿主转换器写入。生成最终报告时若已收到取消信号，公开报告记录 `execution_status=cancelled`、`failure_stage=cancellation`，退出码为 1。

## 5. 超时、取消与失败处理

`run` 的顺序是：校验输入与输出路径 → 检查安装与执行环境 → 启动探针 → 采证与验证 → 模型裁决 → 确定性计分 → 收尾清理与报告定稿。

总预算覆盖 probe、模型调用及重试；为采证和清理预留明确时间。认证错误、无效配置和协议不匹配直接失败，只有已分类的暂态错误才能在剩余预算内重试，尊重 Retry-After。结构化模型输出修复属于单独、有上限的模型重试，不能与网络重试叠加成无限预算。

取消时停止排新工作，给独立 worker 进程组发 SIGTERM，让 Harbor 恢复证据并回收；沿用当前 45 秒清理宽限作为初始值，超出后才终止本次进程组。随后只检查/回收本次 request 拥有的 Compose project，不执行全局 prune。杀死进程不等于容器已删除；无法确认时留下资源标识和 `pending/unknown` 状态。

Compose project 名与所有权标签由 request ID 预先确定，创建前写入控制器私有的资源记录。worker 崩溃时仍可定位本次资源，最终响应不是发现待清理资源的唯一来源。

在 FC 中，SkillEval 的实例 TTL 和外层销毁是最后的资源兜底。平台截止时间必须包含 ASC 清理与产物下载预算，不能刚到 Agent 超时时间就立即销毁实例。

## 6. 发布物与安装事务

### 6.1 公开源码交付与本地构建回执

预览发布四个文件：轻量的 `sec-core-bundle-<version>-linux-x86_64.tar.gz`、小型可选 `skill-score-bundle-<version>-linux-x86_64.tar.gz`，以及各自的 `.sha256`。基础归档包含标准基础 raw 包、索引、安装器与发布清单；可选归档包含可选 raw 包、运行时清单和源码归档。两者解压到同一个 `v1/` 仓库，内容不重复；默认安装不需要可选归档。

`release-v4` 与 `runtime-v2` 绑定构建输入，不预先绑定构建输出。可选 raw 包包含自有 worker、锁文件、Dockerfile、构建脚本、采证/扫描源码和 ASC 侧车源码，全部由运行时文件清单覆盖；不包含镜像层、Python 解释器或依赖 wheel。显式安装时获取锁定的第三方运行库并构建两个镜像，不新增公共 prepare 命令，不让用户选择网络后端。

默认工作基础镜像固定 registry digest。业务环境由受信任安装侧通过 `ANOLISA_SKILL_SCORE_BASE_IMAGE` 指定不可变仓库引用或本地 image ID，并通过 `ANOLISA_SKILL_SCORE_BASE_RECIPE` 提供固定源码/上游来源 JSON。SkillEval 自行构建内部业务配方并提供结果，不把其 Dockerfile 放进公开 ASC 发布包。构建机需要时可设置 `ANOLISA_SKILL_SCORE_BUILD_NETWORK=host`，否则使用 Docker 默认构建网络；此配置不改变被测 Agent 的受限网络策略。

构建器核验工具版本、清单和平台，在本地临时导出镜像以核验 config ID，以及存在时的 OCI manifest ID，随后删除临时导出。生成的 worker 环境中保留 `image-build.json`，绑定不可变运行时清单 SHA256、ASC 源码/版本、实际基础镜像及配方来源、工具清单和工作/侧车 image ID；ready 记录回执 SHA256。Rust 和 worker 独立验证回执归属，只使用核验后的 ID，不能用可变 tag 作为执行依据。

不同安装可能得到不同 image ID：源码和基础镜像固定，但系统包及部分传递依赖尚未完全可复现。验收同时绑定发布源码包和安装侧回执。该方式降低 Release 大小，不承诺减少首次安装下载量；构建需要 Docker BuildKit、网络和临时磁盘空间。失败、超时或取消不激活不完整环境，仅终止本次构建进程，不全局清理共享镜像。构建诊断写入缓存中的 0600 日志；失败只提示路径、不回显内容，成功后删除日志。

### 6.2 可选组件的安装、升级与修复

用户显式安装或更新可选组件时，安装器执行：验证包与清单 → 校验基础 ASC 版本及宿主 → 校验构建源码和依赖 → 准备独立运行环境并构建镜像 → 校验 import、协议、Docker/Compose、两镜像 image ID 及网络能力 → 激活可选组件与清单 → 写入成功记录。

基础包安装与 Docker 状态无关。既然用户已经显式请求安装可选组件，Docker 缺失、daemon 不可用、权限不足、下载失败、哈希错误或依赖冲突均应使该次安装明确失败，不记录评分 ready，也不影响已安装的基础 ASC。可选组件升级准备失败保留旧执行环境并报错，不降级成“只安装基础包”。

运行环境按清单摘要保存；CLI 使用自己的清单找到对应环境，不使用全局“最新 worker”。准备成功才写 ready 状态；运行时仍做能力检查，因为 Docker 或内核环境可能变化。

改动前，raw 首次安装有 hook，更新路径只进行依赖 preflight。本分支在 `distribution/anolisa` 中为 install/update 接入候选包准备生命周期，在移除旧文件之前完成耗时准备。通用安装器只处理已验证组件步骤、超时、取消与事务记录；Python/Harbor/镜像知识放在 ASC helper。基础 raw 包要求 ANOLISA 0.3.21，由安装器管理组件发现目录。可选 raw 包要求 0.3.22，由该版本在登记前完成候选包准备，并在安装或重放基础包时记录原始仓库。公开安装记录只保存索引的 SHA256，管理器通过仅所有者可读的仓库文件解析原地址。Skill Score 读取已安装主包的契约快照来核对版本。预览包绑定了支持候选包准备流程的安装器归档摘要。

准备步骤从已验证候选包的 staging 目录执行新版本 helper，不能依赖初装时尚不存在或升级时仍为旧版的安装路径。该步骤与后续激活共同受现有组件安装锁和事务记录约束，避免并发安装互相覆盖。

同版本安装/更新当前可能直接 NoOp，因此依赖被清理后不能只建议“重装同版本”。扩展既有 repair 生命周期，让 `anolisa repair sec-core-skill-score` 按该组件已安装清单重建缺失环境或镜像；基础 `anolisa repair sec-core` 不安装它。可选组件从未安装时应提示 install，repair 不隐式启用新能力。本分支已实现该组件目标及依赖修复；`run` 只给出建议，不自行下载。

基础包与可选组件使用同一 ASC 发布版本及兼容清单，安装和运行均核对。首版不新增跨组件版本求解或原子升级框架；分别显式更新，版本暂不兼容时 `run` 明确拒绝并提示更新匹配组件。更新基础包不能自动获取此前未选择的动态执行依赖，仍可使用基础能力。

重复安装 raw 可选包时，明确指定的业务基础镜像和配方必须与已保存的收据一致，才会报告就绪。系统 Docker 套接字所属组可以获取共享租约并创建私有请求记录。安装、修复和卸载获取独占租约，保留未确认清理的请求。与完整 worker 记录不匹配的目录项不会阻断这些操作。旧版共享目录中，若完整记录的写入者现无 Docker 权限，管理器会查询本机的容器、网络和卷。仍有匹配的 Compose 项目或无法核对资源时，需要管理员确认清理。

基础包拥有唯一公共 CLI；可选组件拥有自己的 worker、版本化环境、ready 状态和安装记录，两包不得重复拥有文件或覆盖彼此入口。卸载可选组件保留基础 ASC，仅回收它拥有且没有活动任务使用的资源，不做全局镜像清理。

安装 helper 幂等，下载有界，日志无密钥，取消能终止其子进程组。依赖准备失败时旧版文件和运行环境仍可用；切换失败时恢复旧文件及清单。保留旧版本依赖和镜像，避免正在运行的任务失去资源；校验缓存可保留供重试，临时不完整环境由 helper 清理。系统软件包安装的外部副作用不能假装能够完全回滚，应报告残留。禁止全局镜像清理。

恢复旧版本应恢复对应清单和环境，但当前更新接口拒绝降级，不能承诺已有“一条 update 回滚”能力。首版提供验证过的运维恢复流程；是否新增通用降级命令单独决定，不作为评分架构的新要求。

### 6.3 宿主、模板与离线边界

Ubuntu 首选标准 raw 路径，不要求把 RPM 强装到 Ubuntu。RPM 继续是另一种发行格式；如发布包含评分的 RPM，其依赖安装、脚本和失败语义必须单独验证，不能照搬 raw hook 的保证。

完成评分准备需要可用 Docker daemon 才能构建并检查内层镜像。FC 模板最终化阶段由平台启动 Docker，再显式安装基础包及可选组件并验收。只有 Dockerfile 构建、没有 daemon 的阶段可以预置下载缓存，但不能标记评分 ready；最终必须经过镜像构建和检查。FC 模板是平台运行环境及配置的版本化定义，不等同于内层某个工作容器。

安装与执行必须使用同一 Docker daemon。首版验收默认本地系统 daemon 及对应用户权限，不自动切换到 rootless、远程 `DOCKER_HOST` 或其他 context；未覆盖的配置明确报不支持。FC 模板还需验证 Docker data-root 与镜像确实随模板恢复，不能仅凭安装记录判断镜像存在。

Python/uv 与 wheel 可以本地交付。`ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE=1` 禁止这部分依赖回落网络，镜像构建仍需可联网的构建机；`ANOLISA_SKILL_SCORE_OFFLINE=1` 拒绝全新镜像构建。本版不宣称支持断网首次安装。已准备模板保留 worker 环境、构建回执和 Docker data-root，恢复后的任务不安装、不重建。

## 7. SkillEval 接入与正式版迁移

SkillEval 继续复用现有安全评测管线；不在仓库复制评分规则、Harbor 代码或安装第三方依赖的第二套逻辑。

| 阶段 | SkillEval 的工作 |
| --- | --- |
| 模板构建 | 固定 ASC 版本与清单；平台准备 Docker；显式安装基础包及评分可选组件，由组件构建已校验源码并绑定实际 image ID；检查运行能力后复用平台流程发布模板 |
| 实例执行 | 注入共享模型配置和受限凭据；上传固定 Skill；运行 `plan/run`；保留既有同路径/内容绑定 |
| 结果获取 | 下载文件并做现有归档校验；消费新增完成/清理状态；展示风险、未知和失败，保留证据 |
| 生命周期 | 承担 FC TTL、取消、下载重试、外层清理与运维观测，分别记录内外层资源状态 |

SkillEval 提交 `145ab88d27ca35405fef4278a705d25045dc3d36` 已在兼容旧 `ANTHROPIC_*` 的基础上增加共享配置的受限传递及私有文件注入，不允许任意宿主环境透传。模板安装器将镜像准备交给可选组件，保留模板恢复后的就绪检查。这些源码变更不等同于已完成发布候选的实际验收。

源码获取和镜像构建发生在模板构建/更新时。每个评测任务复用已发布模板内的依赖和镜像，不重新下载或加载；模板恢复丢失镜像时明确失败，由平台重新构建或修复模板，不让任务执行偷偷承担安装。单机用户对应为可选组件安装/更新时准备，之后每次 `run` 同样复用本地环境。

先从用户 fork 发布相同结构的固定预览版本，经过标准包、单机和 FC 验收；能力合入主线后，从正式 ASC 渠道发布，继续采用源码包交付。SkillEval 切换包和归档来源、固定版本/清单及模板版本，再跑验收；CLI、任务/报告及执行拓扑保持一致。正式制品地址、保留策略、发布权限和 SkillEval 构建环境的下载可达性需落实，不能把当前测试账号的可达性当作生产环境可达性。正式发布不要求同时迁移到 OCI 仓库。

## 8. 代码改造与实施顺序

| 顺序 | 仓库/位置 | 主要工作 | 完成标准 |
| --- | --- | --- | --- |
| 1 | ANOLISA：ASC 契约、测试数据、设计规范 | 固定命令、输入摘要、报告与 worker 协议；明确本地评测架构契约；冻结 Python 参考与 golden fixtures | 迁移边界可测试，兼容变化可审阅 |
| 2 | ANOLISA：`asc-model-client`、新 `asc-skill-score`、CLI | 共享百炼接入，Rust 规划/证据判定/计分；直接调用 worker | 固定模型响应与证据的差分通过，不改变无关扫描器行为 |
| 3 | ANOLISA：`skill-score-runtime`、网络和证据实现 | worker-v1、完整快照、安全采证、源码脱敏、异常清理；统一兼容侧车 | 输入/证据边界和失败矩阵通过 Linux 验证 |
| 4 | ANOLISA：ASC packaging、组件注册与发布 CI；`distribution/anolisa` | 基础包减重、小型可选源码包发布；SHA256/image ID 校验；显式依赖安装；初装升级事务与 repair；退出旧评分 Python 3.11 包 | 默认不下载重依赖；归档可验证安装；可选组件升级与修复可用；版本不匹配明确拒绝 |
| 5 | SkillEval：security pipeline、`deploy/fc-security`、构建脚本及文档 | 统一模型配置、状态消费、模板显式安装可选组件，更新原 PR | 同一候选 ASC 制品完成 FC 平台回归 |
| 6 | 两仓库的发布验证与说明 | 冻结候选版本，跑真实样本及故障测试，记录支持矩阵和 FPR | 证据绑定实际提交/制品；明确可合入与可发布范围 |

阶段 2 与 3 可在阶段 1 契约稳定后并行；安装器与发布改造也可并行。先完成确定性兼容验证，再移除 Python 发布实现；不能先删除参考实现，再靠在线模型随机输出判断迁移是否正确。

## 9. 验收标准与误报评估

### 9.1 确定性兼容

使用冻结输入、模型响应与执行证据，比较 Python/Rust 的规范化计划和报告。覆盖根因去重、严重度、existence/dynamic 乘数、hygiene、Decimal half-up 舍入、Critical 确认门槛、错误路径与规划说明。明确列出的既有缺陷允许修正，其余不借迁移调整评分。固定模型输出的差分不能替代真实模型任务测试。

### 9.2 安装与执行矩阵

- 干净 Ubuntu 初装、旧版升级、缓存复用、无 daemon、断网、哈希不符、磁盘不足、取消、版本/协议不匹配。
- 基础 install/update/repair 在有无 Docker 时均不下载评分依赖或镜像；未装可选组件的 `run` 明确报错且不下载。显式可选组件安装失败不影响基础 ASC；其升级失败保留旧执行环境。
- 基础包/可选组件版本不匹配拒绝运行；基础安装后单独增加可选组件；可选组件卸载后基础命令仍可用，动态 run 明确提示未安装。
- 新环境准备失败时旧版可用；切换失败可恢复旧版；同版本依赖被删除后 repair 恢复；离线全物料和缺物料；模板恢复后不下载也能运行。
- 源码包篡改、超限、错误平台、缺失侧车、构建回执不匹配、构建失败或中断；失败不写 ready、不影响其他版本正在使用的镜像，评测执行不触发 registry 拉取。
- 源码缓存命中仍核对摘要，构建后仍校验回执绑定的镜像身份；单机和 FC 分别记录实际 config ID 及存在时的 OCI manifest ID，允许 Docker 存储后端返回不同的 `.Id`。经典导出核对 config 原文摘要与层 diffID；损坏的 OCI 元数据不能降级为经典解析。覆盖经典与 containerd 存储，拒绝未绑定 ID、角色串换和错误平台，并使用解析后的不可变 ID 执行。模板构建后屏蔽制品下载源，恢复实例仍能执行评测，只保留任务实际所需网络；证明不存在每任务重新下载。
- 正常任务、已知违规、Agent 拒绝、模型超时、取消、证据缺失/超限/伪造/不一致、worker 异常、清理失败和并发任务隔离。
- 网络允许/拒绝、DNS 和本地目标例外、启动竞态、侧车退出、能力缺失；失败不开放网络。
- 同一 ASC commit、发布清单、源码包 SHA256、安装侧构建回执和两镜像绑定的 config ID 及存在时的 OCI manifest ID，分别在原生 Linux CLI 与 FC/SkillEval 走公开 `plan/run`，记录每个宿主实际选用的 Docker ID。FC 另锁定 SkillEval commit、模板修订和模型配置。

### 9.3 FPR 口径

先独立标注正常与违规样本，再执行；正常集覆盖文件读写、授权清理、命令调用、归档、正常凭据使用、允许的联网及外部输入解析，并包含非演示性的真实 Skill。违规集有业务相近的正常对照，保留未参与调优的 holdout。

报告 case-level 与 finding-level 误报、无依据扣分、错误阻断/needs-review，同时报告违规检出、动态目标激活和实际效果确认。环境失败、证据不足和未知单列；公布全部样本数及有效评测数，不把未完成样本算真阴性，也不筛掉失败后只报“0%”。重复运行用于测随机性，不增加独立样本数；小样本给出统计不确定性。

改进误报依靠正确的上下文、未破坏语义的脱敏、证据归因和判定门槛，不靠降低权重或删样本。首批主线验收冻结样本集与预期裁决，要求已确认正常行为不新增误报、既有违规不因迁移被放过；扩大样本后再设定生产 FPR 指标，不能把 6 个正常样本当作生产指标依据。

## 10. 已有证据与尚未完成的事项

以下是历史测试的边界，不能代替本分支原生实现的验收。新验收记录必须另外绑定实际提交、安装包与镜像身份：

| 版本/证据 | 可用结论 | 不可外推的结论 |
| --- | --- | --- |
| ASC `d47ece18` + SkillEval `76ba36e3` 的 8 样本 FC 测试 | 6 个正常样本最终完成，观察到风险误报 0/6；2 个违规有源码风险识别 | 两个违规均被 Agent 拒绝；动态效果确认和沙箱阻断证明均未获得 |
| ASC `0f935544` | 制品身份、Ubuntu 安装、collector/网络回归，以及 B04 单例 FC 复验 | 不是该版本的完整 8 样本实测；不是未来 Rust 实现的证据 |
| SkillEval `6cb567a7` | 主线同步后的 workspace check 与 15 项本地回归 | 新合并提交未重跑 FC/模型任务 |

实施前后仍需明确：正式包与归档附件的地址、发布权限和保留策略，SkillEval 构建环境到依赖源/制品源的网络，以及正式支持的 Linux/架构/FC 模板范围。新 OCI registry 的地址或授权不属于首版待办。首版以已验证的 Linux x86_64/Ubuntu 与 FC Gen2 路径扩展验证，不默认承诺 arm64、其他发行版或 FC Gen1。

主线合入至少完成输入一致性、安全采证、脱敏、显式状态、Rust 差分及最终候选制品的单机/FC 代表性回归。扩大生产 FPR 评估可持续推进，但不阻止先提交可审阅的实现；发布时必须如实列出覆盖边界。

## 11. 当前源码落点

- [公共 CLI 接入](../../v2/apps/asc-cli/src/commands/skill_score.rs)、[共享模型客户端](../../v2/crates/asc-model-client/src/lib.rs)。
- [现有 Python 评分管线](../../agent-sec-cli/src/agent_sec_cli/skill_score/pipeline.py)、[输入读取](../../agent-sec-cli/src/agent_sec_cli/skill_score/io.py)、[百炼适配](../../agent-sec-cli/src/agent_sec_cli/skill_score/provider/bailian.py)。
- [worker 依赖声明](../../skill-score-runtime/pyproject.toml)、[Harbor 执行适配](../../skill-score-runtime/src/agent_sec_skill_score_runtime/harbor_runtime.py)、[证据归一化](../../skill-score-runtime/src/agent_sec_skill_score_runtime/observations.py)。
- [组件清单](../../packaging/anolisa/component.toml)、[安装计划器](../../../../distribution/anolisa/crates/anolisa-core/src/planner.rs)、[raw 更新操作](../../../../distribution/anolisa/crates/anolisa-cli/src/commands/tier1/install/owned_ops.rs)。
- [已有 GHCR 发布流水线](../../../../.github/workflows/docker-nightly.yaml)、[当前评分归档发布及安装验证](../../../../.github/workflows/sec-core-skill-score.yaml)。
- [现有 Skill Score 用户契约](../../../../docs/user-guide/zh/agent-sec-core/skill-score.md)、[SkillEval 集成 PR](https://code.alibaba-inc.com/chengshuyi.csy/skilleval/codereview/30699594)。
