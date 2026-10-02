> **爬取时间**: 2026-06-17 16:29:19
> **原文链接**: https://help.aliyun.com/zh/alinux/extensibility-for-skill-and-mcp
> **文档更新**: 2026-05-28T10:06:31+08:00

---

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）可以通过 Skill 和 MCP 两种机制扩展能力边界，支持一句话安装 Skill 包或接入第三方 MCP 服务。本文介绍了 Skill 的安装与管理方式、MCP 服务器的三种连接方式（stdio、HTTP Streamable、SSE）及其配置方法。

## 一句话安装 Skill

### 自然语言安装

最简单的安装方式——直接告诉系统 Shell cosh：

```
# 提供zip包下载链接或者本地路径
> 帮我安装这个 Skill https://example.com/skills/deploy-agent.zip
```

将自动下载、解压并加载 Skill，安装完成后即可在对话中使用。

### 手动安装

如果需要手动安装，将 Skill 的 zip 包解压到 `~/.copilot/skills/` 或 `./.copilot/skills/` 目录即可。

### /skills 命令管理

| 操作  | 命令  |
| --- | --- |
| 查看所有已安装的 Skill | `/skills` |
| 运行指定 Skill | `/skills <name>` |
| 在对话中自动调用 | 直接用自然语言描述意图，系统 Shell cosh 自动匹配 Skill |

在 Alibaba Cloud Linux 4 Agentic Edition（ANOLISA） 中使用自然语言描述意图时，Agent 会自动匹配并调用对应的 Skill，无需手动指定：

```
> 帮我部署一个 OpenClaw Agent
> 在线扩容当前 ECS 实例的磁盘
> 诊断一下系统为什么卡住了
```

## 一句话接入 MCP 服务

### 什么是 MCP

MCP（Model Context Protocol）是一种开放协议，用于扩展 AI 模型可调用的工具和数据源。Alibaba Cloud Linux 4 Agentic Edition 原生支持 MCP，允许用户接入各类第三方服务。

在 Alibaba Cloud Linux 4 Agentic Edition 中，MCP 服务器扮演「工具提供者」的角色——每个服务器可以暴露一组工具，系统 Shell cosh 在对话中自动识别并调用合适的工具。典型的 MCP 服务器包括：数据库查询工具（MySQL、PostgreSQL）、云服务管理工具（ECS、OSS）、监控告警工具（Prometheus、Grafana）、文档检索工具（Elasticsearch）等。

### 自然语言配置

最简单的接入方式——直接用自然语言告诉系统 Shell cosh：

```
> 帮我配置 MCP，以下是配置信息：
{
  "mcpServers": {
    "mcp-name": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/data"]
    }
  }
}
```

cosh 自动将配置写入对应的配置文件。

### 手动配置

MCP 服务器在配置文件的 `mcpServers` 字段中定义，支持用户级和项目级配置。

**配置文件位置：**

| 类型  | 路径  | 说明  |
| --- | --- | --- |
| 用户级 | `~/.copilot/settings.json` | 对当前用户所有项目生效 |
| 项目级 | `.copilot/settings.json`（项目根目录） | 仅对当前项目生效，可随代码仓库共享 |

**stdio 连接方式**——通过标准输入/输出与本地 MCP 服务器通信。必填：`command`，可选：`args`、`env`、`cwd`、`timeout`。

```
{
  "server-name": {
    "mcp-name": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/data"]
    }
  }
}
```

**HTTP Streamable 连接方式**——通过 HTTP 连接远程 MCP 服务器，推荐用于新的远程服务。必填：`httpUrl`，可选：`headers`、`timeout`。

```
{
  "mcpServers": {
    "mcp-name": {
      "httpUrl": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer $API_TOKEN"
      },
      "timeout": 60000
    }
  }
}
```

**SSE 连接方式**——通过 Server-Sent Events 连接远程 MCP 服务器（旧协议，建议新服务使用 HTTP Streamable）。必填：`url`，可选：`headers`、`timeout`。

```
{
  "mcpServers": {
    "mcp-name": {
      "url": "https://mcp.example.com/sse",
      "headers": {
        "Authorization": "Bearer $API_TOKEN"
      },
      "timeout": 60000
    }
  }
}
```

### 管理 MCP 服务器

| 操作  | 命令  |
| --- | --- |
| 列出所有 MCP 服务器及其状态 | `/mcp` |
| 查看每个工具的详细描述 | `/mcp desc` |
| 交互式启用/禁用特定工具 | `Ctrl+T` |

MCP 工具在对话中自动可用，只需用自然语言描述需求：

```
> 查询数据库中最近 24 小时的订单数据
> 帮我检查一下 OSS 存储桶的使用量
> 获取 Prometheus 上 CPU 使用率的趋势数据
```

cosh 会自动匹配并调用对应的 MCP 工具完成任务。
