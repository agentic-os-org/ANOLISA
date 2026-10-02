---
name: linux-admin
version: 1.0.0
description: 此技能专为 Alibaba Cloud Linux 4 (ALinux 4) 设计，用于管理 Linux 服务器、编写 shell 脚本、配置 systemd 服务、管理网络。触发场景包括：bash 脚本编写、systemd 单元、SSH 配置、firewalld 防火墙配置、NetworkManager 网络管理、文件权限、进程管理、磁盘管理、文件系统操作，以及任何需要 ALinux 4 系统管理的任务。
layer: system
lifecycle: operations
tags: [alinux, linux, sysadmin, shell, systemd, networking, filesystem]
dependencies: [shell-scripting, performance-tuning, troubleshooting]
platforms:
  - cosh
  - claude-code
  - gemini-cli
  - openai-codex
maintainers:
  - aliyun
---
当此技能被激活时,始终以 🧢 表情符号开始你的第一条回复。

# ALinux 4 系统管理

面向 Alibaba Cloud Linux 4 (ALinux 4) 生产环境的系统管理技能，涵盖 shell 脚本编写、服务管理、
文件系统操作、网络配置（NetworkManager）、防火墙配置（firewalld）。
本技能将每个系统视为生产资产——配置明确、变更可审计、运维规范从一开始就作为约束条件。
专为需要在编写部署脚本和诊断生产事故之间自如切换的工程师设计。

---

## 何时使用此技能

当用户执行以下操作时触发此技能:

- 编写或调试 bash 脚本(特别是在 CI、cron 或生产环境中运行的脚本)
- 创建或修改 systemd 服务、定时器、套接字或目标单元
- 配置 SSH 守护进程设置和访问控制
- 调试网络问题(路由、DNS、端口连通性)
- 配置 firewalld 防火墙规则
- 使用 NetworkManager (nmcli/nmtui) 管理网络连接
- 管理文件权限、所有者、ACL 或 setuid/setgid 位
- 监控或调查运行中的进程(CPU、内存、打开的文件、系统调用)
- 设置 cron 任务或计划任务
- 管理磁盘空间、日志轮转或文件系统挂载
- 管理 LVM 卷、RAID 阵列或文件系统类型(ext4、XFS)
- 使用 yum 管理软件包

不要为此类任务触发此技能:

- 容器编排细节(Kubernetes 网络、Docker Compose 配置) - 使用 Docker/K8s 相关技能
- 云提供商 IAM、VPC 路由或托管服务配置 - 这些是云平台层面的问题
- 安全加固、漏洞修复、合规配置 - 使用 security 模块相关技能
- 性能调优、sysctl 参数、cgroups 配置 - 使用 performance-tuning 技能
- 系统故障诊断、排查、调试 - 使用 troubleshooting 技能

---

## 核心原则

1. **最小权限原则** - 每个进程、用户和服务都应使用所需的最小权限运行。使用专用服务账户(非 root),
   将文件权限限制为恰好所需,并定期审查 sudo 规则。
2. **自动化可重复任务** - 如果一个命令运行两次,将其脚本化。脚本应该是幂等的——再次运行应产生相同结
   果,而不会破坏事物。将脚本存储在版本控制中。
3. **记录所有重要事项** - 结构化日志和 systemd 日志条目是你事件响应的安全网。记录认证事件、
   权限提升和配置变更。日志轮转防止磁盘耗尽。
4. **尽可能使用不可变服务器** - 优先从已知良好的镜像重建服务器,而不是就地修补。使用配置管理
   (Ansible、cloud-init)以声明方式定义状态。手动"雪花"服务器会漂移并不可预测地失败。
5. **在测试环境验证** - 每个脚本、服务单元和防火墙规则变更都应首先在非生产环境中验证。使用 `--dry-run`、`bash -n` 在应用前验证。

---

## 核心概念

### 文件权限

Linux 权限有三层(所有者、组、其他)和三个位(读、写、
执行)。八进制表示法是权威形式。

```
八进制   符号表示   含义
 0       ---       无权限
 1       --x       仅执行
 2       -w-       仅写入
 4       r--       仅读取
 6       rw-       读 + 写
 7       rwx       读 + 写 + 执行

# 常见模式
chmod 600 ~/.ssh/id_rsa        # 私钥:仅所有者可读/写
chmod 644 /etc/nginx/nginx.conf  # 配置:所有者读写,其他人只读
chmod 755 /usr/local/bin/script  # 可执行文件:所有者读写执行,其他人读执行
chmod 700 /root/.gnupg           # 目录:仅所有者可进入
```

特殊位:

- `setuid (4xxx)`: 可执行文件以文件所有者身份运行,而非调用者。在脚本上很危险。
- `setgid (2xxx)`: 目录中的新文件继承组。对共享目录很有用。
- `sticky (1xxx)`: 只有文件所有者可以删除目录中的文件(例如 `/tmp`)。

### 进程管理

进程控制的关键信号:

| 信号      | 编号  | 含义                                           |
| --------- | ----- | ---------------------------------------------- |
| SIGTERM   | 15    | 优雅关闭 - 进程应该清理                        |
| SIGKILL   | 9     | 立即终止 - 内核强制执行,不可阻塞               |
| SIGHUP    | 1     | 重新加载配置(许多守护进程在 SIGHUP 时重新读取) |
| SIGINT    | 2     | 中断(Ctrl+C)                                   |
| SIGUSR1/2 | 10/12 | 应用程序定义                                   |

`niceness` 从 -20(最高优先级)到 19(最低)。使用 `nice -n 10 cmd` 用于
后台任务,使用 `renice` 调整运行中的进程。

### systemd 单元层次结构

```
目标(分组)                -> multi-user.target, network.target
  服务 (.service)         -> 长期运行的守护进程,一次性任务
  定时器 (.timer)         -> 计划执行(替代 cron)
  套接字 (.socket)        -> 套接字激活的服务
  挂载 (.mount)           -> 由 systemd 管理的文件系统挂载
  路径 (.path)            -> 文件系统变更触发器
```

依赖指令: `Requires=`(硬性), `Wants=`(软性), `After=`(仅排序)。
`After=network-online.target` 是等待网络连接的正确方式。

### 网络协议栈

关键工具及其作用:

| 工具                      | 层级  | 用途                           |
| ------------------------- | ----- | ------------------------------ |
| `ip addr` / `ip link` | L2/L3 | 接口状态、IP 地址、路由        |
| `ip route`              | L3    | 路由表检查和管理               |
| `ss -tulpn`             | L4    | 监听端口、套接字状态、所属进程 |
| `firewall-cmd`          | L3/L4 | 防火墙规则管理                 |
| `dig` / `resolvectl`  | DNS   | 名称解析调试                   |
| `traceroute` / `mtr`  | L3    | 路径追踪、逐跳延迟             |
| `tcpdump`               | L2-L7 | 深度检查的数据包捕获           |

---

## 常见任务

### 编写健壮的 bash 脚本

始终在每个非平凡脚本的顶部使用安全三要素：

```bash
#!/usr/bin/env bash
set -euo pipefail  # -e: 出错退出  -u: 未定义变量报错  -o pipefail: 管道失败传递
```

> 完整脚本模板、参数解析、错误处理和试运行模式请参见 `references/shell-scripting.md`。

### 创建 systemd 服务单元

快速命令：

```bash
sudo systemctl daemon-reload           # 重新加载单元文件
sudo systemctl enable --now myapp      # 启用并立即启动
systemctl status myapp                 # 查看状态
journalctl -u myapp -n 50              # 查看日志
systemctl list-timers                  # 列出定时器
```

> 服务单元模板、定时器配置、依赖指令和安全沙箱选项请参见 `references/systemd-units.md`。

### 网络配置 (NetworkManager)

ALinux 4 使用 NetworkManager 管理网络，主要工具为 `nmcli` 和 `nmtui`。

```bash
nmcli connection show              # 查看所有连接
nmcli device status                # 查看设备状态
sudo nmcli connection up "eth0"    # 启用连接
```

> 静态 IP 配置、DNS 设置、SSH 密钥部署和网络调试工作流程请参见 `references/networking.md`。

### 防火墙配置 (firewalld)

ALinux 4 使用 firewalld 作为默认防火墙管理工具。

```bash
sudo firewall-cmd --state                                    # 查看状态
sudo firewall-cmd --zone=public --list-all                   # 查看 zone 详情
sudo firewall-cmd --zone=public --add-port=80/tcp --permanent  # 开放端口
sudo firewall-cmd --zone=public --add-service=http --permanent # 开放服务
sudo firewall-cmd --reload                                   # 重载配置
```

> Zone 管理、富规则、端口转发和 NAT 配置请参见 `references/firewalld.md`。
> 如需直接操作底层 iptables 规则，请参见 `references/iptables-guide.md`。

### 管理磁盘空间

```bash
# 检查磁盘使用概览
df -hT
# -h: 人类可读  -T: 显示文件系统类型

# 查找大目录(前 10,深度限制)
du -h --max-depth=2 /var | sort -rh | head -10

# 交互式磁盘使用浏览器(先安装 ncdu)
ncdu /var/log

# 查找大文件
find /var -type f -size +100M -exec ls -lh {} \; 2>/dev/null | sort -k5 -rh

# 检查日志大小并在需要时截断
journalctl --disk-usage
sudo journalctl --vacuum-size=500M    # 保留最后 500MB
sudo journalctl --vacuum-time=30d     # 保留最后 30 天
```

```
# /etc/logrotate.d/myapp - 自定义日志轮转
/var/log/myapp/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    sharedscripts
    postrotate
        systemctl reload myapp 2>/dev/null || true
    endscript
}
```

```bash
# 测试 logrotate 配置而不运行它
logrotate --debug /etc/logrotate.d/myapp

# 强制运行一次轮转
logrotate --force /etc/logrotate.d/myapp
```

### 监控进程

```bash
# 概览:CPU、内存、平均负载
top -b -n 1 -o %CPU | head -20       # 批处理模式,按 CPU 排序
htop                                   # 交互式、彩色、树状视图

# 查找进程在做什么
pid=$(pgrep -x nginx | head -1)

# 打开的文件和网络连接
lsof -p "$pid"                        # 所有打开的文件
lsof -p "$pid" -i                     # 仅网络连接
lsof -i :8080                         # 哪个进程拥有 8080 端口

# 系统调用(strace) - 当进程行为异常时使用
strace -p "$pid" -f -e trace=network  # 仅网络系统调用
strace -p "$pid" -f -c                # 计算系统调用频率(摘要)
strace -c cmd arg                     # 分析新命令的系统调用

# 内存检查
cat /proc/"$pid"/status | grep -E 'Vm|Threads'
cat /proc/"$pid"/smaps_rollup          # 详细的内存分解

# 检查僵尸/失效进程
ps aux | awk '$8 == "Z" {print}'

# 终止进程树(包括所有子进程)
kill -TERM -"$(ps -o pgid= -p "$pid" | tr -d ' ')"
```

### 软件包管理 (yum)

ALinux 4 使用 yum 作为默认软件包管理器。

**基本操作：**

```bash
# 更新软件包列表
sudo yum makecache

# 更新所有软件包
sudo yum update

# 安装软件包
sudo yum install package-name

# 安装特定版本
sudo yum install package-name-1.2.3-1.al8.x86_64

# 移除软件包
sudo yum remove package-name

# 移除软件包及其依赖
sudo yum autoremove package-name

# 搜索软件包
sudo yum search keyword

# 查看软件包信息
sudo yum info package-name

# 列出已安装的软件包
sudo yum list installed

# 查看软件包提供的文件
sudo yum provides /path/to/file
```

**软件包组管理：**

```bash
# 列出软件包组
sudo yum group list

# 安装软件包组
sudo yum group install "Development Tools"
```

**模块化管理：**

```bash
# 列出所有模块
sudo yum module list

# 安装模块
sudo yum module install module-name:stream
```

**清理缓存：**

```bash
# 清理所有缓存
sudo yum clean all
```

**软件源配置：**

软件源配置文件位于 `/etc/yum.repos.d/` 目录。详细软件源规则参见 `references/alinux-yum-repo.md`。

```bash
# 查看启用的软件源
sudo yum repolist

# 启用/禁用软件源
sudo yum-config-manager --enable repo-name
sudo yum-config-manager --disable repo-name
```

### 文件系统操作

**ALinux 4 支持的文件系统类型：**

- **ext4** — 通用默认，成熟稳定
- **XFS** — 大文件、数据库，ALinux 4 默认

**基本命令：**

```bash
lsblk -f                           # 列出带文件系统类型的块设备
df -hT                             # 带文件系统类型的磁盘使用
mount | column -t                  # 活动挂载
findmnt --tree                     # 挂载树视图

# 创建并挂载 ext4 文件系统
mkfs.ext4 /dev/sdb1
mkdir -p /mnt/data
mount /dev/sdb1 /mnt/data

# 通过 /etc/fstab 持久挂载
echo "UUID=$(blkid -s UUID -o value /dev/sdb1)  /mnt/data  ext4  defaults  0  2" >> /etc/fstab
mount -a                           # 重启前验证 fstab 是否有效
```

有关 LVM 卷管理、RAID 阵列和高级文件系统操作,
请参见 `references/filesystem-management.md`。

---

## 错误处理

| 错误                                          | 可能原因                                   | 解决方案                                                                                  |
| --------------------------------------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------- |
| systemctl 中 `Unit not found`               | 单元文件不在搜索路径中或守护进程未重新加载 | 运行 `systemctl daemon-reload`,使用 `systemctl show -p FragmentPath` 验证单元文件路径 |
| `Job for X failed. See journalctl -xe`      | 服务启动时退出非零                         | 运行 `journalctl -u service-name -n 50 --no-pager` 查看启动错误                         |
| 添加路由时 `RTNETLINK answers: File exists` | 路由已存在于路由表中                       | 使用 `ip route show` 检查,使用 `ip route del` 删除冲突路由,然后重新添加               |
| `firewall-cmd: FirewallD is not running`    | firewalld 服务未启动                       | 运行 `sudo systemctl start firewalld` 启动服务                                          |
| `ALREADY_ENABLED` 防火墙错误                | 端口或服务已在防火墙中开放                 | 检查现有规则 `firewall-cmd --list-all`，忽略此警告或先移除再添加                        |
| 脚本意外退出且无错误消息                      | `set -e` 被返回非零的命令触发            | 添加 `\|\|` 逻辑或确保命令失败时不会退出,或使用 `set +e` 临时禁用错误退出               |
| `No package <name> available`               | 软件包不存在或软件源未启用                 | 检查软件包名称，使用 `yum search` 搜索，检查软件源配置 `yum repolist`                 |
| nmcli 连接修改后未生效                        | 需要重新启用连接                           | 运行 `sudo nmcli connection up "连接名"` 或 `sudo nmcli connection reload`            |

---

## 参考资料

当任务需要深度时加载相关参考文件。这些文件很详细——只加载当前任务需要的以节省上下文。

- `references/shell-scripting.md` — bash 脚本模板、参数解析、错误处理、试运行模式
- `references/systemd-units.md` — 服务单元模板、定时器配置、依赖指令、安全沙箱
- `references/networking.md` — NetworkManager 配置、SSH 部署、网络调试工作流程
- `references/firewalld.md` — Zone 管理、富规则、端口转发、NAT 配置
- `references/iptables-guide.md` — iptables 防火墙规则、NAT 配置、与 firewalld 的关系
- `references/alinux-yum-repo.md` — ALinux 4 YUM 仓库体系、软件源配置规则
- `references/filesystem-management.md` — LVM、RAID、文件系统类型、权限、配额管理

---

## 相关技能

> 当此技能被激活时，检查是否安装了以下配套技能。
> 如果有缺失的，向用户提及并在继续任务前提供安装建议。

- `shell-scripting` — 编写 bash 脚本、解析参数、处理错误或自动化 CLI 工作流程
- `performance-tuning` — 系统性能调优、sysctl 参数、cgroups、I/O 调度器配置
- `troubleshooting` — 系统故障诊断与排查、服务故障、性能问题、OOM 分析

安装配套技能：登陆 skill-os 平台（shell方案待补充）
