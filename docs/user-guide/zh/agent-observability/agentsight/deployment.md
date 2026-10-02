# AgentSight 部署

[English](../../../en/agent-observability/agentsight/deployment.md)

AgentSight 由两个工作进程组成：`agentsight trace`（eBPF 采集，需要 root）和 `agentsight serve`
（API + Dashboard）。下面每种部署形态，本质上都是「用不同方式把这两个进程拉起来」。

## 前置条件

| 条件 | 要求 |
|---|---|
| 操作系统 | Linux x86_64 |
| 内核 | >= 5.8 且开启 BTF（`/sys/kernel/btf/vmlinux` 必须存在） |
| 权限 | root，或 `CAP_BPF` + `CAP_PERFMON` |
| 构建工具链（仅源码构建） | Rust >= 1.80、clang/llvm >= 15、libbpf >= 0.8，前端需要 Node.js |

clang 14 及更早版本会把 eBPF verifier 需要的长度钳制优化掉，导致 `sslsniff`、`tcpsniff` 无法加载。
源码构建请使用 clang 15+。

## 安装包 + systemd（推荐）

```bash
sudo anolisa install agentsight        # 或：sudo yum install agentsight
sudo systemctl enable --now agentsight.service
```

会安装两个 unit：

| Unit | 职责 |
|---|---|
| `agentsight.service` | 运行 `/usr/local/bin/agentsight-start`，由它守护 `agentsight trace` 和 `agentsight serve --host 0.0.0.0` |
| `agentsight-enforcer.service` | 可选的 ActPlane 拦截守护进程；由主 unit 顺带拉起，风险拦截页面依赖它 |

安装包 unit 自带的保障：

- `Restart=always`、`RestartSec=10`，且不限制启动次数，因此反复崩溃或被 OOM 杀掉都不会让主机永久失去观测。
  代价是显式的：完全起不来的服务会每 10 秒重试一次并在 journal 里持续报错，而不是静默消失；
- `OOMPolicy=continue`，单个工作进程被 OOM 杀掉不会拖垮整个 unit —— 守护脚本会停掉另一个工作进程并退出，
  再由 `Restart=always` 把两者拉起。该指令需要 systemd 243+；更老的版本（Anolis 8 / systemd 239）会打一条
  unknown-key 警告并忽略它，退回到停掉 unit 的默认行为，而这种情况 `Restart=always` 同样能恢复；
- `CPUQuota=30%`、`MemoryMax=350M`，按默认 `runtime_limits` 估算；
- `UMask=0077`，`/var/log/sysak/.agentsight` 下的数据仅 root 可读；
- `systemctl reload` 发送 `SIGHUP`，守护脚本会重启两个工作进程以重新读取 `config.json`，无需整体重启。

```bash
systemctl status agentsight.service
journalctl -u agentsight.service -n 50 --no-pager
sudo systemctl reload agentsight.service     # 改完 config.json 之后
```

由于该 unit 把 Dashboard 绑定在 `0.0.0.0`，把主机暴露到不可信网络前，请先在防火墙或云安全组限制
TCP 7396。

## 前台运行（排查用）

同一时间只应有一个 tracer，所以先停服务：

```bash
sudo systemctl stop agentsight.service

# 终端 1
sudo agentsight trace -v

# 终端 2
sudo agentsight serve
```

排查完用 `sudo systemctl start agentsight.service` 恢复。

## 源码构建

```bash
cd src/agentsight

# Anolis / Alibaba Cloud Linux / CentOS / RHEL
sudo yum install -y openssl-devel elfutils-libelf-devel perl-IPC-Cmd libbpf-devel clang llvm bpftool

make build-all          # Dashboard 前端 + agentsight + agentsight-enforcer
sudo ./target/release/agentsight trace &
sudo ./target/release/agentsight serve --host 0.0.0.0
```

只跑 `make build` 会跳过 enforcer，之后每次启动 `serve` 都会打印
`AgentSight enforcement unavailable`。

## 容器与 Sidecar

eBPF 探针需要默认容器配置不会授予的能力：

```bash
docker run --cap-add CAP_BPF --cap-add CAP_PERFMON \
  -v /sys/kernel/btf:/sys/kernel/btf:ro \
  -p 7396:7396 <image>
```

ANOLISA 的容器入口脚本（`docker/docker-entrypoint.sh`）已经按这个规则处理：先检查是否有
`cap_bpf`/`cap_sys_admin`，有就启动 `agentsight-start`，没有则打印需要补上的 `docker run` 参数，而不是
静默失败。

做 Kubernetes Sidecar 时，同样是三件事：

1. 能力——`securityContext.capabilities.add: ["BPF", "PERFMON"]`；不支持细粒度 eBPF 能力的平台上使用
   `privileged: true`；
2. 可见性——Sidecar 需与 Agent 容器共享 PID 命名空间（`shareProcessNamespace: true`），才能看到 Agent
   进程并挂载 uprobe；
3. BTF——以只读方式挂载宿主机的 `/sys/kernel/btf`。

**持久化（必须）**：`/var/log/sysak/.agentsight` 必须挂卷，否则容器每次重启都会清空全部观测数据——
可写层随容器重建而丢弃（`kubectl exec ... pkill`、OOM、镜像升级、节点漂移都会触发）。主机观测数据推荐
`hostPath`；PVC 仅在确实需要跨节点迁移时使用（注意数据随 Pod 生命周期走，Pod 删除即丢失）；
`emptyDir` 只能扛住容器重启，Pod 重建仍会丢失。

```yaml
# sidecar 参考形态（节选）
volumes:
  - name: agentsight-data
    hostPath:
      path: /var/log/sysak/.agentsight
      type: DirectoryOrCreate
containers:
  - name: agentsight
    # ...capabilities / shareProcessNamespace 见上
    volumeMounts:
      - name: agentsight-data
        mountPath: /var/log/sysak/.agentsight
```

> 权限注意：库里含完整 prompt 与回答。`DirectoryOrCreate` 创建的目录是 root 0755，且 sidecar 没有
> 打包 systemd 服务的 `UMask=0077`。生产部署前请预建目录并 `chmod 700`（或用 initContainer 收紧权限），
> 避免主机上其他用户读到对话内容。

Dashboard 端口建议只在集群内可达，通过 Service 或端口转发访问，而不是把 7396 直接暴露到公网。

## Kubernetes DaemonSet（节点级）

DaemonSet 让每个节点各跑一个 AgentSight Pod，观测该节点上的所有 Agent 进程——上面的 Sidecar
形态只看单个 Pod，本形态看整台机器。清单位于 `src/agentsight/packaging/k8s/daemonset.yaml`；
用 `src/agentsight/packaging/docker/Dockerfile` 构建镜像并推送到你的镜像仓库后，相应修改清单中
的 `image:` 字段。

工作原理：`hostPID: true` 使 Pod 进入宿主 PID 命名空间，探针在宿主范围挂载（uprobe 按 inode
注册，其他 Pod 里的 Agent 二进制同样覆盖），而 PID 归属保持正确——AgentSight 按观察者命名空间
上报 PID。

```bash
kubectl apply -f src/agentsight/packaging/k8s/daemonset.yaml
kubectl -n agentsight rollout status ds/agentsight
kubectl -n agentsight port-forward ds/agentsight 7396:7396   # 然后打开 http://localhost:7396
```

清单申请了 `BPF` + `PERFMON` + `SYS_PTRACE` 权能（最后一个用于读取其他 uid 进程的
`/proc/<pid>/maps`——目标进程一旦转为 non-dumpable，缺它会被静默跳过），只读挂载宿主
`/sys/kernel/btf`，数据持久化到 hostPath `/var/log/sysak`；资源限制与安装包的 systemd unit
对齐（300m CPU / 350Mi 内存）。它不访问 Kubernetes API，因此无需 RBAC。Pod 内只运行 trace 与
serve 两个 worker（`agentsight-start`），因此该形态下 Risk Enforcement 页面无数据。如需覆盖
`config.json`，先用随包默认配置创建 `agentsight-config` ConfigMap，再解开清单中标注的注释块。

免 hostPID 形态（改为 bind 挂载宿主 procfs）计划在 procfs-root 工作落地后提供，目前尚不可
用——在此之前请保持 `hostPID: true`。

## macOS

macOS 构建不含 eBPF，只有两个命令：

| 命令 | 在 macOS 上的行为 |
|---|---|
| `agentsight trace` | 仅轨迹采集：扫描本地 Agent JSONL 会话文件，转换为 ATIF v1.7 存入 `trajectories.db` |
| `agentsight serve` | 基于该数据库提供 Dashboard 与轨迹查看 |

```bash
cd src/agentsight && make build-mac
./target/release/agentsight trace     # 终端 1
./target/release/agentsight serve     # 终端 2
```

`--db` 与 `--config` 仅 Linux 可用，Token/审计/中断类命令在 macOS 上不存在。

## 升级

```bash
sudo systemctl stop agentsight.service
sudo yum update agentsight            # 或：sudo anolisa install agentsight
sudo systemctl start agentsight.service
agentsight --version
```

RPM 会保留你的 `/etc/agentsight/config.json`（`%config(noreplace)`）。如果新版本提升了
`schema_version`，AgentSight 会在下次启动时把你的文件复制为 `config.json.bak.<unix秒>`，并写入合并后的
配置：以当前默认配置为底、叠加你设置过的顶层键，因此自定义 Agent 规则能在升级后保留。数据库可直接沿用，
无需迁移步骤。

## 卸载

```bash
sudo systemctl disable --now agentsight.service
sudo yum remove agentsight             # 或：sudo anolisa uninstall agentsight

# 可选：清掉采集到的数据
sudo rm -rf /var/log/sysak/.agentsight
```

## 加固清单

| 项目 | 建议 |
|---|---|
| Dashboard 暴露面 | 尽量保持 `--host 127.0.0.1`；否则用防火墙限制 TCP 7396 |
| 认证 | 保持 `server.auth.enabled` 为 `true`；令牌文件为 `0600` 且属 root |
| 数据目录 | 保持安装时的私有 umask——采集到的提示词与回答都在这里 |
| 日志导出 | 除非外部采集器确实需要，`runtime.sls_logtail_path` 保持为空 |
| 资源上限 | 自定义 unit 时请沿用安装包的 `CPUQuota`/`MemoryMax` |

## 相关页面

- [快速开始](QUICKSTART.md)——第一次运行
- [配置](configuration.md)——装好之后改什么
- [排查](troubleshooting.md)——探针加载失败、没有数据
