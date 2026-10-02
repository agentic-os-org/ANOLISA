// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File write BPF program header
// Monitors vfs_write calls to .jsonl files from traced processes
#ifndef __FILEWRITE_H
#define __FILEWRITE_H

#define TASK_COMM_LEN         16
#define MAX_FILENAME_LEN      256
#define MAX_FILEWRITE_BUF     (16 * 1024)   // 16KB

typedef signed char         s8;
typedef unsigned char       u8;
typedef signed short        s16;
typedef unsigned short      u16;
typedef signed int          s32;
typedef unsigned int        u32;
typedef signed long long    s64;
typedef unsigned long long  u64;

// File write event - captures write content to .jsonl files
struct filewrite_event {
    u32 source;                         // EVENT_SOURCE_FILEWRITE (5)
    u64 timestamp_ns;
    u32 pid;
    u32 tid;
    u32 uid;
    u32 write_size;                     // original count passed to vfs_write
    u32 buf_size;                       // actual bytes copied (up to MAX_FILEWRITE_BUF - 1)
    char comm[TASK_COMM_LEN];           // process name (16 bytes)
    char filename[MAX_FILENAME_LEN];    // basename from dentry (256 bytes)
    u64 cgroup_id;                      // cgroup inode from get_cgroup_id_compat()
    u8  buf[MAX_FILEWRITE_BUF];         // write content (up to 16KB)
};

#endif /* __FILEWRITE_H */
