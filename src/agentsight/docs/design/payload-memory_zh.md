# HTTP payload 所有权与内存限额

[English](payload-memory.md)

AgentSight 为保留的捕获 payload 设置限额，并在配对提前结束时输出不完整调用记录。
实现面向并发流式负载：在投递前释放重复 payload，并在流期间归约 SSE delta。
进程 RSS 仍需按完整运行环境测量，包括 ring buffer、原生解码器状态及 allocator 保留的内存。

## 所有权边界

SSL executable 扫描使用 SIMD 子串搜索，按 4 MiB 有界窗口保留特征重叠区。重叠匹配与跨窗口的
起始位置均保持原有语义；inode 缓存与符号选择规则不变。

```mermaid
flowchart LR
    Probe[Probe payload] --> Capture[按条数和字节限额的队列]
    Capture --> HTTP[HTTP 配对]
    HTTP --> Stream[分片去 chunk 与解压]
    Stream --> Reduce[SSE delta 归约]
    Reduce --> Analysis[共享已解析 JSON]
    Analysis --> Call[Owned 语义调用]
    Call --> Queue[按字节限流的 FFI 队列]
    Queue --> Callback[借用 Callback buffer]
```

分片请求在 body 上限内按声明长度预留容量，只复制一次首片 body，再把首片 TLS source 替换为元数据。
body 完成时释放多余容量。Chunk framing 使用增量游标追踪，原始请求 body 仍可供已有内容解码 helper 使用。

SSE 管线保留未完整事件及 provider delta 分组，并释放 TLS 历史 buffer。OpenAI chat、Responses、
Anthropic 与 DashScope delta 会合并，生命周期事件保留顺序；未知事件逐条保留，并受同一字节限额约束。
Responses 生命周期事件移除已在请求侧捕获的回显 `instructions`、`input`、`tools` 和 `prompt`。
原始事件数、首次有效输出时间、用量和完成时间仍供语义投递使用。

gzip、zstd、deflate 和 Brotli 增量解压；deflate 同时支持 zlib 与 raw framing。HTTP chunk framing、
UTF-8 及 SSE 事件边界均可跨捕获记录。HTTP/2 DATA 使用同一解码器和归约器。完成或移除时，先释放
解码器上下文及大 scratch buffer，再构造归约后的事件。`retain_raw_sse` 和 FFI raw HTTPS 回退会选择
已有的原始序列路径，该路径同样受保留限额约束。

分析路径在审计、Token 估算回退和 GenAI 构造之间共享请求及响应 JSON。Raw 导出或未知格式回退时才
构建完整 HTTP body String。构建语义调用前结束捕获数据的所有权，FFI 入队前结束分析 payload 的生命周期。
只有存在 SQLite sink 时才构建 pending 行所需的 JSON。

Standalone 在构建语义事件前保存 HTTP 视图，随后将 body String 移交给调用；raw 回退继续持有 HTTP
记录。序列化容量超过 body 长度的 125% 时，在进入延迟投递前释放多余容量。分析 payload 和 pending
行摘要在入队或完成写入前结束生命周期。首尾用户文本和单条 system 消息只有一个文本片段时借用原文本，
多个片段继续按原有语义以换行拼接。

延迟调用中的大请求文本和 `user_query` 在可精确匹配时，以 owned 原始 body 或消息文本中的字节范围
保留；无法精确匹配的字段继续持有原分配。队列计量包含去重后的 payload 和范围描述符。超限、
超时、进程退出及关闭等出口均在逐条投递前还原字段，批量 drain 不会同时还原所有请求副本。
配置关闭 session 映射或请求已明确提供 session 时，跳过延迟等待窗口。

SQLite 为唯一活跃 sink 时也跳过 payload 延迟等待：完成的 capture 只插入一次，不构造 pending 行的大 body
摘要。已有 pending 行或 idle snapshot 仍走正常 promotion，保留已捕获的请求证据。尚未获得 session
映射的调用只在已有的 retroactive fix-up registry 中保留 call ID；后续文件写入同时修正 session 列
和完成事件 JSON。FFI 与活跃的非 SQLite exporter 继续延迟投递，因为已经发给这些消费者的事件无法修正。

已注册的 sink 会报告当前是否活跃；未知 exporter 默认按活跃处理。默认 Logtail 的路径选择和实际写入
复用同一组目标文件存在性及 telemetry sentinel 检查。动态 sink 每次重新读取当前路径，因此暂停后
恢复不需要重新注册。立即投递仍会调用不活跃 sink 的原有写入门控：落库过程中启用时不会丢弃当前
调用，该调用可能带有 fallback session metadata；后续调用面对活跃的外部 sink 时继续使用原有解析
等待窗口。

SQLite completion 在获取连接锁前序列化 JSON。容量选择使用存储列的 `octet_length`，读取字节长度，
避免持锁加载并逐字扫描整份 payload。投递仍为同步执行；这些修改缩短其临界区，不增加持久化队列。
完成事件的 JSON 按保留 payload 的估算预留容量，并释放序列化产生的多余容量。SQLite 消息列需要完整
数组时，从该 JSON 借用 `RawValue`；增量子集保留原有序列化方式及完整字段内容。
直接插入与 promotion 的回退插入复用同一份序列化结果，包括容量错误后的重试。

GenAI 维护通过注册路径打开独立连接，不等待写锁，也不触发自动 WAL checkpoint。按时间和容量删除
使用小事务，每批最多 128 行，估算 payload 目标为 4 MiB；单条超大记录仍允许删除，以保证清理能推进。
批次之间让出执行机会，写锁冲突时将剩余工作延后。checkpoint 先通过 passive 模式复制 WAL 页面，再
尝试不等待的截断；未完成 checkpoint 时停止后续容量淘汰。按时间清理、容量清理与 checkpoint 分别计时，
用于区分维护耗时来源。

FFI 将调用和 HTTP 记录 move 到 boxed 队列条目。字节 reservation 在出队后持续到 callback 返回，
发送失败或 receiver 销毁时自动扣回。大 JSON 字段直接序列化为带 NUL 结尾的字节 buffer，长度不包含
终止符。Raw HTTP 字段在 callback 期间借用记录的存储。C 结构布局与函数签名保持兼容，消费方仍需在
返回前复制借用字段。

## 连接生命周期

HTTP/1 强制移除时移交旧状态的所有权，并统一清理所有辅助缓存。HTTP/2 使用同样的不完整记录语义，
同步清理已解码 header 和 continuation 状态。移除原因包括 `lru_evicted`、`capacity_rejected`、`replaced`、`hard_timeout`、
`over_budget`、`pid_exited`、`protocol_error` 和 `shutdown`。正常完成及空 Idle 状态的软超时单独计数。

在途证据转为 `AggregatedResult::Abandoned`，包含 owned 请求或部分响应。容量与字节压力移除在入队前
释放正文、原始 TLS source、SSE reducer 和解码缓冲，仅保留最多八个路由或关联 header，每字段上限
2 KiB。专用元数据 analyzer 与 builder 跳过正文解压、JSON 与消息解析、token 计算和请求 hash，
设置 `metadata_only=true`；其他生命周期移除继续保留已捕获的证据。两条交付路径均输出 `finish_reason="incomplete"`，metadata 包含 `abandon_reason` 和 `incomplete=true`；字节超限还设置
`truncated=true`。这些调用跳过 session 延迟解析和空语义调用的 raw HTTPS 回退，因此关闭 SQLite 或部分
请求无法解析时仍可投递。FFI 队列过载继续采用丢弃最新事件并计数的策略。

空闲软超时只借用请求并写一次 pending 行，不 clone 连接状态；硬超时真正移除调用。Standalone 与 FFI
循环无采集流量时也会维护连接。新流量刷新空闲计时；原请求移除后，迟到的 SSE 响应可作为 response-only
重新开始。进程退出或采集关闭时，先移交剩余证据再释放状态。

HTTP/2 在 END_STREAM 前完成时只留下有界标记，不保留 payload 状态；迟到的压缩尾部不会重新创建在途流，
trailer 仍继续解码以保持 HPACK 一致。
资源驱逐和协议错误也使用同一关闭流路径；丢弃的 DATA payload 计入
`agentsight_http2_closed_stream_discarded_bytes_total`。

HTTP/1 SSE 活动同步提升连接和活动时间 LRU。资源驱逐或协议错误只输出一次诊断记录，并留下
受连接容量约束的标记。匹配标记的后续读取计入 `agentsight_http_sse_discarded_bytes_total{reason}`，
不再重新建立 response-only 配对。新请求或 HTTP 响应清除标记；定期维护按配置的软超时、硬超时和
60 秒三者中的最大值使标记过期。PID 清理和 reset 也释放标记；容量驱逐可能提前移除标记。
容量拒绝的调用连响应 header 也会跳过，直到新请求到达。硬超时移除仍允许迟到响应恢复为 response-only。

HTTP/1 与 HTTP/2 容量准入都跳过最近 5 秒有流量的在途调用，覆盖请求组装、等待响应和流式接收阶段，
包括 raw 保留模式。所有条目均受保护时，
新调用仅输出 `capacity_rejected` 元数据诊断，已有流仍受配置的条数上限约束。硬字节上限优先回收
不受保护的条目，必要时仍可回收受保护流，避免活动保护绕过内存上限。
HTTP/1 完成时释放全部 payload 缓存，仅用连接 ID 与完成时间将名额保留 5 秒。后续同连接请求消耗自己的
保留名额，新连接无法在 keep-alive 请求间隙抢占。活跃状态与保留名额之和不超过配置容量；过期、PID 清理与
reset 会释放名额，重复完成清理不会延长保留时间。保留名额不计入 pending 调用，也不持有采集分配。
HTTP/2 流 ID 不能复用，因此无需保留已完成流的名额。

## 预算与可观测性

默认 HTTP/1 容量为 128，HTTP/2 流容量为 512，共用 64 MiB 配对预算；Probe 队列和 FFI 预算各为 64 MiB，
延迟 GenAI 队列预算为 16 MiB，ring buffer 为 64 MiB。Body 上限为 8 MiB，单流 SSE payload 上限为
2 MiB，空闲软、硬超时分别为 60、300 秒。

计量包含 capacity 和 owned String，对 HTTP/1 共享 TLS source 去重，并计入 continuation、HPACK
及解码器存储。聚合层按 LRU 顺序执行共用字节预算；body 与 SSE 限额在分片到达时检查。这些估算约束
保留的 payload；临时解析、callback 序列化、C++ 复制及 allocator 开销需要额外内存余量。

连接和流的总量在状态变更与移除时更新。Raw SSE source 去重及 HTTP/2 帧计量只处理新追加的条目，
定期维护保留超限兜底检查。HTTP/1 持续保存的 source 去重集合计入共用预算。
Compact 归约在发生变化的 JSON 子树内更新分配估算，并使用结构化 delta key；完成时把已解析的值
移交给语义交付，只在消费者需要字节时延迟序列化。Compact SSE framing 直接从 continuation buffer
借用单行 payload，仅对多行数据进行拼接，避免每帧创建有所有权的 capture 与结果向量。
归约器先扫描 JSON 字面标记；不影响归约或结束判断的帧保留 payload 供下游分析，不提前试解析 JSON。
字段名包含转义时保守地走 JSON 路径，已解析的透传事件直接保留 Value。
Raw parser 在 framing 消费完成前保留多行数据的 capture 来源。
DashScope 显式 `parameters.incremental_output` 选择追加或快照语义，无需前缀比较；未知模式保留
精确的逐事件前缀判断，包括同一流中切换模式的兼容行为。

指标包括活跃条目及保留字节 gauge、按原因和状态分类的移除计数、不完整记录输出数、覆盖 callback
生命周期的 FFI 字节数，以及按条数或字节限额分类的 FFI 丢弃计数。两种运行模式均可使用已有的
`AGENTSIGHT_METRICS_FILE` exporter 导出。
Stage 耗时还覆盖 pending 插入、完成、连接锁等待和 GenAI 维护。

回归测试覆盖 32 路同时存在的 1 MiB 请求，每路交错接收 1000 个 SSE delta，并核对完整文本和 Token
用量；也覆盖所有权保留、同 key 替换与 LRU 淘汰、混合 HTTP 字节预算、无流量硬超时、部分响应、
分片压缩 SSE、开启和关闭 raw 导出时的 provider 字段一致性及 FFI reservation 生命周期。
RSS 与吞吐 bench 单独执行；字节预算并不直接保证进程 RSS 上限。

详见[运行时配置参考](../../../../docs/user-guide/zh/agentsight/configuration.md#运行时上限)。
