# AgentSight 排查

[English](../../../en/agent-observability/agentsight/troubleshooting.md)

按顺序往下看：绝大多数「AgentSight 什么都没有」的反馈，原因都在前三节。

## 先做快速体检

```bash
systemctl is-active agentsight.service                 # 服务在跑吗？
ps -eo pid,cmd | grep -E "agentsight (trace|serve)"    # 两个工作进程都活着吗？
sudo agentsight discover                               # 你的 Agent 被识别了吗？
sudo agentsight summary --last 1                       # 有数据落库吗？
journalctl -u agentsight.service -n 50 --no-pager      # 它在抱怨什么？
```

## 完全没有数据

**1. tracer 不是以 root 运行。** 非特权用户挂载 eBPF 会静默失败，表现就是「没有任何事件」。请用
`sudo agentsight trace`，或者直接用安装包的服务。若环境根本无法提供特权，`agentsight trace --no-ebpf`
仍然可以采集轨迹（参见 [CLI 参考](cli-reference.md)）；但 Token、审计、中断数据仍然不可用。

**2. 内核没有 BTF。** `ls /sys/kernel/btf/vmlinux` 必须成功。否则 CO-RE 探针无法加载，
`journalctl` 里会有探针加载失败的报错。

**3. 你的 Agent 没被任何规则命中。** 这是权限之外最常见的原因。先确认规则里有没有它：

```bash
sudo agentsight discover --list-known | grep -i <你的-agent>
```

`discover` 读取的是 tracer 所用的同一份 `config.json`，因此 `--list-known` 会反映你的自定义规则
（非默认文件用 `--config <path>`）。文件缺失或无法解析时会打印提示并回退到内置规则。做法见
[Agent 发现规则](configuration.md#agent-发现规则)。

请记住：用户提供的 `config.json` 是**替换**内置规则而不是追加——一个只改了一半的配置文件，会静默地让所有
未被提及的 Agent 失去发现能力。请从 `src/agentsight/agentsight.json` 出发再增补。

**4. 供应商域名没配。** TLS 采集只作用于 `https` 列表里的域名，请把自己的网关或供应商域名加上并 reload。

**5. 两个 tracer 在互相抢。** 前台的 `agentsight trace` 加上服务里的那个，会争抢同一批 uprobe。停掉
其中一个：

```bash
sudo systemctl stop agentsight.service     # 启动前台 tracer 之前
```

**6. 查询用的用户不对。** 服务写入的是 root-only 数据，不加 `sudo` 执行 `agentsight token` 读到的是空库
甚至不存在的库。

## 有些 Agent 有数据，某一个没有

- 编译型 Agent（Rust、Go）不会被 `node*` 这类规则命中，需要按二进制名加规则。
- 包装进程同样重要：cosh-ng 会跑 `cosh-shell` 和 `cosh-core`，两者都要有规则。
- 刚发布的 Codex 版本需要新的 offset 条目，用 `src/agentsight/scripts/extract-codex-offsets.py` 重新生成。
- 日志里出现 `[attach_process] pid=… no SSL libraries found in maps`，说明进程被匹配到了但没有可挂载的
  TLS 库——短生命周期的辅助进程出现这条属正常。

## Dashboard 相关问题

**远程浏览器打开是 401 或登录页。** 远程访问需要令牌：

```bash
sudo agentsight dashboard --no-open        # 打印地址 + 令牌
```

然后用 `http://<host>:7396/?token=<TOKEN>`，或把令牌粘进登录框。本机回环访问不会要求令牌。可信内网想
关闭认证，把 `server.auth.enabled` 设为 `false` 后 reload。

**页面根本打不开。** 依次检查监听和网络链路：

```bash
ss -ltnp | grep 7396
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7396/
```

本机返回 `200` 但浏览器超时，说明端口被拦。`serve` 需要绑定 `0.0.0.0`（安装包 unit 已经这样做），并且
防火墙或云安全组要放行 TCP 7396。`agentsight dashboard` 会直接打印 ECS 控制台链接，就是为了这件事。

**少了几个页面。** 导航只列出当前主机能提供的页面：Token 节省需要 Tokenless，安全可观测与系统审计需要
agent-sec-core，风险拦截需要 enforcer，详见[页面可见性](dashboard.md#导航与页面可见性)。

**明明有数据但页面是空的。** Token 节省、Skill 指标、系统审计需要你选好时间范围后点**查询**。另外确认
Agent 过滤项，以及时间范围是否覆盖会话发生的时间。

**Agent 看板显示「未发现 Agent」。** 那个面板只看*当前*在跑的进程，历史会话仍在其他页面。

## 每次启动都打印 `AgentSight enforcement unavailable`

enforcer 守护进程没有运行：

```bash
sudo systemctl status agentsight-enforcer.service
sudo systemctl start agentsight-enforcer.service
```

源码构建要用 `make build-all`，只跑 `make build` 不会产出 enforcer 二进制。采集与分析不受影响，只是风险
拦截页面会消失。

## 数据增长太快

```bash
sudo du -sh /var/log/sysak/.agentsight
```

`genai_events.db` 默认上限 200 MB（到 90% 开始清理），`interruption_events.db` 是 30 天 / 100 MB。调整
方式见[保留与容量上限](data-and-storage.md#保留与容量上限)。服务运行期间 `-wal` 文件较大属正常，干净
退出时会 checkpoint。

## 担心内存或 CPU 占用

安装包 unit 把服务限制在 `CPUQuota=30%`、`MemoryMax=350M`，与默认 `runtime_limits` 匹配。非常繁忙的机器
上，优先调小缓冲区而不是调高上限：

```json
"runtime_limits": {
  "event_channel_capacity": 5000,
  "event_channel_policy": "drop_newest",
  "event_channel_max_bytes_mb": 32,
  "pending_genai_max_bytes_mb": 32,
  "max_connection_body_mb": 4
}
```

`drop_newest` 和 `sample` 以牺牲完整性换取内存的硬上限。

`event_channel_max_bytes_mb` 限定的是槽位数管不了的部分：单条 SSL 记录最大可达 4 MiB，所以“通道没满”并不等于
“内存没满”。设为 `0` 可关闭字节门。当预算开始拒收事件时，服务每分钟记录一条水位日志：

```bash
sudo journalctl -u agentsight | grep "Buffer watermarks"
```

`dropped over budget` 计数持续上涨，说明流水线消费跟不上——为了守住 `MemoryMax`，事件被主动丢弃。如果计数
一直为 0、但 cgroup 内存仍在逼近上限，说明内存占在下游（HTTP 连接缓冲）而不是通道里，应该改为调小
`max_connection_body_mb`：

```bash
sudo cat /sys/fs/cgroup/system.slice/agentsight.service/memory.current
```

服务如果被 OOM 杀掉会自行拉起；安装包 unit 特意关闭了 systemd 的启动频率限制，就是为了避免反复被杀后
主机陷入永久无观测状态。

## 中断事件看起来不对

- **任务确实失败了但没检测到**：先确认 `features.interruption_detection.enabled`；另外检测依赖采集到的
  流量，如果 Agent 根本没连上供应商，就没有 LLM 侧的信号。
- **不确定 `--type` 能填哪些值**：`agentsight interruption list --help` 会打印完整取值；检测器能产出的
  每一种类型都可以填。
- **看起来像重复的事件**：中断按对话去重，因此同一个底层错误出现在两个对话里，本就是两条事件。
- **健康会话上出现 `token_limit`**：它在输出达到 `max_tokens` 的 95% 时触发。如果回答确实被截断，请调高
  Agent 的 `max_tokens`。

## macOS 的限制

只有 `trace`（扫描本地 JSONL 会话的轨迹采集）和 `serve`。没有 eBPF，因此 Token、审计、中断、discover 类
命令都不可用，`--db` / `--config` 也仅 Linux 可用。

## 输出语言看起来不统一

`summary`、`metrics`、`interruption` 输出英文；`discover`、`token` 无论 locale 都输出中文。需要稳定的机器
可读输出时请用 `--json`——注意 `discover` 没有这个参数，需要机器可读的发现结果可以查 `/api/agent-health`。Dashboard 跟随浏览器语言，也可以手动切换。

## 常见问题

**为什么 OpenClaw 没有 Token 数据？** AgentSight 观测的是 `openclaw-gateway` 守护进程。请确认客户端确实
连上了 gateway；出现 "pairing required" 错误时需要执行 `openclaw devices approve`。

**为什么 Token 节省页显示 0？** 这些会话上 Tokenless 没有产生节省——确认它对该 Agent 已启用，且统计
数据库存在。

**为什么累计节省量比单次调用的差值大？** Agent 每一轮都会把历史消息重新发一遍，节省量会跨轮累加。

**能观测已经在运行的 Agent 吗？** 可以。发现机制启动时会扫描 `/proc`，同时监听 `execve`，因此正在运行的
Agent 无需重启即可被纳入。

## 提交问题前收集信息

```bash
agentsight --version
uname -r
ls /sys/kernel/btf/vmlinux
systemctl status agentsight.service
journalctl -u agentsight.service -n 200 --no-pager
sudo agentsight discover --list-known | head -40
sudo agentsight summary --last 24 --json
sudo ls -la /var/log/sysak/.agentsight
```
