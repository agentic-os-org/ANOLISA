# 网络配置指南

本文档详细介绍 ALinux 4 环境下的网络配置，包括 NetworkManager、SSH 和网络调试。

---

## 目录

1. [NetworkManager (nmcli/nmtui)](#networkmanager)
2. [SSH 配置](#ssh-配置)
3. [网络调试工作流程](#网络调试工作流程)
4. [常用网络工具](#常用网络工具)

---

## NetworkManager

ALinux 4 使用 NetworkManager 管理网络，主要工具为 `nmcli`（命令行）和 `nmtui`（TUI 界面）。

### 查看网络状态

```bash
# 查看所有连接
nmcli connection show

# 查看活动连接
nmcli connection show --active

# 查看设备状态
nmcli device status

# 查看连接详情
nmcli connection show "System eth0"

# 查看设备详情
nmcli device show eth0
```

### 配置 IP 地址

```bash
# 修改为静态 IP
sudo nmcli connection modify "System eth0" \
    ipv4.addresses 192.168.1.100/24 \
    ipv4.gateway 192.168.1.1 \
    ipv4.method manual

# 设置为 DHCP
sudo nmcli connection modify "System eth0" ipv4.method auto

# 设置 DNS
sudo nmcli connection modify "System eth0" ipv4.dns "8.8.8.8 8.8.4.4"

# 添加搜索域
sudo nmcli connection modify "System eth0" ipv4.dns-search "example.com"

# 启用连接
sudo nmcli connection up "System eth0"

# 重新加载所有连接
sudo nmcli connection reload
```

### 创建新连接

```bash
# 创建静态 IP 连接
sudo nmcli connection add \
    con-name "my-static" \
    type ethernet \
    ifname eth1 \
    ipv4.addresses 10.0.0.100/24 \
    ipv4.gateway 10.0.0.1 \
    ipv4.dns "8.8.8.8" \
    ipv4.method manual

# 创建 DHCP 连接
sudo nmcli connection add \
    con-name "my-dhcp" \
    type ethernet \
    ifname eth1 \
    ipv4.method auto
```

### 删除和禁用连接

```bash
# 断开连接
sudo nmcli connection down "System eth0"

# 删除连接
sudo nmcli connection delete "my-static"
```

### TUI 界面

```bash
# 打开交互式网络配置界面
nmtui
```

---

## SSH 配置

### 生成和部署密钥

```bash
# 生成现代密钥对（推荐 ed25519）
ssh-keygen -t ed25519 -C "admin@example.com"

# 或使用 RSA（兼容性更好）
ssh-keygen -t rsa -b 4096 -C "admin@example.com"

# 部署公钥到远程服务器
ssh-copy-id admin@server

# 手动部署（如果 ssh-copy-id 不可用）
cat ~/.ssh/id_ed25519.pub | ssh admin@server "mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys"
```

### sshd_config 基本设置

编辑 `/etc/ssh/sshd_config`：

```
# 禁止 root 登录
PermitRootLogin no

# 禁用密码认证（仅密钥）
PasswordAuthentication no
PubkeyAuthentication yes

# 禁用 X11 转发
X11Forwarding no

# 登录超时和重试限制
LoginGraceTime 30
MaxAuthTries 4

# 仅允许特定组
AllowGroups sshusers admins

# 连接保活
ClientAliveInterval 300
ClientAliveCountMax 3
```

### 验证和重启

```bash
# 重启前验证配置语法
sudo sshd -t

# 重启 sshd（保持当前会话打开直到验证通过）
sudo systemctl restart sshd

# 在关闭旧会话前从新会话验证
ssh -v user@host
```

> **重要**：在验证新会话正常工作之前，永远不要关闭现有的 SSH 会话。

---

## 网络调试工作流程

按自上而下的顺序排查：

### 1. 检查接口状态和 IP 分配

```bash
ip addr show
ip link show
```

### 2. 检查路由表

```bash
ip route show
# 预期：通过网关的默认路由，本地子网路由
```

### 3. 测试网关可达性

```bash
ping -c 4 $(ip route | awk '/default/ {print $3}')
```

### 4. 测试 DNS 解析

```bash
# 直接到外部解析器
dig +short google.com @8.8.8.8

# 使用系统解析器（systemd-resolved）
resolvectl query google.com

# 检查配置的解析器
cat /etc/resolv.conf
```

### 5. 检查监听端口和所属进程

```bash
ss -tulpn
# -t: TCP  -u: UDP  -l: 监听  -p: 进程  -n: 不解析名称
```

### 6. 测试特定端口连通性

```bash
# 使用 netcat
nc -zv 10.0.0.5 5432

# 使用 bash 内置
timeout 3 bash -c "</dev/tcp/10.0.0.5/5432" && echo open || echo closed
```

### 7. 追踪路径

```bash
# ICMP 路径追踪
traceroute -n 8.8.8.8

# 带统计的连续路径（比 traceroute 更好）
mtr --report 8.8.8.8
```

### 8. 捕获流量进行深度检查

```bash
# 捕获 eth0 上与主机的所有 443 端口流量
sudo tcpdump -i eth0 -n host 10.0.0.5 and port 443 -w /tmp/capture.pcap

# 不保存的快速查看
sudo tcpdump -i eth0 -n port 53   # 实时查看 DNS 查询
```

---

## 常用网络工具

| 工具 | 层级 | 用途 |
|------|------|------|
| `ip addr` / `ip link` | L2/L3 | 接口状态、IP 地址、路由 |
| `ip route` | L3 | 路由表检查和管理 |
| `ss -tulpn` | L4 | 监听端口、套接字状态、所属进程 |
| `dig` / `resolvectl` | DNS | 名称解析调试 |
| `traceroute` / `mtr` | L3 | 路径追踪、逐跳延迟 |
| `tcpdump` | L2-L7 | 深度检查的数据包捕获 |
| `nc` (netcat) | L4 | 端口连通性测试 |
| `curl` / `wget` | L7 | HTTP/HTTPS 测试 |

---

## 常见问题排查

| 问题 | 可能原因 | 解决方案 |
|------|----------|----------|
| nmcli 连接修改后未生效 | 需要重新启用连接 | `sudo nmcli connection up "连接名"` |
| DNS 解析失败 | DNS 服务器配置错误 | 检查 `/etc/resolv.conf` 或 nmcli dns 设置 |
| 无法 ping 网关 | 接口未启用或 IP 配置错误 | 检查 `ip addr` 和 `ip link` |
| 添加路由时 `File exists` | 路由已存在 | 先 `ip route del` 再添加 |
| SSH 连接超时 | 防火墙阻止或网络不通 | 检查 firewalld 规则和网络连通性 |
