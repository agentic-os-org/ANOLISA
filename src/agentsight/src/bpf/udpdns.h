// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// UDP DNS event structure definition
// BPF side only captures raw DNS payload; domain parsing is done in userspace.

#ifndef UDPDNS_H
#define UDPDNS_H

#define TASK_COMM_LEN 16
// Raw DNS payload buffer (RFC 1035: UDP DNS messages <= 512 bytes)
// We cap at 256 to keep ringbuf events small; covers virtually all real queries.
#define DNS_PAYLOAD_MAX 256

typedef unsigned char       u8;
typedef unsigned short      u16;
typedef unsigned int        u32;
typedef unsigned long long  u64;

struct udpdns_event {
    u32 source;             // EVENT_SOURCE_UDPDNS (6)
    u64 timestamp_ns;
    u32 pid;
    u32 tid;
    u32 uid;
    u32 payload_len;        // actual DNS payload length captured
    char comm[TASK_COMM_LEN];
    u8 payload[DNS_PAYLOAD_MAX]; // raw DNS packet bytes (header + question)
};

#endif
