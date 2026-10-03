// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File watch BPF program header
// Monitors openat syscalls for .jsonl files from traced processes
#ifndef __FILEWATCH_H
#define __FILEWATCH_H

#define TASK_COMM_LEN    16
#define MAX_FILENAME_LEN 256

typedef signed char         s8;
typedef unsigned char       u8;
typedef signed short        s16;
typedef unsigned short      u16;
typedef signed int          s32;
typedef unsigned int        u32;
typedef signed long long    s64;
typedef unsigned long long  u64;

// File watch event - fixed size
struct filewatch_event {
    u32 source;                      // EVENT_SOURCE_FILEWATCH
    u64 timestamp_ns;
    u32 pid;
    u32 tid;
    u32 uid;
    s32 flags;                       // openat flags (O_RDONLY, O_WRONLY, etc.)
    char comm[TASK_COMM_LEN];
    char filename[MAX_FILENAME_LEN]; // captured file path
    u64 cgroup_id;                   // unified cgroup inode from get_cgroup_id_compat()
};

#endif /* __FILEWATCH_H */
