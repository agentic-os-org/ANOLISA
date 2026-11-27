# Semantic Arbiter AW Provider

[English](README.md)

这是一个 observe-only AW Provider，用于记录完整原生工具调用在任务范围、保留意图和快照需要上的语义判断。
它通过 Semantic Arbiter 公共 API 和有界本地观察，将每次调用与同一会话采集的用户 prompt 比较。
本源码实现用于受控 shadow 评估：即使语义结果为 `block`，也不产生 AW effect。当 Host 请求 decision 时，回复携带建议性的 `allow`/`ask`/`block`/`abstain` 推荐及其义务；在 `mode: shadow` 下 AW 记录这些内容但不执行。AW 及其消费方集成负责建议的授权与执行；本 Provider 不阻断工具，也不执行快照、回滚或清理。

## 边界与敏感数据

**只有获得保留和传输载荷的许可后才能启用。** 私有记录包含完整 AW 请求/event、明文 prompt、完整原生参数（包括文件内容）、本地上下文、模型 state/questions、解析后的概率和建议结果。满足条件的模型调用会将 prompt、候选、所选证据、缺口和本地上下文发送到配置的端点。这些输入可能包含密钥或个人数据；排除认证 header、环境凭据值和原始远端响应不等于输入已脱敏。AW `metadata_only` 审计**不会**关闭这份存储。请先使用合成数据。

记录是观察，不是认证、执行 receipt 或可信进程内 `EvaluationRecord` 的持久替代品。Workspace/resource ID 是标签。有界 stat 身份和字节比较不能证明安全归属、挂载隔离、原生工具语义或实际效果。本地 observation 模块读取时不执行候选命令，但独立的 Git 验证器可能运行配置的 clean filter。必须使用**可信受控的 workspace 和 Git 配置**：观察路径约束既不认证也不消除 filter/配置的副作用。不能据此认为整个 Provider 可安全处理不可信仓库。依赖外部 Git 或 Agent 行为前，须核验目标版本的第三方源码。

### 候选投影

每个候选保留完整原始工具名称和 JSON 参数对象，并通过 `candidate_revision` 绑定。精确投影列出 0–64 项变更，每个资源一次；同时被写入和删除的资源表示为 `delete`。空变更列表是完全归因只读调用的诚实投影，不是未决写入。资源约束仍是硬门禁：受保留目标或超出非空允许列表的目标，不经模型推理就产生建议性阻断。Host 提供权威 binding 时，采用其 `candidate_id` 作为候选身份；未绑定调用保留自派生身份。

- 配置的 `Edit`/`edit`、`Write`/`write` 和 `Delete`/`delete` 将一个顶层 `/file_path` 或 `/path` 映射为 `replace` 或 `delete`。Write 要求字符串 `content`；Edit 要求字符串 `old_string` 和 `new_string`，可选布尔值 `replace_all`/`allow_multiple`；Delete 仅接受选定路径。这是显式投影契约，不是原生行为认证。
- 配置的 `Bash`/`bash` 使用 `/command`，可附 `description`、`timeout`、`run_in_background` 和 `dir_path`。词法分析按命令顺序提取重定向目标和受支持的写操作。递归目标还包含其下配置的保留资源。动态执行、无法归属的路径、脚本和未知程序加入 `command:unresolved`；词法分析不执行或展开命令。被词法分析完全归因且无写目标的命令投影为空变更列表和 `read_only` 信号。
- 配置的 `Read`/`read`、`Grep`/`grep`、`Glob`/`glob` 和 `LS`/`ls` 将可选的 `/file_path` 或 `/path` 参数映射为零变更只读候选；Read 要求路径存在，搜索工具缺省为 workspace 根。路径存在时必须能解析到 workspace 内，否则走不透明回退。`Read` 读取约定凭据位置以及所有 `Grep` 内容搜索携带 `sensitive_read`；`Glob`/`LS` 只涉及元数据。这是投影契约，不是原生行为认证。
- adapter 为 `openclaw` 时，配置的 `apply_patch`/`apply-patch` 使用 `/input`。本地解析器将 Add/Update 投影为 `replace`，Delete 为 `delete`，Move 为目标替换加源删除。它不应用 patch，也不认证与原生解析器的等价性。以 `@`/`~` 开头、workspace 之外或指向根目录的路径不能精确映射。AW OpenClaw adapter 目前既不提供 `event.native.cwd`，也不投递 `input.submit`：调用会在上下文检查失败，补齐后仍会在 prompt 可用前保持 `task_unavailable`。

未知工具、不支持的映射/字段和未映射路径，在原生输入为结构合法对象时**不会跳过**。它们成为资源为 `command:unresolved` 的不透明 `replace` 候选，保留完整调用供语义求值。在非空允许列表下，该占位符超出范围，除非显式列出。身份/上下文/任务非法、候选格式非法和候选超限仍为 uncovered。文件名或看似只读的命令不能证明无害：完全归因的只读投影只本地裁决保护类单元（不变更、无损失）；任务范围仍交模型判断该读取是否服务任务。

命名路径使用配置的资源标签，或 `path:<以 ':' 分隔的 workspace 相对路径>`。无法无歧义表示的路径使用不透明回退，不猜测身份。投影是词法处理，不是文件系统 canonicalization 或隔离。

## 配置与运行

此独立 Provider 尚无 `anolisa install` 集成或 RPM。源码构建是开发者入口；AW 不自动打包它。在包含匹配公共库的 checkout 中，从本目录执行：

```sh
cargo build --release --offline --locked
cargo test --workspace --offline --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
```

要求 Linux 和 procfs。此独立 workspace 使用 AW toolchain pin、本地 `aw-provider` 0.1.1 和 `semantic-arbiter` 0.1.0；lockfile 与本地 AW 0.1.1 库匹配。`aw-host`/`aw-config` 仅用于测试。这不意味着 SDK 已独立发布或 AW 实现发生变化。离线构建需要已有依赖缓存。

1. 查看合成 [JSON](examples/config.json) 及等价的 [AW YAML](examples/aw.yaml) 模板。替换 binary、Agent、workspace 和 state 路径。独立安装构建产物，通过 `transport.argv` 选择，不传 Provider CLI 参数。
2. 提供 adapter、绝对 workspace 路径、稳定 workspace ID 和可选 `task.allowed_resource_ids`/`task.preserve_resource_ids`。空列表表示未声明约束；非空 allowed 列表是硬上限，preservation 优先。`scenarios` 接受 `task_scope`、`preservation`、`snapshot` 的不重复子集，默认 `[task_scope]`。已有模型阈值仍可配置；本地观察和策略**不新增用户策略配置项**。
3. 在 `input.submit` 上启用 `capture_task_scope`（observe），在 `tool.before` 上启用 `evaluate_task_scope`（observe、`decision: true`、`mode: shadow`）。每次 prompt 提交追加一轮；每次调用使用以空行连接的全部已采集轮次。任务 origin 为 `native_user`，连接文本的 `sha256:` 为版本。重复 `turn_id` 被忽略（`turn_duplicate`）；空白输入不采集（`empty_input`）。最多保留 64 轮、16,384 字节连接文本；超限保持 `task_too_large`，不截断。缺少 prompt 产生 `task_unavailable`。采集文本不是经过认证的授权。
4. 预先创建运行时用户所有、mode 为 `0700` 且各级无符号链接的记录根目录，尽量放在 Agent 可写 workspace 之外。验证只打开/stat，不创建文件或读取凭据；目录缺失或 owner/mode 不符时拒绝。
5. 保留 `model.config_revision: aw_request`；调用时替换为完整 AW 请求 revision 的域分离哈希。会话身份对 `[adapter, binding_id, instance_id, native_session_id]` 哈希，保持会话与实例独立。原生 session 须非空白、最多 512 字节且无控制字符。Provider cwd 和 `event.native.cwd` 须与 `workspace_path` 完全一致；adapter、binding、instance 和 call 身份须合法。提交还要求非空白、最多 512 字节的 `turn_id`。
6. 阅读源码/文档后选择已授权的公共 `ModelConfig` 后端。HTTPS 验证证书；示例明确为 literal-loopback 测试。凭据只通过 AW 子进程环境中配置的 `key_env` 提供，不写入 JSON/YAML 或记录。仅需要推理时才惰性读取凭据。纯本地决策不需要凭据；满足模型条件的路径缺少凭据时保留为已求值的 unknown 结果，而不是跳过调用。配置验证不访问端点。

从与配置完全一致的 workspace，使用受支持的 Agent 设置启动 AW：

```sh
aw run --config /absolute/path/to/aw.yaml --agent target
```

保留 `effects: [observe]` 和 `on_error: report`。`describe` 声明 `input.submit` 上的 `capture_task_scope` 与 `tool.before` 上的 `evaluate_task_scope`，均带 `observe`；判断操作还声明 `decision`。每个进程处理一个 EOF 分隔请求。合法调用返回原请求 digest、`status: ok` 和 `effects: []`，包括建议拒绝和语义失败。当 Host 请求 decision 时，回复还携带建议性 decision——`allow`、`ask`、`block`，未决调用为 `abstain`——及其义务；abstain 不请求义务。在 `mode: shadow` 下 AW 保留推荐，不投影任何执行性 effect。非法请求/配置产生脱敏协议错误，进程仍退出零。AW 负责进程生命周期、传输错误和原生准入；observe-only 不保证每种 Host 配置都会放行工具。

### 本地观察与判断

首次采集 `input.submit` 时，在 `task.json` 保存仅含元数据的递归清单：最多 12 层路径、256 个访问路径、32 KiB 累计路径字节和 20 KiB 编码清单。排除 `.git`、`node_modules`、`target` 和 `.venv`。排除项、符号链接、不稳定目录、上限和失败产生缺口，绝不推断为不存在。匹配的已留存 device/inode 事实可证明初始存在；证明初始不存在要求无缺口的完整递归覆盖。后续/重复 prompt 不刷新清单，旧会话缺少清单时不回填。复检也消耗访问路径上限。这不是原子性的全工作区快照：子树最终检查后的变化和 inode 重用可能无法被发现。

对已准入候选，`observation::collect` 要求 revision 匹配且目标映射完整。它使用 Linux descriptor-relative `openat2` 约束，不跟随符号链接目标；约束不可用时保守报告，不回退到宽松读取。稳定普通文件读取在原始 deadline 内限制为每文件 64 KiB、每调用 256 KiB。仅对受支持的字面 Write/Edit 比较原始字节并计算哈希；采集器不持久化或发送文件内容。未知目标、不支持的 delta、目录、发生变化的文件和读取上限均保留为缺口。候选自带内容仍作为原生参数留存。

`LocalContext` 冻结文件状态、可用时的精确 delta、初始存在性、摘要、词法动作信号和缺口。信号覆盖 test/config/CI/dependency 路径，以及识别出的删除、Git 改写/发布、跳过检查、通信、网络、外部获取、持久化和时钟动作，另含 `read_only`（完全归因、无写目标）、`sensitive_read`（约定凭据位置，如 dotenv 文件、SSH/云凭据、shell 历史、环境变量转储，或可能触及这些位置的递归内容搜索）和 `working_state_destruction`（`git reset --hard`、非 dry-run 的 `git clean`、`git checkout -- <路径>`、工作树 `git restore`）。它们只是线索，不证明授权、恶意、共享归属或生产用途。不透明执行显式保留，不能排除未识别的副作用。

启用 `preservation` 或 `snapshot` 时，独立的 `git-rebuild` 来源运行 `git hash-object`、`git ls-files -s` 和 `git ls-tree HEAD`，设置 `GIT_TERMINAL_PROMPT=0`、workspace 父目录 ceiling，并在原始 deadline 内附加 5 秒上限。匹配 index/HEAD blob 为 `identical`，已跟踪但不同为 `unique`，路径缺失为 `absent`，未跟踪/不可读/多 stage/非仓库情况为 `unknown`；事实有效期 30 秒。这些是基于 Git/filter 的比较，不是原始字节观察摘要。虽然未要求 Git 写入，但 clean filter 可以执行并产生副作用，不能笼统保证无执行或无网络。无法归属的候选和采集失败产生显式重建缺口。只读候选不指向任何资源，evidence 阶段记录 `no_targets`，不采集事实也不虚构缺口；仅启用 `task_scope` 时记录 `scenario_gated`。

核心将任务缺失、资源约束及过期/不可信/冲突证据保留为硬门禁。缺失/unknown 证据和覆盖缺口允许语义回退，并保持限制可见。快照依次尝试 `no_loss_read_only`、`no_current_bytes_lost`、`protect_unique_bytes`、`protect_preexisting_bytes`、`protect_working_state`；保留意图尝试 `preserve_no_mutation` 和 `preserve_absent_target`。任务范围没有本地策略：无关或越权的读取即使没有写效果也属越界，因此读取一律交模型，`read_only`/`sensitive_read` 仅作线索。未决问题最多合并为一次后端调用。精确条件见[规范规则](../../src/semantic-arbiter/docs/design/protocol_zh.md#判断规则与模型交互)。Trace 中的 `route`、`policy_id` 和 `baseline_reason` 区分规则、完整性失败与模型回退。任何本地规则都不授予任务权限。

五类机制仍只部分覆盖：授权是语义建议；对象身份是有界 stat 证据，不是安全归属证明；test/config/CI/dependency 分类是词法线索；副作用只能从调用中部分识别；恢复只是建议，不执行快照或业务清理。无阻断的 `needs_snapshot` 仅产生绑定 workspace 的建议义务。原生安全和权限检查仍具有权威性。

## 记录、限制与清理

已有记录根目录只允许 `session-<64 hex>/` 目录与 `unbound.jsonl`；异常内容返回 `record_unavailable`。每个会话目录为 `0700`，包含 `task.json`（prompt 和初始清单，最多 128 KiB）与 `calls.jsonl`。更新先同步 `task.json.pending`，再重命名覆盖 `task.json`。所有文件为 `0600`；会话结束不自动过期。无法识别上下文的调用使用 `unbound.jsonl`。

调用行格式为 `{"call":"<unix nanos>-<pid>","stage":S,"data":{...}}`，按阶段追加/fsync：`input`、`task`、`evidence`（采集摘要、`no_targets` 或 `scenario_gated`）、`evaluation`、可选 `model_request`/`model_result`，最后 `result`。`evaluation` 包含 `local_context`、证据、缺口、完整任务/候选及原始预算。仅调用后端时产生模型阶段。`task` 在投影合法及最终大小检查后写入，uncovered 输入没有该行；完整 evaluation 超限前可能已需要加载会话上下文。`input.submit` 更新 `task.json`，不产生调用行。

按 `call` 分组；并发调用可能交错完整行。`evaluated` 表示已留存响应，不表示推理成功。须检查答案、`judgments`（包括路由/错误）及 `outcome.control.candidate`。`uncovered` 表示上下文、任务或候选非法/不可用，包括超限；`skipped` 覆盖准备预算或运行失败。模型凭据缺失则出现在 evaluated 响应中。被杀进程可能留下缺失阶段或截断末行；跳过不能解析的行。下一次写入从新行开始，后续 prompt 采集替换残留 pending 文件。记录失败通过稳定 stderr code 暴露，初始记录失败阻止推理。

配额按实际磁盘字节计算：`max_sessions` 为 1–10,000；`max_session_bytes` 限制每个会话，并单独限制 `unbound.jsonl`；`max_storage_bytes` 限制根目录，最多 1 GiB。字节配额最小为 4 MiB + 128 KiB。调用准入要求会话/unbound 日志和根目录各有 4 MiB 余量；这不是预留，因此并发调用后续仍可能出现 `record_write_failed`。超过存储上限的 prompt 不采集。

准入和每行写入使用独占根目录 `flock`；有界竞争等待只消耗原始预算，并在推理前释放锁。等待输入、映射、清单/观察、持久化、锁等待和推理共用 AW `budget_ms` 与模型 timeout 的较小值，不续期、不进行网络重试。AW 接受 1 MiB；原生候选 JSON 限制为 64 KiB，完整 evaluation 限制为 256 KiB。超限输入完整保留在 `input`，绝不截断后发给模型。Host 必须限制完整进程生命周期，包括文件系统阻塞与输出。

不自动重放动作、重试模型、回退到其他后端、清理、轮转或上传记录。检查、归档或删除自有记录前，停止根目录的所有使用者。删除 `calls.jsonl` 释放字节；删除 `task.json` 移除 prompt 与初始清单，使后续调用在新 prompt 之前不可用；删除会话目录还释放名额。同一用户修改不是安全边界。备份须同等保护。

## 验证与归属

现有测试使用真实 binary、本地 HTTP fixture 和公共 AW Host，采用合成概率，不使用真实 Agent、远程模型或真实凭据。它们检查投影、本地事实/策略、回退/错误契约、空 effects、会话隔离、存储与 deadline，不验证模型效果或原生采用。应在已授权、隔离的环境中单独验收目标 Qoder/AW/Provider/模型组合；fixture 结果不能证明端到端验收通过。

维护者/联系方式：通过仓库 issue 联系 ANOLISA 维护者。源码为本目录以及 `src/aw`、`src/semantic-arbiter` 公共库；使用仓库 Apache-2.0 许可证。参见 [Provider 贡献指南](../README_zh.md)。删除独立安装的 binary 或留存数据前，禁用 AW step/Provider 并停止活动调用。
