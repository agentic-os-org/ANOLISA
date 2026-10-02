# Kernel Module Examples - 内核模块示例

本目录包含多个内核模块示例，从简单到复杂，帮助您学习内核模块开发。

---

## 快速开始

### 环境准备

```bash
# 1. 检查环境
../scripts/check-env.sh

# 2. 如需安装依赖
sudo ../scripts/install-deps.sh

# 3. 或一键 setup
sudo ../scripts/setup.sh
```

---

## 示例列表

| 示例 | 难度 | 描述 | 特性 |
|------|------|------|------|
| [hello_module](hello_module/) | ⭐ 入门 | Hello World 模块 | 基础加载/卸载 |
| [param_module](param_module/) | ⭐⭐ 初级 | 带参数的模块 | 模块参数、类型验证 |
| [proc_module](proc_module/) | ⭐⭐ 初级 | /proc 接口模块 | seq_file、proc 文件系统 |
| [char_device](char_device/) | ⭐⭐⭐ 中级 | 字符设备驱动 | 字符设备、file_operations |

---

## 示例详解

### 1. hello_module - Hello World

最简单的内核模块，演示基础的模块加载和卸载。

**文件结构：**
```
hello_module/
├── hello_module.c    # 模块源码
├── Makefile          # 构建文件
└── README.md         # 详细说明
```

**编译和测试：**
```bash
cd hello_module

# 编译
make

# 加载
sudo make install

# 查看日志
make log

# 卸载
sudo make unload

# 完整测试
make test
```

**预期输出：**
```
# dmesg | tail
[349153.404406] hello_module: Module loaded (alnx4)
[349153.404408] hello_module: Hello, Alinux4 Kernel!
[349167.045912] hello_module: Module unloaded
```

---

### 2. param_module - 带参数的模块

演示如何定义和使用模块参数，支持运行时配置。

**特性：**
- `greeting` (string) - 问候语
- `repeat_count` (int) - 重复次数

**编译和测试：**
```bash
cd param_module

# 编译
make

# 使用默认参数加载
sudo make install

# 使用自定义参数加载
sudo insmod param_module.ko greeting="Welcome" repeat_count=3

# 或使用 make 目标
sudo make load-custom

# 查看参数
make info

# 查看日志
make log

# 卸载
sudo make unload
```

**预期输出：**
```
# dmesg | tail
[349200.123456] param_module: Module loaded (alnx4)
[349200.123458] param_module: Parameters:
[349200.123459] param_module:   greeting = Welcome
[349200.123460] param_module:   repeat_count = 3
[349200.123461] param_module: [1/3] Welcome, Alinux4 Kernel!
[349200.123462] param_module: [2/3] Welcome, Alinux4 Kernel!
[349200.123463] param_module: [3/3] Welcome, Alinux4 Kernel!
```

---

### 3. proc_module - /proc 接口模块

演示如何创建 /proc 文件系统接口，使用 seq_file 接口。

**特性：**
- 创建 `/proc/hello_proc` 文件
- 使用 seq_file 接口简化读取
- 每次读取计数器递增

**编译和测试：**
```bash
cd proc_module

# 编译
make

# 加载
sudo make install

# 测试 /proc 接口
make test

# 手动读取
cat /proc/hello_proc

# 查看日志
make log

# 卸载
sudo make unload
```

**预期输出：**
```
# cat /proc/hello_proc
Hello from /proc/hello_proc!
Counter: 1
Kernel: 6.6.102-5.2.alnx4.x86_64
Architecture: x86_64
```

---

### 4. char_device - 字符设备驱动

演示如何创建字符设备，实现完整的 file_operations。

**特性：**
- 动态分配设备号
- 实现 open/read/write/release
- 创建 `/dev/chardev` 设备节点
- 支持读写操作

**编译和测试：**
```bash
cd char_device

# 编译
make

# 加载
sudo make install

# 测试设备
make test

# 手动测试
echo "Hello" | sudo tee /dev/chardev
sudo cat /dev/chardev

# 查看设备
ls -l /dev/chardev

# 卸载
sudo make unload-test
```

**预期输出：**
```
# sudo cat /dev/chardev
Hello from char_device!

# echo "Test message" | sudo tee /dev/chardev
Test message
```

---

## 自动化测试脚本

### test-module.sh

通用的模块测试脚本，适用于所有模块。

```bash
# 用法
../scripts/test-module.sh <module_name> [parameters]

# 示例
sudo ../scripts/test-module.sh hello_module
sudo ../scripts/test-module.sh param_module greeting="Hi" repeat_count=5
```

**测试流程：**
1. 检查模块文件
2. 显示模块信息
3. 卸载已加载的模块（如有）
4. 加载模块
5. 验证加载状态
6. 检查设备/proc 条目
7. 查看内核日志
8. 等待 3 秒
9. 卸载模块
10. 验证卸载状态

---

## Makefile 目标说明

所有示例模块的 Makefile 都支持以下目标：

| 目标 | 描述 |
|------|------|
| `all` | 编译模块（含依赖检查） |
| `modules` | 仅编译，不检查依赖 |
| `check-deps` | 检查内核构建目录 |
| `clean` | 清理编译产物 |
| `rebuild` | 清理并重新编译 |
| `install` | 加载模块 |
| `unload` | 卸载模块 |
| `reload` | 重新加载模块 |
| `status` | 显示模块状态和日志 |
| `info` | 显示模块信息 |
| `log` | 查看内核日志 |
| `test` | 完整测试流程 |

---

## 常见问题

### Q: 编译失败 "Kernel build directory not found"

```bash
# 安装内核 devel 包
sudo yum install kernel-devel-$(uname -r) kernel-headers-$(uname -r)
```

### Q: insmod 失败 "Invalid module format"

```bash
# 检查内核版本匹配
modinfo your_module.ko | grep vermagic
uname -r

# 如果不匹配，重新编译
make clean && make
```

### Q: 设备节点未创建

```bash
# 检查 dmesg 日志
dmesg | tail

# 手动创建设备节点（如需要）
sudo mknod /dev/chardev c <major> <minor>
```

---

## 下一步

- 阅读每个示例的详细 README
- 修改示例代码，实验不同参数
- 参考 [SKILL.md](../SKILL.md) 了解更多内核开发知识
- 查看 [references/](../references/) 中的内核开发资源

---

**适用版本**: Alinux4 (alnx4)  
**支持架构**: x86_64, aarch64  
**内核版本**: 6.6.102-5.2.alnx4
