// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// UDP DNS BPF program — minimal kernel-side probe
// Only captures raw DNS payload from UDP port 53 queries.
// All complex parsing (QNAME extraction, deduplication) is done in userspace.

#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>
#include "udpdns.h"

// Include common.h with traced_processes map - skip already-traced processes
#include "common.h"

// DNS header constants
#define DNS_HEADER_LEN 12
#define DNS_QR_MASK    0x80  // QR bit in flags byte 0 (1=response, 0=query)
#define DNS_PORT       53

// Payload buffer bitmask (DNS_PAYLOAD_MAX = 256, power of 2)
#define PAYLOAD_MASK (DNS_PAYLOAD_MAX - 1)  // 0xFF

// --- CO-RE compatibility for iov_iter fields ---
// Kernel 6.4+ renamed iov_iter.iov to iov_iter.__iov.
struct iov_iter___new {
    const struct iovec *__iov;
};

// Kernel 6.0+ added ITER_UBUF: read()/write() on sockets use ubuf instead of iov.
struct iov_iter___ubuf {
    void *ubuf;
    u8 iter_type;
};

#define ITER_UBUF_TYPE 5

struct dns_buf_info {
    void *buf;
    __u64 len;
};

static __always_inline struct dns_buf_info get_dns_buf_info(struct msghdr *msg)
{
    struct dns_buf_info info = { .buf = NULL, .len = 0 };
    struct iov_iter *iter = &msg->msg_iter;

    struct iov_iter___ubuf *ubuf_iter = (void *)iter;
    if (bpf_core_field_exists(ubuf_iter->ubuf)) {
        u8 type = BPF_CORE_READ(ubuf_iter, iter_type);
        if (type == ITER_UBUF_TYPE) {
            info.buf = BPF_CORE_READ(ubuf_iter, ubuf);
            info.len = BPF_CORE_READ(iter, count);
            return info;
        }
    }

    struct iov_iter___new *new_iter = (void *)iter;
    const struct iovec *iov;
    if (bpf_core_field_exists(new_iter->__iov)) {
        iov = BPF_CORE_READ(new_iter, __iov);
    } else {
        iov = BPF_CORE_READ(iter, iov);
    }
    if (!iov)
        return info;
    info.buf = BPF_CORE_READ(iov, iov_base);
    info.len = BPF_CORE_READ(iov, iov_len);
    return info;
}

SEC("fentry/udp_sendmsg")
int BPF_PROG(trace_udp_sendmsg, struct sock *sk, struct msghdr *msg, size_t size)
{
    // Fast path: check destination port == 53 (DNS)
    __u16 dport = BPF_CORE_READ(sk, __sk_common.skc_dport);
    if (dport != bpf_htons(DNS_PORT))
        return 0;

    // Minimum DNS query: 12 (header) + 1 (min QNAME) + 4 (QTYPE+QCLASS) = 17 bytes
    if (size < 17)
        return 0;

    // Get process info
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u32 tid = (__u32)pid_tgid;

    // Skip processes already being traced - no need to discover them again
    if (is_pid_traced(pid))
        return 0;

    struct dns_buf_info buf = get_dns_buf_info(msg);
    if (!buf.buf || buf.len < 17)
        return 0;

    // Reserve ring buffer event
    struct udpdns_event *event = bpf_ringbuf_reserve(&rb, sizeof(*event), 0);
    if (!event) {
        record_ring_buffer_drop();
        return 0;
    }

    // Clamp read size to payload buffer capacity
    // Use >= (not >) so read_len never equals DNS_PAYLOAD_MAX (256),
    // because the subsequent mask (read_len & 0xFF) would zero it.
    __u32 read_len = buf.len;
    if (read_len >= DNS_PAYLOAD_MAX)
        read_len = DNS_PAYLOAD_MAX - 1;

    // Read user-space DNS buffer into event payload
    int ret = bpf_probe_read_user(event->payload, read_len & PAYLOAD_MASK, buf.buf);
    if (ret != 0) {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }

    // --- Minimal DNS header validation (cheap, no loops) ---
    // QR bit must be 0 (query, not response)
    if (event->payload[2] & DNS_QR_MASK) {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }

    // QDCOUNT must be >= 1
    __u16 qdcount = ((__u16)event->payload[4] << 8) | (__u16)event->payload[5];
    if (qdcount == 0) {
        bpf_ringbuf_discard(event, 0);
        return 0;
    }

    // Fill event metadata
    event->source = EVENT_SOURCE_UDPDNS;
    event->timestamp_ns = bpf_ktime_get_ns();
    event->pid = current_ns_pid();
    event->tid = tid;
    event->uid = bpf_get_current_uid_gid();
    event->payload_len = read_len;
    bpf_get_current_comm(&event->comm, sizeof(event->comm));

    bpf_ringbuf_submit(event, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
