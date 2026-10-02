# ECS Metadata 服务

## 什么是 Metadata 服务？

ECS Metadata 是阿里云 ECS 实例内置的一个 HTTP 服务，用于提供当前实例的元数据信息。**无需配置 AccessKey，实例内部可直接访问**。

---

## 访问端点

| 版本 | 端点 URL | 说明 |
|------|----------|------|
| V1 | `http://100.100.100.200/` | 简化访问，无需 token |
| V2 | `http://100.100.100.200/latest/meta-data/` | 需要 IMDSv2 token，更安全 |
| 兼容 | `http://169.254.169.254/` | AWS 兼容端点 |

---

## 常用 Metadata 字段

### 实例基本信息

```bash
curl http://100.100.100.200/latest/meta-data/instance-id          # 实例 ID
curl http://100.100.100.200/latest/meta-data/instance-type        # 实例规格
curl http://100.100.100.200/latest/meta-data/region-id            # 地域 ID
curl http://100.100.100.200/latest/meta-data/zone-id              # 可用区 ID
curl http://100.100.100.200/latest/meta-data/hostname             # 主机名
curl http://100.100.100.200/latest/meta-data/serial-number        # 序列号
curl http://100.100.100.200/latest/meta-data/image-id             # 镜像 ID
curl http://100.100.100.200/latest/meta-data/os-type              # 操作系统类型
curl http://100.100.100.200/latest/meta-data/os-name              # 操作系统名称
curl http://100.100.100.200/latest/meta-data/launch-time          # 启动时间
```

### 网络信息

```bash
curl http://100.100.100.200/latest/meta-data/mac                  # MAC 地址
curl http://100.100.100.200/latest/meta-data/vpc-id               # VPC ID
curl http://100.100.100.200/latest/meta-data/vswitch-id           # 交换机 ID
curl http://100.100.100.200/latest/meta-data/private-ipv4         # 私网 IP
curl http://100.100.100.200/latest/meta-data/public-ipv4          # 公网 IP
curl http://100.100.100.200/latest/meta-data/eipv4                # EIP 地址
curl http://100.100.100.200/latest/meta-data/network/interfaces/macs/<MAC>/vpc-id  # 指定网卡的 VPC
```

### 安全相关

```bash
curl http://100.100.100.200/latest/meta-data/security-groups      # 安全组列表
curl http://100.100.100.200/latest/meta-data/ram-role-name        # RAM 角色名
```

### 其他

```bash
curl http://100.100.100.200/latest/meta-data/ntp-conf/ntp-servers # NTP 服务器
curl http://100.100.100.200/latest/meta-data/source-address       # 请求源地址
```

---

## 查看所有可用字段

```bash
curl http://100.100.100.200/latest/meta-data/
```

---

## IMDSv2（更安全的访问方式）

阿里云支持 IMDSv2，需要先生成 token：

```bash
# 第一步：获取 token
TOKEN=$(curl -X PUT "http://100.100.100.200/latest/api/token" \
  -H "X-aliyun-ecs-metadata-token-ttl-seconds: 21600")

# 第二步：使用 token 访问 metadata
curl -H "X-aliyun-ecs-metadata-token: $TOKEN" \
  http://100.100.100.200/latest/meta-data/instance-id
```

---

## Metadata 服务特点

| 特性 | 说明 |
|------|------|
| 仅实例内访问 | 只能从 ECS 实例内部访问，外部无法访问 |
| 无需认证 | V1 模式无需任何凭证 |
| 实时性 | 数据随实例状态实时更新 |
| 只读 | 只能读取，不能修改 |
| 本地服务 | 不产生公网流量 |

---

## 典型使用场景

### 1. 脚本自动获取实例信息

```bash
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
echo "Running on instance: $INSTANCE_ID"
```

### 2. 根据实例规格动态配置

```bash
INSTANCE_TYPE=$(curl -s http://100.100.100.200/latest/meta-data/instance-type)
case $INSTANCE_TYPE in
  *large*) WORKERS=4 ;;
  *xlarge*) WORKERS=8 ;;
esac
```

### 3. 获取 RAM 角色临时凭证

```bash
ROLE_NAME=$(curl -s http://100.100.100.200/latest/meta-data/ram-role-name)
curl -s http://100.100.100.200/latest/meta-data/ram/security-credentials/$ROLE_NAME
```

---

## 安全建议

- 优先使用 IMDSv2（需要 token），防止 SSRF 攻击
- 通过 MetadataOptions 控制 token 跳转限制
- 定期审计实例的 Metadata 访问配置
