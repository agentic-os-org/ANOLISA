# AgentSight 只读接入

[English](../../../en/integrations/agentsight/agentsight-readonly.md)

后续[证据/上下文层](../../runtime/evidence-context.md) 将连接器与已准入原生身份组合；
诊断命令仍独立运行，关联状态保持 unassociated。

## 范围与交付顺序

本文对应 [#7621](https://github.com/agentic-os-org/ANOLISA/issues/7621)，是
“AW 为基础、AgentSight 补充证据”的首个实施切片。
`aw-service::agentsight::Client` 读取指定来源会话的轨迹，不提供独立查询命令。
正常入口为 `aw run`，由 #7622 / #7655 按[生命周期设计](agentsight-integration.md)接入。Hook 分发、Provider 效果、本地 Journal 持久化和
AgentSight 生命周期仍由各自组件负责。

| 阶段 | Issue | 依赖 |
| --- | --- | --- |
| 只读来源连接器 | [#7621](https://github.com/agentic-os-org/ANOLISA/issues/7621) | AgentSight 现有 GET API |
| 运行生命周期、可信身份、证据与有界上下文 | [#7622](https://github.com/agentic-os-org/ANOLISA/issues/7622) | #7621；范围准入与 #4372 协调 |
| 能力覆盖与显式降级模式 | [#7623](https://github.com/agentic-os-org/ANOLISA/issues/7623) | #7621、#7622 |
| AW 公共增量审计导出 | [#7624](https://github.com/agentic-os-org/ANOLISA/issues/7624) | #7622、#7623 |
| AgentSight 幂等审计消费 | [#7625](https://github.com/agentic-os-org/ANOLISA/issues/7625) | #7623、#7624 |

语义评估、Jev-like 模型、评分和动作路由放在二期。All-in-one profile 与生命周期
在能力及审计合同稳定后单独推进。本次不引入 `aw.evidence/v1`、`aw.audit/v1`、
`off`/`auto`/`required` 策略准入，也不增加 YAML 或服务 RPC 字段。

## 查询边界

组合层提供来源地址、HTTPS 凭据文件和已关联的来源会话 ID，连接器本身不建立
关联。HTTP 与 HTTPS 按相同顺序发送 GET 请求，并执行相同的响应校验。

1. `/api/auth/status` 必须报告布尔值 `auth_enabled: true`。这只是来源报告的配置，
   不能证明对端检查了 token。
2. `/api/trajectories/{session_id}` 返回指定 `ATIF-v1.7` 文档。会话 ID 编码为单个
   URL 路径段，其中的斜杠、问号不能改变查询路径。根会话 ID 必须匹配，Agent
   名称非空，步骤 ID 连续，根文档与内嵌子 Agent 的步骤总量受限。

兼容性按上述 API 响应合同及错误语义判断，核对基线为 AgentSight 0.14.0，不推导
产品版本白名单或 SemVer 范围。两种传输都不调用仅限本机的 `/health`。公共查询
接口没有产品版本字段，因此 `source.version` 始终为 null，也不声称 AgentSight
已经提供独立的 API 版本协商。认证字段缺失或不合法、ATIF 版本不支持时明确失败；
轨迹 404 保持记录或存储可用性未知。额外响应字段不能证明兼容性、身份或覆盖。

来源内容保留为 JSON。这是诊断信封，不是 canonical AW 执行元数据，也不是完整的
ATIF 语义校验器。内嵌子 Agent 内容仍属于来源报告。

可复用连接器位于服务组合边界。`aw-core` 不增加 HTTP、探针或数据库依赖。
HTTP 采用仓库已有的 reqwest 系列，使用 blocking 传输与 rustls。共享超时覆盖请求和
响应体传输，内容解析由字节、深度与步骤上限约束。不自建 HTTP 解析器，也不新增跨
组件部署客户端。AgentSecCore 客户端还负责强制执行写操作，职责和合同不同，AW
不依赖它。

## 认证与限制

远程地址要求 HTTPS，并校验证书和主机名。明文 HTTP 只接受字面量 loopback 地址。
拒绝 userinfo、查询参数、fragment、非根路径和端口零。

loopback HTTP 不读取 `token_file`，不发送 Authorization header，也不要求凭据文件。
`source.authentication: loopback_unauthenticated` 如实记录此模式。AgentSight 当前
对这类读取绕过 token 校验，其他本地进程也可能占用端口，因此不能把监听者当作
经过认证的来源，返回内容仍是未受信任的观测数据。

HTTPS 从显式指定的普通文件读取凭据，文件必须由当前有效用户持有，且不允许
group/other 访问；拒绝符号链接和特殊文件。文件最多 4096 字节，允许一个尾部换行。
每个 HTTPS 请求携带标记为敏感的 Bearer header，`bearer_supplied` 只表示已发送，
不证明服务端实施了认证（代理可能通过 loopback 访问 AgentSight）。凭据不进入命令
参数或 URL。Dashboard token 权限较粗，连接器只限制自己的读取范围，不宣称来源
已经签发专用只读角色。

关闭重定向、环境代理、自动重试和连接池。TLS 失败及 HTTP 401/403 都不能触发明文
或匿名重试。传输认证也不能证明 AW 实例身份。

所有请求共享原始截止时间，默认 5000 ms，允许 1..60000 ms；响应体合计共享字节预算，
默认 1048576，允许 1..4194304。根轨迹与子 Agent 文档合计最多 1000 个步骤。
超限时整次查询失败，不静默裁剪。调用方不接管 AgentSight worker。
Hook 路径不执行来源就绪、探针启用或内核/BTF 检查。

## 连接器结果与失败语义

`aw.agentsight.query/v1alpha1` 独立于证据和审计合同，包含 `source`（地址、观察到
的可空产品版本、`authentication`）、`session_id`、`association`、`coverage` 和带标签的
`status` 结果。

| 结果 | 库返回值 |
| --- | --- |
| 合法的指定文档 | `status: available`、`step_count`、`trajectory` |
| 合法空文档 | 同样为 available，步骤为零；覆盖仍为 unknown |
| 详情 API 返回 404 | `status: unknown`、`reason_code: record_or_store_unavailable` |
| 配置/认证错误、重定向、超时、传输、Schema 或上限失败 | 返回脱敏的类型化错误 |

所有返回结果都带 `association: unassociated` 与 `coverage: unknown`。调用方提供的
会话名不是可信 AW 绑定。无匹配或空轨迹不能证明没有活动、工具已执行、完整覆盖或
原生采用。Dashboard capability 名称不是就绪事实。HTTP 401/403 等失败不能变成
空的成功证据。诊断中不包含来源错误正文和凭据。

库将请求内容返回调用方。连接器不把来源内容或查询结果写入 Journal
或缓存，也不创建 daemon/socket。可选查询失败不影响进行中的 AW 执行及既有关键
本地审计边界。

## 共存与验证

[#7411](https://github.com/agentic-os-org/ANOLISA/pull/7411) 负责 PostTool 压缩、
原生 Adapter 顺序、结果投影、Provider 效果和打包。本次不修改这些执行路径。
共有的模块声明、依赖和文档增量，需要对照该 PR 最新 head 检查。后续身份与审计
实现应使用其最终的单次阶段身份，保留本地审计仅存元数据的边界，不独立改动调度。

真实 HTTP fixture 验证请求范围、凭据、空/未知/错误结果、版本和身份拒绝、共享
超时及字节/步骤上限，均通过库接口验收。已有 AW gate 验证无 AgentSight/eBPF 时的
运行。这些检查不证明真实部署、TLS 终结配置、可信原生关联或 eBPF 覆盖已经验收。
回滚时移除可选连接器，不涉及已安装服务、Schema 迁移或来源数据撤销。
