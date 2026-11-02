# 按明确权限运行沙箱命令

[English](../../en/agent-sec-core/sandbox.md)

`agent-sec-cli sandbox run` 根据调用方提供的文件和网络权限限制目标命令及其子进程，
在本地执行，无需 daemon。调用方选择权限，sandbox 在启动命令前安装这些限制。

## 运行要求

按[组件指南](index.md#安装)安装 Rust V2 CLI。此命令要求 Linux、非特权 user namespace，
以及支持 `--bind-fd`、`--ro-bind-fd`、`--cap-add` 和 `--unshare-user` 的非 setuid 系统
bubblewrap；支持发行版回移这些功能的版本。静态链接 `linux-sandbox` 须与 `agent-sec-cli`
安装在同一目录。noexec 规则还要求递归 `mount_setattr` 可用。
缺少前置条件时停止执行，不会退回宿主直接运行。

## 执行命令

下面的示例允许读取宿主文件，只允许写入 `/tmp/asc-demo`，禁止 IP 联网和从该目录直接执行。
为了覆盖运行依赖，示例中的根目录读取授权较宽；限制实际工作负载时，应替换为应用所需的明确读取路径。

在下面命令创建的目录中，将这份 `OsPermissionSet` JSON 保存为 `permissions.json`：

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

`--permissions` 必填，最多读取 4 MiB。程序与参数放在 `--` 后。
命令使用调用方的工作目录和环境变量；环境变量名称和值必须是有效 UTF-8。

## 权限格式

| 字段 | 行为 |
|---|---|
| `version` | 必须为 `1`；未知字段被拒绝。 |
| `filesystem.default` | 必须为 `deny`。 |
| `filesystem.rules[].effect` | `allow` 或 `deny`；deny 始终优先，不依赖条目顺序，也不能被更具体的 allow 覆盖。 |
| `filesystem.rules[].rights` | 非空且不重复的 `read`、`write` 列表。write 包含读取、创建、删除和重命名；禁止 read 也会禁止 write。 |
| `execution.deny_paths` | 对指定文件或目录子树实施挂载级 noexec，包含范围内可见的子挂载。 |
| `network.mode` | `off` 禁止 IP 联网；`full` 使用宿主网络，不过滤目标地址、端口或方向。 |

路径必须已经存在，是规范化的绝对路径，不使用通配符、点路径段或尾部 `/`，根目录 `/` 除外。
文件路径选中单个对象，目录路径选中其子树。`filesystem.rules` 和 `execution.deny_paths`
各最多接受 1,024 项。可在获准写入的目录内创建新文件。命令需要的程序、加载器、库、工作目录和
数据都应在权限中声明，运行时不会自动补充授权。`/proc` 和 `/dev` 保留给 sandbox 基础设施，
不能作为明确的规则目标。

只读不代表不可执行。noexec 阻止经该挂载直接执行，不阻止获准解释器读取脚本、经其他获准别名执行，
也不阻止执行另一位置的副本。文件系统 deny 路径必须是 canonical 路径；后端拒绝经过 symlink 的
deny 路径、重叠的显式挂载别名，以及禁止范围内的硬链接文件。noexec 路径也必须是 canonical 路径。
宿主文件权限仍然生效。

## 执行行为

两种网络模式都保留进程隔离、权限降级和基础 seccomp 限制。socket 类型的 stdio 被拒绝；
调用方提供的终端、管道和普通文件 stdio 保留为数据通道。其他继承 FD 和外部 Unix socket 连接
被阻断；本地 stream、sequenced-packet Unix socket pair 仍可用于子进程 IPC。

CLI 返回目标退出码；收到信号退出报告时，返回 `128 + signal`。它不改变调用方的信号处理，
不设执行时限，也不提供主动取消接口。helper 启动后须在 30 秒内完成隔离安装。
准备或安装失败时目标不会启动，也不会在 sandbox 外重试命令。

主命令结束时清理 sandbox 后代。父进程退出时终止直接 helper 和已建立的隔离进程树。
bubblewrap 初始化期间，父进程被终止可能留下尚未启动目标的安装子进程；这一清理竞态仍是已知限制。
实现与验证边界见[执行设计](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)。
