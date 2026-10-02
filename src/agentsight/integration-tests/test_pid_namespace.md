# PID 命名空间（容器内目标进程）集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

验证 agentsight 从**初始 pid namespace** 观察时，能正确发现并 attach 到**位于其他 pid namespace 内**的目标进程 —— 即 k8s DaemonSet + `hostPID: true` 的部署形态。约定见 [ebpf-probes.md](../docs/design-docs/ebpf-probes.md) 的「PID 命名空间约定」。

不需要 k8s：`unshare --pid --fork --mount-proc` 造出的嵌套 pid namespace 与容器在这一点上同构，目标进程的 host pid 与 ns pid 同样不相等。

## 测试目标

1. 目标进程运行在嵌套 pid namespace 内、agentsight 运行在初始 namespace 时，procmon 的 exec 事件应上报 **host pid**，而不是容器内 pid（判定依据：`RUST_LOG=debug` 下 `Attached to agent: <name> (pid=N)` 里的 N 等于 `/proc/<N>/status` 的 `NSpid:` **第一列**）
2. 承 1，SSL uprobe 应实际挂载成功，并产出 LLM/HTTPS 事件（判定依据：不出现 `[attach_process] pid=N: no SSL libraries found in maps`；stderr 不出现 `Warning: attach_process pid=`）
3. 目标进程与 agentsight 同处初始 namespace（不 unshare）时，行为不应有任何变化（回归：host pid 路径本来就正确）
4. agentsight 自身运行在嵌套 namespace 内、目标进程与其共享该 namespace 时，仍应正常发现并 attach（回归：sidecar + `shareProcessNamespace` 形态，此时上报最内层 pid 才是对的）
5. rodata 开关判定应与实际所处 namespace 一致（判定依据：`RUST_LOG=debug` 下 `observer pid namespace inode 0x... (init=true|false)`）

## 运行条件

- root 权限
- Linux kernel >= 5.8 with BTF
- `unshare` 可用（util-linux）
- 网络可达：目标进程需真实发起一次 HTTPS 请求以触发 SSL uprobe

## 参考步骤

目标 1 / 2（核心场景，修复前必失败）：

```bash
# 终端 A：初始 namespace 内启动 agentsight
RUST_LOG=debug /root/agentsight trace 2>&1 | tee /tmp/agentsight-test-pidns.log

# 终端 B：嵌套 pid namespace 内跑一个匹配 cmdline 规则的目标进程，
# 让它发起 HTTPS 请求；--mount-proc 使其 /proc 只含该 namespace
unshare --pid --fork --mount-proc <target-agent-cmd>

# 终端 C：取该进程的 host pid 与 ns pid，确认两者不等
pgrep -af <target-agent-cmd>
grep -E '^(Name|Pid|NSpid):' /proc/<host-pid>/status
```

修复前的失败特征：日志里 `Attached to agent: ... (pid=N)` 的 N 是容器内小编号，
且 `/proc/N` 对应的是宿主机上另一个无关进程（或不存在），SSL 事件始终为空。

目标 4（sidecar 回归）：把 agentsight 和目标进程放进同一个 unshare 出来的
namespace，此时 `init=false`，上报最内层 pid 即为正确行为。

## 已知不覆盖

agentsight 自身在某个 namespace 内、目标在**更深**嵌套 namespace 的情形不在本测试范围内 —— 该场景下用户态无法解析目标 pid，预期行为是 fail-closed 不 attach。
