---
name: upgrade-alinux-kernel
version: 1.0.0
description: Alibaba Cloud Linux (Alinux) 升级Linux操作系统内核。当用户需要升级、更新或变更ECS Linux实例的内核版本时使用。
layer: system
lifecycle: maintenance
---

# Alinux 升级内核

Alinux 操作系统内核升级指南。

## 前提条件

1. 升级前提示用户以下警示信息，需要用户确认
  * 内核升级需要**重启**实例或者设备，会导致业务中断（约数分钟）；
  * 系统盘数据是否已经做了备份；

## 重要警告

- 重启前务必确认 GRUB 启动项已设置为正确的内核。
- 如果输入没有说明升级那个内核版本，则默认升级最新版本，并提示用户当前可以升级的内核所有版本，并选择了最新版本升级，让用户进行确认。

## 操作流程

复制以下清单并跟踪进度：

```
操作进度：
- [ ] 步骤 1：查看当前的内核版本
- [ ] 步骤 2：检查可用的内核更新
- [ ] 步骤 3：安装/升级内核
- [ ] 步骤 4：升级内核相关组件
- [ ] 步骤 5：更新 GRUB 启动配置
- [ ] 步骤 6：重启系统
```

## 分步操作说明

### 步骤 1：查看当前的内核版本

确定操作系统类型：

```bash
uname -r 
```

### 步骤 2-3-4：升级内核（按升级类型）

* 查看可用的内核版本:

```bash
sudo yum list available kernel
```

* 查看待升级内核相关软件包列表
  1. 获取当前内核版本

  ```bash
  uname -r
  ```

  2. 根据上一步输出的版本号，查询相关组件

  ```bash
  rpm -qa | grep <当前内核版本号>
  ```

#### 升级最新版本

* 升级内核到最新版本
```bash
sudo yum update kernel -y
```

* 升级查询到的内核相关组件

```bash
sudo yum update <待升级内核相关软件包> -y
```

#### 升级指定版本
* 升级到指定版本

```bash
sudo yum install kernel-<version> -y
```
* 升级查询到的内核相关组件

```bash
sudo yum update <待升级内核相关软件包>-<version> -y
```

### 步骤 5：更新 GRUB 配置

```bash
# 列出已安装的内核
sudo grubby --info=ALL | grep ^kernel

# 设置新内核为默认启动项
sudo grubby --set-default /boot/vmlinuz-<new-kernel-version>

# 验证
sudo grubby --default-kernel
```

### 步骤 6：重启并验证

```bash
sudo reboot
```

## 故障排除

### Alibaba Cloud Linux 3.8 升级内核时 dracut 报错

Alibaba Cloud Linux 3.8 版本（镜像日期 20230727~20230925）升级内核时，dracut 会因重复的内核模块配置而报错。该报错不影响内核包安装，但可通过修改 dracut 配置消除。

详细的问题描述、影响范围和修复步骤，见 [fix-alinux3.8-kernel-upgrade-error.md](./reference/fix-alinux3.8-kernel-upgrade-error.md)。
