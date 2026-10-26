# 接入 AgentSight

[English](../../en/aw/observability.md)

AW 将 Agent 运行与 AgentSight 记录关联，便于查看活动和观测缺口。
当前自动观测支持 Qoder、OpenClaw、QwenPaw 和 Hermes。
AgentSight 独立安装和运行，接入 AW 不会自动启动采集器。

## 配置来源

先按 [AgentSight 部署指南](../agentsight/deployment.md)准备查询服务。
退出 Agent 并停止旧 AW 服务，在配置的 `spec.agents.<target>` 下增加以下内容。

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

将端口改为实际服务端口。本机 loopback HTTP 不需要 token。
远程服务必须使用 HTTPS，并增加 `token_file` 指向当前用户拥有、权限为 `0600`
的普通文件。token 不要直接写入命令行；endpoint 使用服务根地址，不添加 `/api`。

## 运行与查看

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

在另一个终端查看同一配置。

```bash
aw status --config "$HOME/aw/aw.yaml"
```

启动信息会显示来源查询状态和审计 key。Agent 发生工具调用后，检查 status 中的
原生关联、来源结果和缺口。查询 `ready` 说明接口可访问；采集器是否取得目标数据、
eBPF 是否可用，还需检查 AgentSight。刚完成的调用可能尚未被采集。

`auto` 模式下，来源不可用会显示原因，Agent 执行和 AW 本地审计继续。
当前不支持 `required` 模式。

## 停用

退出 Agent 并停止 AW，将 `evidence.mode` 改成 `off` 或移除该配置块，再重新启动。
这不会停止独立的 AgentSight 服务。

身份关联、查询限制和嵌入接口见[开发参考](../../../developer-guide/zh/aw/agentsight.md)。
