// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2023 Yusheng Zheng
//
// Based on sslsniff from BCC by Adrian Lopez & Mark Drayton.
// 15-Aug-2023   Yusheng Zheng   Created this.
#ifndef __SSLSNIFF_H
#define __SSLSNIFF_H

// SSL/TCP payload capture is tiered: each event reserves the SMALLEST tier that
// fits its payload, so the shared ring buffer (RING_BUFFER_SIZE in common.h) is
// not padded to the 4 MiB worst case for every event (that padding is #759: the
// 32 MiB ring held only ~8 in-flight 4 MiB reservations and dropped events under
// burst). Each tier is a COMPILE-TIME constant so it can be the size argument to
// bpf_ringbuf_reserve (which requires a literal-constant size); the per-record
// reservation is offsetof(struct probe_SSL_data_t, buf) + <tier>.
// MAX_BUF_SIZE is the TOP tier and equals the pre-existing capture cap, so a
// single SSL call up to 4 MiB is still captured whole (no #763 regression).
#define SSL_TIER_SMALL  (16 * 1024)        // 16 KiB
#define SSL_TIER_MEDIUM (128 * 1024)       // 128 KiB
#define SSL_TIER_LARGE  (512 * 1024)       // 512 KiB
#define MAX_BUF_SIZE    (4 * 1024 * 1024)  // 4 MiB top tier (application cap, not a kernel limit)
#define TASK_COMM_LEN 16

typedef signed char         s8;
typedef unsigned char       u8;
typedef signed short        s16;
typedef unsigned short      u16;
typedef signed int          s32;
typedef unsigned int        u32;
typedef signed long long    s64;
typedef unsigned long long  u64;
typedef _Bool bool;
typedef u32 __be32;
typedef u64 __be64;

// Header fields shared byte-for-byte by EVERY tier record type, so a single
// header-prefix decode (offsetof(probe_SSL_data_t, buf)) works for all of them.
#define SSL_DATA_HEADER_FIELDS                                                  \
    u32 source;       /* EVENT_SOURCE_SSL (from common.h) */                    \
    u64 timestamp_ns;                                                           \
    u64 delta_ns;                                                               \
    u32 pid;                                                                    \
    u32 tid;                                                                    \
    u32 uid;                                                                    \
    u32 len;          /* total bytes of the SSL call (> buf_size if truncated) */\
    u32 buf_size;     /* actual bytes copied into buf for THIS record */        \
    int buf_filled;                                                             \
    int rw;                                                                     \
    int is_handshake;                                                           \
    int truncated;    /* 1 if len exceeded the chosen tier's buf capacity */    \
    u64 ssl_ptr;      /* SSL connection pointer for connection tracking */      \
    char comm[TASK_COMM_LEN];

// Per-tier record types. Each one reserves sizeof(its type) — a true
// compile-time constant — and the payload is copied WITHIN its own buf[]. That
// is the ONLY shape the (6.6) verifier accepts: reserving fewer bytes than
// sizeof(struct) and then writing into a LARGER typed buf[] is REJECTED
// (EACCES at load). buf MUST be the last member of every variant, and the
// header is identical (via SSL_DATA_HEADER_FIELDS) so offsetof(buf) is the same
// for all — the userspace decodes the header once and slices buf by buf_size.
// probe_SSL_data_t is the largest (4 MiB) variant and the canonical type the
// userspace uses for the shared header offsets.
struct probe_SSL_data_small  { SSL_DATA_HEADER_FIELDS u8 buf[SSL_TIER_SMALL]; };
struct probe_SSL_data_medium { SSL_DATA_HEADER_FIELDS u8 buf[SSL_TIER_MEDIUM]; };
struct probe_SSL_data_large  { SSL_DATA_HEADER_FIELDS u8 buf[SSL_TIER_LARGE]; };
struct probe_SSL_data_t      { SSL_DATA_HEADER_FIELDS u8 buf[MAX_BUF_SIZE]; };

#endif /* __SSLSNIFF_H */
