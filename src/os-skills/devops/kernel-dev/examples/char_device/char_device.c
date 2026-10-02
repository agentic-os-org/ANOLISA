/* char_device - Character Device Kernel Module for Alinux4 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/fs.h>
#include <linux/cdev.h>
#include <linux/device.h>
#include <linux/uaccess.h>

#define DEVICE_NAME "chardev"
#define CLASS_NAME "chardev_class"
#define BUFFER_SIZE 256

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("char_device: A simple character device module for alnx4");
MODULE_VERSION("0.1");

/* Device variables */
static dev_t dev_number;
static struct cdev char_cdev;
static struct class *char_class;
static struct device *char_device;
static char message_buffer[BUFFER_SIZE] = "Hello from char_device!";
static int message_size = 23;

/* Function prototypes */
static int chardev_open(struct inode *inode, struct file *file);
static int chardev_release(struct inode *inode, struct file *file);
static ssize_t chardev_read(struct file *file, char __user *buf, size_t count, loff_t *offset);
static ssize_t chardev_write(struct file *file, const char __user *buf, size_t count, loff_t *offset);

/* File operations structure */
static struct file_operations fops = {
    .owner = THIS_MODULE,
    .open = chardev_open,
    .release = chardev_release,
    .read = chardev_read,
    .write = chardev_write,
};

/* Open function */
static int chardev_open(struct inode *inode, struct file *file)
{
    printk(KERN_INFO "char_device: Device opened\n");
    return 0;
}

/* Release function */
static int chardev_release(struct inode *inode, struct file *file)
{
    printk(KERN_INFO "char_device: Device closed\n");
    return 0;
}

/* Read function */
static ssize_t chardev_read(struct file *file, char __user *buf, size_t count, loff_t *offset)
{
    int bytes_to_read;
    int bytes_read;
    
    if (*offset >= message_size) {
        return 0;
    }
    
    bytes_to_read = min((int)(message_size - *offset), (int)count);
    bytes_read = bytes_to_read - copy_to_user(buf, message_buffer + *offset, bytes_to_read);
    *offset += bytes_read;
    
    printk(KERN_INFO "char_device: Sent %d bytes to user\n", bytes_read);
    return bytes_read;
}

/* Write function */
static ssize_t chardev_write(struct file *file, const char __user *buf, size_t count, loff_t *offset)
{
    int bytes_to_write;
    int bytes_written;
    
    bytes_to_write = min((int)count, BUFFER_SIZE - 1);
    bytes_written = bytes_to_write - copy_from_user(message_buffer, buf, bytes_to_write);
    message_buffer[bytes_written] = '\0';
    message_size = bytes_written;
    
    printk(KERN_INFO "char_device: Received %d bytes from user\n", bytes_written);
    return bytes_written;
}

/* Module initialization */
static int __init char_device_init(void)
{
    int ret;
    
    printk(KERN_INFO "char_device: Initializing character device (alnx4)\n");
    
    /* Allocate major number dynamically */
    ret = alloc_chrdev_region(&dev_number, 0, 1, DEVICE_NAME);
    if (ret < 0) {
        printk(KERN_ERR "char_device: Failed to allocate major number\n");
        return ret;
    }
    printk(KERN_INFO "char_device: Registered with major %d, minor %d\n", 
           MAJOR(dev_number), MINOR(dev_number));
    
    /* Initialize cdev */
    cdev_init(&char_cdev, &fops);
    char_cdev.owner = THIS_MODULE;
    
    /* Add cdev to kernel */
    ret = cdev_add(&char_cdev, dev_number, 1);
    if (ret < 0) {
        printk(KERN_ERR "char_device: Failed to add cdev\n");
        unregister_chrdev_region(dev_number, 1);
        return ret;
    }
    
    /* Create device class */
    char_class = class_create(CLASS_NAME);
    if (IS_ERR(char_class)) {
        printk(KERN_ERR "char_device: Failed to create device class\n");
        cdev_del(&char_cdev);
        unregister_chrdev_region(dev_number, 1);
        return PTR_ERR(char_class);
    }
    
    /* Create device */
    char_device = device_create(char_class, NULL, dev_number, NULL, DEVICE_NAME);
    if (IS_ERR(char_device)) {
        printk(KERN_ERR "char_device: Failed to create device\n");
        class_destroy(char_class);
        cdev_del(&char_cdev);
        unregister_chrdev_region(dev_number, 1);
        return PTR_ERR(char_device);
    }
    
    printk(KERN_INFO "char_device: Device created at /dev/%s\n", DEVICE_NAME);
    return 0;
}

/* Module exit */
static void __exit char_device_exit(void)
{
    printk(KERN_INFO "char_device: Cleaning up\n");
    device_destroy(char_class, dev_number);
    class_destroy(char_class);
    cdev_del(&char_cdev);
    unregister_chrdev_region(dev_number, 1);
    printk(KERN_INFO "char_device: Module unloaded\n");
}

module_init(char_device_init);
module_exit(char_device_exit);
