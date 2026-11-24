# Core Hook 策略准入

[English](../en/core-admission.md)

[设计索引](README.md) · [架构总览](architecture.md)

`aw_core::admission` 统一持有 Hook 策略准入规则和已准入步骤类型，并消费
`aw_config::requirements` 提供的目标需求投影。该模块把配置解析结果、原始字节 SHA-256 版本号、目标 Agent、可信 Adapter
能力声明与准入后的 Hook 步骤绑定为不可变 Core 类型，同时为已持有
`aw_config::Configuration` 的调用方提供纯函数规则入口。

`crates/aw-service/src/runtime.rs` 与 `crates/aw-host/src/lib.rs` 的生产 Hook
准备路径通过该接口执行，完整顺序为
`AdmissionEngine::preflight → Host::discover → AdmissionEngine::admit → Host::from_admitted`。
`Host::prepare` 兼容入口内部委托同一流程，原生 Hook 事件执行路径继续保持
`Adapter → Service → Host → aw-exec → Provider`。

## 准入阶段与类型

`AdmissionEngine` 在初始化时离线编译内置 `aw-config` Schema，并提供两个纯函数式
阶段转换及直接面向 `Configuration` 的辅助入口：

| 阶段 | 入口 | 输入 | 输出 |
| --- | --- | --- | --- |
| 静态预检 | `AdmissionEngine::preflight` / `admission::preflight` | 原始配置字节（或已校验的 `Configuration`）、目标名称、可信 `AdapterCapabilities` | `PolicyCandidate` / `Vec<AdmittedStep>` |
| 证据准入 | `AdmissionEngine::admit` / `admission::admit` | 按值消费的 `PolicyCandidate`（或已校验的 `Configuration`）、经协议校验的 `BTreeMap<String, ProviderEvidence>` | `AdmittedPolicy` / `Vec<AdmittedStep>` |
| 选择器投影 | `admission::projected_out_steps` | 已校验的 `Configuration`、目标名称 | `Vec<ProjectedOutStep>` |

`AdmissionEngine::preflight` 使用 `aw_config::Validator` 解析输入字节，按原始输入
字节计算小写十六进制 SHA-256 作为 `revision`，并执行 `admission::preflight` 完成
目标、事件、效果、工具选择器、传输方式与失败处置检查。语义相同但字节编码不同的配
置文档保持不同 `revision`，与 `aw-host::Host::revision` 算法一致。

`AdmissionEngine::admit` 按值消费 `PolicyCandidate`，使已校验的
`aw_config::Configuration`、`revision`、`target` 和 `AdapterCapabilities` 固定在
同一配置快照上，直接基于候选步骤需求核对 Provider 的 operation、events、effects
及私有配置证据，无需重复执行静态预检。使用 `aw-hook/v1alpha1` 或
`native-hook/v1alpha1` 的步骤不需要 `ProviderEvidence` 条目。`ProviderEvidence`
保留在 `aw-provider` 中承载协议校验后的 `Description` 与 `ValidatedConfig`，并由
`aw_core::admission` 重导出。

`AdmissionError` 封装 `aw_config::Error`，并通过 `AdmissionError::Rejected` 表达
Core 准入规则拒绝原因，保持 `Core::prepare` 和 `Core::execute` 使用的
`aw_core::Error` 不变。

## 配置需求投影

`aw_config::requirements::project(&configuration, target)` 返回借用配置的
`HookRequirement` 事件及其有序 `StepRequirement` 步骤，提供生效的事件预算、
可用性要求、事件与步骤选择器、Provider 引用、效果及独立的失败动作。调用方仍可
读取原始事件和步骤字段，继续检查 guard、协议、operation 和原生步骤。投影无需
入口模式、Provider 证据、进程、socket 或安装操作。

静态准入直接消费此投影。禁用的事件和步骤被略过；已证明无法匹配目标 Adapter 的
选择器交集被排除。属于其他 Adapter 的精确原生选择器无法匹配；标准工具名与原生
工具名之间的未知别名关系保留适用，等待 Adapter 提供实际工具身份。过滤保留步骤
顺序，运行时仍取事件与步骤选择器的交集。启用事件没有启用步骤时仍须能力准入，
包括 `required: false`；若所有启用步骤均被选择器排除，则该事件被略过。事件预算
未配置时使用 `execution.default_event_budget_ms`。原生步骤保留原始语义，不补造
Provider 效果。

投影描述期望策略，不携带能力证明。Source 求解和部署编译可结合现有 Admission API
消费投影；运行时登记与执行继续使用已准入策略。

## 离线部署 Descriptor

`aw_core::descriptor::compile(bytes, target, capabilities, availability)` 使用
可信的现有 Adapter Profile 完成静态准入，生成
`aw_contracts::deployment::DeploymentDescriptor`。部署合同注册名称为
`deployment-descriptor-v1alpha1`，`version` 为 `aw-deployment/v1alpha1`。
返回值可用 `serde_json` 序列化；`DeploymentDescriptor::parse` 读取已保存的字节，
拒绝重复键、未知字段、非法结构和不支持的版本。

Descriptor 仅携带原始字节 `configuration_sha256`、配置目标 `target`、Profile 的
`adapter`/`version`/`entrypoint`、绝对路径 `endpoint`、`wait_for_input_submit`、
`availability` 和合同版本，不包含 Provider 配置或执行计划。`auto` endpoint
解析为 `state_dir/aw.sock`；未解析或相对服务路径会被拒绝。编译不读取环境，不启动
进程，不执行安装。

启用的 `input.submit` 要求输入等待，包括可选且没有步骤的事件；静态准入仍检查
Profile 是否支持它。`NativeFallback` 与当前目标适用的已启用 required 事件冲突。
编译、SDK 启动、Service 注册和策略更新均按同一目标投影判定；适用且没有启用步骤的事件也计入。Adapter 负责
Profile 身份、能力声明和更严格的服务要求；CLI 从 Adapter 声明生成可选 Profile。
原生 SDK 接点向公共读取器传入预期身份和服务要求，公共读取器执行通用校验，
不按框架名称分支；Core 对所有传入的 Profile 执行相同的静态准入。

`descriptor::verify` 根据原始配置字节、目标、能力 Profile 和可用性选择重新编译，
逐项比较全部提示，用于可信部署产物的生成验证。SDK 启动时须先核对保存的配置摘要
及预期目标和 Profile，再使用提示；配置摘要标识配置字节，不认证被改写的 Descriptor。
运行时仍须完成 Service 准入、Provider 检查、Ready 和原生效果采用。

## 多源 Hook 求解与 Shadow Plan 等价核对

`aw_core::resolver` 以 `aw_config::requirements::HookRequirement`（保持 `spec.events`
作为期望事件、效果、失败动作与 `required` 的唯一事实源）和候选
`aw_contracts::source::SourceCapability`（`source-capability-v1alpha1`，版本
`aw-source-capability/v1alpha1`）为输入，在不探针进程、不打开 socket、不读写文件且不调用
`Core::prepare/execute` 的前提下，纯函数求解出
`aw_contracts::hook_plan::ResolvedHookPlan`（`resolved-hook-plan-v1alpha1`，版本
`aw-resolved-hook-plan/v1alpha1`）：

- 满足点位全部所需效果与失败动作的候选 Source 按 `SourceKind::Native` 优先
  （`Native > Python > Js > Sight`）排序。
- 每个受控 Hook 点位最多选定 1 个 `HookAuthority::Control` 控制 Owner
  （`control_owner`）。
- 同一 Hook 点位的其余可观测候选 Source 作为辅助 `SelectedSource` 挂载，并固定
  `dispatches_steps: false`，保证同一策略步骤绝不重复触发执行。
- `required: true` 的必需能力无合格 Source 时返回
  `ResolveError::UnsatisfiedRequired` 拒绝；可选能力缺失时在
  `ResolvedHookPlan::coverage` 中生成结构化 `CoverageGap`（`Uncovered` 或
  `Degraded`）记录。
- `aw_contracts::source::project_capability_cells`（以及
  `python3 src/aw/scripts/check_matrix.py --cells-json`）在全部 16 个标准语义 Hook
  事件上投影结构化 `16 × 框架` `CapabilityCell` 网格。
- `resolve_configuration` 在选择 Source 前按配置中的 Provider 协议检查适用步骤。
  布尔 Hook（`aw-hook/v1alpha1`，也是默认协议）无法执行 `replace_input` 或
  `replace_result`。解析后的计划只允许在 `tool.before` 和 `tool.after` 上运行原生命令步骤。
  两个事件均支持 `on_error: report`。如果负责派发的 Source 能阻断工具调用，
  `tool.before` 还支持 `on_error: block`。
- `resolve_shadow_preflight` 在离线测试中将 `AdapterCapabilities` 输入 `resolve`
  并独立编译 `AdmittedStep`，在全部已注册框架 Profile 与 SDK 部署 Profile 上核对
  Shadow Plan 与 `admission::preflight` 的准入判定及步骤编译结果完全一致，同时保持当前运行期准入主路径不变。

## 插桩 Profile（仅合同层）

`aw_contracts::instrumentation::InstrumentationProfile`
（`instrumentation-profile-v1alpha1`，版本
`aw-instrumentation-profile/v1alpha1`）是无副作用的声明式合同，用于描述要包装的
Python 或 JS SDK 函数／方法、所服务的语义 Hook 以及 Hook 字段从调用参数、接收者、
常量或返回值的映射关系。调用
`InstrumentationProfile::source_capability(adapter, entrypoint)` 可将已校验的
Profile 纯函数编译为 `python` 或 `js` 类型的 `SourceCapability`，供离线求解器推演候选语言插桩 Source：

- **仅合同层（未接入运行期派发）**：配置 schema、`aw-service`、`aw-adapter`、
  `aw-cli` 以及 Python/JS SDK 运行时均尚未消费 `instrumentation-profile-v1alpha1`。
  声明或校验 Profile 不授予任何可执行支持、不启用任何运行期包装，也不改变能力网格。
- **多目标保守合并**：同一 Profile 中多个目标声明同一语义 Hook 事件时，
  `source_capability` 对 `effects` 与 `failure_actions` 取交集，仅当该事件的全部目标均为
  `control` 时才保留 `HookAuthority::Control`；否则降级为 `HookAuthority::Observe`
  并丢弃控制类效果与失败动作。只有所有被包装路径都能执行的效果，Source 才可宣称。
- **自定义接点指纹约束**：`builtin` Profile 可省略 `code_sha256`，但 `origin: "custom"`
  Profile 的每个目标都必须提供 64 位小写十六进制 SHA-256 `code_sha256` 指纹。
- **JS 包版本绑定**：JS 目标的 `module` 必须等于声明的 `package.name`，或为该包内的
  子路径，使版本范围对应目标所属的包。
- **最终结果候选映射**：声明 `security.violation` 的目标必须提供 `output` 字段绑定，
  用于映射待检查的候选输出。
- **流式与事件边界约束**：由 AW 自身产生的事件（`runtime.observed`、`runtime.exited`、
  `coverage.changed`）不可插桩；迭代器调用形态（`sync_iter` / `async_iter`）在
  `tool.after` 声明 `replace_result` 或 `withhold_result`，或声明 `output` 读取
  `result` 及其子路径的 `security.violation` 控制门时，必须声明带 `buffer: true`
  的有界 `stream` 配置。

## 不可变 Scope 路由编译与缓存

`aw_core::routes` 独立于 `Core::prepare/execute`，将 `AdmittedStep` 切片与精确 `u64`
毫秒级事件预算纯函数编译为不可变 `PreparedRoutes` 快照（`compile`）与原生分组调度计划
（`schedule_event`）。每个 `EventRoute` 持有整事件预算、可选的最终结果替换步骤 ID 与步骤
`on_error` 策略，`HookRoute` 保留步骤注册顺序、`$aw.observe` 合成观测路由及
`SkillCheck` 标记。

`aw-adapter` 各框架方言提供声明式 `RouteOptions` 并将 `RouteError` 映射为方言既有诊断文案。
标准常驻 Source Profile 通过 `RouteOptions::standard(capabilities)` 编译；在后续 Source
SPI 接管回调归一化前，`reload::resident_supported` 要求 `aw_adapter::select` 已注册该
Adapter，确保未知 Adapter 在注册期即早失败（`registration_adapter_unsupported`），而不会推迟到回调期报错（`sdk_adapter`）。

`aw-service` 在每个 `Instance` 上缓存 `Option<Arc<PreparedRoutes>>`（在
`Instance::prepared` 构建、向子任务 `Scope` 共享引用，并在 `activate_policy` 热更新时原子刷新），
使 `ScopePolicy` 与 `sdk_callback` 共享同一份预计算路由快照并以 O(1) 时间定位事件和步骤。

## 不变量与运行时边界

`PolicyCandidate` 与 `AdmittedPolicy` 的内部字段全部私有，不实现 `Deserialize`
或公开结构体构造器，仅暴露只读访问器（`configuration`、`revision`、`target`、
`capabilities`、`providers`、`requirements` 与 `steps`）。调用方无法注入未校验的
`AdmittedStep`，也无法在预检、Provider 发现与 Host 构建之间替换配置快照或
Provider 证据。`Debug` 输出省略配置文档与 Provider 证据内容，避免泄露 Provider
私有配置值。

`AdmittedPolicy` 仅表示所供快照通过能力准入：

- `aw-host` 负责有界 `describe`/`validate_config` 交互、运行时文件读锁、
  Provider 进程执行与清理。
- `aw-service` 负责活跃绑定、任务 scope、步骤数与调用预算、策略激活门以及
  `aw_core::journal::FileJournal` 持久审计。
- `aw-adapter` 负责原生 Hook 注册、回调核验以及目标 Agent 对阻断或结果替换的实际
  采用。
