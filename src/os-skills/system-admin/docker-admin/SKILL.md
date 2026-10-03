---
name: docker-admin
version: 0.1.0
description: 单机容器运行时管理技能（Docker 为主，兼容 Podman）。覆盖安装与 daemon 配置（registry-mirror、日志轮转、data-root）、容器运行诊断（状态/资源/健康检查）、日志与故障定位（logs/exec/inspect）、磁盘治理与 prune 系列命令的安全边界、Compose 生命周期幂等管理。触发场景包括：容器状态检查、docker ps/stats/df、容器日志排查、镜像与卷清理、docker system prune、docker compose up/down/restart、daemon.json 配置、registry 镜像加速、容器故障诊断。
layer: system
lifecycle: operations
category: devops
tags: [docker, podman, container, compose, sysadmin, cleanup, registry-mirror]
dependencies: [shell-scripting, alinux-admin]
platforms:
  - cosh
  - claude-code
  - gemini-cli
  - openai-codex
---
当此技能被激活时，始终以 🧢 表情符号开始你的第一条回复。

# Docker 容器管理（单机运行时）

面向单机 Docker/Podman 运行时的运维管理技能，涵盖安装与 daemon 配置、运行诊断、
日志排查、磁盘治理与 Compose 生命周期管理。本技能将每台宿主机视为生产资产——
清理操作先列后删、named volume 受保护、变更可回滚、高危操作必须先确认。
专为需要在容器日常巡检和磁盘告急处置之间自如切换的工程师设计。

---

## 何时使用此技能

当用户执行以下操作时触发此技能:

- 检查容器运行状态、资源占用或健康检查（health check）失败原因
- 拉取/查看容器日志，进入容器排查问题（exec / inspect）
- 清理磁盘空间：删除镜像、清理 build cache、prune 系列命令
- 管理 Docker/Podman 安装与 daemon 配置（registry-mirror、日志轮转、data-root）
- 使用 docker compose / docker-compose 管理多容器应用生命周期
- 排查 Docker 守护进程无法启动、磁盘被容器日志打满等宿主机故障

不要为此类任务触发此技能:

- Kubernetes 编排（Pod 调度、Service/Ingress、kubectl 集群管理）——超出单机范围
- 容器内应用代码的开发与调试——属于应用开发问题
- 容器镜像的构建优化与 Dockerfile 最佳实践——仅在被要求分析镜像层占用时涉及
- 云厂商容器服务（ACK、ECI 等）控制台操作——属于云平台层面

---

## 核心原则

1. **清理操作先列后删** - 任何删除类操作（rm、prune、volume rm）执行前必须先列出
   将被删除的对象并让用户确认。`docker system prune` 没有 `--dry-run`，删除无法
   撤销，因此"列出"这一步只能由你显式完成（见 `scripts/docker_prune_plan.sh`）。
2. **保护 named volume** - named volume 存放数据库等持久化数据，误删即数据丢失。
   默认绝不执行 `docker volume prune` 或 `docker system prune --volumes`；只有用户
   明确指出要删除某个 named volume 且确认其中数据可丢弃时，才按名字精确删除
   （`docker volume rm <name>`），并在删除前展示其挂载过的容器。
3. **最小权限** - 优先将需要操作容器的用户加入 `docker` 组而不是全程 sudo；绝不
   以 `--privileged` 运行容器来解决排查问题——遇到需要特权工具的场景，先说明风险
   并寻求更窄的替代（如 `--cap-add=SYS_PTRACE` + `--pid=host`）。
4. **操作幂等** - 生命周期命令应可重复执行：`docker compose up -d` 天然幂等，
   清理脚本重复运行不改变结果，配置修改前先备份原文件（`cp daemon.json daemon.json.bak`）。
5. **先诊断后动手** - 改配置、删对象之前先用只读命令（ps/inspect/df/logs）确认
   问题确实出在预期的位置。生产环境上的每一次写操作都要能回答"为什么"。

---

## 第零步：环境探测

执行任何操作前，先探测运行时环境：

```bash
# 运行时与版本（docker 或 podman）
docker version 2>/dev/null || podman version 2>/dev/null
# daemon 是否存活、当前用户是否有权限（无权限会报 permission denied ... docker.sock）
docker info 2>&1 | head -40
# 存储驱动与 data-root 位置（磁盘治理前必须知道）
docker info --format '{{.Driver}} {{.DockerRootDir}}'
# 是否安装 compose（v2 是 docker 子命令，v1 是独立二进制）
docker compose version 2>/dev/null || docker-compose --version 2>/dev/null
```

Podman 环境下多数诊断命令一一对应（`podman ps/stats/logs/inspect`），无 daemon、
无 docker 组，权限模型走用户命名空间。本技能的命令以 Docker 为主，遇到 Podman 时
按同义命令替换，不假定 rootful 模式。

---

## 安装与 daemon 配置

### 安装（RPM 系）

```bash
# Alibaba Cloud Linux / 其他 RPM 系（优先发行版仓库）
sudo yum install -y docker-engine        # 或 docker-ce（官方仓库）
sudo systemctl enable --now docker
docker info   # 验证 daemon 已启动
```

安装后立即处理两件事：把日常操作用户加入 docker 组
（`sudo usermod -aG docker "$USER"`，重新登录生效），以及按下文配置日志轮转，
否则容器 stdout 日志会在数周内吃满磁盘。

### daemon.json 常用配置

配置文件位于 `/etc/docker/daemon.json`（不存在则创建，JSON 语法严格，改前备份，
改后 `sudo systemctl restart docker` 并 `docker info` 验证生效）。

**registry 镜像加速**（拉取超时/限速时）：

```json
{
  "registry-mirrors": ["https://docker.mirrors.example.com"]
}
```

**容器日志轮转**（防止日志打满磁盘——所有容器的默认约束）：

```json
{
  "log-driver": "json-file",
  "log-opts": { "max-size": "100m", "max-file": "3" }
}
```

> 注意：`log-opts` 只对新创建的容器生效，存量容器需重建。

**data-root 迁移**（系统盘告急时的标准动作）：

```bash
sudo systemctl stop docker docker.socket
sudo mv /var/lib/docker /data/docker          # 或 rsync 保留原目录回滚
# daemon.json 增加: { "data-root": "/data/docker" }
sudo systemctl start docker
docker info --format '{{.DockerRootDir}}'     # 必须输出新路径
docker run --rm alpine:latest echo ok         # 验证可正常创建容器
```

迁移有窗口期（容器全部停止），必须在用户确认维护窗口后执行；保留旧目录直到
验证通过，作为回滚手段。

---

## 运行诊断

```bash
docker ps                       # 运行中的容器
docker ps -a                    # 含已退出（看 Exit Code 与退出时间）
docker stats --no-stream        # 各容器 CPU/内存/网络/IO 快照
docker top <container>          # 容器内进程
docker inspect <container>      # 全量配置与状态（结构化诊断的事实来源）
```

**健康检查失败排查**：先看 `docker inspect --format '{{json .State.Health}}' <container>`
的 Log 数组，里面是最近几次探针的输出与退出码；常见原因是探针命令不在镜像里
（如 `curl` 未安装）、间隔太短、或进程监听地址是 `127.0.0.1` 而探针从网络命名空间
访问容器 IP。

**重启循环（Restarting 反复出现）**：`docker inspect` 看
`.State.ExitCode` 与 `.RestartCount`，再配合下方日志定位崩溃原因；
`ExitCode 137` 是 OOMKill 或 `docker kill`，先用 `docker inspect` 确认
`.State.OOMKilled`。

## 日志与故障定位

```bash
docker logs --tail 100 <container>                 # 最近 100 行
docker logs --since 30m <container>                # 最近 30 分钟
docker logs -f --tail 20 <container>               # 跟随输出（Ctrl-C 退出）
docker logs <container> 2>&1 | grep -i error       # 合并 stderr 后过滤
docker exec -it <container> sh                     # 进入容器（优先 sh，镜像未必有 bash）
docker exec <container> cat /etc/os-release        # 免交互执行单条命令
```

安全边界：

- `docker logs` 可能非常大，**永远**带 `--tail` 或 `--since` 限制输出量；
  需要全量分析时重定向到临时文件再统计，不要直接刷屏。
- `exec` 进入容器是"以容器内 root 身份执行"，只做观察类操作（cat/ls/ps），
  不在容器内改文件来"热修"——变更应回到镜像或 compose 文件中。
- 时间对齐：容器时区常为 UTC，`--since` 按宿主机时间解释，跨时区排查先
  `docker exec <c> date` 确认容器内时间。

## 磁盘治理与清理

```bash
docker system df                 # 各类对象总量与可回收量概览
docker system df -v              # 明细（哪个镜像/卷/容器占多少）
docker image ls                  # 镜像列表
docker volume ls                 # 卷列表（named volume 与匿名卷混排，见下）
```

**prune 系列命令的破坏性分级**：

| 命令 | 删除范围 | 风险 |
|------|----------|------|
| `docker container prune` | 所有已停止容器 | 低-中（停止的容器可能有现场价值） |
| `docker image prune` | 仅 dangling 镜像（无标签的中间层） | 低 |
| `docker image prune -a` | 所有未被任何容器引用的镜像 | 中（下次启动需重新拉取） |
| `docker builder prune` | build cache | 低-中（重新构建变慢） |
| `docker volume prune` | **未被使用的卷，含匿名卷** | **高（可能含持久化数据）** |
| `docker system prune` | 停止容器 + dangling 镜像 + 未用网络 + build cache | 中 |
| `docker system prune -a --volumes` | 以上全部 + 未被引用的所有镜像与卷 | **极高** |

执行任何 prune 前必须先运行只读盘点，把将受影响的对象拿给用户确认：

```bash
sh SKILL_DIR/scripts/docker_prune_plan.sh     # 盘点将被清理的对象并给出建议命令
sh SKILL_DIR/scripts/docker_df_summary.sh     # Docker 占用与宿主机磁盘对照
```

带时间窗过滤时优先 `--filter until=72h`，把清理范围限制在明确的时间窗口内。
用户确认后逐类执行（不要一把 `system prune -a --volumes` 全清），每类之间
复查 `docker system df` 的变化。

## Compose 生命周期

以项目目录中的 `compose.yaml`（或 `docker-compose.yml`）为唯一事实来源：

```bash
docker compose config                          # 校验文件语法与变量展开（改后必跑）
docker compose up -d                           # 幂等：按需创建/重建变更过的服务
docker compose pull                            # 拉取新镜像（配合 up -d 完成滚动更新）
docker compose ps                              # 项目内容器状态
docker compose logs --tail 50 <service>        # 单服务日志
docker compose restart                         # 重启（不重新读取 compose 文件变更）
docker compose down                            # 停止并删除容器与网络（保留卷）
docker compose down --volumes                  # 额外删除 named volume——高危，需明确确认
```

要点：

- 配置变更后 `restart` **不会**生效，需要 `up -d`（compose 会重建受影响容器）。
- `down` 默认保留卷正是为了保护数据；`--volumes` 是数据删除操作，等同
  volume 保护原则，必须逐个确认卷名。
- 修改 compose 文件前先 `cp` 备份；变更后 `docker compose config` 校验再应用。

---

## 常见故障速查

| 症状 | 定位 | 处置 |
|------|------|------|
| daemon 起不来 | `sudo journalctl -u docker -n 50` | 多为 daemon.json 语法错误/与已有配置冲突，回滚备份后重启 |
| `permission denied ... /var/lib/docker.sock` | 当前用户不在 docker 组 | `usermod -aG docker` 后重新登录；一次性场景用 sudo |
| 磁盘满（/var/lib/docker） | `docker system df -v` + 上文脚本 | 按"磁盘治理"流程逐类清理，勿直接 `prune -a --volumes` |
| 容器日志打满磁盘 | `du -sh /var/lib/docker/containers/*/*-json.log` | 配置 log-opts 轮转并重建容器；应急可 truncate 单个日志文件 |
| 镜像拉取超时 | `docker pull` 报 timeout | 配置 registry-mirrors；核对 DNS 与代理 |
| 容器立即退出 | `docker inspect` 看 `.State.ExitCode` 与日志 | 按退出码与日志修因；137 先查 OOM |
| 端口占用启动失败 | `docker: Error ... address already in use` | `ss -ltnp \| grep <port>` 找占用进程，改映射或停旧进程 |

更多命令见 `references/diagnostics-quickref.md`；执行清理前过一遍
`references/cleanup-safety-checklist.md`。

---

## 高危操作确认清单

以下操作执行前必须向用户展示影响面并获得明确确认：

1. `docker volume rm` / `docker volume prune` / `down --volumes`（数据删除）
2. `docker system prune -a`、任何带 `--volumes` 的 prune（大范围删除）
3. `--privileged` 运行容器（宿主机级权限暴露）
4. data-root 迁移与 daemon 重启（业务中断窗口）
5. `docker rm -f` 运行中的容器（未经 compose 管理的现场可能丢失）

确认时给出：将影响的对象清单 + 回滚方式 + 不执行的替代方案。用户未明确同意前，
只执行只读命令。
