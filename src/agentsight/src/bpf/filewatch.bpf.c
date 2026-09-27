/*
 * @Descripttion: 
 * @version: 
 * @Author: Jietao Xiao
 * @Date: 2026-06-08 10:48:08
 * @LastEditors: Jietao Xiao
 * @LastEditTime: 2026-06-08 11:52:29
 */
// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File watch BPF program
// Monitors openat syscalls for .jsonl files from traced processes
#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "filewatch.h"
#include "common.h"
#include "cgroup_helper.h"

// Tracepoint for openat - captures file open events
// Filters for .jsonl suffix at BPF layer to minimize user-space overhead
SEC("tp/syscalls/sys_enter_openat")
int trace_openat_enter(struct trace_event_raw_sys_enter *ctx)
{
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;

    u64 cg_id;
    if (!traced_pid_cgroup_gate_allow(pid, &cg_id))
        return 0;

    // Reserve space in ring buffer
    struct filewatch_event *event = bpf_ringbuf_reserve(&rb, sizeof(*event), 0);
    if (!event) {
        record_ring_buffer_drop();
        return 0;
    }

    // Read filename from user-space
    const char *filename_ptr = (const char *)ctx->args[1];
    /* Use long like the helper's return type; narrow checks in two steps so the
     * verifier proves len >= 0 before filename[off + k] pointer arithmetic.
     */
    long len = bpf_probe_read_user_str(event->filename, MAX_FILENAME_LEN, filename_ptr);
    if (len < 0) {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }
    /* len includes null terminator; need at least 8 chars ('x'.jsonl\0) */
    if (len < 8 || len > MAX_FILENAME_LEN) {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }

    /* Check .jsonl suffix (len includes null terminator, so last char is at len-2)
     * suffix starts at offset len-7: '.', 'j', 's', 'o', 'n', 'l', '\0'
     */
    long off = len - 7;
    if (event->filename[off]     != '.' ||
        event->filename[off + 1] != 'j' ||
        event->filename[off + 2] != 's' ||
        event->filename[off + 3] != 'o' ||
        event->filename[off + 4] != 'n' ||
        event->filename[off + 5] != 'l') {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }

    // Fill remaining event fields
    event->source = EVENT_SOURCE_FILEWATCH;
    event->timestamp_ns = bpf_ktime_get_ns();
    event->pid = current_ns_pid();
    event->tid = (u32)pid_tgid;
    event->uid = bpf_get_current_uid_gid();
    event->flags = (s32)ctx->args[2];
    bpf_get_current_comm(&event->comm, sizeof(event->comm));
    event->cgroup_id = cg_id;

    bpf_ringbuf_submit(event, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
