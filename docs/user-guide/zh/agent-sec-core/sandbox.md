# 按明确权限运行沙箱命令

[English](../../en/agent-sec-core/sandbox.md)

`sandbox run` 根据调用方提供的文件和网络权限限制目标命令及其子进程。它在本地运行，
无需 daemon 或策略审批。权限文件是执行输入，不代表已经获得 PDP 授权。

## 运行要求

使用[组件指南](index.md)中的 Rust V2 构建。此命令要求 Linux、非特权 user namespace、
bubblewrap 0.10 或更高版本，以及与 `agent-sec-cli` 安装在同一目录的静态链接
`linux-sandbox`。noexec 规则还要求递归 `mount_setattr` 可用。缺少能力时停止执行，
不会退回宿主直接运行。

## 执行命令

下面的示例允许读取宿主文件，只允许写入 `/tmp/asc-demo`，禁止 IP 联网和从该目录直接执行。
为了覆盖运行依赖，示例中的根目录读取授权较宽；限制实际工作负载时，应替换为应用所需的明确读取路径。

```json
{
  "version": 1,
  "filesystem": {
    "default": "deny",
    "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]},
      {"effect": "allow", "path": "/tmp/asc-demo", "rights": ["read", "write"]}
    ]
  },
  "execution": {"deny_paths": ["/tmp/asc-demo"]},
  "network": {"mode": "off"}
}
```
```bash
mkdir -p /tmp/asc-demo
cd /tmp/asc-demo
# Save the JSON above as permissions.json.
agent-sec-cli sandbox run --permissions permissions.json -- /bin/sh -c 'printf "sandbox\n" > report.txt'
cat report.txt
```

路径必须已经存在。文件路径选中单个对象，目录路径选中其子树，不使用通配符或尾部 `/`。
可在获准写入的目录内创建新文件。命令需要的程序、加载器、库、工作目录和数据都应在权限中声明，
运行时不会自动补充授权。

| 字段 | 行为 |
|---|---|
| `filesystem.default` | 必须为 `deny`。 |
| `rules[].effect` | `allow` 或 `deny`；deny 始终优先，不依赖条目顺序。 |
| `rules[].rights` | `read` 或 `write`；write 包含读取、创建、删除和重命名。 |
| `execution.deny_paths` | 对指定文件或目录子树实施挂载级 noexec。 |
| `network.mode` | `off` 禁止 IP 联网；`full` 使用宿主网络，不过滤目标地址。 |

只读不代表不可执行。noexec 阻止经该挂载直接执行，不阻止获准解释器读取脚本，也不阻止执行另一位置的副本。
后端拒绝无法准确隔离的禁止路径别名和硬链接目标。宿主文件权限仍然生效。socket 类型的 stdio 被拒绝；
调用方提供的终端、管道和普通文件 stdio 保留为明确的数据通道。其他继承 FD 与外部 Unix socket 被阻断。

命令继承环境变量并返回目标退出码，不设执行时限，也不提供主动取消接口；PEP 不改变调用方的信号处理。
helper 启动后须在 30 秒内完成隔离安装，准备或安装失败时目标不会启动。主命令结束时清理后代；
父进程退出时终止直接 helper 和已建立的隔离进程树。bubblewrap 初始化中断的清理仍有竞态边界，见设计文档。
外部信号终止调用方时，不保证最终回执。

## 接入策略授权

需要授权的应用通过 `asc-pep-runtime::PepRuntime` 和 `DaemonAuthorizationClient` 接入，
再使用同一执行后端。[`authorized_sandbox` 示例](../../../../src/agent-sec-core/v2/crates/asc-pep-runtime/examples/authorized_sandbox.rs)
展示请求准备、执行或 ask 前的策略警告及回执；它与 `sandbox run` 分开。

通过现有 `policy create/update` 命令管理 Cedar 策略，策略文件使用
[Policy 指南](policy-cli.md)中的源码封装。更新成功后，下一次授权请求使用新版本。

通过 `agent-sec-daemon --authorization-config /etc/agent-sec-core/authorization.json`
启用授权。此 root 配置只选择 PAP 策略 ID 和运行参数，不保存 Cedar 策略正文：

```json
{
  "baseline_policy": "<policy ID returned by PAP>",
  "environment_permissions": {
    "version": 1,
    "filesystem": {"default": "deny", "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]}
    ]},
    "execution": {"deny_paths": []},
    "network": {"mode": "off"}
  },
  "ttl_seconds": 60
}
```

可选 `task_policy` 指定另一份 PAP 策略 ID。两份策略均使用
`schema: {schemaId: "seccore.system", revision: 2}`，分别求值且不能突破环境上限。
配置及祖先目录须由 root 管理，不可被组或其他用户写入；修改策略选择或运行参数后重启 daemon。

每次 evaluate 从 PAP 一起读取所选策略的当前版本；版本未变时复用已编译结果。
策略缺失、被删除或无法用于授权时停止，不回退旧策略。已有 Scope/Binding 继续使用各自固定版本。

PEP 须在 ttl_seconds 内一次性接收决定；接收后的审批等待、安装和运行不再因该期限失败。
策略更新影响后续请求，已接收决定仍绑定原命令和权限；拒绝或执行失败后不能复用决定。

`authorization.evaluate` 返回 ALLOW、DENY、MODIFY、STEP_UP 或 DEFER，不执行命令。
STEP_UP 在本地 ask，通过后直接执行，不再次 evaluate。事实齐备时，PDP 在同一次求值中完成
修改后命令与权限的授权，PEP 本地核验后执行；只有缺少准备事实时才通过 DEFER 补充并再次 evaluate。
策略中的 warn advice 通过 warnings 返回，不要求审批，也不新增第六种决策。示例在执行或 ask 前显示警告，
并将其放入 JSON 结果；daemon 同时把警告记录到执行回执事件。
`authorization.report` 将执行结果记录到已有 SQLite/JSONL 安全事件存储，上报失败不会重跑命令。
未配置授权接入时这两个 RPC 返回 `unavailable`；直接 `sandbox run` 仍然可用。

schema、组件分工和验证范围见[实现设计](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)。
