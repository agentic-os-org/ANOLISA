# Module Signing Guide - 内核模块签名指南

当系统启用 Secure Boot 时，未签名的内核模块将无法加载。本指南介绍如何为内核模块签名。

---

## 检查 Secure Boot 状态

```bash
# 检查 Secure Boot 状态
mokutil --sb-state

# 如果显示 "SecureBoot enabled"，则需要签名模块
```

---

## 方法一：临时禁用 Secure Boot（推荐用于开发）

### 1. 重启进入 MOK 管理

```bash
# 禁用签名验证
sudo mokutil --disable-validation
```

### 2. 重启系统

重启时会进入蓝色 MOK 管理界面：
1. 选择 "Enroll MOK"
2. 选择 "Continue"
3. 输入密码（如果设置了）
4. 选择 "Reboot"

### 3. 验证状态

```bash
mokutil --sb-state
# 应显示 "SecureBoot disabled"
```

---

## 方法二：为模块签名（生产环境推荐）

### 1. 生成密钥对

```bash
# 创建目录
mkdir -p ~/module-signing
cd ~/module-signing

# 生成私钥和证书
openssl req -new -x509 -newkey rsa:2048 \
    -keyout MOK.priv \
    -outform DER -out MOK.der \
    -nodes -days 36500 \
    -subj "/CN=My Module Signing Key/"
```

### 2. 注册密钥到 MOK

```bash
# 导入密钥到 MOK
sudo mokutil --import MOK.der

# 设置密码（记住这个密码）
```

### 3. 重启并注册密钥

重启系统，进入 MOK 管理界面：
1. 选择 "Enroll MOK"
2. 选择 "Continue"
3. 输入刚才设置的密码
4. 选择 "Reboot"

### 4. 签名模块

```bash
# 编译模块后签名
cd /path/to/your/module
make

# 签名模块
sudo scripts/sign-file sha256 \
    ~/module-signing/MOK.priv \
    ~/module-signing/MOK.der \
    your_module.ko

# 或使用 sign-file 工具（如果已安装）
sudo /usr/src/kernels/$(uname -r)/scripts/sign-file sha256 \
    ~/module-signing/MOK.priv \
    ~/module-signing/MOK.der \
    your_module.ko
```

### 5. 验证签名

```bash
# 检查模块签名
modinfo your_module.ko | grep signer

# 应显示签名信息
```

### 6. 加载模块

```bash
sudo insmod your_module.ko
```

---

## 方法三：自动签名（集成到 Makefile）

在 Makefile 中添加自动签名目标：

```makefile
obj-m += your_module.o

KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)
MODULE_NAME := your_module
MOK_PRIV := $(HOME)/module-signing/MOK.priv
MOK_DER := $(HOME)/module-signing/MOK.der
SIGN_FILE := /usr/src/kernels/$(shell uname -r)/scripts/sign-file

all: modules sign

modules:
	make -C $(KERNEL_DIR) M=$(PWD) modules

sign: modules
	@if [ -f "$(MOK_PRIV)" ] && [ -f "$(MOK_DER)" ]; then \
		echo "Signing $(MODULE_NAME).ko..."; \
		sudo $(SIGN_FILE) sha256 $(MOK_PRIV) $(MOK_DER) $(MODULE_NAME).ko; \
	else \
		echo "Warning: MOK keys not found. Module not signed."; \
		echo "Run: mokutil --import MOK.der to register keys"; \
	fi

clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean

install: modules sign
	sudo insmod $(MODULE_NAME).ko

unload:
	sudo rmmod $(MODULE_NAME)

.PHONY: all modules sign clean install unload
```

---

## 故障排查

### Q1: sign-file 工具不存在

```bash
# 安装 kernel-devel 包
sudo yum install kernel-devel-$(uname -r)

# 工具位置
ls /usr/src/kernels/$(uname -r)/scripts/sign-file
```

### Q2: 模块仍然无法加载

```bash
# 检查密钥是否已注册
mokutil --list-enrolled

# 检查模块签名
modinfo your_module.ko

# 查看内核日志
dmesg | tail -20
```

### Q3: 忘记密码

```bash
# 清除 MOK
sudo mokutil --clear-mok

# 或重置所有 MOK
sudo mokutil --reset
```

---

## 最佳实践

### 开发环境
- 使用 **方法一**（禁用 Secure Boot）
- 更快速，无需每次签名

### 生产环境
- 使用 **方法二**（签名模块）
- 保持 Secure Boot 启用
- 妥善保管私钥

### 密钥管理
```bash
# 备份密钥
cp ~/module-signing/MOK.* /secure/location/

# 设置权限
chmod 600 ~/module-signing/MOK.priv
chmod 644 ~/module-signing/MOK.der
```

---

## 参考资源

- [Linux Kernel Signing](https://www.kernel.org/doc/html/latest/admin-guide/module-signing.html)
- [mokutil 手册](https://manpages.org/mokutil)
- [Secure Boot 文档](https://wiki.debian.org/SecureBoot)

---

**适用版本**: Alinux4 (alnx4)  
**支持架构**: x86_64, aarch64
