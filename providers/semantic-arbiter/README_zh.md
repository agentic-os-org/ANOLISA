# Semantic Arbiter AW Provider

[English](README.md)

这是一个 observe-only AW Provider，用于记录明确配置的单文件变更与任务范围之间的语义判断。
它通过 Semantic Arbiter 公共 API，将完整原生工具调用与操作者提供的一项任务进行比较。
它面向受控的 shadow 评估：即使语义结果为 `block`，也不会产生任何 AW effect，更不会执行工具、落实 obligation 或授予权限。

## 边界与敏感数据

**只有获得保留和传输这些数据的许可后才能启用。** 私有记录包含完整 AW 请求/event、任务文本、操作者采集信息、原生参数（包括替换文本/文件内容）、模型 state/questions、解析后的概率和建议结果。任务和完整映射后的 candidate 会发送给配置的模型。即使 Provider 不记录认证 header、从环境读取的凭据值或原始远端响应，这些输入仍可能包含密钥或个人数据。AW 的 `metadata_only` 审计配置**不会**关闭这份独立的敏感数据存储。请先使用合成数据。

这些记录只是本地观察，不是身份认证、执行 receipt、arbiter 可信进程内 `EvaluationRecord` 的持久化替代品，也不是文件系统身份的证明。配置或派生的 workspace/resource ID 只是操作者范围内的标签，不构成关于所指文件的任何证据。路径检查只处理字符串：Provider 不打开 candidate 文件，不验证符号链接、硬链接、挂载点、实际效果或原生工具语义。原生权限和安全检查始终独立且具有权威性。

精确配置的 `tool.native_name`：`Edit`、`Write`、`edit`、`write` 映射为 `replace`；`Delete`、`delete` 映射为 `delete`。每个映射只选取一个顶层 `/file_path` 或 `/path` 字符串。Write 输入必须包含该路径和字符串 `content`；Edit 输入必须包含该路径、字符串 `old_string` 和 `new_string`，可选布尔字段 `replace_all`/`allow_multiple`。Delete 输入只能包含选定路径。这些是操作者显式声明的输入契约，不表示每个 Agent 都提供这些名称。未知输入字段、任意 Bash、MultiEdit、多资源调用和其他方言都标记为 uncovered，不进行推理。这个受限映射不认证原生工具行为。

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
2. 明确提供一个已知的原生 session ID、任务版本/文本、adapter、绝对 workspace 路径及操作者管理的稳定 workspace ID。协议上的任务 origin 固定为 `operator`。语义单元依据任务原文判断，因此该字段要写真实任务。`task.provenance` 记录这段原文的来源：逐字转录原生用户输入而未改写时 `source` 为 `user_prompt_verbatim`，由操作者自行撰写时为 `operator_authored`，另外包含操作者标识 `captured_by`、采集时间 `captured_at_ms` 和 `text_sha256`。声明 `user_prompt_verbatim` 只是操作者断言，本 Provider 无法认证，既不提升 ingress 也不授予权限。`text_sha256` 是对原文字节的普通摘要——`printf '%s' "$text" | sha256sum`，加 `sha256:` 前缀——声明摘要与配置原文不一致的配置会在离线阶段被拒绝，使传递链路无法在不被发现的情况下改动采集内容。资源枚举是可选的：没有显式标签的路径会得到派生身份 `path:<以 ':' 分隔的 workspace 相对路径>`，allowed/preserved 列表为空表示未声明约束，范围和保留要求交给语义单元判断。非空 allowed 列表仍是硬上限，非空 preserved 列表仍优先于允许；两者都接受显式标签和派生身份。任务/session 变化时更新任务/版本/采集信息并重新加载 AW 配置。不自动发现任务、挖掘历史、从 candidate 推导权限、选择多任务或猜测 session。不知道原生 session ID 时，不能宣称已覆盖该会话。
3. 用运行时用户手动创建 mode 为 `0700` 的记录根目录，尽量放在 Agent 可写 workspace 以外。路径中的每一级都必须是真实目录，不能是符号链接。验证只打开/stat 已存在目录，不创建文件、不读取凭据。目录不存在、owner 或 mode 不符时拒绝。
4. 保留 `model.config_revision: aw_request`。调用时它会替换为完整 AW 请求 `config_revision` 的域分离哈希，并与 evaluation binding 一致。完整原生 session、instance/binding 和 call ID 经过域分离哈希，不截断。公共 `candidate_revision` 对完整 candidate（含原始工具名称和 JSON 输入）计算哈希。任务/resource ID 保持其明确配置的含义。
5. 选择合法的公共 `ModelConfig` backend。生产环境使用证书验证的 HTTPS；凭据只通过 AW 子进程环境中配置的 `key_env` 提供。示例使用明确标注的 literal-loopback HTTP 和合成变量名。配置验证不访问任何 endpoint。不要将真实 key 写入 JSON/YAML 或记录。配置实际 backend 前，请核对 Semantic Arbiter 公共 backend 文档/源码。
6. 从与配置完全一致的 workspace，使用正常受支持的 Agent 设置启动 AW：

```sh
aw run --config /absolute/path/to/aw.yaml --agent target
```

Provider 进程 cwd **以及** `event.native.cwd` 都必须与 `workspace_path` 完全一致；session/adapter 也必须匹配。缺少 instance/binding/call ID、上下文不匹配或操作未映射时，保存 `uncovered` 记录，不进行推理。相对 candidate 路径按配置的 workspace 解析；拒绝 `..`、`.`、重复/末尾斜线、反斜线及 workspace 外路径。未配置标签的路径若派生身份会产生歧义或非法——包含字面 `:`、超过 128 字节，或含 `[A-Za-z0-9._\-:@]` 之外的字符——同样记为 `uncovered`，不会被静默判断。不进行文件系统 canonicalization，也不读取文件。

按示例保留 `effects: [observe]` 和 `on_error: report`。`describe` 只声明 `evaluate_task_scope` / `tool.before` / `observe`。合法调用始终返回原请求 digest、`status: ok` 和 `effects: []`；语义错误/拒绝只保留为私有观察。简短的结构化 stderr 诊断只暴露稳定状态码。畸形请求或失败的 `validate_config` 返回脱敏协议错误，进程退出码仍为零。AW 仍负责终止进程、协议/传输错误和原生准入；observe-only 不保证所有 host 配置都会放行工具。

## 记录、限制与清理

每次调用在已有根目录下预留一个独立私有 `record-*` 目录。所有记录文件均为 `0600`，经过同步并以不会覆盖已有名称的原子方式发布：

- `00-input.json`：完整 AW 输入，在构造 backend 或推理前持久化。
- `05-task.json`：声明的任务采集信息——来源、操作者标识、采集时间、声明摘要、实际投影原文的摘要及其字节长度。原文本身不在此重复保存。
- `10-evaluation.json`：映射后的 candidate、任务、ID 和原始预算；仅在已覆盖且未超限时生成。
- `20-model-request.json`：实际 state、questions 和 backend identity，在推理前写入。
- `30-model-result.json`：解析后的 reply 或脱敏错误；进程仍存活时也记录 timeout cancellation。
- `40-result.json`：`evaluated`、`uncovered` 或 `skipped`，以及可用时的建议性 evaluation。

`evaluated` 表示已保存 evaluation 响应，不代表模型成功作答：查看 `evaluation.judgments` 中的未知答案或模型错误，以及 `evaluation.outcome.control.candidate` 中的建议决策。`uncovered` 表示输入或上下文超出支持的映射范围，包括超限候选。`skipped` 表示无法开始评估，例如缺少凭据或准备阶段耗尽预算。

进程被杀死后可能只留下前序文件或 `.pending` 文件，其预留配额仍被占用。任何记录都不授权动作；没有自动重试、fallback、清理、轮转或远程上传。初始记录失败时不进行推理。即使最终状态无法保存，记录错误也会通过结构化 stderr code 显示。

`max_records` 范围为 1–10,000；`max_storage_bytes` 范围为 4 MiB–1 GiB。每次调用保守地占用**固定 4 MiB 预留量**，小记录/不完整记录也一样。两个限制在非阻塞目录锁内检查，并发调用不能超额预留；锁竞争时跳过推理，不等待。根目录逐级打开且不跟随符号链接，再用 descriptor 固定。同一用户的外部修改不是安全边界；异常目录内容会被拒绝。只有停止使用该目录的进程后，才应手动检查、归档或删除自有记录；删除一份记录会释放其预留量。备份也应采用同等级保护。

AW 单请求上限为 1 MiB；arbiter 单 evaluation 上限为 256 KiB，完整原生 JSON 参数上限为 64 KiB。更大的 candidate 会完整保存在 `00-input.json` 中，并标为 `oversized_candidate`，不会截断后推理。`Ingress::capture` 在读取 stdin 前执行。deadline 从此时开始，取 AW `budget_ms` 和模型 timeout 的较小值；等待输入、解析、映射和持久化均消耗该预算，准备阶段不会续期。超时可能留下部分记录。host 必须限制完整进程生命周期，包括输出和文件系统阻塞。

## 验证与归属

测试使用真实 binary、本地 HTTP fixture 和公共 AW Host，不使用真实 Agent、远程模型或真实凭据。覆盖 allow/deny 时空 effects、完整 candidate、独立权限配置、任务采集记录与采集不一致时的拒绝、身份变化、不支持的输入、超大输入、畸形配置、凭据/模型错误、私有权限、符号链接替换、非阻塞/并发配额、原子发布、进程被杀及准备/HTTP deadline。这些是契约测试，不是原生 Agent 采用效果或生产模型质量的证据。

维护者/联系方式：通过仓库 issue 联系 ANOLISA 维护者。源码位于本目录及 `src/aw`、`src/semantic-arbiter` 公共库；使用仓库 Apache-2.0 许可证。参见 [Provider 贡献指南](../README_zh.md)。删除独立安装的 binary 或私有数据前，先禁用 AW step/provider 并停止活动调用。
