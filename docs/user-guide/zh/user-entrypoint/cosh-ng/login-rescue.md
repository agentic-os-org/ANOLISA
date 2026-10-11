# cosh-ng 接管登录失效时的登录救援

[English](../../../en/user-entrypoint/cosh-ng/login-rescue.md)

AgenticOS 镜像将 root 及新建用户的登录 shell 指向 `/usr/bin/cosh-login`，
由它 exec cosh-ng 运行时（`/usr/bin/cosh`，指向 `cosh-shell` 的符号链接）。
本页说明该运行时损坏、SSH 正常登录失败时的救援方法。

## 症状

- SSH 会话立即关闭；非交互命令不执行：

  ```text
  $ ssh root@<ecs-ip> 'echo hello'
  /usr/bin/cosh: line 1: GARBAGE-NOT-AN-ELF: command not found
  ```

- 退出码为 127，远端命令无任何输出。
- 云助手 RunCommand 仍可用：其脚本不经过登录 shell。

## 当前版本 cosh-ng 的行为

从将 `cosh-login` 收编进 cosh-ng RPM 的版本起，wrapper 在 exec 前校验
`/usr/bin/cosh` 的 ELF magic。运行时损坏、缺失或不可读不再导致锁死：
登录会降级到 `/bin/bash` 并输出一行提示：

```text
Error: cosh-ng login runtime unavailable; falling back to /bin/bash.
```

照常 SSH 登录（进入普通 bash），然后按下文修复运行时。

## 修复步骤

通过云助手 RunCommand 执行，或在降级后的 bash 会话中执行：

```bash
# 1. 确认损坏：校验会标出被损坏的文件。
rpm -V cosh-ng

# 2. 重装软件包以恢复被损坏的文件。
yum reinstall -y cosh-ng

# 3. 验证修复：无输出表示所有文件与软件包一致。
rpm -V cosh-ng
head -c 4 /usr/libexec/anolisa/cosh-ng/cosh-shell | od -An -tx1
# 期望输出 magic： 7f 45 4c 46
```

随后新开一个 SSH 会话确认登录恢复正常。

## 云助手不可用时

使用 ECS 控制台（VNC）进入单用户模式：在 GRUB 中中断启动，向内核命令行
追加 `init=/bin/bash` 后启动。系统直接启动 bash、不经过登录 shell；
重新挂载根文件系统为可写（`mount -o remount,rw /`），再执行上述修复步骤。

## 说明

- `cosh-login` 只保护自身的 exec 目标。`/bin/bash` 自身损坏会破坏所有
  登录路径（包括云助手），不在本机制范围内；请用单用户模式恢复。
- 不要把用户登录 shell 永久改回 `/bin/bash` 作为“修复”；应修复软件包，
  使下次启动回到受支持的路径。
