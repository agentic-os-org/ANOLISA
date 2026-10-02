# Example: 编译示例内核 Module

适用于 Alinux4 (alnx4)，演示一个 Hello World 内核模块的完整生命周期：创建、编译、加载、验证、卸载。

## 完整流程

```bash
# 创建测试目录
mkdir -p ~/kernel_module_test && cd ~/kernel_module_test

# 创建模块源码
cat > hello_alinux.c << 'EOF'
/* Hello Alinux4 - 示例内核模块 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("Hello World module for Alinux4 (alnx4)");
MODULE_VERSION("0.1");

static int __init hello_init(void)
{
    printk(KERN_INFO "hello_alinux: Hello Alinux4 (alnx4)!\n");
    return 0;
}

static void __exit hello_exit(void)
{
    printk(KERN_INFO "hello_alinux: Goodbye!\n");
}

module_init(hello_init);
module_exit(hello_exit);
EOF

# 创建 Makefile
cat > Makefile << 'EOF'
obj-m += hello_alinux.o

KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)

all:
	make -C $(KERNEL_DIR) M=$(PWD) modules

clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean

install:
	sudo insmod hello_alinux.ko

unload:
	sudo rmmod hello_alinux

.PHONY: all clean install unload
EOF

# 编译模块
make

# 查看模块信息
modinfo hello_alinux.ko

# 加载模块
sudo insmod hello_alinux.ko

# 验证加载
lsmod | grep hello_alinux

# 查看日志输出
dmesg | tail -5

# 卸载模块
sudo rmmod hello_alinux

# 确认卸载日志
dmesg | tail -5

# 清理
make clean
```
