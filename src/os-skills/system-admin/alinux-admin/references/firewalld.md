# firewalld 防火墙配置指南

本文档详细介绍 ALinux 4 默认防火墙管理工具 firewalld 的配置和使用。

> 如需直接操作底层 iptables 规则，请参见 `iptables-guide.md`。

---

## 目录

1. [基本概念](#基本概念)
2. [基本操作](#基本操作)
3. [Zone 管理](#zone-管理)
4. [端口和服务管理](#端口和服务管理)
5. [富规则](#富规则rich-rules)
6. [端口转发和 NAT](#端口转发和-nat)
7. [常见场景配置](#常见场景配置)

---

## 基本概念

### Zone（区域）

Zone 是 firewalld 的核心概念，定义了网络连接的信任级别：

| Zone | 信任级别 | 默认行为 |
|------|----------|----------|
| `drop` | 最低 | 丢弃所有入站，不回复 |
| `block` | 低 | 拒绝所有入站，回复 ICMP 拒绝 |
| `public` | 低 | 默认 zone，仅允许选定服务 |
| `external` | 低 | 用于外部网络，启用伪装 |
| `dmz` | 低 | DMZ 区域 |
| `work` | 中 | 工作网络 |
| `home` | 高 | 家庭网络 |
| `internal` | 高 | 内部网络 |
| `trusted` | 最高 | 允许所有连接 |

### 运行时 vs 永久配置

- **运行时**：立即生效，重启后丢失
- **永久**：需要 `--permanent` 参数，重载后生效

```bash
# 运行时（测试用）
sudo firewall-cmd --add-port=8080/tcp

# 永久（生产用）
sudo firewall-cmd --add-port=8080/tcp --permanent
sudo firewall-cmd --reload
```

---

## 基本操作

```bash
# 查看防火墙状态
sudo systemctl status firewalld
sudo firewall-cmd --state

# 启动/停止防火墙
sudo systemctl start firewalld
sudo systemctl stop firewalld

# 开机自启
sudo systemctl enable firewalld

# 重载配置（不中断现有连接）
sudo firewall-cmd --reload

# 完全重载（中断连接）
sudo firewall-cmd --complete-reload
```

---

## Zone 管理

```bash
# 查看所有 zone
sudo firewall-cmd --get-zones

# 查看默认 zone
sudo firewall-cmd --get-default-zone

# 设置默认 zone
sudo firewall-cmd --set-default-zone=public

# 查看活动 zone
sudo firewall-cmd --get-active-zones

# 查看 zone 详情
sudo firewall-cmd --zone=public --list-all

# 查看所有 zone 的详情
sudo firewall-cmd --list-all-zones

# 将接口分配到 zone
sudo firewall-cmd --zone=internal --change-interface=eth1 --permanent
```

---

## 端口和服务管理

### 端口管理

```bash
# 查看开放的端口
sudo firewall-cmd --zone=public --list-ports

# 开放端口（永久）
sudo firewall-cmd --zone=public --add-port=80/tcp --permanent
sudo firewall-cmd --zone=public --add-port=443/tcp --permanent

# 开放端口范围
sudo firewall-cmd --zone=public --add-port=8000-8100/tcp --permanent

# 移除端口
sudo firewall-cmd --zone=public --remove-port=80/tcp --permanent

# 重载使永久配置生效
sudo firewall-cmd --reload
```

### 服务管理

```bash
# 查看可用服务
sudo firewall-cmd --get-services

# 查看服务定义（包含哪些端口）
sudo firewall-cmd --info-service=http

# 查看已开放的服务
sudo firewall-cmd --zone=public --list-services

# 开放服务（永久）
sudo firewall-cmd --zone=public --add-service=http --permanent
sudo firewall-cmd --zone=public --add-service=https --permanent
sudo firewall-cmd --zone=public --add-service=ssh --permanent
sudo firewall-cmd --reload

# 移除服务
sudo firewall-cmd --zone=public --remove-service=http --permanent
```

---

## 富规则（Rich Rules）

富规则提供更精细的控制，支持源地址、目标端口、动作等组合。

### 基本语法

```bash
sudo firewall-cmd --zone=<zone> --add-rich-rule='<rule>' --permanent
```

### 常用富规则示例

```bash
# 允许特定 IP 访问特定端口
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" port protocol="tcp" port="3306" accept' --permanent

# 允许特定 IP 访问所有端口
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="10.0.0.100" accept' --permanent

# 拒绝特定 IP
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="10.0.0.100" reject' --permanent

# 丢弃特定 IP（不回复）
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="10.0.0.100" drop' --permanent

# 限制 SSH 连接速率（每分钟最多 3 次）
sudo firewall-cmd --zone=public --add-rich-rule='rule service name="ssh" limit value="3/m" accept' --permanent

# 记录被拒绝的连接
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="10.0.0.0/8" log prefix="blocked: " level="info" reject' --permanent

# 查看所有富规则
sudo firewall-cmd --zone=public --list-rich-rules

# 移除富规则
sudo firewall-cmd --zone=public --remove-rich-rule='rule family="ipv4" source address="10.0.0.100" reject' --permanent
```

---

## 端口转发和 NAT

### 本地端口转发

```bash
# 将 8080 转发到 80
sudo firewall-cmd --zone=public --add-forward-port=port=8080:proto=tcp:toport=80 --permanent

# 查看端口转发规则
sudo firewall-cmd --zone=public --list-forward-ports
```

### 转发到其他主机

```bash
# 需要先启用 IP 伪装
sudo firewall-cmd --zone=public --add-masquerade --permanent

# 转发到其他主机
sudo firewall-cmd --zone=public --add-forward-port=port=80:proto=tcp:toaddr=192.168.1.100 --permanent

# 转发到其他主机的不同端口
sudo firewall-cmd --zone=public --add-forward-port=port=80:proto=tcp:toport=8080:toaddr=192.168.1.100 --permanent

sudo firewall-cmd --reload
```

### NAT 伪装

```bash
# 启用 NAT 伪装（出站流量）
sudo firewall-cmd --zone=public --add-masquerade --permanent

# 检查是否启用
sudo firewall-cmd --zone=public --query-masquerade

# 禁用
sudo firewall-cmd --zone=public --remove-masquerade --permanent
```

---

## 常见场景配置

### Web 服务器

```bash
sudo firewall-cmd --zone=public --add-service=http --permanent
sudo firewall-cmd --zone=public --add-service=https --permanent
sudo firewall-cmd --reload
```

### 数据库服务器（仅允许内网访问）

```bash
# MySQL 仅允许内网 IP 访问
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" port protocol="tcp" port="3306" accept' --permanent

# PostgreSQL
sudo firewall-cmd --zone=public --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" port protocol="tcp" port="5432" accept' --permanent

sudo firewall-cmd --reload
```

### 开发环境（多端口）

```bash
# 开放常用开发端口范围
sudo firewall-cmd --zone=public --add-port=3000-3100/tcp --permanent
sudo firewall-cmd --zone=public --add-port=8000-8100/tcp --permanent
sudo firewall-cmd --reload
```

---

## 常见问题排查

| 错误 | 可能原因 | 解决方案 |
|------|----------|----------|
| `FirewallD is not running` | firewalld 服务未启动 | `sudo systemctl start firewalld` |
| `ALREADY_ENABLED` | 端口或服务已开放 | 忽略此警告，或先移除再添加 |
| 规则未生效 | 未重载或未使用 `--permanent` | `sudo firewall-cmd --reload` |
| 无法访问服务 | zone 设置错误 | 检查 `--get-active-zones` 和接口分配 |
| 富规则语法错误 | 语法不正确 | 检查引号和空格，参考示例 |

---

## 配置文件位置

- 系统预定义：`/usr/lib/firewalld/`
- 用户自定义：`/etc/firewalld/`
  - zones: `/etc/firewalld/zones/`
  - services: `/etc/firewalld/services/`
