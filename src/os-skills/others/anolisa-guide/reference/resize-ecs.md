> **爬取时间**: 2026-06-17 16:29:25
> **原文链接**: https://help.aliyun.com/zh/alinux/resize-ecs-online-in-one-sentence
> **文档更新**: 2026-05-26T17:16:19+08:00

---

## **背景信息**

-   ECS 数据盘在线扩容是运维中的高频操作，当收到磁盘空间告警时需要快速响应。
    
-   传统的扩容流程分为两个阶段：**云平台扩容**（通过阿里云 API 扩展云盘容量）和 **OS 扩容**（在操作系统内扩展文件系统）。两个阶段涉及不同的命令和工具，且 XFS、EXT4、Btrfs 三种文件系统的扩容命令各不相同，操作不当可能导致数据丢失。
    
-   裸跑 Agent 通常无法获知 aliyun-cli 工具的存在，依旧要求用户从阿里云控制台手动操作。通过加载Skills，Agentic OS 可以一句话完成“云平台 + OS”两阶段全链路扩容。
    

## **前提条件**

-   已创建 Agentic OS ECS 实例。
    

> **重要：**阿里云云盘**只能扩容不能缩容**，扩容前建议创建快照备份。（备份创建亦可自然语言发起）

## **操作步骤**

### **步骤一：发起扩容指令**

1.  在 Agentic OS 终端中，向 Agent 发送以下指令。
    

```
磁盘在线扩容1G
```

2.  Agent 将自动执行以下操作：
    
    -   检测当前 ECS 实例的磁盘信息和挂载点。
        
    -   识别目标数据盘的磁盘 ID 和当前容量。
        
    -   识别文件系统类型（XFS / EXT4 / Btrfs）。
        

```
+ I need to first check the current disk layout to understand which disk/partition to expand before proceeding with the resize operations.
+ I'll help you expand your disk by 1GB online. Let me first check your current disk configuration.
✓ Shell lsblk -f (Check current disk and filesystem layout)
    NAME        FSTYPE FSVER LABEL UUID                                 FSAVAIL FSUSE% MOUNTPOINTS
    nvme0n1
    |-nvme0n1p1
    |-nvme0n1p2 vfat   FAT16       ED2E-C891                              193.6M     3% /boot/efi
    `-nvme0n1p3 ext4   1.0   root  d058a4e7-5028-4759-a668-c51494aab9d9   32.2G    13% /
+ I've identified the disk structure; now I need to check the current cloud disk size via阿里云 CLI to determine the target size for the 1GB expansion.
+ I can see your disk structure. The main system disk is nvme0n1 with partition nvme0n1p3 (ext4 filesystem) mounted at /.
```

### **步骤二：执行扩容方案**

Agent 将依次执行两阶段扩容：

-   **云平台阶段**：通过 aliyun-cli 调用 `ResizeDisk` API 扩展云盘容量。
    
-   **OS 阶段**：根据文件系统类型自动选择对应的扩容命令。
    

> **说明：**Agent 会根据检测到的文件系统类型自动选择正确的扩容命令：XFS 使用 `xfs_growfs`，EXT4 使用 `resize2fs`，Btrfs 使用 `btrfs filesystem resize`。

```
Shell aliyun ecs ResizeDisk --DiskId d-bp146ieahkdeuzpi7m9c --NewSize 42 -Type online (Expand cloud disk from 41GB to 42GB online)
  ERROR: parse failed not support flag form -Type
+ The command failed due to incorrect flag format; let me retry with the correct parameter syntax using --NewSizeType instead of -Type.
+ Let me fix the command syntax:
Shell aliyun ecs ResizeDisk --DiskId d-bp146ieahkdeuzpi7m9c --NewSize 42 --Type online (Expand cloud disk from 41GB to 42GB online)
  {
    "RequestId": "FDEC6F69-D7A8-509E-BC23-C93CA9F63F78"
  }
+ Cloud disk expansion succeeded, now I need to check if growpart is installed before expanding the partition in the OS.
+ Excellent! The cloud disk has been expanded to 42GB. Now I need to expand the partition and filesystem in the OS. Let me check if the required tools are installed:
Shell which growpart xfs_growfs resize2fs 2>/dev/null || echo "Some tools may need installation" (Check if扩容 tools are installed)
  which growpart xfs_growfs resize2fs 2>/dev/null || echo "Some tools may need installation"
  Allow execution of: 'which, echo'?
  1. Yes, allow once
  2. Yes, allow always ...
  3. No, suggest changes (esc)
```

### **步骤三：验证扩容结果**

1.  扩容完成后，Agent 会自动执行验证命令，展示扩容前后的磁盘容量对比。
    
2.  确认文件系统容量已正确扩展至目标值。
    

```
Shell resize2fs /dev/nvme0n1p3 (Expand ext4 filesystem to fill the partition)
  Filesystem at /dev/nvme0n1p3 is mounted on /; on-line resizing required
  old_desc_blocks = 3, new_desc_blocks = 3
  The filesystem on /dev/nvme0n1p3 is now 10958331 (4k) blocks long.
  resize2fs 1.47.0 (5-Feb-2023)
+ The filesystem expansion completed successfully, so I should verify the final result by checking the disk layout and available space.
+ Perfect! Let me verify the expansion was successful:
Shell lsblk /dev/nvme0n1 && echo "---" && df -h / (Verify disk expansion results)
  NAME          MAJ:MIN RM  SIZE RO TYPE MOUNTPOINTS
  nvme0n1       259:0    0   42G  0 disk
  |-nvme0n1p1   259:1    0    1M  0 part
  |-nvme0n1p2   259:2    0  200M  0 part /boot/efi
  `-nvme0n1p3   259:3    0 41.8G  0 part /
  ---
  Filesystem      Size  Used Avail Use% Mounted on
  /dev/nvme0n1p3   41G  5.0G   35G  13% /
+ All expansion steps completed successfully - I should summarize the results showing the before/after disk capacity changes.
+ Disk expansion completed successfully!
Summary:
  - ✅Cloud disk expanded: 41GB → 42GB (online)
  - ✅Partition nvme0n1p3 expanded: 39.8GB → 41.8GB
  - ✅Filesystem expanded: 39GB → 41GB available
Your root filesystem now has approximately 1GB additional space (from ~39GB to ~41GB total capacity).
```

## **注意事项**

1.  阿里云云盘只能扩容不能缩容，操作前务必确认扩容需求。
    
2.  扩容前建议创建磁盘快照，以备在异常情况下恢复磁盘数据（创建备份亦可自然语言发起）
