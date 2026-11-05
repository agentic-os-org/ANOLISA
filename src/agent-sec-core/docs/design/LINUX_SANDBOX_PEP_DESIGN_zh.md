# 独立 Linux Sandbox 执行设计

`agent-sec-cli sandbox run` 接收调用方明确提供的 `OsPermissionSet` 和命令，在 Linux
上安装文件、网络与进程限制后执行目标。调用方可以直接运行需要隔离的程序，无需启动 daemon。
权限在本次进程树生命周期内固定；无法实施时返回错误，不扩大权限、不改写命令、不回退到宿主执行。

本文基线为 ANOLISA `d88985b36`，描述在该基线上新增的独立执行路径。原有
`linux-sandbox` CLI/JSON 路径保留原语义，新入口使用单独的严格执行协议。
用户命令与完整 JSON 示例见 [Sandbox 指南](../../../../docs/user-guide/zh/agent-sec-core/sandbox.md)。

## 输入与权限语义

```bash
agent-sec-cli sandbox run --permissions permissions.json -- python3 agent.py
```

`--permissions` 必填，JSON 输入上限为 4 MiB。`--` 后为程序和参数；工作目录和环境来自调用方。
程序按调用方的 PATH 与工作目录解析，固定解析后的路径及对象身份，保留原始程序名和参数。
环境变量名称、值与执行路径必须可表示为 UTF-8。

`asc-os-permission-types` 定义 `OsPermissionSet`。`asc-sandbox-types` 定义 `LaunchSpec`、`PathFact`、`ExecutionSpec`
和执行状态类型。`OsPermissionSet.version` 为 1，未知字段被拒绝：

| 对象 | 语义 |
|---|---|
| `filesystem.default` | 固定为 `deny`，未声明的宿主路径不可访问。 |
| `filesystem.rules` | 现存文件选中单个对象，现存目录选中子树；路径为规范化绝对字面量。 |
| `read` / `write` | write 包含 read，以及目录内创建、删除、重命名等粗粒度修改。 |
| `deny` | 始终覆盖重叠 allow；deny-read 同时禁止 write，deny-write 保留已有 read。 |
| `execution.deny_paths` | 指定可见挂载范围的 noexec，不是独立的文件 execute 授权模型。 |
| `network.mode` | `off` 阻断 IP 网络；`full` 保留宿主网络，不做目标、端口或方向过滤。 |

命令、加载器、库、工作目录和数据所需的读写范围必须全部声明。路径缺失或程序、工作目录没有
读取权限时拒绝；可在已声明可写目录内创建新文件。`/proc`、`/dev` 使用 sandbox 自身视图，
不允许作为显式规则目标。

noexec 通过 `mount_setattr(..., AT_RECURSIVE, MOUNT_ATTR_NOEXEC)` 安装在指定文件或目录
挂载上，覆盖范围内可见子挂载。初始程序落在 noexec 范围时拒绝启动。该限制不阻止获准解释器
读取脚本、通过其他获准别名执行或将内容复制到另一可执行位置。只读挂载仍可执行；宿主的文件
权限继续生效。

文件系统 deny 与 noexec 路径须为 canonical 路径。文件系统 deny 还拒绝范围内的硬链接文件
和重叠的显式挂载别名。后端固定禁止子范围的祖先挂载，避免已允许的目录重命名移走限制。
这些检查不把路径权限扩展为全局文件身份控制。

## 执行链路

```text
asc-cli sandbox run
  → asc-pep-runtime::prepare_launch
  → asc-pep-runtime::execute(LaunchSpec, OsPermissionSet)
  → 准备并复核 PathFact，生成 ExecutionSpec
  → supervisor：sealed memfd 输入 + 独立状态 pipe
  → linux-sandbox 外层：固定对象 FD、构造挂载、启动 bubblewrap
  → linux-sandbox 内层：安装 noexec、降权、seccomp
  → Installed → 启动目标 → Started → Exited
```

`SandboxBackend::installed` 从当前程序的同目录定位 `linux-sandbox`；嵌入方可通过
`SandboxBackend::new` 显式指定受信 helper 路径。helper 不从目标 PATH 中查找。
`asc-pep-runtime/src/linux_sandbox.rs` 负责准备和复核，`supervisor.rs` 负责安装握手及等待，
`linux-sandbox/src/runtime.rs` 负责严格后端。CLI 在本进程内调用这条路径，不产生 daemon RPC。

执行计划通过带写入封印的 memfd 传递；状态 pipe 只由 helper 持有，目标不能继承控制 FD。
后端校验计划中的路径与 canonical 路径、设备号和 inode，使用已打开对象 FD 构造挂载。
任一准备、安装或协议错误终止本次执行；没有隐式补权、重试或宿主执行分支。

此实现采用静态 helper，使隔离安装过程不依赖权限裁剪后的文件系统中的动态加载器与系统库。
构建与安装交付静态 ELF；运行时拒绝带动态解释器或动态依赖的 helper。bubblewrap 仍由系统
提供，须为非 setuid 版本，支持 `--bind-fd`、`--ro-bind-fd`、`--cap-add` 和 `--unshare-user`。
按功能探测兼容性，支持发行版回移这些功能的版本，不以版本号作为硬性门槛。
noexec 实际安装失败也必须阻断目标，不能仅凭工具存在就视为已安装隔离。

bubblewrap 创建 user、PID、IPC、UTS 和 mount namespace；`off` 另隔离 network namespace。
内层 helper 在启动目标前移除 setup capabilities，安装 `no_new_privs` 与基础 seccomp，
关闭多余 FD，再报告 `Installed`。目标只在此后接收自己的环境变量。
两种网络模式均禁止修改挂载、创建新 namespace 及连接外部 Unix socket；本地 stream 和
sequenced-packet Unix socket pair 保留。socket stdio 被拒绝，终端、管道与普通文件 stdio
继续是调用方提供的数据通道。

## 进程生命周期与错误

supervisor 不安装宿主信号处理器，不接管调用方终端，不提供主动取消接口或目标运行超时。
存在终端 stdio 时保留前台进程组；其他情况为 helper 建立独立进程组。
安装阶段自 helper 启动起限时 30 秒，超时或状态协议错误会终止 helper。

内层 helper 作为 PID namespace 的 PID 1 等待主目标并回收后代。主目标退出后，helper
退出使内核清理该 namespace 的其余进程。父进程退出会终止直接 helper 和已经建立的隔离进程树。
已知限制是 bubblewrap 在内部 clone 后等待父进程初始化的窗口：父进程此时被终止，可能留下
尚未运行目标的安装子进程。直接 helper 与已安装进程树的清理覆盖不能证明这个窗口已关闭。

状态协议区分约束已安装、目标已启动和目标退出。CLI 返回目标退出码或报告的 `128 + signal`；
安装失败、目标启动失败及不完整握手作为错误返回。状态结果不会触发目标重跑。

## 验证范围

权限类型与参数测试验证 JSON、路径格式、输入边界和 CLI 分派；后端单元测试验证 deny 优先、
路径身份、别名拒绝、FD 传递和握手。真实 Linux 用例负责证明约束安装后的访问结果：
读写与禁止子范围、文件及目录 noexec、可见子挂载、IP off/full、Unix socket 与 FD、
Python/Node 子进程，以及终端、信号和进程树清理。

`tests/e2e/linux-sandbox/strict_cli.py` 从公开 CLI 在 daemon 不可用的环境中执行真实命令。
参数快照、Mock 或编译成功不能代替 Linux 隔离验证；具体通过情况以相应提交的测试结果为准。
本设计不声称 macOS 可以构建或运行此 Linux 后端，也不将上述初始化清理竞态计为已通过。
