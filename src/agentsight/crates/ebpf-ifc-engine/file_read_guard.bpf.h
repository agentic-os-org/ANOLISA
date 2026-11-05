/* Read-guard fast path stays independent of path classification to keep mapping hooks shallow. */
#ifndef __FILE_READ_GUARD_BPF_H
#define __FILE_READ_GUARD_BPF_H

static __always_inline int te_deny_file_read(struct file *file)
{
	if (!enforce_mode || !file)
		return 0;
	pid_t pid = bpf_get_current_pid_tgid() >> 32;
	if (!te_pid_active(pid))
		return 0;
	const struct inode_guard_val *guard = te_inode_guarded(
		file, NULL, TE_REF_FILE, INODE_GUARD_READ, pid);
	if (!guard)
		return 0;
	te_inode_guard_violation(pid, BPF_CORE_READ(file, f_path.dentry),
				 guard, TE_EVENT_OP_READ);
	return -EPERM;
}

#endif
