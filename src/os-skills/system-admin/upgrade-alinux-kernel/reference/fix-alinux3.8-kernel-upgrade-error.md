# 修复案例：Alibaba Cloud Linux 3.8 版本镜像升级内核报错

## 问题现象

在 Alibaba Cloud Linux 3.8 版本的镜像中升级内核版本时，安装内核包的过程中会出现 **dracut 报错信息**，但对应的内核包可以正常安装成功。

报错信息示例（dracut 提示无法找到某些内核模块）。

## 问题原因

Alibaba Cloud Linux 3.8 版本镜像为支持更多规格的机型，对镜像的 **dracut 配置**（`/etc/dracut.conf.d/virt-drivers.conf`）新增了额外的内核模块。其中部分新增的内核模块已经被 **built-in** 集成到内核 `vmlinuz` 文件中，导致在安装内核包时，dracut 工具因无法以模块形式找到这些已内置的驱动而报错。

**该报错不会影响内核软件包的安装和升级，内核包仍可正常安装成功。**

## 影响范围

镜像 ID 中日期在 **20230727 ~ 20230925** 范围内，且版本为以下的 Alibaba Cloud Linux 3 镜像：

- Alibaba Cloud Linux 3.2104 LTS 64位
- Alibaba Cloud Linux 3.2104 LTS 64位 快速启动版
- Alibaba Cloud Linux 3.2104 LTS 64位 等保2.0三级版
- Alibaba Cloud Linux 3.2104 LTS 64位 UEFI版
- Alibaba Cloud Linux 3.2104 LTS 64位 ARM版
- Alibaba Cloud Linux 3.2104 LTS 64位 ARM版 等保2.0三级版

### 确认是否受影响

运行以下命令查询镜像 ID 和版本：

```bash
cat /etc/image-id
```

输出示例：

```
image_name="Alibaba Cloud Linux 3.2104 LTS 64 bit"
image_id="aliyun_3_x64_20G_alibase_20230727.vhd"
release_date="20230728162541"
```

如果 `image_id` 中的日期在 20230727 ~ 20230925 之间，则受此问题影响。

## 修复步骤

### 1. 登录实例

远程登录 Alibaba Cloud Linux 3.8 版本镜像的 ECS 实例。

### 2. 移除 dracut 配置中重复的内核模块

根据实例的 CPU 架构执行对应命令：

**x86 架构：**

```bash
sudo sed -i "s/virtio_blk//" /etc/dracut.conf.d/virt-drivers.conf
```

**ARM 架构：**

```bash
sudo sed -i "s/xen-blkfront xen-netfront//" /etc/dracut.conf.d/virt-drivers.conf
```

### 3. 重新升级内核，验证修复

```bash
sudo yum install kernel
```

执行后确认不再出现 dracut 报错信息即为修复成功。
