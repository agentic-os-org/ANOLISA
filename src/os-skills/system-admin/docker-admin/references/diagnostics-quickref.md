# Docker 诊断命令速查（单机运行时）

只读命令按"看全局 → 看对象 → 看细节"排列，可安全用于生产环境。

## 全局状态

| 命令 | 用途 |
|------|------|
| `docker info` | daemon 版本、存储驱动、data-root、registry mirror 是否生效 |
| `docker system df` | 镜像/容器/卷/缓存总量与可回收量 |
| `docker system df -v` | 上述明细（逐镜像、逐卷占用） |
| `docker ps` | 运行中容器 |
| `docker ps -a --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}'` | 全部容器与退出状态 |

## 单容器诊断

| 命令 | 用途 |
|------|------|
| `docker inspect <c>` | 配置与状态事实来源（网络、挂载、重启策略、退出码） |
| `docker inspect --format '{{.State.Status}} {{.State.ExitCode}} {{.State.OOMKilled}}' <c>` | 三项关键状态一行看 |
| `docker inspect --format '{{json .State.Health}}' <c> \| jq` | 健康检查探针历史 |
| `docker stats --no-stream <c>` | 单容器资源快照 |
| `docker top <c>` | 容器内进程 |
| `docker port <c>` | 端口映射 |
| `docker logs --tail 100 --since 30m <c>` | 受限日志窗口（永远带限制） |
| `docker exec <c> sh -c 'command'` | 免交互进入执行观察命令 |

## 磁盘与日志占用

| 命令 | 用途 |
|------|------|
| `df -h /var/lib/docker` | docker root 所在文件系统余量 |
| `du -sh /var/lib/docker/containers/*/*-json.log 2>/dev/null \| sort -h \| tail` | 找最大的容器日志文件 |
| `du -sh /var/lib/docker/overlay2 2>/dev/null` | 镜像层总占用 |
| `docker volume ls` / `docker volume inspect <v>` | 卷清单与挂载点 |

## Compose 项目

| 命令 | 用途 |
|------|------|
| `docker compose config` | 校验 compose 文件（变更后必跑） |
| `docker compose ps` | 项目内容器状态 |
| `docker compose top` | 项目内进程 |
| `docker compose logs --tail 50 <svc>` | 单服务日志 |

## daemon 侧

| 命令 | 用途 |
|------|------|
| `sudo journalctl -u docker -n 100 --no-pager` | daemon 启动失败/崩溃日志 |
| `sudo systemctl status docker` | 服务状态 |
| `docker info --format '{{.RegistryConfig.Mirrors}}'` | 生效中的 registry mirror |
