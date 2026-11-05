# Troubleshooting

Common issues and solutions when using ANOLISA components.

---

## Diagnostic Tools

ANOLISA provides built-in diagnostic commands to help identify and resolve problems.

### anolisa doctor

Runs a comprehensive health check across all installed components:

```bash
anolisa doctor
```

Checks include:
- Component binary availability
- Configuration file validity
- Runtime dependencies (FUSE, btrfs, eBPF)
- Adapter connectivity
- Permission issues

### anolisa bug

Generates a diagnostic report for filing bug reports:

```bash
anolisa bug
```

This collects system info, component versions, configuration, and recent logs into a single report file.

### anolisa logs

View component logs:

```bash
# View logs for a specific component
anolisa logs <component>

# Show warning and error records (--level is an alias for --severity)
anolisa logs <component> --severity warn

# Show last N lines
anolisa logs <component> --limit 50
```

---

## Common Issues

### Permission Errors

**Symptom**: `Permission denied` when running `anolisa install`

**Cause**: Some components require system mode (root privileges).

**Solution**:

```bash
# For system-mode components (agentsight, agent-sec-core)
sudo anolisa install <component>

# For user-mode components, ensure ~/.local/bin is writable
ls -la ~/.local/bin/
```

---

**Symptom**: `Permission denied` accessing `/dev/fuse`

**Cause**: User not in the `fuse` group or device not available.

**Solution**:

```bash
# Add user to fuse group
sudo usermod -aG fuse $USER

# Verify device exists
ls -la /dev/fuse
```

---

### Component Installation Failures

**Symptom**: `anolisa install tokenless` fails with network error

**Solution**:

```bash
# Inspect the detected environment
anolisa env

# Retry with verbose output
anolisa --verbose install tokenless

# Alternative: use YUM
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

---

**Symptom**: `cargo build` fails during source compilation

**Solution**:

```bash
# Ensure Rust toolchain is installed
rustup show

# Update to latest stable
rustup update stable

# Inspect the detected build environment
anolisa env
```

---

### Adapter Issues

**Symptom**: Tokenless hook not activating in cosh

**Solution**:

```bash
# Verify hook installation
ls ~/.config/cosh/hooks/

# Reinstall the hook
/usr/share/tokenless/scripts/install.sh --cosh

# Check cosh hook config
cat ~/.config/cosh/config.toml | grep -A5 hooks
```

---

**Symptom**: ws-ckpt plugin not detected by OpenClaw

**Solution**:

```bash
# Reinstall the plugin
ws-ckpt plugin install --runtime openclaw

# Verify plugin registration
anolisa status ws-ckpt

# Check OpenClaw plugin directory
ls ~/.config/openclaw/plugins/
```

---

### ws-ckpt Issues

**Symptom**: `ws-ckpt checkpoint` fails with "not a btrfs filesystem"

**Solution**:

```bash
# Check filesystem type
df -T /path/to/workspace

# ws-ckpt will fall back to rsync if btrfs is unavailable
# Ensure workspace path is correctly configured
ws-ckpt config
```

---

**Symptom**: "workspace path must not be Agent startup directory"

**Cause**: ws-ckpt workspace is set to the Agent's CWD or a parent directory.

**Solution**: Change the workspace path to a dedicated project directory:

```bash
ws-ckpt config set workspace.path /home/user/projects/my-project
```

---

**Symptom**: `ws-ckpt checkpoint` or `init` fails with "workspace root is an
active mount point", or on older versions with "failed to rename original
directory to backup: Device or resource busy (os error 16)"

**Cause**: Initializing a workspace moves the original directory aside as a
backup, and `rename(2)` fails with `EBUSY` on a directory that is itself a mount
point. The usual trigger is mounting SkillFS in-place and then snapshotting the
same path.

**Solution**: Confirm whether the path is a mount point, unmount it, and retry:

```bash
# Confirm whether it is a mount point
findmnt /path/to/workspace

# In-place SkillFS mount
skillfs stop /path/to/workspace

# Any other FUSE mount
fusermount3 -u /path/to/workspace

ws-ckpt checkpoint -w /path/to/workspace -s my-snapshot
```

Only the workspace root itself is rejected. A mount nested inside the workspace
does not block `init`, but the mount stays attached to the backup directory moved
aside during initialization while the new workspace only receives a plain copy of
its contents — unmount nested mounts first, or keep mount points outside the
workspace tree. Mounting SkillFS after the workspace is already initialized also
leaves later snapshots working.

---

### SkillFS Issues

**Symptom**: `skillfs mount` fails with "FUSE not available"

**Solution**:

```bash
# Install FUSE3
sudo yum install fuse3 fuse3-devel

# Load FUSE kernel module
sudo modprobe fuse

# Verify
ls /dev/fuse
```

---

### AgentSight Issues

**Symptom**: AgentSight shows no eBPF data

**Cause**: Insufficient kernel capabilities or eBPF not supported.

**Solution**:

```bash
# Check kernel version (>= 5.4 recommended)
uname -r

# Inspect kernel capabilities, then diagnose the installed component
anolisa env
sudo anolisa --install-mode system doctor agentsight

# AgentSight requires system mode
sudo anolisa install agentsight
```

---

## Getting Help

If the above steps don't resolve your issue:

1. Run `anolisa bug` and attach the report
2. Check component-specific logs: `anolisa logs <component>`
3. File an issue on the ANOLISA GitHub repository
