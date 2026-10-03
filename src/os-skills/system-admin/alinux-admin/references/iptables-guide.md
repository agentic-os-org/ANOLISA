# iptables 防火墙指南

iptables 是 Linux 内核级防火墙工具，提供精细的网络流量控制。虽然 ALinux 4 默认使用 firewalld，
但 iptables 在某些场景下仍然需要使用，如：复杂的 NAT 配置、精确的数据包控制、与旧系统兼容。

## 目录

1. [基本概念](#基本概念)
2. [查看规则](#查看规则)
3. [基本规则配置](#基本规则配置)
4. [常用规则示例](#常用规则示例)
5. [NAT 配置](#nat-配置)
6. [规则持久化](#规则持久化)
7. [与 firewalld 的关系](#与-firewalld-的关系)

---

## 基本概念

### 表 (Tables)

iptables 有四个内置表：

| 表 | 用途 |
|---|------|
| `filter` | 默认表，用于数据包过滤 |
| `nat` | 网络地址转换 |
| `mangle` | 修改数据包 |
| `raw` | 绕过连接跟踪 |

### 链 (Chains)

每个表包含若干链：

**filter 表：**
- `INPUT` - 进入本机的数据包
- `OUTPUT` - 从本机发出的数据包
- `FORWARD` - 经过本机转发的数据包

**nat 表：**
- `PREROUTING` - 数据包到达时修改目的地址 (DNAT)
- `POSTROUTING` - 数据包离开时修改源地址 (SNAT/MASQUERADE)
- `OUTPUT` - 本机发出的数据包

### 目标 (Targets)

| 目标 | 说明 |
|------|------|
| `ACCEPT` | 接受数据包 |
| `DROP` | 丢弃数据包（不响应） |
| `REJECT` | 拒绝数据包（返回错误） |
| `LOG` | 记录数据包 |
| `SNAT` | 源地址转换 |
| `DNAT` | 目的地址转换 |
| `MASQUERADE` | 动态源地址转换 |

---

## 查看规则

```bash
# 查看 filter 表所有规则
sudo iptables -L -n -v

# 查看 nat 表规则
sudo iptables -t nat -L -n -v

# 以数字形式显示规则（带行号）
sudo iptables -L -n --line-numbers

# 查看特定链
sudo iptables -L INPUT -n -v
```

参数说明：
- `-L` - 列出规则
- `-n` - 不解析主机名（更快）
- `-v` - 详细输出（显示计数器）
- `--line-numbers` - 显示行号

---

## 基本规则配置

### 设置默认策略

```bash
# 设置 INPUT 链默认策略为 DROP（谨慎使用）
sudo iptables -P INPUT DROP
sudo iptables -P FORWARD DROP
sudo iptables -P OUTPUT ACCEPT
```

### 清空规则

```bash
# 清空所有规则
sudo iptables -F

# 删除所有自定义链
sudo iptables -X

# 清零计数器
sudo iptables -Z

# 清空 nat 表
sudo iptables -t nat -F
```

### 添加规则

```bash
# 添加规则到链末尾
sudo iptables -A INPUT -p tcp --dport 80 -j ACCEPT

# 插入规则到链开头
sudo iptables -I INPUT -p tcp --dport 22 -j ACCEPT

# 插入规则到指定位置
sudo iptables -I INPUT 2 -p tcp --dport 443 -j ACCEPT
```

### 删除规则

```bash
# 按规则内容删除
sudo iptables -D INPUT -p tcp --dport 80 -j ACCEPT

# 按行号删除
sudo iptables -D INPUT 3
```

### 替换规则

```bash
# 替换指定位置的规则
sudo iptables -R INPUT 1 -p tcp --dport 22 -j ACCEPT
```

---

## 常用规则示例

### 基础服务器配置

```bash
# 清空规则
iptables -F
iptables -X

# 设置默认策略
iptables -P INPUT DROP
iptables -P FORWARD DROP
iptables -P OUTPUT ACCEPT

# 允许回环接口
iptables -A INPUT -i lo -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT

# 允许已建立的连接
iptables -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

# 允许 SSH
iptables -A INPUT -p tcp --dport 22 -j ACCEPT

# 允许 HTTP/HTTPS
iptables -A INPUT -p tcp -m multiport --dports 80,443 -j ACCEPT

# 允许 ICMP (ping)
iptables -A INPUT -p icmp --icmp-type echo-request -j ACCEPT

# 记录并丢弃其他流量
iptables -A INPUT -j LOG --log-prefix "iptables DROP: "
iptables -A INPUT -j DROP
```

### SSH 速率限制（防暴力破解）

```bash
# 限制每分钟新建 SSH 连接数
iptables -A INPUT -p tcp --dport 22 -m conntrack --ctstate NEW \
    -m recent --set --name SSH --rsource
iptables -A INPUT -p tcp --dport 22 -m conntrack --ctstate NEW \
    -m recent --update --seconds 60 --hitcount 4 --name SSH --rsource -j DROP
iptables -A INPUT -p tcp --dport 22 -j ACCEPT
```

### 限制特定 IP

```bash
# 允许特定 IP
iptables -A INPUT -s 192.168.1.100 -j ACCEPT

# 允许 IP 段
iptables -A INPUT -s 192.168.1.0/24 -j ACCEPT

# 拒绝特定 IP
iptables -A INPUT -s 10.0.0.100 -j DROP
```

### 限制特定端口的源 IP

```bash
# 仅允许内网访问 MySQL
iptables -A INPUT -p tcp -s 192.168.1.0/24 --dport 3306 -j ACCEPT
iptables -A INPUT -p tcp --dport 3306 -j DROP
```

### 限制连接数

```bash
# 限制每个 IP 最多 10 个并发连接
iptables -A INPUT -p tcp --dport 80 -m connlimit --connlimit-above 10 -j DROP
```

---

## NAT 配置

### 启用 IP 转发

```bash
# 临时启用
echo 1 > /proc/sys/net/ipv4/ip_forward

# 永久启用
echo "net.ipv4.ip_forward = 1" >> /etc/sysctl.d/99-ipforward.conf
sysctl -p /etc/sysctl.d/99-ipforward.conf
```

### SNAT（源地址转换）

用于内网主机通过网关访问外网：

```bash
# 静态 SNAT
iptables -t nat -A POSTROUTING -s 192.168.1.0/24 -o eth0 -j SNAT --to-source 203.0.113.10

# 动态 SNAT（适用于动态 IP）
iptables -t nat -A POSTROUTING -s 192.168.1.0/24 -o eth0 -j MASQUERADE
```

### DNAT（目的地址转换）

用于端口转发：

```bash
# 将外网 8080 端口转发到内网服务器的 80 端口
iptables -t nat -A PREROUTING -p tcp --dport 8080 -j DNAT --to-destination 192.168.1.100:80

# 同时需要允许转发
iptables -A FORWARD -p tcp -d 192.168.1.100 --dport 80 -j ACCEPT
```

### 本地端口转发

```bash
# 将本地 8080 端口转发到 80 端口
iptables -t nat -A OUTPUT -p tcp --dport 8080 -j REDIRECT --to-port 80
```

---

## 规则持久化

### 保存规则

```bash
# 保存当前规则
iptables-save > /etc/sysconfig/iptables

# 或使用 iptables-services
sudo yum install iptables-services
sudo systemctl enable iptables
sudo service iptables save
```

### 恢复规则

```bash
# 从文件恢复
iptables-restore < /etc/sysconfig/iptables

# 重启 iptables 服务
sudo systemctl restart iptables
```

### 规则文件格式

```
# /etc/sysconfig/iptables 示例
*filter
:INPUT DROP [0:0]
:FORWARD DROP [0:0]
:OUTPUT ACCEPT [0:0]
-A INPUT -i lo -j ACCEPT
-A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
-A INPUT -p tcp --dport 22 -j ACCEPT
-A INPUT -p tcp -m multiport --dports 80,443 -j ACCEPT
COMMIT

*nat
:PREROUTING ACCEPT [0:0]
:POSTROUTING ACCEPT [0:0]
:OUTPUT ACCEPT [0:0]
-A POSTROUTING -s 192.168.1.0/24 -o eth0 -j MASQUERADE
COMMIT
```

---

## 与 firewalld 的关系

### 重要提示

在 ALinux 4 上，firewalld 和 iptables 不能同时使用。firewalld 底层也是使用 iptables/nftables，
两者同时使用会导致规则冲突。

### 切换到 iptables

如果需要使用原生 iptables：

```bash
# 停止并禁用 firewalld
sudo systemctl stop firewalld
sudo systemctl disable firewalld

# 安装并启用 iptables-services
sudo yum install iptables-services
sudo systemctl enable iptables
sudo systemctl start iptables
```

### 切换回 firewalld

```bash
# 停止 iptables
sudo systemctl stop iptables
sudo systemctl disable iptables

# 启用 firewalld
sudo systemctl enable firewalld
sudo systemctl start firewalld
```

---

## 调试技巧

### 记录日志

```bash
# 记录被丢弃的数据包
iptables -A INPUT -j LOG --log-prefix "iptables DROP: " --log-level 4
```

查看日志：
```bash
journalctl -k | grep "iptables"
dmesg | grep "iptables"
```

### 测试规则（不立即应用）

```bash
# 使用 iptables-restore 测试语法
iptables-restore --test < rules.txt
```

### 计数器分析

```bash
# 查看每条规则的匹配计数
iptables -L -n -v

# 清零计数器后重新统计
iptables -Z
# ... 等待一段时间 ...
iptables -L -n -v
```

---

## 参考命令速查

| 操作 | 命令 |
|------|------|
| 查看规则 | `iptables -L -n -v` |
| 清空规则 | `iptables -F` |
| 添加规则 | `iptables -A INPUT ...` |
| 插入规则 | `iptables -I INPUT ...` |
| 删除规则 | `iptables -D INPUT ...` |
| 设置策略 | `iptables -P INPUT DROP` |
| 保存规则 | `iptables-save > /etc/sysconfig/iptables` |
| 恢复规则 | `iptables-restore < /etc/sysconfig/iptables` |
| 查看 NAT | `iptables -t nat -L -n -v` |
