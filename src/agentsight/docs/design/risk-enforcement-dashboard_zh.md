# 风险拦截 Dashboard 设计

[English](risk-enforcement-dashboard.md)

## 状态

设计已确认，可以进入实现。本方案为 AgentSight 新增独立的顶层“风险拦截”入口，
使用 AgentSight enforcement coordinator 和由 ActPlane 驱动的
`agentsight-enforcer`，不依赖 AgentSecCore。

首版能力严格限定为：阻止选定 Agent 进程树打开一个已经存在的敏感文件。taint
传递、任意 ActPlane DSL、`unless`/`since` 时序规则、网络拦截、审批流和事件负责人
不属于首版范围。

## 用户目标

运维人员可以在 Dashboard 完成一次完整拦截闭环：

1. 确认特权 enforcer 已就绪；
2. 将文件打开拦截策略绑定到存活的 Agent PID；
3. 看到 ActPlane 将 binding 确认为 `enforced`；
4. 查看文件打开被拒绝后产生的标准化违规事件；
5. 解除 binding 并恢复访问。

enforcer 不可用时页面仍然可读。历史 binding 状态和违规事件继续展示，变更类操作
被禁用，同时展示可执行的就绪错误原因。

## 导航与布局

新增顶层路由 `/enforcement`，菜单名称为“风险拦截”，与 `/security` 并列但相互独立。
现有 `/security` 继续表示由 AgentSecCore 提供的安全可观测能力。

页面复用现有 Dashboard 视觉体系，采用单屏运维控制台布局：

- 顶部卡片展示 enforcer 就绪状态、backend 名称、活跃 binding 数和已拦截事件数；
- 活跃 binding 表展示状态、Agent、PID、文件路径、策略版本和受保护的解除操作；
- 新建文件策略表单只包含产品级字段；
- 违规事件表按时间倒序展示时间、Agent、PID、操作、目标、effect、结果和 binding。

创建表单只接收 `agent_id`、可选 `session_id`、`root_pid` 和绝对 `path`。binding UUID、
Linux 进程 start time、策略 revision 和 ActPlane DSL 均不暴露给用户。

## API 边界

页面并行读取现有接口：

```text
GET /api/enforcement/health
GET /api/enforcement/bindings
GET /api/enforcement/violations?limit=100
DELETE /api/enforcement/bindings/{binding_id}
```

为表单新增一个产品级接口：

```http
POST /api/enforcement/file-bindings
Content-Type: application/json

{
  "agent_id": "qoder",
  "session_id": "optional-session",
  "root_pid": 45231,
  "path": "/root/.ssh/id_rsa"
}
```

服务端将该请求转换成现有的版本化 `ApplyPolicy` 协议。原始
`POST /api/enforcement/bindings` 继续供可信集成方使用，但 Dashboard 不直接调用。

## 服务端策略构造

服务端在调用 coordinator 前执行：

1. 去除 Agent identity 首尾空白并校验非空；
2. 要求目标是已经存在的绝对普通文件，并解析 canonical path；
3. 读取 `/proc/<pid>/stat`，拒绝 PID 0/1 和 AgentSight 服务进程，使用字段 22 作为
   process start time；
4. 生成 binding UUID；
5. 根据 binding 生成稳定 policy ID，并使用产品模板 revision
   `agentsight-file-open-v1`；
6. 将 canonical path 安全转义为 ActPlane quoted string，只生成一条
   `block open file` 规则，不附加 taint 或时序条件；
7. 调用 `EnforcementCoordinator::apply`，仅在特权 backend 确认后返回。

策略模板由服务端唯一维护，避免 frontend 与 ActPlane adapter 分别演进。包含 NUL、
目标不是普通文件、进程身份不可读或 quoted-string 无法安全编码时，必须在持久化和
attach 之前拒绝请求。

## 页面数据流

进入页面和手动刷新时，并行加载 health、bindings 和 violations。单个请求失败不会
清空其他面板已成功获得的数据。创建或解除成功后统一刷新三类资源。请求进行中禁用
提交按钮；解除操作必须二次确认，并且只在 enforcer ready 时允许执行。

处于 `pending`、`failed`、`degraded` 或 `detaching` 的 binding 仍展示其 backend
message。已 detached 的 binding 作为审计状态保留，但不计入活跃数。违规事件是不可变
事实，首版不支持编辑或忽略。

## 错误语义

REST 边界保留可操作的错误分类：

| 状态码 | 含义 | 页面行为 |
|---|---|---|
| `400` | PID、路径或请求字段无效 | 保留表单并展示字段原因 |
| `404` | 进程、文件或 binding 已不存在 | 展示原因后刷新状态 |
| `409` | binding 状态冲突 | 保留当前状态，提示刷新或先解除 |
| `422` | ActPlane 编译或 attach 被拒绝 | 展示 backend 原因，不自动重试 |
| `503` | enforcer 或 BPF-LSM 不可用 | readiness 标记 degraded，并禁用变更操作 |

认证继续复用现有 Dashboard session cookie。浏览器不会直接访问特权 UDS。

## 测试与验收

实现遵循 red-green-refactor。

Backend 测试覆盖请求校验、`/proc` 身份读取、canonical path、ActPlane 字符串转义、
策略模板输出、`ApplyPolicy` 转换和 HTTP 状态映射。Frontend 测试覆盖导航入口、独立
不可用状态、刷新成功、创建成功与失败、解除确认和标准化违规事件展示。

必要检查包括 Dashboard 单测、TypeScript typecheck、嵌入式 frontend 构建、
AgentSight Rust format、Clippy 和测试。最终在启用 BPF LSM 的真实 Linux 主机验收：

1. 部署当前分支并启动 AgentSight 与 `agentsight-enforcer`；
2. 为存活测试进程和已存在敏感文件创建 binding；
3. 确认页面显示 `enforced`；
4. 确认测试进程打开文件时收到 `EPERM`；
5. 确认页面出现包含正确 binding 和路径的违规事件；
6. 从页面解除 binding，并确认进程恢复文件访问。

部署前备份已安装 binary 和 unit 文件，保留现有 AgentSight 数据库，并在重启后确认
Dashboard 继续通过 7396 端口响应。
