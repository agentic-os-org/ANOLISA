/* hello_module - Kernel Module for Alinux4 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("hello_module: A simple kernel module for alnx4");
MODULE_VERSION("0.1");

static int __init hello_module_init(void)
{
    printk(KERN_INFO "hello_module: Module loaded (alnx4)\n");
    printk(KERN_INFO "hello_module: Hello, Alinux4 Kernel!\n");
    return 0;
}

static void __exit hello_module_exit(void)
{
    printk(KERN_INFO "hello_module: Module unloaded\n");
}

module_init(hello_module_init);
module_exit(hello_module_exit);
