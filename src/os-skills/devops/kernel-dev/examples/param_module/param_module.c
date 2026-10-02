/* param_module - Kernel Module with Parameters for Alinux4 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

/* Module parameters */
static char *greeting = "Hello";
static int repeat_count = 1;

module_param(greeting, charp, 0644);
MODULE_PARM_DESC(greeting, "A string to display (default: Hello)");

module_param(repeat_count, int, 0644);
MODULE_PARM_DESC(repeat_count, "Number of times to repeat the greeting (default: 1)");

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("param_module: A kernel module with parameters for alnx4");
MODULE_VERSION("0.1");

static int __init param_module_init(void)
{
    int i;
    
    printk(KERN_INFO "param_module: Module loaded (alnx4)\n");
    printk(KERN_INFO "param_module: Parameters:\n");
    printk(KERN_INFO "param_module:   greeting = %s\n", greeting);
    printk(KERN_INFO "param_module:   repeat_count = %d\n", repeat_count);
    
    for (i = 0; i < repeat_count; i++) {
        printk(KERN_INFO "param_module: [%d/%d] %s, Alinux4 Kernel!\n", 
               i + 1, repeat_count, greeting);
    }
    
    return 0;
}

static void __exit param_module_exit(void)
{
    printk(KERN_INFO "param_module: Module unloaded\n");
    printk(KERN_INFO "param_module: Goodbye!\n");
}

module_init(param_module_init);
module_exit(param_module_exit);
