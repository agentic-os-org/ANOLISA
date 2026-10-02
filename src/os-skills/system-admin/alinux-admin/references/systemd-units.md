# systemd 服务单元配置指南

本文档详细介绍 ALinux 4 环境下 systemd 服务单元的创建和管理。

---

## 单元层次结构

```
目标(分组)                -> multi-user.target, network.target
  服务 (.service)         -> 长期运行的守护进程,一次性任务
  定时器 (.timer)         -> 计划执行(替代 cron)
  套接字 (.socket)        -> 套接字激活的服务
  挂载 (.mount)           -> 由 systemd 管理的文件系统挂载
  路径 (.path)            -> 文件系统变更触发器
```

---

## 依赖指令

| 指令 | 类型 | 说明 |
|------|------|------|
| `Requires=` | 硬性依赖 | 依赖单元失败则本单元也失败 |
| `Wants=` | 软性依赖 | 依赖单元失败不影响本单元 |
| `After=` | 排序 | 仅控制启动顺序，不建立依赖 |
| `Before=` | 排序 | 本单元在指定单元之前启动 |

> `After=network-online.target` 是等待网络连接的正确方式。

---

## 服务单元模板

### 长期运行的守护进程

```ini
# /etc/systemd/system/myapp.service
[Unit]
Description=My Application Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=myapp
Group=myapp
WorkingDirectory=/opt/myapp
ExecStart=/opt/myapp/bin/server
Restart=on-failure
RestartSec=5

# 安全沙盒
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/myapp /var/log/myapp
PrivateTmp=true

# 日志
StandardOutput=journal
StandardError=journal
SyslogIdentifier=myapp

[Install]
WantedBy=multi-user.target
```

### 一次性任务（如备份）

```ini
# /etc/systemd/system/db-backup.service
[Unit]
Description=数据库备份
After=network-online.target postgresql.service
Wants=network-online.target
Requires=postgresql.service

[Service]
Type=oneshot
User=backup
Group=backup

# 安全沙盒
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/backups/db
PrivateTmp=true

ExecStart=/usr/local/bin/db-backup.sh
StandardOutput=journal
StandardError=journal

# 失败时重试
Restart=on-failure
RestartSec=60

[Install]
WantedBy=multi-user.target
```

---

## 定时器单元（替代 cron）

```ini
# /etc/systemd/system/db-backup.timer
[Unit]
Description=每天 02:00 运行数据库备份
Requires=db-backup.service

[Timer]
# 每天 02:00 运行
OnCalendar=*-*-* 02:00:00
# 如果上次运行被错过则立即运行(例如服务器宕机)
Persistent=true
# 在 5 分钟内随机化启动时间以避免惊群效应
RandomizedDelaySec=300

[Install]
WantedBy=timers.target
```

### OnCalendar 语法

| 表达式 | 含义 |
|--------|------|
| `*-*-* 02:00:00` | 每天 02:00 |
| `Mon *-*-* 09:00:00` | 每周一 09:00 |
| `*-*-01 00:00:00` | 每月 1 日 00:00 |
| `hourly` | 每小时 |
| `daily` | 每天 00:00 |
| `weekly` | 每周一 00:00 |

---

## 常用管理命令

```bash
# 重新加载单元文件
sudo systemctl daemon-reload

# 启用并立即启动
sudo systemctl enable --now myapp.service

# 查看状态
systemctl status myapp.service

# 查看日志
journalctl -u myapp.service -n 50 --no-pager

# 列出定时器
systemctl list-timers

# 查看单元文件路径
systemctl show -p FragmentPath myapp.service

# 手动触发一次性任务
sudo systemctl start db-backup.service
```

---

## 安全沙盒选项

| 选项 | 说明 |
|------|------|
| `NoNewPrivileges=true` | 禁止获取新特权 |
| `ProtectSystem=strict` | 以只读方式挂载 /usr 和 /boot |
| `ProtectHome=true` | 使 /home 不可见 |
| `PrivateTmp=true` | 使用独立的 /tmp |
| `ReadWritePaths=` | 允许写入的路径（白名单） |
| `ProtectKernelTunables=true` | 保护 /proc 和 /sys |
| `ProtectKernelModules=true` | 禁止加载内核模块 |

---

## 常见问题排查

| 错误 | 可能原因 | 解决方案 |
|------|----------|----------|
| `Unit not found` | 单元文件不在搜索路径中或未重新加载 | 运行 `systemctl daemon-reload` |
| `Job for X failed` | 服务启动时退出非零 | 查看 `journalctl -u service-name -n 50` |
| 定时器未触发 | 定时器未启用 | 运行 `systemctl enable --now xxx.timer` |
| 服务启动后立即退出 | Type 设置错误 | 对于前台进程使用 `Type=simple` |
