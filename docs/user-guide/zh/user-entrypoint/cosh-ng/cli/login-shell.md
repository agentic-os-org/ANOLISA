# 管理登录 Shell 注册

[English](../../../../en/user-entrypoint/cosh-ng/cli/login-shell.md)

使用 `cosh-cli login-shell` 检查或显式配置 Linux 登录 Shell 接入。手动运行 `cosh`
无需注册或修改账户。安装 cosh、注册可用 Shell、选择账户的 Shell、启用 Agent 会话接入
是独立决定。这些命令不会启用 AW、启动 Herdr、修改 Agent Hook 或改变运行时登录身份。

## 检查安装入口

```bash
cosh-cli login-shell status --user alice
cosh-cli login-shell --shell /opt/anolisa/bin/cosh status --user alice
```

默认选择当前 `cosh-cli` 可执行文件同目录的 `cosh`。管理其他安装位置或运行开发二进制时，
使用 `--shell`。它必须是绝对 UTF-8 路径，不包含空白、`#`、冒号或点路径段。
注册保留这个公开入口路径；符号链接解析后的可执行文件单独报告，替代 Provider 因此可以
继续使用账户配置中的稳定入口。检查过程不会执行选中的 Shell。

JSON `data` 包含 `shell`、`resolved_executable`、`executable`、`registered`、`user`、
`account_shell`、`using_accounts`、`changed`、`previous_shell` 和 `retained_provider`。
`account_access_checked` 表示是否已完成目标账户权限检查。
`account_shell` 保留账户记录的原值，包括空字符串。未指定 `--user` 时，
`user` 和 `account_shell` 为 null。
`registered` 按 `/etc/shells` 中公开入口的原始拼写匹配。`using_accounts` 按路径段
比较 `getent passwd` 枚举结果，忽略重复分隔符，但不解析符号链接；不覆盖无法枚举的
目录服务账户或其他路径别名，移除 Shell 前需另行检查这些情况。

`status` 与变更操作的 `--dry-run` 不需要 root 权限，也不创建管理锁或状态文件，但仍需
有权读取所选路径。Linux 需要 `/usr/bin/getent`；账户变更还需要发行版账户管理软件包中的
`/usr/sbin/usermod`。其他平台返回不支持的错误。

## 分别注册和选择

```bash
cosh-cli login-shell register --dry-run
sudo cosh-cli login-shell register
cosh-cli login-shell set --user alice --expect-shell /bin/bash --dry-run
sudo cosh-cli login-shell set --user alice --expect-shell /bin/bash
```

`register` 将已有可执行文件加入 `/etc/shells`，不会为账户选择 Shell。`set` 要求显式本地
账户、已注册的可执行目标，以及匹配账户当前 Shell 的 `--expect-shell`。特权执行
`set`/`restore`（包括特权预览）时，独立 cosh-cli 子进程先切换到目标账户 UID、主组及
NSS 附加组，再由内核检查执行权限；父目录不可访问、ACL 限制或目标不可执行都会拒绝变更。
检查不会执行目标 Shell，管理进程保留原身份。`executable` 只表示普通文件带至少一个执行位；
`account_access_checked` 表示目标账户权限预检已通过。非特权预览将该字段保留为 false，
选择前可使用特权预览完成检查。预检不保证未来权限、程序格式、解释器、动态库或登录启动成功。
命令不会从 `sudo`、`HOME` 或当前登录会话猜测账户。账户 Shell 字段为空时，
显式传入 `--expect-shell ''`。

实际变更需要 root。重复注册或重复选择当前已经使用的目标不会写入。`changed` 表示实际
变更或 dry-run 计划变更；dry-run 中的状态字段仍描述当前状态。账户变更影响后续登录，
不会重启当前 Shell。

## 恢复显式目标并注销

```bash
cosh-cli login-shell restore --user alice --expect-shell /usr/bin/cosh --to /bin/bash --dry-run
sudo cosh-cli login-shell restore --user alice --expect-shell /usr/bin/cosh --to /bin/bash
sudo cosh-cli login-shell unregister
sudo cosh-cli login-shell unregister --if-missing
```

使用此前报告的账户 Shell 或另一个有意选择的目标；`restore` 不猜测、不维护隐藏的历史
Shell。`--to` 必须是 `/etc/shells` 中已注册的可执行文件，`--expect-shell` 必须匹配账户当前
状态。目标已经被选择时，重复执行成功且不写入。管理员已经切换到其他 Shell 时，预期值不匹配
会拒绝修改。原字段为空时，恢复要求管理员显式选择一个已注册的 Shell，不接受 `--to ''`。

只要枚举到的账户仍使用入口，`unregister` 就拒绝操作，即使可执行文件已经删除。
`--if-missing` 还会在入口仍存在可执行 Provider 时保留注册，包括替代软件包。
该选项适用于显式的配置清理，但不能替代软件包卸载保护。两种操作都不删除可执行文件或账户。

## 安装与镜像配置边界

在已有受支持的 Linux 安装方式之后使用同一显式命令：

- **RPM：** 保留软件包的注册与卸载 scriptlet；普通安装不会选择账户 Shell。
  使用管理命令检查状态或显式改变账户。直接 RPM 卸载保留已有 scriptlet 与在用账户检查。
  更安全的替代 Provider 清理由 [PR #3367](https://github.com/agentic-os-org/ANOLISA/pull/3367)
  单独跟踪，不包含在本管理命令中。
- **Raw：** 制品包含 `cosh-cli`。用户模式安装不写系统 Shell 配置。对于已有受支持的 Linux
  raw 布局，调用其安装后的 CLI，并在需要时显式提供公开入口。管理员检查路径可访问性后，
  可显式注册和选择。此命令不扩大 raw 软件包支持的发行版范围。
- **镜像：** 将制品安装到镜像后，在镜像环境内部运行命令，显式指定账户和入口。
  没有 `--root` 模式；从宿主机传入带镜像目录前缀的路径，会管理宿主机配置。

本命令不替换或迁移 RPM scriptlet。尤其卸载后的清理必须在软件包二进制已移除时仍可运行，
不要在 post-uninstall 阶段调用已删除的 `cosh-cli`。共享生命周期逻辑提取及进一步的
raw/Provider 移除接线仍属于独立工作。

## 更新与并发保证

Shell 表更新保留无关条目、注释、符号链接关系、权限、所有者及扩展属性。在目标文件同目录
准备并刷盘后执行原子重命名，再同步目录。重命名前的错误保留原内容；重命名后目录同步失败
仍报告错误，此时新内容已经可见。硬链接、超大文件、准备期间被修改的表会被拒绝。
中断不会留下部分写入的表；强制杀死进程可能在目标目录留下 `.cosh-shells-*` 临时文件，
由管理员检查并清理。

私有 `/etc/.cosh-login-shell.lock` 串行化这些命令，并保留为可复用锁文件。
命令不自行锁定账户数据库，由 `usermod` 负责其生命周期。在调用 `usermod` 前立即复核预期
Shell，调用后检查结果。这**不是相对于其他管理员工具的原子比较并交换**。请协调同时进行的
账户或 Shell 表管理；无法排除不遵循同一锁的工具在最后检查与提交之间写入。
账户工具失败会明确报告，不会静默重试。

## 机器可读错误

`error.code` 区分 `UnsupportedPlatform`、`LoginShellConflict`、
`LoginShellAccessDenied`、`LoginShellBackendError` 和 `LoginShellIoError`。
参数无效使用 `InvalidInput`；文件系统权限、路径缺失和超时错误分别使用
`PermissionDenied`、`NotFound` 和 `Timeout`。账户工具失败使用 `LoginShellBackendError`
并保留原始诊断。冲突包括 Shell 仍在使用、管理员已修改状态等，需要检查后再决定是否重试；
所有失败都不会自动标记为可恢复。
