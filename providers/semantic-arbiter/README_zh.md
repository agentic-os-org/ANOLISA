# Semantic Arbiter AW Provider

[English](README.md)

这是一个 observe-only AW Provider，用于记录明确配置的单文件变更和 shell 命令与任务范围之间的语义判断。
它通过 Semantic Arbiter 公共 API，将完整原生工具调用与同一会话中采集到的用户 prompt 进行比较。
它面向受控的 shadow 评估：即使语义结果为 `block`，也不会产生任何 AW effect，更不会执行工具、落实 obligation 或授予权限。

## 边界与敏感数据

**只有获得保留和传输这些数据的许可后才能启用。** 私有记录包含完整 AW 请求/event、以明文保存的每条已采集用户 prompt、原生参数（包括替换文本/文件内容）、模型 state/questions、解析后的概率和建议结果。已采集的 prompt 和完整映射后的 candidate 会发送给配置的模型。即使 Provider 不记录认证 header、从环境读取的凭据值或原始远端响应，这些输入仍可能包含密钥或个人数据。AW 的 `metadata_only` 审计配置**不会**关闭这份独立的敏感数据存储。请先使用合成数据。

这些记录只是本地观察，不是身份认证、执行 receipt、arbiter 可信进程内 `EvaluationRecord` 的持久化替代品，也不是文件系统身份的证明。配置或派生的 workspace/resource ID 只是操作者范围内的标签，不构成关于所指文件的任何证据。路径检查只处理字符串：Provider 不打开 candidate 文件，不验证符号链接、硬链接、挂载点、实际效果或原生工具语义。原生权限和安全检查始终独立且具有权威性。

精确配置的 `tool.native_name`：`Edit`、`Write`、`edit`、`write` 映射为 `replace`；`Delete`、`delete` 映射为 `delete`。每个映射只选取一个顶层 `/file_path` 或 `/path` 字符串。Write 输入必须包含该路径和字符串 `content`；Edit 输入必须包含该路径、字符串 `old_string` 和 `new_string`，可选布尔字段 `replace_all`/`allow_multiple`。Delete 输入只能包含选定路径。这些是操作者显式声明的输入契约，不表示每个 Agent 都提供这些名称。未知输入字段、MultiEdit、多资源文件调用和其他方言都标记为 uncovered，不进行推理。这个受限映射不认证原生工具行为。

配置为 `Bash` 或 `bash` 的工具使用 `command_pointer: /command`。其输入还可包含 `description`、`timeout`、`run_in_background` 和 `dir_path`；任何其他字段都标记为 uncovered。命令字符串只做词法切分，从不执行或展开。重定向目标和常见写入命令（`rm`、`rmdir`、`unlink`、`touch`、`mkdir`、`tee`、`truncate`、`cp`、`mv`、`ln`、`sed -i`、`sort -o`、`git rm`/`mv`/`checkout --`/`restore`）的操作数成为命名目标；heredoc 正文和引号内文本视为数据。只有一个目标成为 candidate：保留目标，或包含保留资源的递归目标优先，并且不调用模型直接阻断；否则在非空 allowed 列表下取第一个超出列表的目标；否则取第一个命名目标。无法归属任何目标时（脚本、解释器、未列出的程序、命令替换、变量、`find` 动作、`git reset --hard`/`clean`、`cd` 之后或另一 `dir_path` 下的相对路径），candidate 为 `command:unresolved`，由模型判断完整命令。在非空 allowed 列表下，该占位符属于超出范围，除非你列出 `command:unresolved`。只包含只读部分的命令不产生 candidate，以 `no_mutation` 跳过。词法分析看不到程序内部执行的写入：这是用于 shadow 评估的映射，不是隔离手段。

## 配置与运行

此独立 Provider 尚未集成 `anolisa install`，也没有 RPM。当前开发者只能从源码构建；AW 不会自动打包它。在包含匹配公共库的 checkout 中，从本目录运行：

```sh
cargo build --release --offline --locked
cargo test --workspace --offline --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
```

本 workspace 拥有独立 lockfile，toolchain pin 与 AW 一致。要求 Linux 和 procfs。公共 path 依赖为同一源码修订中的 `aw-provider` 0.1.0 和 `semantic-arbiter` 0.1.0；不假设 SDK 已独立发布。`aw-host`/`aw-config` 仅用于公共集成测试。离线构建要求已有依赖缓存；测试不会下载三方源码。

1. 查看 [examples/config.json](examples/config.json) 与 [examples/aw.yaml](examples/aw.yaml) 中等价的内联配置。两者都是**合成模板**，不是生产 endpoint 或凭据。替换 binary、Agent、workspace 和 state 路径。将构建产物放入自行管理的可执行文件位置，用 `transport.argv` 选择它（Provider 不接受 CLI 参数）。
2. 使用私有配置 `version: 1`，提供 adapter、绝对 workspace 路径、操作者管理的稳定 workspace ID，以及 `task.allowed_resource_ids`/`task.preserve_resource_ids` 列表。闭合配置只接受已声明字段，且不包含任务文本。`scenarios` 列出要执行的语义检查，默认为 `[task_scope]`。`preservation` 暂不支持：配置中列出它时，`validate_config` 返回 `unsupported_scenario`，AW 拒绝启动，而不是静默跳过该检查。AW 公共线协议版本不变。

   按示例在 `input.submit` 和 `tool.before` 上同时启用同一个 `evaluate_task_scope` step。每次 `input.submit` 把一轮用户 prompt 追加到所属会话；每次 `tool.before` 用该会话已采集的全部轮次（按顺序以空行连接）判断调用。evaluation 任务 origin 为 `native_user`，版本是连接后文本的 `sha256:` 摘要，因此每新增一轮都会改变版本。重复投递的 `turn_id` 被忽略（`turn_duplicate`），空白 prompt 不记录（`empty_input`）。一个会话最多 64 轮、连接后文本最多 16,384 字节；超出后该会话保持 `task_too_large`，后续调用记为 uncovered，不会用截断后的任务判断。会话没有已采集 prompt 时（例如 AW 在 prompt 提交后才启动），调用记为 `task_unavailable` 的 uncovered，不进行推理。prompt 就是用户输入的内容：它未经认证，不授予任何权限，资源约束只来自配置。资源枚举仍可选：未标记路径获得 `path:<以 ':' 分隔的 workspace 相对路径>`。空 allowed/preserved 列表表示未声明约束；非空 allowed 列表是硬上限，非空 preserved 列表优先于允许。两者均接受显式标签和派生身份。
3. 用运行时用户手动创建 mode 为 `0700` 的记录根目录，尽量放在 Agent 可写 workspace 以外。路径中的每一级都必须是真实目录，不能是符号链接。验证只打开/stat 已存在目录，不创建文件、不读取凭据。目录不存在、owner 或 mode 不符时拒绝。
4. 保留 `model.config_revision: aw_request`。调用时它会替换为完整 AW 请求 `config_revision` 的域分离哈希，并与 evaluation binding 一致。AW 已提供经过校验的 event/原生 session、instance、binding 和配置修订。Provider 的有效 `session_id` 是 `[adapter, binding_id, instance_id, native_session_id]` 的域分离哈希，使用完整值、不截断。Instance/binding 和 call ID 也经过域分离哈希。公共 `candidate_revision` 对完整 candidate（含原始工具名称和 JSON 输入）计算哈希。任务/resource ID 保持配置中的含义。
5. 选择合法的公共 `ModelConfig` backend。生产环境使用证书验证的 HTTPS；凭据只通过 AW 子进程环境中配置的 `key_env` 提供。示例使用明确标注的 literal-loopback HTTP 和合成变量名。配置验证不访问任何 endpoint。不要将真实 key 写入 JSON/YAML 或记录。配置实际 backend 前，请核对 Semantic Arbiter 公共 backend 文档/源码。
6. 从与配置完全一致的 workspace，使用正常受支持的 Agent 设置启动 AW：

```sh
aw run --config /absolute/path/to/aw.yaml --agent target
```

每个 Provider 进程处理一个以 EOF 分隔的请求。`input.submit` 只更新所属会话文件，不创建 `record-*` 目录。`tool.before` 只有在调用本身通过映射和大小检查后才读取会话 prompt，因此 uncovered 调用从不读取 prompt。会话按作用域隔离的 `session_id` 存储，多个会话和实例可以并行推理；相同原生 session 字符串也会按不同 binding 或 instance 隔离。

Provider 进程 cwd **以及** `event.native.cwd` 都必须与 `workspace_path` 完全一致，adapter 必须匹配。原生 session 必须非空白、最多 512 个 UTF-8 字节且不含控制字符；不与预设 session 比较。`input.submit` 执行相同的 adapter、cwd 和身份检查，并且需要非空白、最多 512 字节的 `turn_id`；被拒绝的提交不记录任何内容。Instance、binding 或 call ID 缺失/非法、adapter/cwd 不匹配或操作未映射时，保存 `uncovered` 记录，不进行推理。相对 candidate 路径按配置的 workspace 解析；拒绝 `..`、`.`、重复/末尾斜线、反斜线及 workspace 外路径。未配置标签的路径若派生身份会产生歧义或非法——包含字面 `:`、超过 128 字节，或含 `[A-Za-z0-9._\-:@]` 之外的字符——同样记为 `uncovered`，不会被静默判断。不进行文件系统 canonicalization，也不读取文件。

按示例保留 `effects: [observe]` 和 `on_error: report`。`describe` 只声明 `evaluate_task_scope` / `input.submit`、`tool.before` / `observe`。合法调用始终返回原请求 digest、`status: ok` 和 `effects: []`；语义错误/拒绝只保留为私有观察。不支持或未确认的输入/效果不授予权限。简短的结构化 stderr 诊断只暴露稳定状态码。畸形请求或失败的 `validate_config` 返回脱敏协议错误，进程退出码仍为零。AW 仍负责终止进程、协议/传输错误和原生准入；observe-only 不保证所有 host 配置都会放行工具。

## 记录、限制与清理

已采集的 prompt 保存在已有根目录下的 `sessions/` 中：该目录为 `0700`，每个作用域会话一个 `0600` JSON 文件，以 64 位十六进制摘要命名，明文保存全部已采集轮次。每次更新先同步写入 `.pending` 文件，再原子重命名覆盖旧版本。会话文件会一直保留到你删除为止；原生会话结束时不会自动过期。

每次 `tool.before` 调用在同一根目录下预留一个独立私有 `record-*` 目录。所有记录文件均为 `0600`，经过同步并以不会覆盖已有名称的原子方式发布：

- `00-input.json`：完整 AW 输入，在构造 backend 或推理前持久化。
- `05-task.json`：格式版本 1，记录 `native_session_id`、作用域隔离的 `session_id`、AW `adapter`/`binding_id`/`instance_id`、AW `config_revision` 哈希、`task_version`、连接的轮次数 `turns` 及 `text_bytes`。仅在投影合法且通过大小检查后写入；uncovered 输入（包括超限 candidate）不生成 `05-task.json`。prompt 原文不在此重复保存，它位于 `10-evaluation.json` 和会话文件中。
- `10-evaluation.json`：映射后的 candidate、任务、ID 和原始预算；仅在已覆盖且未超限时生成。
- `20-model-request.json`：实际 state、questions 和 backend identity，在推理前写入。
- `30-model-result.json`：解析后的 reply 或脱敏错误；进程仍存活时也记录 timeout cancellation。
- `40-result.json`：`evaluated`、`uncovered` 或 `skipped`，以及可用时的建议性 evaluation。

`evaluated` 表示已保存 evaluation 响应，不代表模型成功作答：查看 `evaluation.judgments` 中的未知答案或模型错误，以及 `evaluation.outcome.control.candidate` 中的建议决策。`uncovered` 表示输入或上下文超出支持的映射范围，包括超限候选和没有可用已采集 prompt 的会话。`skipped` 表示无法开始评估，例如缺少凭据或准备阶段耗尽预算。

进程被杀死后可能只留下前序文件或 `.pending` 文件，其预留配额仍被占用。任何记录都不授权动作；没有自动模型重试、动作重放、fallback、清理、轮转或远程上传。初始记录失败时不进行推理。如果准备或等待锁在预留前耗尽预算，不创建记录目录，结构化 stderr 报告 `budget_exceeded`。即使最终状态无法保存，记录错误也会通过结构化 stderr code 显示。

`max_records` 范围为 1–10,000，分别限制记录目录数和会话文件数；`max_storage_bytes` 范围为 4 MiB + 128 KiB 到 1 GiB。每次 `tool.before` 保守地占用**固定 4 MiB 预留量**，小记录/不完整记录也一样；每个会话文件占用**固定 128 KiB 预留量**。配额已满时报告 `record_unavailable`。两个限制在记录根目录的配额 `flock` 内检查，并发调用不能超额预留。锁竞争通过 Tokio 协作等待，但只消耗原始预算的剩余时间；耗尽时跳过推理。锁在推理前释放，允许会话/实例并行推理。这种有界锁等待不是网络重试，不产生独立或续期的 deadline。根目录逐级打开且不跟随符号链接，再用 descriptor 固定。同一用户的外部修改不是安全边界；异常目录内容会被拒绝。只有停止使用该目录的进程后，才应手动检查、归档或删除自有记录；删除一份记录或会话文件会释放其预留量，该会话后续调用在下一条 prompt 之前为 `task_unavailable`。备份也应采用同等级保护。

AW 单请求上限为 1 MiB；arbiter 单 evaluation 上限为 256 KiB，完整原生 JSON 参数上限为 64 KiB。更大的 candidate 会完整保存在 `00-input.json` 中，并标为 `oversized_candidate`，不会截断后推理，也不生成任务上下文记录。`Ingress::capture` 在读取 stdin 前执行。deadline 从此时开始，取 AW `budget_ms` 和模型 timeout 的较小值；等待输入、解析、映射、等待配额锁、持久化和推理均消耗该预算，准备阶段不会续期。不执行网络重试。超时可能留下部分记录。host 必须限制完整进程生命周期，包括输出和文件系统阻塞。

## 验证与归属

测试使用真实 binary、本地 HTTP fixture 和公共 AW Host，不使用真实 Agent、远程模型或真实凭据。必需契约检查包括 allow/deny 时空 effects、完整 candidate、独立权限配置、按会话采集 prompt 的顺序/重复投递/大小限制、闭合私有配置与 scenario 校验、独立重建及并行会话/实例上下文、身份校验、不支持/超限输入不生成任务记录、畸形配置、凭据/模型错误、私有权限、符号链接替换、有界/并发配额准入且推理前释放锁、原子发布、进程被杀及原始准备/锁等待/HTTP deadline。这些是契约检查，不是原生 Agent 采用效果或生产模型质量的证据。

维护者/联系方式：通过仓库 issue 联系 ANOLISA 维护者。源码位于本目录及 `src/aw`、`src/semantic-arbiter` 公共库；使用仓库 Apache-2.0 许可证。参见 [Provider 贡献指南](../README_zh.md)。删除独立安装的 binary 或私有数据前，先禁用 AW step/provider 并停止活动调用。
