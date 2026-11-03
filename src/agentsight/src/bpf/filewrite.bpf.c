// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File write BPF program
// Monitors vfs_write calls from traced processes writing to agent session
// files (.jsonl and cosh-core atomic-write temp files)
// Uses fentry (BPF trampoline) for minimal overhead on the hot vfs_write path
#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "filewrite.h"
#include "common.h"
#include "cgroup_helper.h"

// UUID format: 8-4-4-4-12 hex digits with hyphens (36 chars total)
#define UUID_LEN 36
// Expected filename: <uuid>.jsonl = 36 + 6 = 42 chars
#define UUID_JSONL_LEN (UUID_LEN + 6)
// cosh-core atomic-write temp file: .<uuid>.<uuid>.tmp = 1 + 36 + 1 + 36 + 4 = 78 chars
#define COSH_TMP_LEN (1 + UUID_LEN + 1 + UUID_LEN + 4)

static __always_inline int is_hex(char c)
{
    return (c >= '0' && c <= '9') ||
           (c >= 'a' && c <= 'f') ||
           (c >= 'A' && c <= 'F');
}

// Validate UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
// Hyphens at positions 8, 13, 18, 23; hex digits elsewhere
static __always_inline int is_uuid(const char *s)
{
    #pragma unroll
    for (int i = 0; i < UUID_LEN; i++) {
        if (i == 8 || i == 13 || i == 18 || i == 23) {
            if (s[i] != '-')
                return 0;
        } else {
            if (!is_hex(s[i]))
                return 0;
        }
    }
    return 1;
}

// fentry hook on vfs_write - triggers before write executes
// Signature: ssize_t vfs_write(struct file *file, const char __user *buf, size_t count, loff_t *pos)
SEC("fentry/vfs_write")
int BPF_PROG(trace_vfs_write, struct file *file, const char *buf, size_t count, loff_t *pos)
{
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;

    u64 cg_id;
    if (!traced_pid_cgroup_gate_allow(pid, &cg_id))
        return 0;

    // Extract filename from file->f_path.dentry->d_name.name (basename)
    // Check early so we can skip non-.jsonl files before reserving ringbuf
    const unsigned char *name_ptr = BPF_CORE_READ(file, f_path.dentry, d_name.name);
    if (!name_ptr)
        return 0;

    char fname[MAX_FILENAME_LEN];
    int ret = bpf_probe_read_kernel_str(fname, sizeof(fname), name_ptr);
    if (ret <= 0)
        return 0;

    // We accept three filename shapes; all checks use only compile-time fixed
    // offsets, so the eBPF verifier doesn't have to track dynamic length math.
    //   1. `<UUID>.jsonl`              (exactly 42 chars + NUL = 43)
    //                                  OpenClaw / Cosh / Claude Code
    //   2. `rollout-<...>-<UUID>.jsonl`
    //                                  Codex CLI rollouts; userspace
    //                                  extracts the trailing UUID via
    //                                  ResponseSessionMapper.
    //   3. `.<UUID>.<UUID>.tmp`        (exactly 78 chars + NUL = 79)
    //                                  cosh-core atomic session writes:
    //                                  content goes to this temp file and is
    //                                  then renamed to `<session-UUID>.json`,
    //                                  so the rename target never reaches
    //                                  vfs_write — the temp name is the only
    //                                  chance to capture session content.
    //                                  Userspace extracts the first UUID as
    //                                  the session id.
    //
    // The rollout check is intentionally a *prefix* match (`rollout-`)
    // without verifying the `.jsonl` suffix: the eBPF verifier rejects
    // variable-length suffix checks, and the userspace
    // `extract_session_id` performs precise filtering. The extra ring
    // buffer events from non-jsonl `rollout-*` files are negligible
    // because such files are rare outside Codex's own session directory.
    int matched_strict = (ret == UUID_JSONL_LEN + 1)
        && fname[UUID_LEN]     == '.'
        && fname[UUID_LEN + 1] == 'j'
        && fname[UUID_LEN + 2] == 's'
        && fname[UUID_LEN + 3] == 'o'
        && fname[UUID_LEN + 4] == 'n'
        && fname[UUID_LEN + 5] == 'l'
        && is_uuid(fname);

    int matched_rollout = (ret >= 9)
        && fname[0] == 'r'
        && fname[1] == 'o'
        && fname[2] == 'l'
        && fname[3] == 'l'
        && fname[4] == 'o'
        && fname[5] == 'u'
        && fname[6] == 't'
        && fname[7] == '-';

    // Strict full-pattern match (both UUIDs validated) so that generic
    // dotfile temp names from other programs don't flood the ring buffer.
    int matched_cosh_tmp = (ret == COSH_TMP_LEN + 1)
        && fname[0] == '.'
        && fname[1 + UUID_LEN] == '.'
        && fname[2 + 2 * UUID_LEN] == '.'
        && fname[3 + 2 * UUID_LEN] == 't'
        && fname[4 + 2 * UUID_LEN] == 'm'
        && fname[5 + 2 * UUID_LEN] == 'p'
        && is_uuid(fname + 1)
        && is_uuid(fname + 2 + UUID_LEN);

    if (!matched_strict && !matched_rollout && !matched_cosh_tmp)
        return 0;

    // Reserve space in ring buffer
    struct filewrite_event *event = bpf_ringbuf_reserve(&rb, sizeof(*event), 0);
    if (!event) {
        record_ring_buffer_drop();
        return 0;
    }

    // Fill metadata
    event->source = EVENT_SOURCE_FILEWRITE;
    event->timestamp_ns = bpf_ktime_get_ns();
    // Use the tgid-level ns pid so this event correlates with sslsniff
    // (which also records the process-group pid). `pid_tgid >> 32` is the
    // host tgid; the gate above already returned the corresponding ns pid
    // when the lookup hit the traced map, but we want a consistent value
    // for cross-probe correlation with the HTTP/SSE pipeline.
    event->pid = pid;
    event->tid = (u32)pid_tgid;
    event->uid = bpf_get_current_uid_gid();
    event->write_size = (u32)count;
    bpf_get_current_comm(&event->comm, sizeof(event->comm));
    event->cgroup_id = cg_id;

    // Copy filename we already read into the event
    __builtin_memcpy(event->filename, fname, MAX_FILENAME_LEN);

    // Copy write content (up to MAX_FILEWRITE_BUF - 1 bytes)
    // Pre-clamp before masking to avoid zeroing at power-of-two boundaries:
    // e.g. 16384 & 0x3FFF = 0, which would silently lose all content.
    u32 copy_size = (u32)count;
    if (copy_size >= MAX_FILEWRITE_BUF)
        copy_size = MAX_FILEWRITE_BUF - 1;
    copy_size &= (MAX_FILEWRITE_BUF - 1);

    ret = bpf_probe_read_user(event->buf, copy_size, buf);
    if (ret != 0) {
        // Failed to read user buffer
        bpf_ringbuf_discard(event, 0);
        return 0;
    }
    event->buf_size = copy_size;

    bpf_ringbuf_submit(event, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
