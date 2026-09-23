# AgentSight 数据与存储

[English](../../../en/agent-observability/agentsight/data-and-storage.md)

AgentSight 采集到的一切都以 SQLite 数据库形式留在本机。Dashboard、CLI 和 HTTP API 只是同一批文件的三种
视图。

## 数据放在哪里

所有数据库位于 `/var/log/sysak/.agentsight/`，以私有 umask 创建，仅 root 可读。

| 文件 | 内容 |
|---|---|
| `genai_events.db` | 主库：保存 LLM 调用及其 Agent 进程的定时 CPU/RSS 采样，含耗时、会话与对话 ID |
| `agentsight.db` | 审计记录（LLM 调用与进程动作）以及 Token 消费聚合 |
| `interruption_events.db` | 检测到的中断，含类型、严重级别与证据 |
| `optimization.db` | Dashboard 优化分析的结果 |
| `trajectories.db` | ATIF v1.7 轨迹，仅在开启 `features.trajectory_collection` 时存在 |
| `.agentsight-private/security.db` | 安全事件、案例、证据与处置状态 |
| `.agentsight-private/enforcement.db` | 拦截策略、违规记录与状态流转 |
| `.agentsight-private/reuse.db` | 轨迹复用标签、人工决定、LLM verdict 与标签审计事件 |
| `.agentsight-private/causal.db` | 持久化的因果归因 case |
| `.dashboard_token` | Dashboard 访问令牌（64 位十六进制，仅 root 可读） |
| `optimization_config.json` | 在 Dashboard 设置页填写的 LLM 配置（API Key 存于此） |
| `*.db-wal`、`*.db-shm` | SQLite 预写日志与共享内存；属正常文件，干净退出时会做 checkpoint |

`serve`、`dashboard`、`skill-metrics` 支持用 `--db` 指向别的数据库文件，这也是浏览副本或归档的方式。
tracer 自身始终写入默认目录。

> `serve --db <path>` 会让所有兄弟库都从 `--db` 所在目录解析——GenAI 事件、中断库、轨迹库以及
> health checker 都跟着它走。私有的安全、拦截、复用与因果库从其 `.agentsight-private/` 子目录解析。
> 因此归档副本是隔离展示的，不会混入当前主机的数据。请把兄弟 `.db` 文件以及存在时的
> `.agentsight-private/` 目录放在你传入的那个文件的同一目录下。裸相对路径 `--db name.db` 使用当前目录。

> 这些文件包含完整的提示词与模型回答，请按敏感数据对待：保持安装时的目录权限，往外拷贝时务必谨慎。

## 保留与容量上限

| 存储 | 默认策略 | 配置项 |
|---|---|---|
| `agentsight.db` | 30 天、500 MiB、每 1,000 次写入检查 | `storage.primary` |
| `genai_events.db` | 30 天、200 MiB、每次写入检查；包含评估结果 | `storage.genai` |
| `interruption_events.db` | 30 天、100 MiB、每 60 秒检查 | `storage.interruptions` |
| `trajectories.db` | 30 天、500 MiB、每 300 秒检查 | `storage.trajectories` |
| `optimization.db` | 30 天、200 MiB、每 300 秒检查 | `storage.optimization` |
| `.agentsight-private/security.db` | 30 天、200 MiB、每小时检查，并保护活动案例图 | `storage.security_audit` |
| `.agentsight-private/enforcement.db` | 保留最新 100,000 条 violation | 固定行数上限 |

保留时间、容量或检查间隔设为 `0` 时关闭对应规则。上限按逻辑容量计（物理文件大小减去空闲页），清理按最旧
且允许淘汰的记录优先，直到逻辑容量收敛到阈值内。清理后
物理文件不会自动缩小：释放的页进入空闲页表并被后续写入复用，文件大小稳定在
历史峰值。如需向文件系统归还磁盘空间，在低峰期手动执行
`sudo sqlite3 /var/log/sysak/.agentsight/<db> 'VACUUM;'`（VACUUM 会重建整个文件，
建议先停止服务，避免 cgroup 内存限制下触发 OOM）。

> 容器部署请注意：这些保留语义只在数据目录持久化时才有意义。不挂卷时容器每次重启都会清空全部数据，
> 详见 [容器与 Sidecar](deployment.md#容器与-sidecar) 的持久化一节。

修改限制时，编辑 `/etc/agentsight/config.json` 的 `storage` 配置节，再 reload 服务。Settings 页面会展示
每个数据库当前生效的策略、物理占用和逻辑占用。

通过 API 查看当前占用：

```bash
TOKEN=$(sudo cat /var/log/sysak/.agentsight/.dashboard_token)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7396/api/storage/status \
  | python3 -m json.tool
```

也可以直接检查目录：

```bash
sudo du -sh /var/log/sysak/.agentsight
sudo ls -la /var/log/sysak/.agentsight
```

## 清空数据

```bash
sudo systemctl stop agentsight.service
sudo rm -rf /var/log/sysak/.agentsight
sudo systemctl start agentsight.service
```

删掉目录也会删掉 Dashboard 令牌，下次启动会重新生成。如果想保留历史，先把目录拷到别处，之后用
`agentsight serve --db /path/to/genai_events.db` 浏览。

## HTTP API

服务自己会给出路由清单，不用猜：

```bash
curl -s http://127.0.0.1:7396/api/docs | python3 -m json.tool
```

非本机请求需要令牌：

```bash
TOKEN=$(sudo cat /var/log/sysak/.agentsight/.dashboard_token)
curl -s -H "Authorization: Bearer $TOKEN" http://<host>:7396/api/sessions
```

0.11 的端点分组：

| 分组 | 示例 | 用途 |
|---|---|---|
| 服务 | `GET /health`、`GET /metrics`、`GET /api/docs` | 存活探测、Prometheus 指标、路由清单（`/health` 与 `/metrics` 仅本机可访问） |
| 认证 | `GET /api/auth/status`、`GET /api/auth/verify`、`POST /api/auth/login` | 认证状态、能力列表、令牌换 cookie |
| 会话与调用 | `GET /api/sessions`、`GET /api/sessions/{id}/traces`、`GET /api/sessions/{id}/resources`、`GET /api/traces/{id}`、`GET /api/conversations/{id}`、`POST /api/sessions/search` | 会话列表、会话内对话摘要（以 `conversation_id` 为键）与进程资源、按 response_id 的单次调用详情、语义搜索 |
| 指标 | `GET /api/timeseries`、`GET /api/metrics/latency`、`GET /api/agent-names` | Token 时序、延迟分位、Agent 过滤项 |
| 中断 | `GET /api/interruptions`、`/count`、`/stats`、`/session-counts`、`/conversation-counts`、`POST /api/interruptions/{id}/resolve` | 排查与关闭 |
| Agent 健康 | `GET /api/agent-health`、`DELETE /api/agent-health/{pid}`、`POST /api/agent-health/{pid}/restart` | 实时状态与恢复动作 |
| Token 节省 | `GET /api/token-savings`、`GET /api/token-savings/session/{id}` | Tokenless 节省量 |
| ATIF 导出 | `GET /api/export/atif/session/{id}`（还有 `trace`、`conversation`） | 轨迹导出 |
| 轨迹 | `GET /api/trajectories`、`/filters`、`/steps`、`/{session_id}` | 已采集轨迹。列表支持可选的 `label`、`exclude_label`、`human_backed` 过滤；`label` 是逗号分隔的有效标签，例如 `good,bad` |
| 复用标签 | `POST /api/reuse/triage`、`GET /api/reuse/sessions`、`POST /api/reuse/sessions/{session_id}/label`、`POST /api/reuse/sessions/labels:batch-confirm`、`GET /api/reuse/label-stats`、`POST /api/reuse/judge` | 规则分诊与人工标签决定。judge 需要 `features.reuse_llm_judge=true` 与已配置的 LLM 凭据，并会产生付费模型调用 |
| 偏好 | `GET /api/preferences`、`/export`、`/turns` | 用户偏好分析、Markdown 导出，以及供 Agent 侧推理使用的用户原始轮次 |
| 存储 | `GET /api/storage/status` | SQLite 生效策略与物理/逻辑占用，不返回文件路径 |
| Skill 指标 | `GET /api/skill-metrics`、`/downloads`、`/loads`、`/usage-ratio`、`/distribution`、`/hotness` | Skill 采纳情况 |
| 优化分析 | `POST /api/optimize/sessions/{id}/{dimension}`、`GET /api/optimize/results`、`GET` 与 `POST /api/optimize/config` | LLM 辅助分析 |
| 质量与归因 | `POST /api/grader/evaluate`、`GET /api/grader/latest`、`POST /api/causal-attribution` | 会话质量评分、根因归因 |
| 安全与审计 | `GET /api/security/*`、`GET /api/audit/*`、`POST /api/audit/cases/{id}/review` | 装了 agent-sec-core 时可用 |
| 拦截 | `GET /api/enforcement/health`、`POST /api/enforcement/bindings`、`GET /api/enforcement/violations` | 装了 enforcer 时可用；写操作始终要求令牌 |

时间范围参数是纳秒时间戳（`start_ns`、`end_ns`），与 CLI 的 `--last` 窗口对应。

```bash
# 最近一小时的会话
NOW=$(date +%s%N); AGO=$((NOW - 3600000000000))
curl -s "http://127.0.0.1:7396/api/sessions?start_ns=$AGO&end_ns=$NOW" | python3 -m json.tool | head
```

获取某个 Session 的原始 CPU/RSS 采样点和活动区间：

```bash
curl -s "http://127.0.0.1:7396/api/sessions/<SESSION_ID>/resources?max_points=2000" | python3 -m json.tool
```

每个采样点都是进程级数据，包含 Epoch 纳秒时间戳、PID、CPU 百分比和以字节计的 RSS 内存。对于多进程
Agent，Dashboard 会汇总该 Session 关联的所有 PID。这些数据表示 Session 运行时的进程环境，并非严格的
Session 资源归因：同一个共享 Agent 进程可能同时服务多个 Session。LLM 区间使用采集到的请求和响应时间；
Tool Call 区间从产生工具请求的 LLM 响应结束开始，到携带对应工具结果的下一次 LLM 请求开始为止。未被
LLM 调用或已匹配 Tool Call 覆盖的间隙会返回为 `idle`；无法匹配结果的 Tool Call 不会虚构结束时间。

## Prometheus 指标

```bash
curl -s http://127.0.0.1:7396/metrics | head
```

```
# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)
# TYPE agentsight_token_input_total counter
agentsight_token_input_total{agent="CoshNG"} 100000
agentsight_token_input_total{agent="Cosh"} 50000
```

计数器按 Agent 维度、取累计值：`agentsight_token_input_total`、`agentsight_token_output_total`、
`agentsight_token_total_total`、`agentsight_llm_requests_total`。`/metrics` 只允许本机访问，因此请用
节点本地的 Prometheus agent 抓取，或者通过本机反向代理暴露。`agentsight metrics` 在命令行输出同样内容。

## 轨迹导出（ATIF v1.7）

任意会话、对话或单次调用都能导出为自包含的 JSON 轨迹——Agent 元信息、步骤、消息、工具调用与 Token 汇总：

```bash
curl -s http://127.0.0.1:7396/api/export/atif/session/<SESSION_ID> > session.atif.json
```

Dashboard 的轨迹查看页通过**下载 JSON** 提供同一份文件，也能导入在别的机器上采集的轨迹。适合离线分析、
共享复现场景，或喂给评测流水线。

## 外部日志导出

AgentSight 可以把结构化事件写入文件，供外部日志采集器读取：

```json
{
  "runtime": { "sls_logtail_path": "/var/log/anolisa/agentsight/events.jsonl" },
  "features": { "sls_logtail": true }
}
```

该路径支持运行期修改——设为 `""` 即暂停导出。如果希望数据完全留在本机，保持默认即可。采集器侧的配置
（端点、凭证）不属于 AgentSight 的范围。

## 备份

```bash
sudo systemctl stop agentsight.service
sudo tar czf agentsight-data-$(date +%F).tar.gz -C /var/log/sysak .agentsight
sudo systemctl start agentsight.service
```

先停服务可以确保 WAL 已 checkpoint，归档内容才是一致的。

## 相关页面

- [CLI 参考](cli-reference.md)——用命令行查同一批数据
- [Dashboard 指南](dashboard.md)——这些数据库之上的界面
- [配置](configuration.md)——存储与保留相关开关
