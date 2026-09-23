# 更新日志

## 未发布

### 修复
- `agentsight-enforcer` 升级后可自行恢复：旧版本遗留的 enforcement 布局无法被新二进制复用时，现在会自动检测并替换，文件删除防护与 enforcement 健康检查不再需要人工清理旧布局才能恢复。(#3445)

## 0.13.0

### 新功能
- 新增 `GET /api/preferences` 与 `GET /api/preferences/export`，从近期会话中识别用户的工作习惯（语言偏好、先出方案再动手、要求补测试、纠正模式、常用工具倾向），并通过 `/api/preferences/turns` 返回本次分析所依据的用户输入。分析在请求时实时计算，Linux 读取 `genai_events`，其他平台读取已采集的轨迹，因此不产生需要迁移或会过期的派生数据。
- 新增 `GET /api/trajectories/steps`，按派生类别（`user_input`、`system`、`agent_message`、`thinking`、`tool_call`、`tool_result`）检索 ATIF 步骤，并随每个命中返回其上下文步骤。类别是多标签的：同一个 agent 步骤可能同时包含消息、推理、工具调用与观测结果。
- 新增可复用轨迹标注体系：为轨迹打上可复用分诊标签、支持人工裁定标签、新增复用标签评审页，并将搜索限定为提供已标注的轨迹历史。
- 为 5.10/6.6 内核的 file-delete-guard 新增 inode guard，含域隔离、违规事件与启动清理。

### 修复
- macOS `trace` 路径初始化日志，使轨迹采集失败能够暴露而非被静默丢弃。
- 新增 token-plan provider 预设。
- 修复 enforcer ringbuf 消费者在 mmap 推进后的 fd 失效问题。
- traces 列表中移除 trace_id 别名。(#3261)
- 主日志器遵循 RUST_LOG 正则。(#3186)
- 命名空间 PID 解析为宿主 PID，并做 init-ns 自检。(#3041)
- 先创建 conversation_id 列再建其索引。(#3378)

## 0.12.1

### 破坏性变更
- 内核事件时间戳对齐：BPF 单调时钟改与实时时钟校准，不再基于容器启动时长。当时处于排队中的事件其历史墙钟时间无法重建；已持久化的记录不受影响。

### 新功能
- 支持采集 cosh-ng 的 rustls 明文，并按名称挂载 cosh 明文探针。(#3191)

### 修复
- token 总量统计不再重复计入缓存 token。(#3081)
- 静态 SSL 特征扫描改为流式处理。(#3087)
- SSL uprobe 重挂载 TTL 缩短至 30 秒。
- 拼接被分片的 HTTP 响应。
- 收到 SSE 结束标记时完结 h2 流，并正确统计通过请求头而非消息体开关声明的流式调用。(#3147)
- 合并长时间空闲后恢复的调用所产生的重复记录。
- 启动时数据库清理遇锁重试，并识别启动恢复中的结构化 oom-kill 信息。(#3135)
- 识别带 --max-old-space-size V8 参数启动的 OpenClaw 网关。
- 补充 agentsight-enforcer 日志。

## 0.12.0

### 新功能
- 新增 `--no-ebpf` 仅轨迹采集模式，支持 eBPF 不可用的环境。
- 支持 DashScope/Bailian 原生协议。(#2954)
- 新增 Kubernetes DaemonSet 打包。(#2976)
- 监控 Agent 资源使用情况。(#3005)
- 因果归因基于确定性、可复核的证据进行判断。(#3011)
- 新增仅依赖 LSM 的 Agent 文件删除防护 profile，并上报其能力。(#2979)
- 新增 vendored ABI guard、一键审计防护和 Dashboard 联动。(#2775)

### 修复
- 数据库清理跳过 VACUUM，避免触发 cgroup OOM。(#2916)
- 合并跨事件的 Anthropic SSE Token 用量。(#2920)
- 数据目录位于 overlay 存储时输出告警。(#2977)
- 通过可配置 procfs 根目录解析 PID。
- 按 session 统计 conversation 数量。(#3014)
- 为因果归因保留 tool 结果。
- 同步 ActPlane policy delta，并防止 enforcement fail-open。(#2979, #3016)
- 拒绝超长 enforcement 文件路径，避免静默截断。
- 从 systemd cgroup driver 布局提取容器 ID。(#2997)
- 识别现代 OOM-kill dmesg 格式。(#3001)
- 在初始化 BPF `cap_task` 状态前解析 PID namespace。(#3022)
- 将 Dockerfile RPM 包选择固定为 `TARGETARCH`。(#3025)

### 文档
- 补充 `--no-ebpf` trace 模式文档。

## 0.11.2

### 新功能
- 展示 Agent 历史活动记录。(#2817)

### 修复
- SSL uprobe 被内核静默注销后按 TTL 自动重挂载，抓包可自愈，无需重启。(#2792)
- agentsight 被 OOM 杀死后自动重启。(#2925)
- 探针事件通道按字节数限制容量，避免事件洪峰时内存激增。(#2925)
- CLI 与 dashboard 与文档对齐，并去除指南侧边栏重复项。(#2899)
- 数据库超限时按行比例裁剪。(#2872)
- 中断事件数据库裁剪至容量上限。(#2823)
- 保持 binding 接口状态一致。(#2794)

### 文档
- AgentSight 用户指南扩展为任务导向页面。(#2825)

## 0.11.1

### 修复
- 中断事件按 session/conversation 拆分时保留无 session 的记录，保证分组计数与总数一致。(#2796)
- 新增 Bun SSL_do_handshake prologue 模式，支持捕获 Claude Code（Bun 构建，>= 2.1.113）的 LLM 流量。(#2782)

## 0.11.0

### 变更
- 为 RPM 打包发布提升版本号，相对 0.10.3 无功能变更。

## 0.10.3

### 新功能
- 会话列表新增语义搜索。(#2733)
- 新增 DeepSeek Harness 的 dsh 插件。
- Agent 发现按类型分组。
- agent-health 新增 filtered_count。
- 新增 /api/docs 路由清单。
- Agent 健康卡片展示 LLM latency 指标。(#2586)

### 修复
- 移除 savings 上硬编码的负号。(#2754)
- 将 analyzer 调用归类为 internal。(#2755)
- savings 比率以分数形式返回。(#2732)
- 循环检测中匹配 tool 参数。(#2719)
- 探测伴随组件以提供 dashboard 能力。(#2696)
- graceful reap 仅作用于 worker agent。
- dashboard 中将 no_port gateway 计为健康。(#2694)
- 全新安装时打印 deadloop 提示。
- SIGHUP 时重新加载配置。
- 审计事件补齐 provider 与 session_id。
- 将 decompress fallback 降级为 debug 日志。
- Agent 发现中排除 sftp-server。
- 修正 copilot cmdline 规则拼写错误。

## 0.10.2

### 新功能
- 对 sqlite 存储实施大小上限。(#2644)
- Dashboard UI 支持中英双语。(#2612)
- 新增 raw 打包，用于不可变发布产物。(#2550)
- 新增 LLM latency 指标 API。(#2578)

### 修复
- latency agent 匹配不区分大小写。(#2590)
- 处理 latency 指标 CI 后续问题。(#2583)

## 0.10.1

### 新功能
- 初始化 dashboard i18n，按浏览器首选语言匹配。
- 在 agent 发现规则中新增 agentscope 框架。
- 精简组件接入文档并澄清服务启动方式。

### 修复
- 将 pause_turn 视为正常 SSE 结束。(#2320)
- 在 SSE continuation delta 之间保持 tool_call 名称。
- 在 observer namespace 中报告 pid。(#2360)
- 校验 enforcer RPM payload、保留 RPM staging 目标，并抑制重复的 enforcer 错误。
- 将 extractor 错误包装为 JSON envelope，并按变体分类 store 错误。(#2417, #2418)
- 去重 unavailable 状态并优化 dashboard metadata 同步。

## 0.10.0

### 新功能
- 新增 case containment 生命周期，包含策略交接、无空窗替换、专用 API 与 Dashboard 视图。
- 新增 system audit 协议、事件存储、Dashboard，并将 audit service 提取为独立 crate。
- 新增 ActPlane 风险执法，包含 enforcer 服务、硬化文件控制、安全生命周期与风险执法 Dashboard。
- 将 Codex rollout JSONL 转换为 ATIF，并覆盖 Codex routing 与 tool calls 测试。
- 在 raw HTTPS 事件上报告 process metadata，并在通用 envelope 中携带。

### 修复
- tool-call stop 时保持 turn 打开，并将 user_message_count 加入 turn.id bucket key。
- 流式解码分片 zstd SSE 响应。
- 提升 QwenCode trace 数据准确性，并在 agentsight config 中新增 QwenCode allow rules。
- cosh 重启后保持 session。
- 映射 cosh session 临时文件写入。(#2080)

## 0.9.1

### 新功能
- 重做 optimization Dashboard 视图，新增基于 detour 的成本浪费分析。
- 将 agent health 与 interruptions 移至专用 Dashboard 页面。

### 修复
- 为 OpenAI（`prompt_tokens_details.cached_tokens`）和 DashScope（顶层 `cached_tokens`）响应提取 cache token，并从 cosh-ng adapter prompt 模板提取真实 user query。
- 捕获通过绝对路径或 node wrapper 启动的 Claude Code。
- 正常退出时跳过 `agent_crash` 检测，并在 agent 退出时 flush 延迟的 GenAI 事件。
- 对非 LLM 路径跳过消息解析。
- 加密 optimization config API key。
- 展示已知 agent 规则。

### CI
- 将 eBPF 构建固定到 clang 15，并使用专用 runner pool。

## 0.9.0

### 新功能
- 新增 optimization 分析工作区、API、持久化分析历史，以及 accuracy/performance/cost 审查 Dashboard 页面。
- 新增 Qoder 轨迹采集、ATIF v1.7 导出、批量分析工具，以及带拓扑视图的 subagent 轨迹导航。
- 新增 CoshNG 命令行发现规则，并通过命令行上下文归一化 LLM 事件归属。
- 新增六种中断类型，以及对不可解析 LLM HTTPS 流量的回退捕获。

### 修复
- 修复 Anthropic SSE 解析、system prompt 注入与 cache token 核算。
- 修复 ATIF 批量输出以使用共享 ATIF v1.7 schema，并移除过期 v1.6 路径。
- 修复 optimization 与轨迹采集的边界情况，包括过期 conversation anchor 和 syscall tracepoint probe attach。
- 修复 Dashboard 空状态、错误提示措辞、auth loopback 处理与 session 导航行为。
- 将 raw HTTPS FFI 输出改为 opt-in，并对 OpenAI 和 Anthropic 流跳过重复 SSE 消息解析。

### 变更
- 将 optimization 维度分析归组到 per-target run root 下，并将并行 LLM 调用表示为 ATIF subagent 轨迹。
- 精简并门控默认 SLS 输出，除非显式启用否则不上传 trace 内容。

## 0.8.1

### 修复
- 将 `lock().unwrap()` 替换为 poison-safe `unwrap_or_else` 以恢复 mutex。
- 将 SSL 库归属从 aws-lc/BoringSSL 更正为 OpenSSL 3.x。
- 将 Claude 进程名加入 BoringSSL 分类。
- schema 迁移时保留用户配置而非覆盖。
- 不再自动覆盖无效 JSON 配置；在 ns pid helper 中记录进程 pid 而非线程 tid。
- 将高频事件日志从 debug 降为 trace 以减少噪声。

### 测试
- 扩展 handlers、interruption store 和 token store 的单元测试。
- 为 mutex `unwrap_or_else` 变更新增 poison-recovery 测试。

## 0.8.0

### 新功能
- 新增基于 Dashboard token 的认证，auth 配置仅限文件。
- Dashboard 中新增 LAN/公网 IP 地址展示与中文输出。
- Dashboard 新增 ECS 安全组指南与 metadata 集成。
- 新增 conversation grader API 与 Dashboard 控制。
- 新增 `COSH_SESSION_ID` 导出，用于 per-run session 关联。
- 通过 `schema_version` 自动升级过期配置。

### 修复
- 修复 Codex SSL 捕获与 SSE token 提取。
- 修复误报中断信号。
- 持久化 idle 流与 tool 结果以避免快照丢失。
- 显式检测 SSE 流错误。
- 将 `/health`、`/metrics` 和 server auth 限制为 localhost/仅文件配置。
- 从默认构建依赖中移除 `hf-hub` git fork。
- 修复 IMDSv2 token 获取、probe deadline 和 ECS metadata 去重。
- 修复 RPM 构建以将 `agentsight.json` 复制到 source tarball。
- 处理 clippy `single_match`、嵌套 if-let 和架构边界问题。

### 测试
- 新增 Dashboard mock HTTP 和单元测试以满足覆盖率门控。
- 新增 `build_output` 和 `public_address` 测试。
- 将 probe 测试标记为 `#[ignore]` 以适配 CI ECS runner。

## 0.7.1

### 修复
- 改进严重级别标签与 agent 侧边栏 UX。
- 在 summary 命令中展示所有 verdict。
- 同步 component.toml 版本与 package 版本。

## 0.7.0

### 新功能
- 新增 Codex CLI 适配，采用三级 SSL probe attach（符号表 → 字节模式 → offset 表）与跨 chunk SSE 续接 buffer。
- 新增安全可观测性 Dashboard 与 server proxy，用于 agent 威胁可见性。
- 新增内存优化，包含有界事件 buffer、feature flag（`features.*`）和可配置运行时限制（`runtime_limits.*`）。
- 在 `AgentsightLLMData` 中新增 `container_id`，用于容器级归属。
- 从进程环境变量和请求 metadata 派生 `session_id`，而非消息内容。
- 在 GenAI 语义事件中新增 `call_kind` 分类（chat / completion / embedding / tool_use）。
- 为 `agentsight audit` CLI 新增 `--exclude` 过滤以降噪，并在 audit 输出中展示非流式 LLM 调用。
- 新增统一 `agentsight summary` 命令用于一次性状态概览。
- 增强 token 节省页面，包含基线对比、策略分解、行级 diff 高亮与优化建议。
- 通过 SLS Logtail exporter 上传 skill metrics。
- 改进 agent health UX：角色徽章（P1/P2）、TTL 清理、进程族分组与 Session ID 帮助提示。
- 从 health API 过滤客户端进程以减少 Dashboard 噪声。
- 新增 anolisa component contract 用于 RPM 生命周期集成。

### 修复
- 修复 kernel 5.15 上 sslsniff BPF verifier 拒绝，并新增 BPF 加载测试。
- 修复 traced_processes BPF map 泄漏导致的长时间运行后 uprobe attach 失败。
- 在 detach 时将 inode 保留在 `traced_files` 中以防止重复 uprobe `Link`。
- 解码压缩（zstd/brotli）SSE 流，使 Claude Code 等 agent 被完整捕获。
- 加固压缩 SSE 解码以处理部分 chunk 边界。
- 从非流式和 HTTP/2 响应中提取 token 用量。
- 修复 udpdns 和 tcpsniff probe 中的 namespace PID 使用。
- 在容器化环境中为 uprobe attach 剥离 `/proc/{pid}/root` 前缀。
- 实现分级 SSL 和 tcpsniff ring buffer 预留以减少事件丢失。
- 在 filewrite/udpdns BPF probe 中先 clamp 再 mask；将 stdout payload 上限设为 `MAX-1`。
- 将 cgroup 门控改为 OR 语义，并新增 `trace_cgroup` FFI 接口。
- 收紧 SSE 截断检测，并为延迟 GenAI 调用写入 pending 行。
- 在 SLS exporter 模式选择中尊重动态 sysom 路径；用 `logtail_path` 过滤器替换已移除的 `sysom_logtail_path`。
- 在启动时校验 ring buffer size 为 2 的幂。
- 将 feature flag 和运行时限制接入实际运行时代码路径。

### 重构
- 将 `genai/builder.rs` 拆分为 4 个聚焦模块，`genai.rs` 拆分为 5 个子模块。
- 将共享 BPF map 打包进 `SharedMaps` 以减少重复。
- 提取后台线程模块并支持 stop-signal。
- 用 `if-let` / `?` 模式替换剩余 `unwrap()` 调用。

### CI 与质量
- 新增 fmt、clippy、单元测试覆盖率和架构边界检查 CI 门控。
- 新增 `clippy.toml` + `cargo-deny` 用于 lint 和供应链审计。
- 新增架构边界检查脚本（`check-arch-boundary.py`）。
- 为 FFI、unified orchestrator 和 storage 模块新增 scoped AGENTS.md。
- 定义 Footprint Ladder 用于代码表面增长控制。
- 新增 `agentsight-code-review` 和 `pr-body` develop-skills。

## 0.6.1

- 在 trace 模式中新增实时 agent_crash 检测。
- 新增 OOM 崩溃检测。
- 新增 cgroup 级事件过滤，兼容 v1/v2。
- 通过 per-user home 扫描支持 QwenCode skill 发现。
- 通过动态路径支持 SLS Logtail 激活可逆。
- 支持通过 token-collector 开关将 ilogtail `SLS_LOG_PATH` 桥接到 config。
- 默认 `traceEnabled` 设为 false，以默认从 SLS 中丢弃对话内容。
- 当 `traceEnabled=false` 时从 SLS 上传中丢弃 `gen_ai.system_instructions`。
- 将 session_id 和 conversation_id 的派生从 message 内容改为 response_id。
- 修复 CJK deadloop 检测、`kill()` 错误检查和 SIGKILL 升级。
- 通过 VACUUM 优化修复 SQLite 读写竞争。
- 修复 rpm-build.sh agentsight 构建失败。
- 修复重复 new+start 时的 allow log path 重新初始化。

## 0.6.0

- 新增失控 agent 进程的 deadloop 检测与自动 kill 机制。
- 新增 retry storm 检测和 `/metrics` 中断计数器。
- 新增 BPF 层 HTTP 协议过滤和针对未知 IP/port 目标的通配捕获（`*`）。
- 新增敏感消息字段的客户端混合加密。
- 新增 `traceEnabled` 配置开关，由 SLS 上传层强制执行。
- 新增通过 DNS 解析到 tcpsniff BPF map 的 HTTP domain 规则。
- 新增默认 DashScope HTTPS 规则和 `anolisa_release` 模块。
- 新增 `tcp_targets` 和 `input_delta` 配置的 FFI 接口。
- 为 kernel 6.0+ 的 UDP DNS probe 新增 CO-RE 兼容性。
- 通过 config 热加载支持运行时 SLS logtail 路径。
- 扩展中断类型并新增 logtail 导出。
- 将 config 重构为 `https`/`http` 规则。
- 重构按 `tool_use_id` 查询 `stats.db` 并统一 savings 展示。
- 重构从 `agentsight.json` 加载加密公钥。
- 修复 HPACK Huffman header 解码。
- 修复 BoringSSL probe attach、FFI 事件投递和 chunked-body panic。
- 修复在 event-stream 响应中保留初始 SSE chunk。
- 修复 `c_char` / BPF comm 可移植性（i8 vs u8）。
- 移除死代码和已废弃 API。

## 0.5.0

- 新增 Claude Code 支持，包含 BoringSSL 的 SSL probe attach、Anthropic SSE thinking/tool_use 内容块，以及基于 `message.id` 的 session 关联。
- 新增 tcpsniff probe 用于明文 HTTP 流量捕获，支持可配置 IP/port 过滤（默认禁用，`tcp_targets` 为空）。
- 新增基于 User-Agent 的 agent 检测，带 `comm` 回退以简化 agent 匹配。
- 新增 UDP DNS probe 用于 agent 发现（替代 TLS SNI probe），QNAME 解析移至用户态。
- 新增 TLS SNI probe 模块，并将发现重构为 config 驱动规则。
- 新增用于已建立 LLM API 连接的连接扫描器。
- 在 `AgentsightLLMData` FFI 结构中新增 `tools` 字段，作为原始 JSON 透传。
- 在 BPF traced 进程过滤和事件发射中新增容器 PID namespace 支持。
- 新增 agent 匹配规则，并将 BPF ring buffer 降至 32MB。
- 在 SLS 日志中新增 `uid` 字段，使用 `OnceLock` 缓存和启动校验。
- 支持基于 profile 的安装。
- 修复 LLM data 中 `duration_ns` 计算。
- 修复进程退出时 SSL probe 的过期 inode 清理。
- 通过移除 `udpdns.bpf.c` 中的嵌套 `#pragma unroll` 和在旧内核上 mask `payload_len`，修复 BPF verifier `-E2BIG` 问题。
- 修复 Hermes agent 架构的 skill 提取。
- 修复 OpenClaw matcher 中 Node.js `process.title` 变更处理。

## 0.4.0

- 新增分片 SSL write 的 HTTP/1.1 请求 body 重组。
- 新增基于 cosh 文件系统的 skill metrics 分析。
- 新增 GenAI 事件的 SLS 上传和 Logtail 文件 exporter。
- 新增 hermes agent matcher 用于 LLM 进程发现。
- 在 SSL sniffer 中检测 uv Python 静态 OpenSSL。
- 移除基于 AK/SK 的 SLS 直接上传，保留 Logtail 文件导出。

## 0.3.1

- 修复简化 agent_crash 检测和修复多进程去重。(#411)
- 修复 audit CLI db 路径使用 SqliteConfig。(#399)
- 修复在 agent health UI 隐藏 Cosh 并移除 keepalive 支持。(#401)
- 修复 AGENTS.md 中的 API endpoint 表。(#397)

## 0.3.0

- 新增中断检测系统，带 drain 机制和 Dashboard 集成。(#315)
- 新增 token 节省页面和 API endpoint 用于优化可视化。(#310)
- 新增复合 token 节省和请求计数追踪。(#320)
- 新增带 cbindgen 头文件生成的 C FFI API。(#306)
- 新增 filewatch 和 filewrite eBPF probe 用于文件访问监控。(#308, #309)
- 支持 cosh 的 SysOM AK/SK GenAI 捕获。(#305)
- 使用 LLM API response_id 作为 trace_id 并新增 conversation_id 字段。(#304)
- 从 agent 自身 session 通过 ResponseSessionMapper 解析 session_id。(#303)
- 修复 interruption CLI 并对齐 conversation_id 命名。(#318)
- 修复通过支持 snake_case response_id 的 cosh session_id 识别。(#307)
- 修复 token savings 复合中的错误 tool call id。(#316, #317)
- 修复标准化 call_id，新增 tool_call_ids 列。(#319)
- 修复 genai builder 和 storage 中的 session_id 和 response_id 映射。(#321)
- 修复对话列表中的 token savings 展示。(#322)
- 修复按 pid 缓存 agent name 用于死进程解析。(#358)
- 修复移除自定义 db 路径并使用默认路径。(#359)
- 支持 CI 中 nightly docker 镜像构建。(#302)

## 0.2.2

- 支持以 AgentSight 服务启动 backend-server 用于 Dashboard。
- 修复 Dashboard 前端针对多种 display-size 的动态宽度。

## 0.2.1

- 为 agent 发现向 CoshMatcher 新增 `/usr/lib/copilot-shell` 路径。(#190)
- 为 `genai_events.db` 新增 200MB 大小限制以防止无限增长。(#211)
- 移除返回错误数据的 `/api/stats` endpoint。(#197)
- 从 HttpRecord 提取 audit 并过滤非 LLM 调用。(#196)
- 在 token 查询中使用 `--compare` flag 时始终展示对比数据。(#194)
- 修复 README 文档中错误的 `discover` 命令。(#191)
- 移除 breakdown 命令并保留 token 消耗注释。(#193)
- 在 systemd service 文件中用 `MemoryMax` 替换已废弃的 `MemoryLimit`。(#181)

## 0.2.0

- AgentSight Dashboard Web UI，带实时监控界面。(#74)
- Agent health 监控，带离线告警和卡死进程 Dashboard 重启。(#158)
- 从 Dashboard 一键导航到 ATIF trace 分析页面。(#116)
- `/metrics` endpoint，暴露标准 Prometheus 格式数据。(#134)
- 支持 HTTP 2.0 协议。(#147)
- 支持构建 RPM 包。(#166)
