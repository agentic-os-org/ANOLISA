# 通过 AW run 观测 Agent

[English](../../en/user-entrypoint/aw-agentsight.md)

配置一次可选 AgentSight 来源，随后使用 `aw run` 自动关联原生回调并读取有界证据。
不要求动作 Provider 或 eBPF。来源不可用时，AW 仍保留本地审计。本功能不启用语义评估。

本期自动观测与来源身份合同适用于 Qoder、OpenClaw、QwenPaw 和 Hermes。
Pi 保留已有原生策略能力；其纯观测回调及可信来源关联需另行准入 Adapter 合同。

## 前提与配置

按 [Preview 安装指南](aw-preview.md)安装 AW。client、回调与 daemon 必须来自支持
`aw-service/v1alpha3` 的同一构建。升级前先退出所属会话，用旧二进制停止旧 daemon，
保留 Journal 历史。Provider stdio 仍使用 `aw-provider/v1alpha1`。

AgentSight 查询服务由外部独立管理，并已开启认证。两种传输均校验公共 auth-status
响应和 ATIF-v1.7，不调用 `/health`，不按产品版本准入；来源产品版本未知，为 null。查询服务在线不能证明采集器或 eBPF 探针就绪，参见
[AgentSight 部署](../agent-observability/agentsight/deployment.md)。

在现有 `spec.agents.<target>` 下增加以下块。来源配置属于该 target，回调请求不能
替换它。以下字段除 `mode` 外均为必填，省略 mode 时使用 `auto`。HTTPS 还要求
`token_file`，loopback HTTP 不需要。

```yaml
evidence:
  mode: auto
  source_id: local-sight
  endpoint: http://127.0.0.1:7396
  query_timeout_ms: 1000
  query_bytes: 1048576
  max_items: 32
  max_bytes: 262144
```

远程 origin 要求 HTTPS，并校验证书与主机名。HTTP 只接受字面量 loopback 地址。
地址不带 `/api`、userinfo、query 或 fragment。Loopback HTTP 不读取 token 文件，
不发送 Authorization；即使提供 `token_file` 也会忽略，模式记录为
`loopback_unauthenticated`。

HTTPS 需增加 `token_file: /absolute/private/agentsight-token`。文件必须由当前用户
持有且无 group/other 权限，拒绝符号链接、特殊文件和超过 4096 字节的 token。
凭据正文不能放入参数。`bearer_supplied` 只表示已发送 header；两种模式都不证明
来源进程身份或来源签发了只读角色。

| 设置 | 上限 |
| --- | --- |
| `mode` | `auto` 或 `off`；能力准入实现前拒绝 `required` |
| `source_id` | 1..128 个字母/数字/点/下划线/连字符，只是标签 |
| `query_timeout_ms` | 1..60000 ms；自动启动/观测读取还受 1000 ms 上限限制 |
| `query_bytes` | 1..4194304 响应字节总量，包含兼容探测 |
| `max_items` | 1..128 条投影记录，包含原生候选 |
| `max_bytes` | 完整投影上下文 4096..2097152 字节 |

## 运行与查看

正常启动配置目标，在另一终端查看同一个服务。示例使用已有 Qoder target，其他框架
沿用各自的[原生启动参数](aw.md)。

```bash
aw run --config ./aw.yaml --agent qoder
aw status --config ./aw.yaml
```

启动输出观测审计 key 和来源查询状态。status 展示活动观测、原生关联、来源结果与
显式缺口。查询 `ready` 表示 auth-status 合同已响应，存储、采集、eBPF 和完整覆盖
仍为 `unknown`。原生
session 在可信回调到达后关联，没有回调的运行可以成功退出并保持未关联。

事件没有动作步骤时，AW 注册纯观测回调；已有动作时复用其回调。纯观测返回原生
无操作响应，后台来源读取不拖住 Hook 应答。必要的本地元数据仍在应答前完成写入。

每个实例只有一个 worker、两个排队候选和一个有界投影。每个实例最多观测 256 个
回调，超额进入 `gaps`，已有动作继续执行。查询还受原始回调截止时间限制。重复
回调不增加读取。历史按精确原生 session 和[明确的框架名称合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/evidence-context_zh.md#身份关联)关联，不推断进程身份。
并发冲突实例得到 `source_session_conflict`；首个实例完成收尾后，恢复运行可以将
同一 session 关联为历史证据。

正常退出、可处理信号和启动失败只释放所属任务与注册。最多一个在途和两个排队读取
按预算收尾，launcher 的释放期限为五秒。退出输出来源状态和审计 key，共享 AW daemon
及外部 AgentSight 继续运行。launcher 被强杀后可能留下未完成实例，自动失联回收尚未
实现。进程仍存活时，原生 Stop/继续不结束 AW 实例。覆盖限于已支持的工具回调，不包含
每个模型回合。

## 来源或 eBPF 缺失

`auto` 在凭据、来源服务、存储或兼容数据缺失时保留 AW 执行和本地审计，并显示原因。
`off` 不发送来源请求、不增加观测回调，配置动作保持原行为。禁用时删除该块或设置
`off`，再使用匹配配置重启所属 daemon。

外部采集器支持该 Agent 时，无 eBPF 也可使用用户态轨迹。查询成功不能证明 eBPF
就绪。AW 不安装组件、不提权，也不启停 AgentSight worker。完整能力要求与丢失/恢复
策略由 #7623 补齐，公共增量审计和消费分别由 #7624、#7625 接续。

默认用户态轨迹采集器在扫描轮次间等待 30 秒。GET 读取已入库历史，立即再查不能
保证新鲜数据。AW 将来源时间标记为未验证，Hook 应答不等待这些读取，也不使用
晚到证据改写已完成决定。

## 固定活动 AW 事件的证据

嵌入方仍可固定活动动作事件的原始候选、约束和选取证据。正常 `aw run` 不需要这个
高级操作。将原始句柄写入 `operation.json`。

```json
{
  "method": "freeze_context",
  "event_id": "<active event ID>",
  "instance_id": "<owning instance ID>",
  "candidate_revision": "<candidate revision from the event handle>"
}
```

```bash
aw request --socket /absolute/private/state/aw.sock --timeout-ms 5000 < operation.json
```

普通 `open_event` 的身份未核实，不能选取来源历史。成功后的重复请求复用同一快照。
关闭、超时、并发请求和过期版本会明确失败，不创建新预算。原生完成视图不能替换
原始候选。自动观测有自己的有界生命周期，不能重开事件。

结果使用 `aw.evidence-context/v1` 和 `aw.evidence/v1`，包含内容摘要、主体/版本核对
和缺口。模型回显不增加执行次数，来源进程身份及未经证明的覆盖保持未知。同用户
私有 socket 不能隔离同 UID 的恶意程序，返回正文不宣称自动脱敏。Journal 只保存
元数据，缓存正文随所属事件或观测实例释放，调用方自行保存的响应由其管理。
没有来源块时，显式冻结只返回 AW 材料，报告 `source_not_configured`，默认上限为
32 条 / 262144 字节。
