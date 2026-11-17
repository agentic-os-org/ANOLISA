/* Exact-file reads stay on the inode-guard fast path; directory-tree reads
 * classify the accessed path only for domains that activated a tree read mask. */
#ifndef __FILE_READ_GUARD_BPF_H
#define __FILE_READ_GUARD_BPF_H

/* Returns the tree read bits protecting an entry, or zero when it is outside,
 * excluded or unclassifiable. UNKNOWN stays fail-open like ordinary writes:
 * entries on internal mounts (memfd, anonymous inodes) never reach a
 * namespace root and must not turn every such read into a denial. */
static __always_inline __u64 te_policy_read_bits(const struct path *dir,
	struct dentry *dentry, bool existing,
	const struct te_policy_domain_val *domain,
	struct te_policy_classification *classification)
{
	te_policy_classify(dir, dentry, existing, domain->read_mask, domain,
			   classification);
	return classification->kind == TE_POLICY_CLASS_PROTECTED ?
		classification->protected_mask : 0;
}

/* Classification follows the accessed name, so new, replaced and moved-in
 * files are covered without enumerating the tree at apply time. */
static __always_inline int te_deny_tree_read(struct file *file, pid_t pid)
{
	const struct te_policy_domain_val *domain = te_policy_domain(pid);
	if (!domain || !domain->read_mask)
		return 0;
	__u32 zero = 0;
	struct te_policy_permission_scratch *scratch =
		bpf_map_lookup_elem(&te_policy_permission_scratch, &zero);
	struct dentry *dentry = NULL, *parent = NULL;
	struct vfsmount *vfsmount = NULL;
	/* Only regular-file contents are secret; directory listings stay visible. */
	if (!scratch || BPF_CORE_READ_INTO(&dentry, file, f_path.dentry) || !dentry ||
	    BPF_CORE_READ_INTO(&vfsmount, file, f_path.mnt) || !vfsmount ||
	    BPF_CORE_READ_INTO(&parent, dentry, d_parent) || !parent ||
	    (BPF_CORE_READ(dentry, d_inode, i_mode) & 0170000) != 0100000)
		return 0;
	__builtin_memset(scratch, 0, sizeof(*scratch));
	scratch->parent_path.mnt = vfsmount;
	scratch->parent_path.dentry = parent;
	if (!te_policy_read_bits(&scratch->parent_path, dentry, true, domain,
				 &scratch->classification))
		return 0;
	te_inode_guard_violation(pid, dentry, &scratch->classification.guard,
				 TE_EVENT_OP_READ);
	return -EPERM;
}

static __always_inline int te_deny_file_read(struct file *file)
{
	if (!enforce_mode || !file)
		return 0;
	pid_t pid = bpf_get_current_pid_tgid() >> 32;
	if (!te_pid_active(pid))
		return 0;
	const struct inode_guard_val *guard = te_inode_guarded(
		file, NULL, TE_REF_FILE, INODE_GUARD_READ, pid);
	if (guard) {
		te_inode_guard_violation(pid, BPF_CORE_READ(file, f_path.dentry),
					 guard, TE_EVENT_OP_READ);
		return -EPERM;
	}
	return te_deny_tree_read(file, pid);
}

/* Path classification cannot follow an inode to a second name, so a bound
 * process must not link protected content to a name with fewer read bits. */
static __always_inline int te_guard_tree_read_link(struct dentry *old_dentry,
	const struct path *new_dir, struct dentry *new_dentry)
{
	if (!enforce_mode || !old_dentry || !new_dir)
		return 0;
	pid_t pid = bpf_get_current_pid_tgid() >> 32;
	if (!te_pid_active(pid))
		return 0;
	const struct te_policy_domain_val *domain = te_policy_domain(pid);
	if (!domain || !domain->read_mask)
		return 0;
	__u32 zero = 0;
	struct te_policy_permission_scratch *scratch =
		bpf_map_lookup_elem(&te_policy_permission_scratch, &zero);
	struct dentry *parent = NULL;
	struct vfsmount *vfsmount = NULL;
	if (!scratch || BPF_CORE_READ_INTO(&parent, old_dentry, d_parent) || !parent ||
	    BPF_CORE_READ_INTO(&vfsmount, new_dir, mnt) || !vfsmount)
		return 0;
	__builtin_memset(scratch, 0, sizeof(*scratch));
	/* linkat(2) rejects cross-mount sources before this hook, so the
	 * destination mount also names the source entry. */
	scratch->parent_path.mnt = vfsmount;
	scratch->parent_path.dentry = parent;
	__u64 old_bits = te_policy_read_bits(&scratch->parent_path, old_dentry,
					     true, domain, &scratch->classification);
	if (!old_bits)
		return 0;
	struct inode_guard_val guard = scratch->classification.guard;
	__u64 new_bits = te_policy_read_bits(new_dir, new_dentry, false, domain,
					     &scratch->classification);
	if (!(old_bits & ~new_bits))
		return 0;
	te_inode_guard_violation(pid, old_dentry, &guard, TE_EVENT_OP_LINK);
	return -EPERM;
}

/* Renames by a bound process must not move protected content to a name with
 * fewer read bits, and must not replace or exchange a stable root/ancestor:
 * the tree is anchored by directory identity, so a substituted directory
 * would silently receive later secrets outside the protected scope. */
static __always_inline int te_guard_tree_read_rename(pid_t pid,
	const struct te_policy_domain_val *domain, const struct path *old_dir,
	struct dentry *old_dentry, const struct path *new_dir,
	struct dentry *new_dentry, unsigned int flags, bool flags_valid)
{
	__u32 zero = 0;
	struct te_policy_permission_scratch *scratch =
		bpf_map_lookup_elem(&te_policy_permission_scratch, &zero);
	struct inode *new_inode = NULL;
	if (!scratch || BPF_CORE_READ_INTO(&new_inode, new_dentry, d_inode)) {
		te_inode_guard_violation(pid, old_dentry, &domain->guard,
					 TE_EVENT_OP_RENAME);
		return -EPERM;
	}
	__builtin_memset(scratch, 0, sizeof(*scratch));
	__u64 old_bits = te_policy_read_bits(old_dir, old_dentry, true, domain,
					     &scratch->classification);
	bool old_stable = old_bits && scratch->classification.stable;
	struct inode_guard_val guard = scratch->classification.guard;
	__u64 new_bits = te_policy_read_bits(new_dir, new_dentry, new_inode != NULL,
					     domain, &scratch->classification);
	bool new_stable = new_bits && scratch->classification.stable;
	if (!old_bits && !new_bits)
		return 0;
	bool exchange = flags & RENAME_EXCHANGE;
	/* Without syscall flags an exchange cannot be told from a move. */
	if (flags_valid && !(old_bits & ~new_bits) &&
	    !(exchange && ((new_bits & ~old_bits) || old_stable)) &&
	    !(new_inode && new_stable))
		return 0;
	if (!old_bits)
		guard = scratch->classification.guard;
	te_inode_guard_violation(pid, old_bits ? old_dentry : new_dentry, &guard,
				 TE_EVENT_OP_RENAME);
	return -EPERM;
}

#endif
