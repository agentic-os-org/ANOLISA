/* proc_module - Kernel Module with /proc Interface for Alinux4 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/proc_fs.h>
#include <linux/seq_file.h>
#include <linux/uaccess.h>
#include <linux/uts.h>
#include <linux/utsname.h>

#define PROC_NAME "hello_proc"

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("proc_module: A kernel module with /proc interface for alnx4");
MODULE_VERSION("0.1");

/* Proc entry */
static struct proc_dir_entry *proc_entry;
static int counter = 0;

/* Seq_file show function */
static int hello_proc_show(struct seq_file *m, void *v)
{
    counter++;
    seq_printf(m, "Hello from /proc/%s!\n", PROC_NAME);
    seq_printf(m, "Counter: %d\n", counter);
    seq_printf(m, "Kernel: %s\n", utsname()->release);
    seq_printf(m, "Architecture: %s\n", utsname()->machine);
    return 0;
}

/* Seq_file open function */
static int hello_proc_open(struct inode *inode, struct file *file)
{
    return single_open(file, hello_proc_show, NULL);
}

/* Proc file operations */
static const struct proc_ops hello_proc_ops = {
    .proc_open = hello_proc_open,
    .proc_read = seq_read,
    .proc_lseek = seq_lseek,
    .proc_release = single_release,
};

/* Module initialization */
static int __init proc_module_init(void)
{
    printk(KERN_INFO "proc_module: Creating /proc/%s (alnx4)\n", PROC_NAME);
    
    proc_entry = proc_create(PROC_NAME, 0644, NULL, &hello_proc_ops);
    if (!proc_entry) {
        printk(KERN_ERR "proc_module: Failed to create /proc entry\n");
        return -ENOMEM;
    }
    
    printk(KERN_INFO "proc_module: Module loaded successfully\n");
    return 0;
}

/* Module exit */
static void __exit proc_module_exit(void)
{
    printk(KERN_INFO "proc_module: Removing /proc/%s\n", PROC_NAME);
    
    proc_remove(proc_entry);
    printk(KERN_INFO "proc_module: Module unloaded\n");
}

module_init(proc_module_init);
module_exit(proc_module_exit);
