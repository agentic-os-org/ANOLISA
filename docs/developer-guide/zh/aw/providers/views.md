# Provider 只读展示

[English](../../../en/aw/providers/views.md)

AW 通过 `aw inspect --config ABS_FILE --json` 提供单个桌面快照，不执行绑定、工具
事件、批准、配置编辑或服务启动。复用现有有界执行器和本地服务客户端，不新增 daemon、
领域引擎、数据库读取器或统计存储。

## 登记与兼容性

可选的 `spec.providers.<id>.view` 包含 `protocol`、`implementation` 和受信任的
`transport`，沿用现有 stdio 结构，要求绝对可执行路径。登记独立于事件 transport 和
不透明私有配置，由离线校验器和生成 Tokenless 配置的包工具支持，不改变事件准入，
也不授予原生效果。[共用示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)
为两种入口登记相同 CLI/数据目录。

`aw-provider-view/v1alpha1` 独立于 `aw-provider/v1alpha1` 和
`aw-service/v1alpha7`，事件请求/响应 schema 保持不变。使用匹配的 Preview 二进制
和配置；旧二进制可能拒绝新登记。缺失或不支持的展示能力不改变 sec-core 检查、
after 完成观察或用户 Hook 调度。

## 可执行合同

[内置 schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/schemas/view-v1alpha1.schema.json)包含
`$defs/request`、`$defs/response`、`$defs/panel`。Rust 校验器复用已有有界 JSON
解析器，拒绝重复键、嵌套/大小超限和整数溢出。`_ms` 时间均为 Unix 毫秒，
无需日期格式依赖。计数为不超过 `2^53 - 1` 的非负可互操作 JSON 整数。

| 消息 | 合同 |
| --- | --- |
| describe 请求 | protocol、operation、request_id、provider_id、完整 config_revision、budget_ms |
| query 请求 | 同一身份，加 `view: metrics.summary` 与 `parameters: {limit: 200}` |
| 响应 | 回显全部身份及 operation，加 status、observed_at_ms、code、data |
| describe 数据 | implementation、可空 version、支持的视图 |
| query 数据 | 白名单总计/operation、明确的存储/窗口范围、unknown 录制状态、unverified 采用状态 |
| Panel | api_version、generated_at_ms、config_revision、service、providers、recent_execution |

每 Provider 的 describe/query 共用最多两秒。迟到响应、身份/实现不匹配、未知 schema
字段、版本不兼容或不支持视图均不能成为成功指标。响应不接受命令、URL、标记语言、
任意私有字段或事件效果。这是受信任同用户进程接入，不是恶意 Provider 的安全沙箱。

快照预算三秒、输出 1 MiB；现有执行器清理可另加一秒验证预算及协作终止宽限。
Tokenless view 模式处理终止信号，取消并回收嵌套 CLI 进程组；既有事件入口生命周期
保持不变。Provider 在快照预算内串行查询，剩余视图可超时，不延长预算。消费者应合并
请求、丢弃迟到响应，不堆叠无界轮询。查询失败不改变事件失败策略。

| 状态 | 含义 |
| --- | --- |
| ok | 此视图按所述范围返回了有效数据 |
| disabled | 没有启用的事件步骤引用此配置 Provider |
| unsupported | 未登记/不支持视图，或使用不支持的统计库覆盖 |
| no_data | 缺库，或窗口内没有有效记录 |
| unavailable | 查询、传输、输出、schema 或身份失败 |
| partial | 保留给消费者的部分可用快照；各卡片保留实际状态 |
| stale | 无法再确认服务观察；消费者保留旧数据时须标陈旧 |

诊断码由 `$defs/code` 枚举。缺库的 no_data 带 `store_absent`；已有空库不带错误码。
未知数值不编造成零。静态配置健康不证明后台存活、Provider 准入、原生安装或模型采用；
没有独立证据合同时保持 unknown/unverified。

## 数据来源与留存

Tokenless view 使用显式目录调用公共 `stats summary --json --limit 200`，
只接受源 schema `1.1`。调用前检查 `stats.db` 是否存在，避免建空库；已有库查询可能
发生组件自身迁移/维护。非空 `TOKENLESS_STATS_DB` 或有效 DSH 覆盖暂不支持，
等待公共有效存储位置查询合同，不在 AW 重写组件路径解析。

校验记录上限、安全计数、operation 合计及优化前后差值，仅投影白名单。dry-run 排除
复用公共 summary；不把全表 retrieve 归因与有限压缩窗口混算。存储历史可能来自非 AW
接入；这些数字不证明会话、今日、计费、净收益或原生采用。

服务的只读 `recent_executions`，返回本次 generation 最近最多 20 条完成步骤投影，
新记录在前。有界内存队列仅在持久 append 成功后更新；展示缓存失败不能让事件失败或
改变审计健康度。发布缓存时保留 journal 锁，维持并发写入顺序。读取至多复制 20 条，
不跨 Provider I/O 持锁。不新增持久历史；重启
清空队列，原有按 key 查询的持久审计继续保留。投影只含 Provider/步骤、方法、耗时、
状态、失败类别、失败动作及结构化阻断请求。可选 `kind` 字段区分 `provider`、`native`
和 `skipped`；跳过步骤没有启动调用进程。读取端仍接受省略 `kind` 的服务投影。
投影不含原始输入/输出、原生环境、替换正文或批准凭据。

inspect 绑定配置修订及服务 generation，完成 view 查询后复查同一客户端；无法确认时
清空近期执行数据，并从最终响应刷新审计健康度。近期执行的成功结果必须包含数据且
code 为 null，其他状态的 data 为 null。组件存储历史与活跃 AW 会话独立。原有 Tokenless SQLite/SLS 留存
是另一边界；组件的数据留存由其配置和清理操作管理。

## 回归与客户端 fixture

[人工 fixture](https://github.com/agentic-os-org/ANOLISA/tree/main/src/aw/crates/aw-provider/examples/views)覆盖两种方法及六种 Panel 状态，
不证明原生采用。测试覆盖严格校验、子进程限制/取消、不启动/不建库、身份/配置不匹配、
generation 重置、有界历史及保留阻断。包 CI 额外通过真实公共 CLI 写入统计，
验证排除 dry-run，并与同一 summary 对照。
