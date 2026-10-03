# 清理操作安全检查清单

执行任何 docker/podman 删除类命令（rm / prune / down --volumes）前，
逐项核对以下清单。任何一项不满足，停止并回到只读诊断。

## 删除前（先列后删）

- [ ] 已运行 `sh scripts/docker_prune_plan.sh`（可带时间窗参数，如 `72h`），
      将受影响对象清单展示给用户
- [ ] 已运行 `docker system df` 记录基线，便于删除后对比回收量
- [ ] 已停止容器中的现场不再需要（`docker ps -a` 中的退出容器可能在被人工排查）
- [ ] dangling 镜像确认为构建中间层而非被 `docker run <image-id>` 直接引用
- [ ] 使用 `--filter until=...` 把范围限制在明确的时间窗，而不是无差别全清

## 卷保护（named volume）

- [ ] 没有执行 `docker volume prune` 或 `docker system prune --volumes`
- [ ] 没有 `docker compose down --volumes`（除非用户逐卷确认）
- [ ] 逐个删除时使用精确名字 `docker volume rm <name>`，不用通配
- [ ] 删除前已展示该卷曾被哪些容器挂载（`docker ps -a --filter volume=<name>`）
- [ ] 数据库类卷（postgres/mysql/redis 等命名）默认不可删，即使用户口头说"删旧的"

## 执行中

- [ ] 一次只执行一个类别（先 container，再 image，再 builder），类别间复查
      `docker system df`
- [ ] 不使用 `docker system prune -a --volumes` 一把梭
- [ ] 出现预期外对象（不认识的容器/卷）立即停止，回到只读盘点

## 删除后

- [ ] `docker system df` 对比基线，向用户报告实际回收量
- [ ] 业务容器仍健康：`docker ps` 状态无异常重启
- [ ] 如有误删，立即按回滚预案处置（镜像可重拉；卷只能从事先的备份恢复——
      这正是删除前确认存在备份的原因）

## 绝对禁止（无需清单，直接拒绝）

- `--privileged` 容器作为排查手段
- 删除 `/var/lib/docker` 目录本身来"清磁盘"
- 对生产库卷执行任何形式的 prune
