# 文件系统管理指南

管理 Linux 文件系统、LVM、RAID、权限和存储的完整参考。

## 目录

1. [文件系统类型](#文件系统类型)
2. [逻辑卷管理器 (LVM)](#逻辑卷管理器-lvm)
3. [RAID 配置](#raid-配置)
4. [挂载和 fstab](#挂载和-fstab)
5. [权限和 ACL](#权限和-acl)
6. [磁盘使用管理](#磁盘使用管理)

## 文件系统类型

### 对比

| 文件系统 | 最佳用途 | 最大文件大小 | 快照 | 说明 |
|----------|----------|--------------|------|------|
| **ext4** | 通用 | 16 TB | 否 | 大多数发行版的默认选择，成熟稳定 |
| **XFS** | 大文件、数据库 | 8 EB | 否 | RHEL 默认，性能优秀 |
| **Btrfs** | 快照、写时复制 | 16 EB | 是 | 现代特性，写时复制 |
| **ZFS** | 企业级、数据完整性 | 16 EB | 是 | 不在主线内核中，适用于 NAS/存储 |

### 创建文件系统

**ext4:**
```bash
sudo mkfs.ext4 /dev/sdb1
sudo mkfs.ext4 -L mylabel /dev/sdb1     # 带标签
```

**XFS:**
```bash
sudo mkfs.xfs /dev/sdb1
sudo mkfs.xfs -L mylabel /dev/sdb1
```

**Btrfs:**
```bash
sudo mkfs.btrfs /dev/sdb1
sudo mkfs.btrfs -L mylabel /dev/sdb1
```

## 逻辑卷管理器 (LVM)

### LVM 概念

**三层结构:**
1. **物理卷 (PV)** - 原始磁盘/分区
2. **卷组 (VG)** - PV 的池
3. **逻辑卷 (LV)** - 从 VG 创建的虚拟分区

### 创建 LVM 设置

**步骤 1: 创建物理卷**
```bash
sudo pvcreate /dev/sdb
sudo pvcreate /dev/sdc

# 查看 PV
sudo pvdisplay
sudo pvs
```

**步骤 2: 创建卷组**
```bash
sudo vgcreate vg_data /dev/sdb /dev/sdc

# 查看 VG
sudo vgdisplay
sudo vgs
```

**步骤 3: 创建逻辑卷**
```bash
# 固定大小
sudo lvcreate -L 10G -n lv_data vg_data

# VG 百分比
sudo lvcreate -l 100%FREE -n lv_data vg_data

# 查看 LV
sudo lvdisplay
sudo lvs
```

**步骤 4: 创建文件系统**
```bash
sudo mkfs.ext4 /dev/vg_data/lv_data
```

**步骤 5: 挂载**
```bash
sudo mkdir /mnt/data
sudo mount /dev/vg_data/lv_data /mnt/data
```

### 扩展 LVM 卷

**扩展 LV:**
```bash
# 增加 5GB
sudo lvextend -L +5G /dev/vg_data/lv_data

# 使用所有可用空间
sudo lvextend -l +100%FREE /dev/vg_data/lv_data
```

**调整文件系统大小:**
```bash
# ext4
sudo resize2fs /dev/vg_data/lv_data

# XFS
sudo xfs_growfs /mnt/data

# Btrfs
sudo btrfs filesystem resize max /mnt/data
```

**一步扩展和调整:**
```bash
sudo lvextend -L +5G --resizefs /dev/vg_data/lv_data
```

### 缩减 LVM 卷

**警告:** 如果不小心操作，可能导致数据丢失！

**仅适用于 ext4 (XFS 无法缩减):**
```bash
# 先卸载
sudo umount /mnt/data

# 检查文件系统
sudo e2fsck -f /dev/vg_data/lv_data

# 先调整文件系统大小
sudo resize2fs /dev/vg_data/lv_data 8G

# 然后缩减 LV
sudo lvreduce -L 8G /dev/vg_data/lv_data

# 重新挂载
sudo mount /dev/vg_data/lv_data /mnt/data
```

### LVM 快照

```bash
# 创建快照 (原始大小的 10% 用于变更)
sudo lvcreate -L 1G -s -n lv_data_snap /dev/vg_data/lv_data

# 挂载快照
sudo mkdir /mnt/snapshot
sudo mount /dev/vg_data/lv_data_snap /mnt/snapshot

# 从快照恢复
sudo lvconvert --merge /dev/vg_data/lv_data_snap

# 删除快照
sudo lvremove /dev/vg_data/lv_data_snap
```

## RAID 配置

### RAID 级别

| 级别 | 描述 | 最少磁盘 | 可用空间 | 容错能力 |
|------|------|----------|----------|----------|
| RAID 0 | 条带化 | 2 | 100% | 无 (任何磁盘故障 = 数据丢失) |
| RAID 1 | 镜像 | 2 | 50% | N-1 个磁盘 |
| RAID 5 | 条带化 + 奇偶校验 | 3 | (N-1)/N | 1 个磁盘 |
| RAID 6 | 条带化 + 双重奇偶校验 | 4 | (N-2)/N | 2 个磁盘 |
| RAID 10 | 镜像 + 条带化 | 4 | 50% | 每个镜像 1 个磁盘 |

### 使用 mdadm 创建软件 RAID

**安装 mdadm:**
```bash
sudo apt install mdadm              # Ubuntu/Debian
sudo dnf install mdadm              # RHEL/Fedora
```

**创建 RAID 1 (镜像):**
```bash
sudo mdadm --create /dev/md0 \
    --level=1 \
    --raid-devices=2 \
    /dev/sdb /dev/sdc

# 监控创建过程
watch cat /proc/mdstat
```

**创建 RAID 5:**
```bash
sudo mdadm --create /dev/md0 \
    --level=5 \
    --raid-devices=3 \
    /dev/sdb /dev/sdc /dev/sdd
```

**创建文件系统并挂载:**
```bash
sudo mkfs.ext4 /dev/md0
sudo mkdir /mnt/raid
sudo mount /dev/md0 /mnt/raid
```

**保存 RAID 配置:**
```bash
sudo mdadm --detail --scan | sudo tee -a /etc/mdadm/mdadm.conf
sudo update-initramfs -u
```

**检查 RAID 状态:**
```bash
cat /proc/mdstat
sudo mdadm --detail /dev/md0
```

**添加备用磁盘:**
```bash
sudo mdadm --add /dev/md0 /dev/sde
```

**移除故障磁盘:**
```bash
sudo mdadm --fail /dev/md0 /dev/sdb
sudo mdadm --remove /dev/md0 /dev/sdb
# 更换磁盘
sudo mdadm --add /dev/md0 /dev/sdf
```

## 挂载和 fstab

### 手动挂载

```bash
# 挂载文件系统
sudo mount /dev/sdb1 /mnt/data

# 带选项挂载
sudo mount -o rw,noexec,nosuid /dev/sdb1 /mnt/data

# 按标签挂载
sudo mount LABEL=mylabel /mnt/data

# 按 UUID 挂载
sudo mount UUID=xxxx-xxxx /mnt/data

# 用不同选项重新挂载
sudo mount -o remount,ro /mnt/data

# 卸载
sudo umount /mnt/data
```

### /etc/fstab 配置

**格式:**
```
<设备> <挂载点> <类型> <选项> <转储> <检查>
```

**示例:**
```bash
# /etc/fstab

# 按设备
/dev/sdb1  /mnt/data  ext4  defaults  0  2

# 按 UUID (推荐)
UUID=xxx-xxx  /mnt/data  ext4  defaults  0  2

# 按标签
LABEL=mylabel  /mnt/data  ext4  defaults  0  2

# 带特定选项
UUID=xxx  /mnt/data  ext4  rw,noexec,nosuid  0  2

# NFS 挂载
server:/export  /mnt/nfs  nfs  defaults  0  0

# 临时文件系统
tmpfs  /tmp  tmpfs  defaults,noatime,mode=1777  0  0
```

**常用挂载选项:**
- `defaults` - rw, suid, dev, exec, auto, nouser, async
- `ro` - 只读
- `rw` - 读写
- `noexec` - 不允许程序执行
- `nosuid` - 忽略 SUID 位
- `nodev` - 不解释块特殊设备
- `noatime` - 不更新访问时间 (性能)
- `nodiratime` - 不更新目录访问时间
- `nofail` - 设备缺失时不导致启动失败

**应用 fstab 变更:**
```bash
sudo mount -a                      # 挂载 fstab 中所有
sudo findmnt --verify              # 验证 fstab 语法
```

## 权限和 ACL

### 标准权限

**权限类型:**
- **r** (4) - 读
- **w** (2) - 写
- **x** (1) - 执行

**三个组:**
- 所有者
- 组
- 其他

**示例:**
```bash
# 符号模式
chmod u+x file                     # 为用户添加执行权限
chmod g+w file                     # 为组添加写权限
chmod o-r file                     # 移除其他人的读权限
chmod a+x file                     # 为所有人添加执行权限

# 数字模式
chmod 644 file                     # rw-r--r--
chmod 755 file                     # rwxr-xr-x
chmod 600 file                     # rw-------
chmod 777 file                     # rwxrwxrwx (避免使用！)

# 递归
chmod -R 755 directory
```

**更改所有权:**
```bash
chown user file                    # 更改所有者
chown user:group file              # 更改所有者和组
chown -R user:group directory      # 递归
chgrp group file                   # 仅更改组
```

**特殊权限:**
```bash
# SUID (Set User ID) - 4000
chmod u+s executable               # 以文件所有者身份运行
chmod 4755 executable

# SGID (Set Group ID) - 2000
chmod g+s executable               # 以文件组身份运行
chmod g+s directory                # 新文件继承目录组
chmod 2755 directory

# Sticky bit - 1000
chmod +t directory                 # 只有所有者可以删除文件
chmod 1777 /tmp                    # /tmp 的典型设置
```

### 访问控制列表 (ACL)

扩展权限，超越标准的所有者/组/其他。

**查看 ACL:**
```bash
getfacl file
```

**设置 ACL:**
```bash
# 给用户特定权限
setfacl -m u:username:rw file

# 给组特定权限
setfacl -m g:groupname:rx file

# 移除 ACL
setfacl -x u:username file

# 移除所有 ACL
setfacl -b file

# 目录的默认 ACL (新文件继承)
setfacl -d -m u:username:rw directory

# 递归
setfacl -R -m u:username:rw directory
```

**复制 ACL:**
```bash
getfacl file1 | setfacl --set-file=- file2
```

## 磁盘使用管理

### 检查磁盘使用

**文件系统使用:**
```bash
df -h                              # 人类可读
df -i                              # Inode 使用
df -T                              # 显示文件系统类型
df -h /path                        # 特定挂载点
```

**目录使用:**
```bash
du -sh /path                       # 摘要
du -h --max-depth=1 /path          # 仅一层深度
du -sh /* | sort -h                # 按大小排序
ncdu /path                         # 交互式 (需安装)
```

**查找大文件:**
```bash
find /path -type f -size +100M     # 大于 100MB 的文件
find /path -type f -size +100M -exec ls -lh {} \;

# 最大的 10 个文件
find /path -type f -exec du -h {} + | sort -rh | head -10
```

**查找大目录:**
```bash
du -h /path | sort -rh | head -20
```

### 清理磁盘空间

**日志文件:**
```bash
# 查找大日志
find /var/log -type f -size +10M

# 截断日志 (不要删除 - 可能破坏应用)
sudo truncate -s 0 /var/log/large.log

# 轮转日志
sudo logrotate -f /etc/logrotate.conf

# 清理 systemd 日志
sudo journalctl --vacuum-size=500M
sudo journalctl --vacuum-time=7d
```

**软件包缓存:**
```bash
# Ubuntu/Debian
sudo apt clean
sudo apt autoremove

# RHEL/Fedora
sudo dnf clean all
```

**临时文件:**
```bash
sudo find /tmp -type f -atime +7 -delete
sudo find /var/tmp -type f -atime +30 -delete
```

**已删除但仍打开的文件:**
```bash
# 查找持有已删除文件的进程
sudo lsof | grep deleted

# 重启服务以释放
systemctl restart service_name
```

## 最佳实践

1. **始终在 fstab 中使用 UUID** (设备名称可能改变)
2. **重启前用 `mount -a` 测试 fstab**
3. **LVM 操作前备份数据**
4. **使用 LVM 以获得灵活性** (易于调整大小)
5. **定期监控 RAID 阵列**
6. **设置适当的权限** (最小权限原则)
7. **使用 noatime/nodiratime 提升性能**
8. **定期进行文件系统检查** (在维护窗口期间执行 fsck)

## 参考资料

- mount(8): `man mount`
- fstab(5): `man fstab`
- lvm(8): `man lvm`
- mdadm(8): `man mdadm`
- chmod(1): `man chmod`
- setfacl(1): `man setfacl`
