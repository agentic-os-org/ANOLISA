# Kernel Build Troubleshooting Guide

## Common Issues and Solutions

### Issue 1: Missing Header Files (Upstream Build)

**Error:**
```
fatal error: tools/be_byteshift.h: No such file or directory
```

**Cause:** Upstream kernel source may have missing generated headers or incompatible tooling.

**Solution:**
```bash
# Clean and regenerate headers
cd /root/upstream-kernel/linux-*
make clean
make mrproper

# Regenerate configuration
make defconfig

# Retry build
make -j8 bzImage
```

**Alternative:** Use a different kernel version or the SRPM method which includes all necessary patches.

### Issue 2: SRPM Build Fails with Dependency Errors

**Error:**
```
error: Failed build dependencies:
  perl(Data::Dumper) is needed by kernel-6.6.102-5.2.alnx4.x86_64
```

**Solution:**
```bash
# Install missing Perl modules
sudo yum install -y perl-Data-Dumper

# Or install all recommended dependencies
sudo yum install -y \
  gcc gcc-c++ make binutils \
  flex bison libelf-devel openssl-devel ncurses-devel \
  pahole perl perl-devel python3 python3-devel \
  git ccache dwarves wget curl kmod \
  rpm-build rpmdevtools yum-utils asciidoc
```

### Issue 3: Out of Memory During Compilation

**Symptoms:**
- Build suddenly stops
- OOM killer messages in dmesg
- System becomes unresponsive

**Solution:**
```bash
# Check available memory
free -h

# Reduce parallel jobs
make -j2  # Instead of -j8

# Or add swap space
sudo fallocate -l 8G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# Verify swap is active
swapon --show
```

### Issue 4: GCC Version Mismatch Warning

**Warning:**
```
warning: compiler 'gcc-12.3' differs from the one used to build the kernel 'gcc-11.4'
```

**Solution:** This is usually harmless. If you want to match the kernel's GCC version:

```bash
# Check which GCC version built the current kernel
cat /proc/version

# Check your current GCC version
gcc --version

# If needed, install the matching version
sudo yum install gcc-11.4.0
```

### Issue 5: Disk Space Exhausted

**Error:**
```
No space left on device
```

**Solution:**
```bash
# Check disk usage
df -h /root
du -sh /root/rpmbuild /root/upstream-kernel

# Clean up old builds
rm -rf /root/rpmbuild/BUILDROOT/*
rm -rf /root/upstream-kernel/linux-*/

# Or expand disk if using cloud instance
```

**Minimum Requirements:**
- SRPM method: 20GB free space
- Upstream method: 15GB free space

### Issue 6: Build Hangs or Freezes

**Symptoms:**
- Build progress stops
- CPU usage drops to 0%
- No new log entries

**Solution:**
```bash
# Find stuck processes
ps aux | grep -E "make|cc1"

# Kill stuck build
pkill -9 make
pkill -9 cc1

# Clean and restart
cd /root/upstream-kernel/linux-*
make clean

# Check for system issues
dmesg | tail -50
top -n 1
```

### Issue 7: Module Signature Verification Failed

**Error:**
```
keyring is not available
module verification failed: signature/verification failed
```

**Solution:**
```bash
# For development, disable module signature enforcement
# Add to /etc/modprobe.d/disable-signature.conf
install * /bin/true

# Or disable Secure Boot in BIOS/UEFI

# For production, properly sign modules
sudo mokutil --disable-validation
sudo reboot
```

### Issue 8: GRUB2 Not Updated

**Problem:** New kernel installed but not showing in boot menu.

**Solution:**
```bash
# Manually update GRUB2
sudo grub2-mkconfig -o /boot/grub2/grub.cfg

# Verify kernel files exist
ls -lh /boot/vmlinuz-*
ls -lh /boot/initramfs-*

# Check GRUB2 configuration
grep -i "6.12" /boot/grub2/grub.cfg
```

### Issue 9: BTF Generation Skipped

**Warning:**
```
Skipping BTF generation due to unavailability of vmlinux
```

**Solution:** This is informational, not an error. BTF (BPF Type Format) is optional.

To enable BTF:
```bash
# Enable BTF in kernel config
cd /root/upstream-kernel/linux-*
scripts/config --enable DEBUG_INFO_BTF
make olddefconfig

# Install pahole (already included in dependencies)
sudo yum install dwarves
```

### Issue 10: Build Time Too Long

**Problem:** Compilation takes more than 3 hours.

**Solutions:**

1. **Enable ccache:**
```bash
sudo yum install ccache
export CC="ccache gcc"
export CXX="ccache g++"
```

2. **Use more parallel jobs:**
```bash
# Use all CPU cores
make -j$(nproc)
```

3. **Use tinyconfig for testing:**
```bash
make tinyconfig
make -j8 bzImage
```

4. **Build only what you need:**
```bash
# Build specific subsystem only
make M=drivers/net -j8
```

## Performance Comparison

| Method | Time (8 cores) | Time (4 cores) | Disk Usage |
|--------|---------------|----------------|------------|
| SRPM (full) | 1-3 hours | 2-5 hours | ~25GB |
| Upstream (defconfig) | 30-60 min | 1-2 hours | ~15GB |
| Upstream (tinyconfig) | 15-30 min | 30-60 min | ~5GB |

## Getting Help

```bash
# Check build logs
tail -100 /tmp/kernel-build.log
tail -100 /tmp/kernel-rpmbuild.log
tail -100 /tmp/upstream-kernel-build.log

# Use the status command
./scripts/build-kernel.sh status

# Check system resources
free -h
df -h
top -n 1

# Verify dependencies
./scripts/verify-env.sh
```

## Quick Recovery

If build fails and you're not sure what went wrong:

```bash
# 1. Clean everything
cd /root/upstream-kernel/linux-*
make mrproper
make clean

# 2. Reinstall dependencies
sudo yum reinstall gcc gcc-c++ make kernel-devel-$(uname -r)

# 3. Try SRPM method instead (more reliable)
./scripts/build-kernel.sh srpm 6.6.102-5.2.alnx4.x86_64

# 4. Or use a known-good kernel version
./scripts/build-kernel.sh upstream 6.6.102 8 defconfig
```
