// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Cgroup ID helper: provides get_cgroup_id_compat() returning a single u64
// cgroup inode ID suitable for container association.
//
// v1/v2 detection is done by user-space and passed via rodata (cgroup_v2_mode).
// - v2: uses bpf_get_current_cgroup_id() helper directly (zero CO-RE overhead)
// - v1: CO-RE reads subsys[MEMORY]->cgroup->kn->id

#ifndef CGROUP_HELPER_H
#define CGROUP_HELPER_H

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>

/* cgroup subsystem index for memory controller (Anolis/RHEL standard) */
#define CGRP_SUBSYS_MEMORY 4

/*
 * User-space sets this before BPF load:
 *   true  = cgroup v2 unified hierarchy (bpf_get_current_cgroup_id() is valid)
 *   false = cgroup v1 (must CO-RE read memory subsys cgroup)
 *
 * Detection: check existence of /sys/fs/cgroup/cgroup.controllers
 */
const volatile bool cgroup_v2_mode = false;

/*
 * Local definition of kernfs_node_id union for CO-RE compatibility.
 * On kernels >= 5.14, this union no longer exists in BTF (kernfs_node.id is
 * directly u64). We define it locally so struct kernfs_node___old compiles
 * on both old and new build environments. The CO-RE resolver matches by
 * field name (not local type name), so this does not affect runtime behavior.
 *
 * Guard with a custom macro to avoid duplicate definition when older vmlinux.h
 * already provides this union via BTF.
 */
#ifndef __KERNFS_NODE_ID_DEFINED
#define __KERNFS_NODE_ID_DEFINED
union kernfs_node_id {
    struct {
        u32 ino;
        u32 generation;
    };
    u64 id;
};
#endif /* __KERNFS_NODE_ID_DEFINED */

/*
 * CO-RE flavor types for kernfs_node->id field compatibility.
 *
 * Three variants covering the full kernel version range:
 *
 *   ___419  — vanilla 4.19: kn->id is "union kernfs_node_id"
 *             (struct { u32 ino; u32 generation; } + u64 id overlay)
 *
 *   ___rh8  — RHEL8 / Anolis 8 ANCK 5.10: KABI preservation wrapper
 *             kn->id is an anonymous union exposing u64 id directly,
 *             with the old union stashed under rh_kabi_hidden_172.
 *             bpf_core_field_exists(kn_rh8->id) is TRUE on RHEL8 and
 *             also on modern kernels where id is already plain u64.
 *
 *   ___new  — alinux3 / Anolis 23+ / upstream >= 5.14:
 *             kn->id is plain u64 with no union wrapper.
 *             Kept for documentation; in practice ___rh8 also covers
 *             this case since the anonymous union member matches u64.
 *
 * Detection order mirrors mem_latency.bpf.c (sysAK reference):
 *   1. bpf_core_field_exists(kn_rh8->id)  → RHEL8 + modern  (u64 accessible)
 *   2. fallback                            → 4.19            (union variant)
 */

/* 4.19-style: id is union kernfs_node_id */
struct kernfs_node___419 {
    const char *name;
    union kernfs_node_id id;
};

/* RHEL8 KABI-preserved layout: anonymous union exposes u64 id directly */
struct kernfs_node___rh8 {
    const char *name;
    union {
        u64 id;
        struct {
            union kernfs_node_id id;
        } rh_kabi_hidden_172;
        union { };
    };
};

/* Modern (>= 5.14 / alinux3): id is plain u64 */
struct kernfs_node___new {
    u64 id;
};

/*
 * __read_kn_id - Read kernfs_node->id with three-variant CO-RE compatibility
 *
 * Priority:
 *   1. RHEL8 / modern: bpf_core_field_exists(kn_rh8->id) TRUE
 *      -> read u64 id directly via anonymous union member
 *   2. 4.19 legacy: id is union kernfs_node_id
 *      -> read the u64 overlay of the union
 */
static __always_inline u64 __read_kn_id(struct kernfs_node *kn)
{
    if (!kn)
        return 0;

    struct kernfs_node___rh8 *kn_rh8 = (void *)kn;

    if (bpf_core_field_exists(kn_rh8->id)) {
        /*
         * RHEL8: anonymous union exposes u64 id directly.
         * Modern kernels (plain u64 id) also match this branch.
         */
        u64 id;
        bpf_core_read(&id, sizeof(u64), &kn_rh8->id);
        return id;
    }

    /* 4.19: kn->id is union kernfs_node_id; read as u64 overlay */
    struct kernfs_node___419 *kn_419 = (void *)kn;
    u64 id;
    bpf_core_read(&id, sizeof(u64), &kn_419->id);
    return id;
}

/*
 * get_cgroup_id_compat - Get the effective cgroup inode ID for container association
 *
 * Strategy (determined by user-space via cgroup_v2_mode rodata):
 *   v2 mode: call bpf_get_current_cgroup_id() directly.
 *            Returns dfl_cgrp->kn->id — the canonical kernel cgroup id.
 *            Zero CO-RE overhead, available since Linux 4.18.
 *
 *   v1 mode: CO-RE read task->cgroups->subsys[MEMORY]->cgroup->kn->id.
 *            Returns the memory controller cgroup's kernfs inode,
 *            matching stat(v1_memory_cgroup_path).st_ino.
 *
 * Kernel requirements: BTF enabled, Linux 4.18+ (for bpf_get_current_cgroup_id)
 * Supported environments: Anolis OS 8 ANCK 5.10+, alinux3/Anolis 23+
 */
static __always_inline u64 get_cgroup_id_compat(void)
{
    if (cgroup_v2_mode) {
        /*
         * cgroup v2 unified hierarchy:
         * bpf_get_current_cgroup_id() returns dfl_cgrp->kn->id directly.
         * No CO-RE reads, no __ksym dependency.
         */
        return bpf_get_current_cgroup_id();
    }

    /*
     * cgroup v1 path:
     * Container isolation is via v1 memory subsystem.
     * Read subsys[MEMORY]->cgroup->kn->id via CO-RE.
     */
    struct task_struct *task = (void *)bpf_get_current_task();
    if (!task)
        return 0;

    struct cgroup_subsys_state *css = BPF_CORE_READ(task, cgroups,
                                                     subsys[CGRP_SUBSYS_MEMORY]);
    if (!css)
        return 0;

    struct cgroup *mem_cgrp = BPF_CORE_READ(css, cgroup);
    if (!mem_cgrp)
        return 0;

    struct kernfs_node *kn = BPF_CORE_READ(mem_cgrp, kn);
    return __read_kn_id(kn);
}

/*
 * traced_pid_cgroup_gate_allow — shared "PID listed OR cgroup allowed" admission.
 *
 * Include `common.h` *before* this header so `traced_processes`, `filter_cgroup_enabled`,
 * `cgroup_filter`, and `NO_CGROUP_FILTER` are defined.
 *
 * @traced_map_pid: key into `traced_processes` (e.g. current tgid for syscalls,
 *                  parent tgid for execve-enter lineage checks).
 * @cg_id_out: filled with get_cgroup_id_compat() for **current task** (event payloads).
 * Returns 1 to continue the probe, 0 to drop.
 */
static __always_inline int traced_pid_cgroup_gate_allow(u32 traced_map_pid, u64 *cg_id_out)
{
    /* Reuse is_pid_traced() dual-lookup: host_pid first, then ns_pid fallback.
     * This preserves container scenarios where user-space registers ns_pid. */
    int pid_allow = is_pid_traced(traced_map_pid) ? 1 : 0;

    u64 cg_id = get_cgroup_id_compat();

    *cg_id_out = cg_id;

#ifndef NO_CGROUP_FILTER
    if (!filter_cgroup_enabled)
        return pid_allow;
    return pid_allow || (bpf_map_lookup_elem(&cgroup_filter, &cg_id) ? 1 : 0);
#else
    return pid_allow;
#endif
}

#endif /* CGROUP_HELPER_H */
