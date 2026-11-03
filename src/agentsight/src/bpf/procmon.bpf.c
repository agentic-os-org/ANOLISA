// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Process monitor BPF program
// Lightweight monitoring of process creation and exit
#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "procmon.h"
#define NO_TRACED_PROCESSES_MAP
#define NO_CGROUP_FILTER
#include "common.h"

// Use the kernel's `struct syscall_trace_exit` (what a syscall tracepoint
// program actually receives), NOT `trace_event_raw_sys_exit`. The latter
// declares the syscall number as `long id` (8 bytes); on kernels carrying the
// PREEMPT_LAZY backport (e.g. RHEL/Anolis 9 5.14) `struct trace_entry` grows a
// `preempt_lazy_count` field, and CO-RE then relocates `ret` past the real
// tracepoint record, so the kernel rejects the attach with EACCES.
// `syscall_trace_exit` uses `int nr` (4 bytes), matching the record layout, so
// `ret` relocates to the correct offset on every kernel. See
// inspektor-gadget#2444 / BCC#4920.

// Tracepoint for execve exit - captures process execution after it completes
// NOTE: We use sys_exit_execve instead of sys_enter_execve because:
// - At sys_enter_execve, the process hasn't completed execve yet
// - bpf_get_current_comm() returns the OLD process name
// - Reading /proc/[pid]/comm returns old values
// - At sys_exit_execve, execve has completed and process info is updated
SEC("tp/syscalls/sys_exit_execve")
int trace_execve_exit(struct syscall_trace_exit *ctx)
{
    // Check execve return value - skip if failed
    // ret == 0: success, ret < 0: error (e.g., -ENOENT, -EACCES)
    if (ctx->ret != 0)
        return 0;

    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    // Get parent PID
    u32 ppid = 0;
    struct task_struct *task = (struct task_struct *)bpf_get_current_task();
    ppid = BPF_CORE_READ(task, real_parent, tgid);

    // Reserve space in ring buffer
    struct procmon_event *event = bpf_ringbuf_reserve(&rb, sizeof(*event), 0);
    if (!event) {
        record_ring_buffer_drop();
        return 0;
    }

    // Fill event
    event->source = EVENT_SOURCE_PROCMON;
    event->timestamp_ns = ts;
    // Report the pid as user-space's /proc will show it, not the target's
    // innermost-namespace pid; user-space resolves cmdline/exe from this.
    event->pid = current_observer_pid();
    event->tid = tid;
    event->ppid = ppid;
    event->uid = uid;
    event->event_type = PROCMON_EVENT_EXEC;
    bpf_get_current_comm(&event->comm, sizeof(event->comm));
    // exit_code only carries data for EXIT events
    event->exit_code = 0;

    bpf_ringbuf_submit(event, 0);
    return 0;
}

// Tracepoint for process exit
SEC("tp/sched/sched_process_exit")
int trace_process_exit(void *ctx)
{
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;

    // Only trace main thread exit (pid == tid)
    if (pid != tid)
        return 0;

    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();
    struct task_struct *task = (struct task_struct *)bpf_get_current_task();

    // Reserve space in ring buffer
    struct procmon_event *event = bpf_ringbuf_reserve(&rb, sizeof(*event), 0);
    if (!event) {
        record_ring_buffer_drop();
        return 0;
    }

    // Fill event
    event->source = EVENT_SOURCE_PROCMON;
    event->timestamp_ns = ts;
    event->pid = current_ns_pid();
    event->tid = tid;
    event->ppid = 0;
    event->uid = uid;
    event->event_type = PROCMON_EVENT_EXIT;
    bpf_get_current_comm(&event->comm, sizeof(event->comm));
    // The sched_process_exit tracepoint format exposes no exit_code field, so
    // read it from the current task via CO-RE. By the time this tracepoint
    // fires, do_exit() has already stored the raw wait(2)-encoded status in
    // task_struct->exit_code; decoding is done in userspace.
    event->exit_code = (u32)BPF_CORE_READ(task, exit_code);

    bpf_ringbuf_submit(event, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
