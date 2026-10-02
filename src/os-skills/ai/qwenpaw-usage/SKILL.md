---
name: qwenpaw-usage
description: "QwenPaw 命令行使用技巧：涵盖心跳（Heartbeat）配置、定时任务管理、Docker 打包部署、工作区导出迁移等操作。当用户要求添加心跳任务、配置定时自检、打包 QwenPaw 实例、部署到其他服务器、生成 Dockerfile 或 docker-compose 文件时使用本 skill。"
version: 1.0.0
tags: ["qwenpaw"]
---
# QwenPaw 命令行使用技巧

本 Skill 覆盖两类高频场景：**心跳（Heartbeat）任务配置** 和 **打包部署 QwenPaw 实例到其他服务器**。

---

## 场景一：添加 / 修改心跳（Heartbeat）任务

### 什么是心跳

心跳是 QwenPaw 的定时自检机制：按固定间隔读取 `HEARTBEAT.md` 的内容作为用户消息发给 QwenPaw，QwenPaw 执行后可选择将回复投递到上次对话的频道。适合做「定期自检、每日摘要、定时提醒」。

### 操作步骤

**1. 编写 HEARTBEAT.md**

文件位于工作目录下，默认路径：`~/.qwenpaw/HEARTBEAT.md`。
可通过环境变量 `QWENPAW_HEARTBEAT_FILE` 更改文件名。

直接用文本编辑器或 `cat` / `echo` 写入即可，内容是每次心跳要问 QwenPaw 的问题：

```markdown
# Heartbeat checklist

- 扫描收件箱紧急邮件
- 查看未来 2h 的日历
- 检查待办是否卡住
- 若安静超过 8h，轻量 check-in
```

> 文件为空则跳过心跳，不会触发任何操作。

**2. 配置心跳参数**

心跳参数有两层配置：

- **全局默认**：`~/.qwenpaw/config.json` → `agents.defaults.heartbeat`（对所有智能体生效）
- **智能体独立**：`~/.qwenpaw/workspaces/{agent_id}/agent.json` → `heartbeat`（覆盖全局默认）

可用字段：

| 字段                  | 类型        | 默认值      | 说明                                                        |
| --------------------- | ----------- | ----------- | ----------------------------------------------------------- |
| `every`             | string      | `"30m"`   | 间隔，支持 `Nh`、`Nm`、`Ns` 组合（如 `"1h30m"`）    |
| `target`            | string      | `"main"`  | `"main"` 仅执行不投递；`"last"` 发到上次对话的频道/用户 |
| `activeHours`       | object/null | `null`    | 可选活跃时段限制                                            |
| `activeHours.start` | string      | `"08:00"` | 开始时间（HH:MM）                                           |
| `activeHours.end`   | string      | `"22:00"` | 结束时间（HH:MM）                                           |

配置示例（每 30 分钟自检，不发到频道，写在 config.json 中）：

```json
"agents": {
  "defaults": {
    "heartbeat": {
      "every": "30m",
      "target": "main"
    }
  }
}
```

配置示例（每 1 小时，发到上次频道，限 08:00-22:00，写在 agent.json 中）：

```json
"heartbeat": {
  "every": "1h",
  "target": "last",
  "activeHours": { "start": "08:00", "end": "22:00" }
}
```

**3. 生效方式**

保存文件后，若服务正在运行会自动加载新配置。也可通过以下命令手动重载：

```bash
qwenpaw daemon reload-config
```

> 注意：频道和 MCP 配置的变更需要在对话中执行 `/daemon restart` 或重启进程后才能生效。

### 心跳 vs 定时任务

|      | 心跳                                       | 定时任务 (cron)              |
| ---- | ------------------------------------------ | ---------------------------- |
| 数量 | 每个智能体只有一份 HEARTBEAT.md            | 可创建多个                   |
| 间隔 | 一个全局间隔                               | 每个任务独立 cron 表达式     |
| 投递 | 仅 `main`（不发）或 `last`（上次频道） | 每个任务独立指定频道和用户   |
| 适用 | 固定的一套自检/摘要                        | 多条不同时间、不同内容的任务 |

> 如果用户需要的是「每天 9 点发早安到钉钉」「每 2 小时检查待办发到飞书」这类多条独立任务，应引导使用 `qwenpaw cron create`（参见下方定时任务部分），而非心跳。

### 完整操作示例

用户说：「帮我添加一个 heartbeat 任务，每小时检查一下有没有新邮件，如果有就通知我」

执行步骤：

```bash
# 1. 写入心跳内容
cat > ~/.qwenpaw/HEARTBEAT.md << 'EOF'
# 心跳任务

- 检查收件箱是否有新邮件
- 如果有新的未读邮件，列出发件人和主题
- 如果有紧急邮件，标注提醒
EOF

# 2. 编辑 agent.json 中的 heartbeat 配置
# 将 heartbeat 设为：
# {
#   "every": "1h",
#   "target": "last",
#   "activeHours": { "start": "08:00", "end": "22:00" }
# }

# 3. 重载配置
qwenpaw daemon reload-config
```

---

## 补充：定时任务（Cron）快速参考

当用户需要多条定时任务时，使用 `qwenpaw cron` 命令（需服务运行中）：

```bash
# 创建 agent 类型任务（向 QwenPaw 提问并发结果到频道）
qwenpaw cron create \
  --type agent \
  --name "检查邮件" \
  --cron "0 */1 * * *" \
  --channel dingtalk \
  --target-user "USER_ID" \
  --target-session "SESSION_ID" \
  --text "检查收件箱有没有新邮件，如果有列出来"

# 创建 text 类型任务（定时发固定文案）
qwenpaw cron create \
  --type text \
  --name "每日早安" \
  --cron "0 9 * * *" \
  --channel dingtalk \
  --target-user "USER_ID" \
  --target-session "SESSION_ID" \
  --text "早上好！新的一天开始了！"

# 常用管理命令
qwenpaw cron list                  # 列出所有任务
qwenpaw cron get <job_id>          # 查看任务配置
qwenpaw cron state <job_id>        # 查看运行状态
qwenpaw cron pause <job_id>        # 暂停任务
qwenpaw cron resume <job_id>       # 恢复任务
qwenpaw cron run <job_id>          # 立刻执行一次
qwenpaw cron delete <job_id>       # 删除任务

# 从 JSON 文件创建（复杂配置）
qwenpaw cron create -f job_spec.json
```

可选参数：`--timezone`（默认用户时区）、`--enabled/--no-enabled`、`--mode`（`stream`/`final`）、`--base-url`、`--agent-id`。

Cron 表达式速查（五段式：分 时 日 月 周）：

```
0 9 * * *      每天 09:00
0 */2 * * *    每 2 小时
30 8 * * 1-5   工作日 08:30
*/15 * * * *   每 15 分钟
0 0 * * 0      每周日零点
```

---

## 场景二：打包 QwenPaw 实例部署到其他服务器

### 方案概览

| 方案                              | 适用场景                               | 复杂度 |
| --------------------------------- | -------------------------------------- | ------ |
| **工作区导出 + 官方镜像**   | 已有配置和记忆需迁移到新服务器（推荐） | 低     |
| **docker-compose 快速部署** | 新服务器全新部署                       | 低     |
| **自定义 Dockerfile**       | 需要定制镜像（加自定义依赖/技能）      | 中     |

### 方案 A：工作区导出 + 官方 Docker 镜像（推荐）

最简单的方式：导出当前工作区，在新服务器上用官方镜像挂载。

**步骤 1：导出当前工作区**

通过控制台（智能体 -> 工作区 -> 下载）可将整个工作区下载为 `.zip` 文件。

或手动打包：

```bash
# 打包工作目录（包含配置、对话、记忆、技能等）
cd ~/.qwenpaw
tar czf qwenpaw-workspace.tar.gz \
  config.json \
  workspaces/
```

**步骤 2：在新服务器创建 docker-compose.yml**

```yaml
version: '3.8'

volumes:
  qwenpaw-data:
    name: qwenpaw-data
  qwenpaw-secrets:
    name: qwenpaw-secrets

services:
  qwenpaw:
    image: agentscope/qwenpaw:latest
    container_name: qwenpaw
    restart: always
    ports:
      - "127.0.0.1:8088:8088"
    volumes:
      - qwenpaw-data:/app/working
      - qwenpaw-secrets:/app/working.secret
    environment:
      # 按需传入 API Key
      - DASHSCOPE_API_KEY=${DASHSCOPE_API_KEY}
      # 如需连接宿主机的 Ollama/LM Studio，取消下方注释
      # - OLLAMA_BASE_URL=http://host.docker.internal:11434
    # 如需连接宿主机服务，取消下方注释
    # extra_hosts:
    #   - "host.docker.internal:host-gateway"
```

> 国内用户可替换为 ACR 镜像：`agentscope-registry.ap-southeast-1.cr.aliyuncs.com/agentscope/qwenpaw:latest`

**步骤 3：恢复工作区**

```bash
# 启动容器（会自动初始化默认配置）
docker compose up -d

# 找到数据卷挂载点
MOUNT_PATH=$(docker volume inspect qwenpaw-data --format '{{ .Mountpoint }}')

# 将备份解压到数据卷
sudo tar xzf qwenpaw-workspace.tar.gz -C "$MOUNT_PATH"

# 重启使配置生效
docker compose restart
```

或在控制台中使用「上传工作区」功能恢复 `.zip` 文件（最大 100 MB）。

**步骤 4：验证服务**

```bash
curl -s http://localhost:8088/api/agent/status | head -20
```

### 方案 B：docker-compose 全新部署

适合不需要迁移旧数据的场景。

```bash
# 1. 创建 docker-compose.yml（同方案 A 的 step 2）

# 2. 通过环境变量传入 API Key
echo "DASHSCOPE_API_KEY=your-key-here" > .env

# 3. 启动
docker compose up -d

# 4. 访问控制台完成初始化
# 浏览器打开 http://<server_ip>:8088
```

如需对外暴露，将 `127.0.0.1:8088:8088` 改为 `0.0.0.0:8088:8088`（注意安全风险，建议配合反向代理和认证）。

### 方案 C：自定义 Dockerfile

当需要在镜像中预装自定义技能、额外 Python 包或特殊系统依赖时使用。

参考 Dockerfile：

```dockerfile
# 基于官方镜像
FROM agentscope/qwenpaw:latest

# 预装额外 Python 包
RUN pip install --no-cache-dir some-package another-package

# 复制自定义技能
COPY my_skills/ /app/working/customized_skills/

# 复制自定义配置（可选）
# COPY config.json /app/working/config.json

# 如需更改默认端口
# ENV QWENPAW_PORT=3000
# EXPOSE 3000
```

构建并运行：

```bash
docker build -t my-qwenpaw .
docker run -d \
  --name qwenpaw \
  --restart always \
  -p 127.0.0.1:8088:8088 \
  -v qwenpaw-data:/app/working \
  -v qwenpaw-secrets:/app/working.secret \
  -e DASHSCOPE_API_KEY="your-key" \
  my-qwenpaw
```

### Docker 部署关键参数

| 环境变量                   | 默认值                                          | 说明               |
| -------------------------- | ----------------------------------------------- | ------------------ |
| `QWENPAW_WORKING_DIR`      | `/app/working`                                | 工作目录（容器内） |
| `QWENPAW_SECRET_DIR`       | `/app/working.secret`                         | 敏感数据目录       |
| `QWENPAW_PORT`             | `8088`                                        | 服务端口           |
| `QWENPAW_ENABLED_CHANNELS` | `discord,telegram,dingtalk,feishu,qq,console` | 启用的频道         |

### Docker 中连接本机模型服务

容器内 `localhost` 指向容器自身，需使用 `host.docker.internal`：

```bash
docker run ... \
  --add-host=host.docker.internal:host-gateway \
  -e OLLAMA_BASE_URL=http://host.docker.internal:11434 \
  agentscope/qwenpaw:latest
```

---

## 使用建议

- 用户描述模糊时，先询问确认：是要心跳（单一自检循环）还是定时任务（多条独立任务）？是要迁移现有实例还是全新部署？
- 涉及 API Key 等敏感信息时，引导用户使用环境变量或 `qwenpaw env set` 命令，不要在配置文件中明文写入。
- 多智能体场景下，几乎所有命令支持 `--agent-id` 参数，默认为 `default`。
- 配置修改后可用 `qwenpaw daemon reload-config` 热加载，无需重启服务。
