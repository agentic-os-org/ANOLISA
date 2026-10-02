---
name: aliyun-ecs
version: 1.0.0
description: 通过aliyun命令行管理阿里云弹性计算服务(ECS)。用于查询或创建实例、启动/停止/重启实例、管理磁盘/快照/镜像/安全组/密钥对/弹性网卡、查询状态以及故障排查等工作流。
layer: application
lifecycle: operations
---

# 阿里云ECS管理

## 前置准备

### 1. 安装阿里云CLI

```bash
# 检查是否已安装
aliyun --version

# 未安装时执行
yum install aliyun-cli -y
```

### 2. 配置认证

首次使用或命令报错时需要配置认证。

**根据你的使用场景选择认证模式：**

| 使用场景 | 推荐模式 | 配置命令 |
|----------|----------|----------|
| **在ECS实例内执行** | EcsRamRole（最推荐） | `aliyun configure --mode EcsRamRole --ram-role-name <RAM角色名>` |
| **本地开发/测试** | AK | `aliyun configure --mode AK --access-key-id <AK> --access-key-secret <SK>` |
| **本地交互式操作** | OAuth | `aliyun configure --mode OAuth` |
| **跨账号/精细化权限** | RamRoleArn | `aliyun configure --mode RamRoleArn --access-key-id <AK> --access-key-secret <SK> --ram-role-arn <角色ARN>` |

**快速配置示例：**

```bash
# 交互式配置（推荐新手使用）
aliyun configure

# 或命令行直接配置（替换<>中的参数）
aliyun configure --mode <模式> --region <地域ID>
```

> **安全建议**：优先使用临时凭证（EcsRamRole、RamRoleArn、OAuth），避免使用长期AccessKey。详细认证方式说明及优先级建议请参考 [authentication.md](references/authentication.md)

### 3. 获取实例元数据（ECS实例内）

在ECS实例内部，无需任何认证即可通过Metadata服务获取当前实例的元数据信息：

```bash
# 获取实例ID
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)

# 获取地域ID
REGION_ID=$(curl -s http://100.100.100.200/latest/meta-data/region-id)

# 获取实例规格
INSTANCE_TYPE=$(curl -s http://100.100.100.200/latest/meta-data/instance-type)
```

> 更多Metadata字段及使用方法请参考 [metadata-api.md](references/metadata-api.md)

## 常用操作

### 实例管理

| 操作 | 命令示例 |
|------|----------|
| 查询实例列表 | `aliyun ecs DescribeInstances` |
| 查询实例详情 | `aliyun ecs DescribeInstanceAttribute --InstanceId <实例ID>` |
| 查询实例状态 | `aliyun ecs DescribeInstanceStatus` |
| 创建实例 | `aliyun ecs RunInstances --ImageId <镜像ID> --InstanceType <规格> --RegionId <地域>` |
| 启动实例 | `aliyun ecs StartInstance --InstanceId <实例ID>` |
| 停止实例 | `aliyun ecs StopInstance --InstanceId <实例ID>` |
| 重启实例 | `aliyun ecs RebootInstance --InstanceId <实例ID>` |
| 释放实例 | `aliyun ecs DeleteInstance --InstanceId <实例ID>` |

### 磁盘管理

| 操作 | 命令示例 |
|------|----------|
| 查询磁盘列表 | `aliyun ecs DescribeDisks` |
| 创建磁盘 | `aliyun ecs CreateDisk --RegionId <地域> --ZoneId <可用区> --Size <大小GB>` |
| 挂载磁盘 | `aliyun ecs AttachDisk --InstanceId <实例ID> --DiskId <磁盘ID>` |
| 卸载磁盘 | `aliyun ecs DetachDisk --InstanceId <实例ID> --DiskId <磁盘ID>` |
| 离线扩容磁盘 | `aliyun ecs ResizeDisk --DiskId <磁盘ID> --NewSize <新大小GB>` |
| 在线扩容磁盘 | `aliyun ecs ResizeDisk --DiskId <磁盘ID> --NewSize <新大小GB> --Type online` |
| 删除磁盘 | `aliyun ecs DeleteDisk --DiskId <磁盘ID>` |

### 快照管理

| 操作 | 命令示例 |
|------|----------|
| 查询快照列表 | `aliyun ecs DescribeSnapshots` |
| 创建快照 | `aliyun ecs CreateSnapshot --DiskId <磁盘ID>` |
| 删除快照 | `aliyun ecs DeleteSnapshot --SnapshotId <快照ID>` |
| 回滚磁盘 | `aliyun ecs ResetDisk --DiskId <磁盘ID> --SnapshotId <快照ID>` |

### 镜像管理

| 操作 | 命令示例 |
|------|----------|
| 查询镜像列表 | `aliyun ecs DescribeImages` |
| 创建自定义镜像 | `aliyun ecs CreateImage --InstanceId <实例ID>` |
| 删除镜像 | `aliyun ecs DeleteImage --ImageId <镜像ID>` |
| 复制镜像 | `aliyun ecs CopyImage --ImageId <镜像ID> --DestinationRegionId <目标地域>` |

### 安全组管理

| 操作 | 命令示例 |
|------|----------|
| 查询安全组列表 | `aliyun ecs DescribeSecurityGroups` |
| 查询安全组规则 | `aliyun ecs DescribeSecurityGroupAttribute --SecurityGroupId <安全组ID>` |
| 创建安全组 | `aliyun ecs CreateSecurityGroup --RegionId <地域>` |
| 添加入方向规则 | `aliyun ecs AuthorizeSecurityGroup --SecurityGroupId <安全组ID> --IpProtocol tcp --PortRange 22/22 --SourceCidrIp 0.0.0.0/0` |
| 添加出方向规则 | `aliyun ecs AuthorizeSecurityGroupEgress --SecurityGroupId <安全组ID> --IpProtocol tcp --PortRange 80/80 --DestCidrIp 0.0.0.0/0` |
| 删除安全组 | `aliyun ecs DeleteSecurityGroup --SecurityGroupId <安全组ID>` |

### 密钥对管理

| 操作 | 命令示例 |
|------|----------|
| 查询密钥对列表 | `aliyun ecs DescribeKeyPairs` |
| 创建密钥对 | `aliyun ecs CreateKeyPair --KeyPairName <密钥对名称> --RegionId <地域>` |
| 导入密钥对 | `aliyun ecs ImportKeyPair --KeyPairName <密钥对名称> --PublicKeyBody <公钥内容>` |
| 绑定密钥对 | `aliyun ecs AttachKeyPair --InstanceIds '["<实例ID>"]' --KeyPairName <密钥对名称>` |
| 解绑密钥对 | `aliyun ecs DetachKeyPair --InstanceIds '["<实例ID>"]' --KeyPairName <密钥对名称>` |
| 删除密钥对 | `aliyun ecs DeleteKeyPairs --KeyPairNames <密钥对名称>` |

### 弹性网卡管理

| 操作 | 命令示例 |
|------|----------|
| 查询弹性网卡列表 | `aliyun ecs DescribeNetworkInterfaces` |
| 创建弹性网卡 | `aliyun ecs CreateNetworkInterface --RegionId <地域> --VSwitchId <交换机ID>` |
| 挂载弹性网卡 | `aliyun ecs AttachNetworkInterface --NetworkInterfaceId <网卡ID> --InstanceId <实例ID>` |
| 卸载弹性网卡 | `aliyun ecs DetachNetworkInterface --NetworkInterfaceId <网卡ID> --InstanceId <实例ID>` |
| 删除弹性网卡 | `aliyun ecs DeleteNetworkInterface --NetworkInterfaceId <网卡ID>` |

### 地域与可用区

| 操作 | 命令示例 |
|------|----------|
| 查询地域列表 | `aliyun ecs DescribeRegions` |
| 查询可用区列表 | `aliyun ecs DescribeZones --RegionId <地域ID>` |

### 实例规格

| 操作 | 命令示例 |
|------|----------|
| 查询实例规格族 | `aliyun ecs DescribeInstanceTypeFamilies` |
| 查询实例规格详情 | `aliyun ecs DescribeInstanceTypes` |

## 高级用法

### 使用过滤器

```bash
# 按状态过滤实例
aliyun ecs DescribeInstances --RegionId cn-hangzhou --Status Running

# 按标签过滤
aliyun ecs DescribeInstances --RegionId cn-hangzhou --Tag.1.Key Environment --Tag.1.Value Production
```

### 分页查询

```bash
# 分页查询，每页10条，查询第1页
aliyun ecs DescribeInstances --RegionId cn-hangzhou --PageSize 10 --PageNumber 1
```

### 输出格式

```bash
# JSON格式输出
aliyun ecs DescribeInstances --RegionId cn-hangzhou --output json

# 表格格式输出
aliyun ecs DescribeInstances --RegionId cn-hangzhou --output table
```

## 执行前检查

**重要**：示例命令仅展示基本用法。执行前请评估：

1. 示例参数是否满足实际需求？
2. 是否需要额外参数（如标签、安全组、资源组等）？
3. 不确定时，**必须先查看帮助**：

```bash
aliyun ecs <API名称> --help
```

**以下场景必须查看帮助：**
- 创建资源（RunInstances、CreateDisk、CreateSecurityGroup 等）
- 修改配置（ResizeDisk、ModifyInstanceAttribute 等）
- 涉及安全/网络的规则设置（AuthorizeSecurityGroup 等）

---

## 获取帮助

### 查询API列表

```bash
aliyun ecs --help
```

### 查询具体API参数

```bash
aliyun ecs <API名称> --help

# 示例
aliyun ecs DescribeInstances --help
aliyun ecs RunInstances --help
```

## 常用API速查

### 实例生命周期
- `CreateInstance` - 创建实例
- `RunInstances` - 批量创建实例
- `StartInstance` - 启动实例
- `StopInstance` - 停止实例
- `RebootInstance` - 重启实例
- `DeleteInstance` - 释放实例
- `DescribeInstances` - 查询实例详情
- `DescribeInstanceStatus` - 查询实例状态

### 磁盘与存储
- `CreateDisk` - 创建磁盘
- `AttachDisk` - 挂载磁盘
- `DetachDisk` - 卸载磁盘
- `DeleteDisk` - 删除磁盘
- `ResizeDisk` - 扩容磁盘
- `DescribeDisks` - 查询磁盘列表

### 快照与镜像
- `CreateSnapshot` - 创建快照
- `DeleteSnapshot` - 删除快照
- `DescribeSnapshots` - 查询快照列表
- `CreateImage` - 创建镜像
- `DeleteImage` - 删除镜像
- `DescribeImages` - 查询镜像列表

### 网络与安全
- `CreateSecurityGroup` - 创建安全组
- `AuthorizeSecurityGroup` - 添加安全组入方向规则
- `AuthorizeSecurityGroupEgress` - 添加安全组出方向规则
- `DescribeSecurityGroups` - 查询安全组列表
- `CreateNetworkInterface` - 创建弹性网卡
- `AttachNetworkInterface` - 挂载弹性网卡

### 密钥与访问
- `CreateKeyPair` - 创建密钥对
- `ImportKeyPair` - 导入密钥对
- `AttachKeyPair` - 绑定密钥对
- `DetachKeyPair` - 解绑密钥对
- `DescribeKeyPairs` - 查询密钥对列表
