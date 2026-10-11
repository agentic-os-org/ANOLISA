# Login Rescue When the cosh-ng Login Shell Fails

[中文版](../../../zh/user-entrypoint/cosh-ng/login-rescue.md)

AgenticOS images point the login shell of root and new users at
`/usr/bin/cosh-login`, which execs the cosh-ng runtime (`/usr/bin/cosh`, a
symlink to `cosh-shell`). This page covers the case where that runtime is
damaged and normal SSH login fails.

## Symptoms

- SSH sessions close immediately; non-interactive commands never run:

  ```text
  $ ssh root@<ecs-ip> 'echo hello'
  /usr/bin/cosh: line 1: GARBAGE-NOT-AN-ELF: command not found
  ```

- The exit code is 127 and the remote command produces no output.
- Cloud Assistant RunCommand still works: its scripts do not go through the
  login shell.

## Behavior with a current cosh-ng

Starting with the release that ships `cosh-login` in the cosh-ng RPM, the
wrapper verifies the ELF magic of `/usr/bin/cosh` before exec. A damaged,
missing, or unreadable runtime no longer locks you out: the login degrades to
`/bin/bash` and prints a one-line notice:

```text
Error: cosh-ng login runtime unavailable; falling back to /bin/bash.
```

Log in over SSH as usual (you land in plain bash), then repair the runtime as
described below.

## Repair

Run these through Cloud Assistant RunCommand, or in the degraded bash
session:

```bash
# 1. Confirm the damage: verification flags the corrupted file.
rpm -V cosh-ng

# 2. Reinstall the package to restore the damaged files.
yum reinstall -y cosh-ng

# 3. Verify the repair: no output means all files match the package.
rpm -V cosh-ng
head -c 4 /usr/libexec/anolisa/cosh-ng/cosh-shell | od -An -tx1
# expected magic:  7f 45 4c 46
```

Then open a fresh SSH session to confirm normal login.

## If Cloud Assistant is unavailable

Use the ECS console (VNC) and boot into single-user mode: interrupt GRUB,
append `init=/bin/bash` to the kernel command line, and boot. The system
starts bash directly without the login shell; remount the root filesystem
read-write (`mount -o remount,rw /`) and run the repair steps above.

## Notes

- `cosh-login` only guards its own exec target. A damaged `/bin/bash` itself
  breaks every login path (including Cloud Assistant) and is outside the
  scope of this mechanism; use single-user mode to recover.
- Do not "fix" a damaged runtime by pointing user login shells back at
  `/bin/bash` permanently; repair the package instead so the next boot is
  back on the supported path.
