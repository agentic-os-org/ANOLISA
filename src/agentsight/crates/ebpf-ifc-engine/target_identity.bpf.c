// SPDX-License-Identifier: GPL-2.0-only
#include "vmlinux.h"

#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>

#define MAX_PID_NAMESPACE_LEVEL 32

struct target_identity_record {
	__u32 kernel_pid;
	__u32 local_pid;
	__u32 pidns_inum;
	__u32 reserved;
	__u64 start_boottime;
};

SEC("iter/task")
int resolve_target_identity(struct bpf_iter__task *ctx)
{
	struct target_identity_record record = {};
	struct task_struct *task = ctx->task;
	struct pid *pid;
	struct upid *upid;
	__u32 level;

	if (!task)
		return 0;
	if (BPF_CORE_READ(task, pid) != BPF_CORE_READ(task, tgid))
		return 0;
	pid = BPF_CORE_READ(task, thread_pid);
	if (!pid)
		return 0;
	level = BPF_CORE_READ(pid, level);
	if (level >= MAX_PID_NAMESPACE_LEVEL)
		return 0;
	upid = &pid->numbers[level];

	record.kernel_pid = BPF_CORE_READ(task, tgid);
	record.local_pid = BPF_CORE_READ(upid, nr);
	record.pidns_inum = BPF_CORE_READ(upid, ns, ns.inum);
	record.start_boottime = BPF_CORE_READ(task, start_boottime);
	bpf_seq_write(ctx->meta->seq, &record, sizeof(record));
	return 0;
}

char LICENSE[] SEC("license") = "GPL";
