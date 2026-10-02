# 阿里云快照备份与恢复

适用范围：阿里云 ECS 云盘的块级快照备份、自动快照策略、镜像创建、跨地域复制。
此层面由阿里云平台管理，独立于操作系统，即使系统损坏也不受影响。

---

## 操作准则

1. **必须先检测阿里云环境**，确认是 ECS 实例后再继续
2. **必须向用户说明将执行的操作并获得明确确认**，再调用任何 aliyun-cli 命令
3. 快照会产生**阿里云费用**，操作前告知用户
4. 回滚磁盘会**覆盖当前数据**，务必二次确认

---

## JSON 解析辅助函数

aliyun-cli 返回 JSON，用 python3 解析比 grep 更可靠：

```bash
# 用法：echo "$JSON" | json_get '.SnapshotId'
json_get() {
    python3 -c "import sys,json; d=json.load(sys.stdin); print(eval('d' + sys.argv[1].replace('.','[\"') .replace('[\"','[\"',1) + '\"]' if '.' in sys.argv[1] else 'd[\"' + sys.argv[1].lstrip('.') + '\"]'))" "$1" 2>/dev/null
}
```

如果系统有 `jq`（`sudo yum install -y jq`），可以直接用 `jq -r '.SnapshotId'`，更简洁。下面的示例同时提供两种写法。

---

## 第一步：检测阿里云 ECS 环境

```bash
INSTANCE_ID=$(curl -s --connect-timeout 3 \
    http://100.100.100.200/latest/meta-data/instance-id 2>/dev/null)

if [ -z "$INSTANCE_ID" ]; then
    echo "[FAIL] 未检测到阿里云 ECS 环境，无法使用云快照功能"
    exit 1
fi

REGION=$(curl -s http://100.100.100.200/latest/meta-data/region-id)
ZONE=$(curl -s http://100.100.100.200/latest/meta-data/zone-id)

echo "[OK] 检测到阿里云 ECS 环境"
echo "  实例 ID：$INSTANCE_ID"
echo "  地域：  $REGION"
echo "  可用区：$ZONE"
```

---

## 第二步：安装并配置 aliyun-cli

Alinux 4 仓库源内置了aliyun-cli，直接 dnf 或 yum 安装

```bash
dnf install aliyun-cli 
# or
yum install aliyun-cli
```

### 配置认证方式

**方式 A：RAM 角色（推荐——ECS 绑定 RAM 角色，无需 AK）**

```bash
RAM_ROLE=$(curl -s http://100.100.100.200/latest/meta-data/ram/security-credentials/)

if [ -n "$RAM_ROLE" ]; then
    echo "[OK] 检测到 RAM 角色：$RAM_ROLE"
    aliyun configure set \
        --mode EcsRamRole \
        --ram-role-name "$RAM_ROLE" \
        --region "$REGION"
else
    echo "[WARN] 未绑定 RAM 角色，将使用 AccessKey 认证"
fi
```

**方式 B：AccessKey（手动输入）**

```bash
# 交互式配置（会提示输入 AccessKey ID 和 Secret）
aliyun configure
```

### 验证配置

```bash
aliyun ecs DescribeInstanceAttribute \
    --InstanceId "$INSTANCE_ID" \
    --region "$REGION" \
    | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'实例名：{d[\"InstanceName\"]}  状态：{d[\"Status\"]}')"
```

---

## 云盘快照备份

### 查询实例挂载的云盘

```bash
aliyun ecs DescribeDisks \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    --output cols=DiskId,DiskName,Category,Size,Device,Status \
    --output rows=Disks.Disk
```

### 对单块云盘创建快照

```bash
DISK_ID="d-bp1xxxxxxxxxxxx"
SNAPSHOT_NAME="manual-$(date +%Y%m%d-%H%M%S)"

echo "即将对云盘 $DISK_ID 创建快照，名称：$SNAPSHOT_NAME"
echo "此操作将产生阿里云快照存储费用，是否继续？[y/N]"
read -r CONFIRM
[ "$CONFIRM" != "y" ] && echo "已取消" && exit 0

SNAP_RESULT=$(aliyun ecs CreateSnapshot \
    --region "$REGION" \
    --DiskId "$DISK_ID" \
    --SnapshotName "$SNAPSHOT_NAME" \
    --Description "手动备份 $(date '+%Y-%m-%d %H:%M:%S')")

# 用 jq 或 python3 解析 JSON
SNAPSHOT_ID=$(echo "$SNAP_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['SnapshotId'])")
# 如果有 jq：SNAPSHOT_ID=$(echo "$SNAP_RESULT" | jq -r '.SnapshotId')

echo "[OK] 快照创建请求已提交"
echo "  快照 ID：$SNAPSHOT_ID"
echo "  状态：进行中（快照在后台异步完成，不影响磁盘使用）"
```

### 批量备份实例所有云盘

```bash
#!/bin/bash
set -euo pipefail

REGION=$(curl -s http://100.100.100.200/latest/meta-data/region-id)
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

# 获取所有云盘 ID
DISK_IDS=$(aliyun ecs DescribeDisks \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    | python3 -c "
import sys, json
disks = json.load(sys.stdin)['Disks']['Disk']
for d in disks:
    print(d['DiskId'])
")

echo "发现以下云盘，即将全部创建快照："
echo "$DISK_IDS"
echo "确认执行？[y/N]"
read -r CONFIRM
[ "$CONFIRM" != "y" ] && echo "已取消" && exit 0

for DISK_ID in $DISK_IDS; do
    SNAP_NAME="batch-${TIMESTAMP}-${DISK_ID}"
    RESULT=$(aliyun ecs CreateSnapshot \
        --region "$REGION" \
        --DiskId "$DISK_ID" \
        --SnapshotName "$SNAP_NAME")
    SNAP_ID=$(echo "$RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['SnapshotId'])")
    echo "[OK] 云盘 $DISK_ID → 快照 $SNAP_ID"
done
```

### 查询快照状态

```bash
# 查询指定快照的进度
aliyun ecs DescribeSnapshots \
    --region "$REGION" \
    --SnapshotIds "[\"$SNAPSHOT_ID\"]" \
    --output cols=SnapshotId,SnapshotName,Status,Progress,SourceDiskSize \
    --output rows=Snapshots.Snapshot

# 列出当前实例的所有快照
aliyun ecs DescribeSnapshots \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    --output cols=SnapshotId,SnapshotName,CreationTime,Status,SourceDiskSize \
    --output rows=Snapshots.Snapshot
```

---

## 自动快照策略

### 创建自动快照策略

```bash
# 每天凌晨 2 点自动快照，保留 7 天
POLICY_RESULT=$(aliyun ecs CreateAutoSnapshotPolicy \
    --region "$REGION" \
    --autoSnapshotPolicyName "daily-backup-policy" \
    --timePoints '["2"]' \
    --repeatWeekdays '["1","2","3","4","5","6","7"]' \
    --retentionDays 7)

POLICY_ID=$(echo "$POLICY_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['AutoSnapshotPolicyId'])")
echo "[OK] 自动快照策略已创建：$POLICY_ID"
```

### 将策略应用到云盘

```bash
aliyun ecs ApplyAutoSnapshotPolicy \
    --region "$REGION" \
    --autoSnapshotPolicyId "$POLICY_ID" \
    --diskIds "[\"$DISK_ID\"]"

echo "[OK] 自动快照策略已应用到云盘 $DISK_ID"
```

### 查看现有策略

```bash
aliyun ecs DescribeAutoSnapshotPolicyEX \
    --region "$REGION" \
    --output cols=AutoSnapshotPolicyId,AutoSnapshotPolicyName,TimePoints,RetentionDays,Status \
    --output rows=AutoSnapshotPolicies.AutoSnapshotPolicy
```

---

## 快照恢复

> **回滚是高危操作（覆盖当前数据、不可撤销），建议引导用户到阿里云控制台操作。**
> 控制台提供可视化确认流程，比 CLI 更安全。

### 回滚云盘（引导用户到控制台）

告知用户：

1. 打开实例控制台：`https://ecs.console.aliyun.com/server/${INSTANCE_ID}/detail?regionId=${REGION}#/`
2. 备份 → 选择要回滚的云盘备份 → 点击「回滚云盘」
3. 选择目标快照，确认执行
4. **系统盘回滚**需要先停止实例（控制台会自动提示）
5. **数据盘回滚**需要先卸载数据盘

提醒用户：
- 回滚会**覆盖云盘当前所有数据，不可撤销**
- 系统盘回滚需要**停机**，业务会中断
- 建议回滚前先对当前状态**再创建一个快照**作为保底

### 从快照新建云盘（不影响当前数据，可 CLI 执行）

这是更安全的恢复方式——从快照创建新云盘，挂载后手动拷贝所需文件：

```bash
SNAPSHOT_ID="s-bp1xxxxxxxxxxxx"

NEW_DISK_RESULT=$(aliyun ecs CreateDisk \
    --region "$REGION" \
    --ZoneId "$ZONE" \
    --SnapshotId "$SNAPSHOT_ID" \
    --DiskName "restored-$(date +%Y%m%d)" \
    --DiskCategory cloud_essd)

NEW_DISK_ID=$(echo "$NEW_DISK_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['DiskId'])")
echo "[OK] 新云盘已创建：$NEW_DISK_ID"
echo "  可挂载到实例后手动拷贝所需文件，无需停机"

# 挂载新云盘到当前实例
aliyun ecs AttachDisk \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    --DiskId "$NEW_DISK_ID"

echo "[OK] 云盘已挂载，使用 lsblk 查看设备名后手动挂载文件系统"
```

---

## 镜像备份（整机备份）

镜像包含系统盘 + 所有数据盘快照，可用于创建完全相同的新实例。

```bash
IMAGE_NAME="image-$(hostname)-$(date +%Y%m%d)"

echo "即将基于当前实例创建整机镜像（含系统盘快照）"
echo "镜像名称：$IMAGE_NAME"
echo "创建镜像期间实例可正常运行，但可能有轻微 I/O 影响"
echo "确认执行？[y/N]"
read -r CONFIRM
[ "$CONFIRM" != "y" ] && echo "已取消" && exit 0

IMAGE_RESULT=$(aliyun ecs CreateImage \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    --ImageName "$IMAGE_NAME" \
    --Description "整机备份 $(date '+%Y-%m-%d %H:%M:%S')")

IMAGE_ID=$(echo "$IMAGE_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['ImageId'])")
echo "[OK] 镜像创建请求已提交"
echo "  镜像 ID：$IMAGE_ID（创建完成前状态为 Creating）"

# 查询镜像状态
aliyun ecs DescribeImages \
    --region "$REGION" \
    --ImageId "$IMAGE_ID" \
    --output cols=ImageId,ImageName,Status,Progress \
    --output rows=Images.Image
```

---

## 快照跨地域复制

用于异地容灾备份：

```bash
SOURCE_SNAPSHOT_ID="s-bp1xxxxxxxxxxxx"
TARGET_REGION="cn-beijing"

echo "即将将快照 $SOURCE_SNAPSHOT_ID 复制到地域 $TARGET_REGION"
echo "跨地域复制会产生额外费用，确认执行？[y/N]"
read -r CONFIRM
[ "$CONFIRM" != "y" ] && echo "已取消" && exit 0

COPY_RESULT=$(aliyun ecs CopySnapshot \
    --region "$REGION" \
    --SnapshotId "$SOURCE_SNAPSHOT_ID" \
    --DestinationRegionId "$TARGET_REGION" \
    --DestinationSnapshotName "cross-region-$(date +%Y%m%d)")

echo "[OK] 跨地域复制已提交"
echo "  目标地域：$TARGET_REGION"
echo "$COPY_RESULT" | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'  目标快照 ID：{d.get(\"SnapshotId\", \"见返回结果\")}')" 2>/dev/null || true
```

---

## 快照清理

```bash
# 列出超过 30 天的旧快照
aliyun ecs DescribeSnapshots \
    --region "$REGION" \
    --InstanceId "$INSTANCE_ID" \
    | python3 -c "
import sys, json
from datetime import datetime, timedelta
cutoff = datetime.utcnow() - timedelta(days=30)
snaps = json.load(sys.stdin)['Snapshots']['Snapshot']
for s in snaps:
    ct = datetime.strptime(s['CreationTime'][:19], '%Y-%m-%dT%H:%M:%S')
    if ct < cutoff:
        print(f'{s[\"SnapshotId\"]}  {s[\"SnapshotName\"]}  {s[\"CreationTime\"][:10]}')
"

# 删除指定快照（谨慎操作）
SNAP_TO_DELETE="s-bp1xxxxxxxxxxxx"
echo "即将删除快照 $SNAP_TO_DELETE，此操作不可撤销"
echo "输入 YES 确认："
read -r CONFIRM
[ "$CONFIRM" != "YES" ] && echo "已取消" && exit 0

aliyun ecs DeleteSnapshot \
    --region "$REGION" \
    --SnapshotId "$SNAP_TO_DELETE"

echo "[OK] 快照 $SNAP_TO_DELETE 已删除"
```

---

## 常见错误处理

| 错误信息                      | 原因                 | 解决方法                                                 |
| ----------------------------- | -------------------- | -------------------------------------------------------- |
| `InvalidAccessKeyId`        | AK 无效或已失效      | 重新配置 `aliyun configure`                            |
| `Forbidden.RAM`             | RAM 角色权限不足     | 在 RAM 控制台为角色添加 `EcsFullAccess` 或快照相关权限 |
| `IncorrectDiskStatus`       | 云盘状态不允许操作   | 检查云盘是否已挂载/正在使用                              |
| `OperationConflict`         | 实例正在进行其他操作 | 等待当前操作完成后重试                                   |
| `SnapshotCreationFail`      | 快照创建失败         | 检查账户余额、云盘状态                                   |
| `command not found: aliyun` | aliyun-cli 未安装    | 执行第二步的安装流程                                     |
