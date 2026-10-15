// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// TCP plain-text traffic capture BPF program.
// Hooks tcp_sendmsg (fentry) and tcp_recvmsg (fentry+fexit) to capture
// HTTP traffic on configurable IP/port targets. Emits probe_SSL_data_t events
// (same format as sslsniff) so the entire downstream pipeline works unchanged.
// Filters by destination IP/port only; no process-level filtering.

#define NO_TRACED_PROCESSES_MAP
#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>
#include "sslsniff.h"
#include "common.h"

// MSG_PEEK is a socket flag not exported by vmlinux.h
#ifndef MSG_PEEK
#define MSG_PEEK 2
#endif

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

// ITER_UBUF = 5 in kernel 6.0+ (ITER_IOVEC=0, ITER_KVEC=1, ITER_BVEC=2, ...)
#define ITER_UBUF_TYPE 5

// Result of extracting user buffer from msghdr
struct msg_buf_info {
    void *buf;   // user-space buffer pointer
    u64 len;     // length of THIS buffer (not total msg size)
};

// Extract user-space buffer pointer and length from msghdr's iov_iter.
// For ITER_UBUF: returns ubuf pointer and iter->count (contiguous).
// For ITER_IOVEC: returns iov[0].iov_base and iov[0].iov_len (first segment only).
//   writev() scatters data across iovecs; we capture only the first segment
//   to avoid reading beyond its boundary into unrelated memory.
static __always_inline struct msg_buf_info get_msg_buf_info(struct msghdr *msg)
{
    struct msg_buf_info info = { .buf = NULL, .len = 0 };
    struct iov_iter *iter = &msg->msg_iter;

    // Try ITER_UBUF first (kernel 6.0+, used by read()/write() on sockets)
    struct iov_iter___ubuf *ubuf_iter = (void *)iter;
    if (bpf_core_field_exists(ubuf_iter->ubuf)) {
        u8 type = BPF_CORE_READ(ubuf_iter, iter_type);
        if (type == ITER_UBUF_TYPE) {
            info.buf = BPF_CORE_READ(ubuf_iter, ubuf);
            info.len = BPF_CORE_READ(iter, count);
            return info;
        }
    }

    // Fall back to ITER_IOVEC (sendmsg/recvmsg/writev syscalls)
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

// --- IP/port filter map (populated from userspace) ---
// Key: destination IP + port, 0 = wildcard (any IP or any port).
struct tcp_target_key {
    __be32 ip;    // destination IPv4, network byte order; 0 = any IP
    __be16 port;  // destination port, network byte order; 0 = any port
    __u16  pad;   // alignment padding
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 64);
    __type(key, struct tcp_target_key);
    __type(value, __u8);
} tcp_targets SEC(".maps");

// --- Per-connection HTTP protocol cache ---
// Once a connection is identified as HTTP (first request/response matches),
// all subsequent data on that connection is passed through without re-checking.
// This is critical for SSE/chunked responses where later chunks don't start
// with HTTP keywords.  LRU eviction handles cleanup without explicit close hooks.
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 4096);
    __type(key, u64);   // sock pointer as connection identifier
    __type(value, u8);  // 1 = confirmed HTTP
} tcp_http_conns SEC(".maps");

// --- Stash map for tcp_recvmsg entry → exit ---
struct tcp_recv_args {
    u64 sk;           // struct sock * as u64
    u64 user_buf;     // user-space buffer pointer captured at fentry
    u64 buf_len;      // buffer capacity captured at fentry
    u64 start_ns;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, u32);   // tid
    __type(value, struct tcp_recv_args);
} tcp_recv_stash SEC(".maps");

// Check if the connection's destination matches any configured target.
// Lookup priority (most-specific first):
//   1. exact ip+port match
//   2. ip-only match (port=0 means any port)
//   3. port-only match (ip=0 means any ip)
//   4. full wildcard (ip=0, port=0) — capture every TCP connection
static __always_inline bool is_target_conn(struct sock *sk)
{
    struct tcp_target_key key = {};
    __be32 daddr = BPF_CORE_READ(sk, __sk_common.skc_daddr);
    __be16 dport = BPF_CORE_READ(sk, __sk_common.skc_dport);

    // 1. exact ip+port
    key.ip = daddr;
    key.port = dport;
    if (bpf_map_lookup_elem(&tcp_targets, &key))
        return true;

    // 2. ip-only (port wildcard)
    key.port = 0;
    if (bpf_map_lookup_elem(&tcp_targets, &key))
        return true;

    // 3. port-only (ip wildcard)
    key.ip = 0;
    key.port = dport;
    if (bpf_map_lookup_elem(&tcp_targets, &key))
        return true;

    // 4. full wildcard — match-all (ip=0, port=0)
    key.port = 0;
    return bpf_map_lookup_elem(&tcp_targets, &key) != NULL;
}

// Lightweight HTTP protocol detection on already-copied buffer.
// Returns true if the payload starts with a known HTTP method or "HTTP" response.
// Used to filter non-HTTP traffic (TLS, Redis, MySQL, etc.) in the BPF layer
// before submitting to the ring buffer, avoiding costly kernel→userspace copies.
static __always_inline bool is_http_payload(const char *buf, u32 len)
{
    if (len < 4)
        return false;
    if (buf[0] == 'H' && buf[1] == 'T' && buf[2] == 'T' && buf[3] == 'P')
        return true;
    if (buf[0] == 'G' && buf[1] == 'E' && buf[2] == 'T' && buf[3] == ' ')
        return true;
    if (buf[0] == 'P' && buf[1] == 'O' && buf[2] == 'S' && buf[3] == 'T')
        return true;
    if (buf[0] == 'P' && buf[1] == 'U' && buf[2] == 'T' && buf[3] == ' ')
        return true;
    if (buf[0] == 'H' && buf[1] == 'E' && buf[2] == 'A' && buf[3] == 'D')
        return true;
    if (len >= 6 && buf[0] == 'D' && buf[1] == 'E' && buf[2] == 'L' &&
        buf[3] == 'E' && buf[4] == 'T' && buf[5] == 'E')
        return true;
    if (len >= 5 && buf[0] == 'P' && buf[1] == 'A' && buf[2] == 'T' &&
        buf[3] == 'C' && buf[4] == 'H')
        return true;
    if (buf[0] == 'O' && buf[1] == 'P' && buf[2] == 'T' && buf[3] == 'I')
        return true;
    if (buf[0] == 'C' && buf[1] == 'O' && buf[2] == 'N' && buf[3] == 'N')
        return true;
    return false;
}

// --- Tiered ring-buffer emit (shares sslsniff.h per-tier record types) ---
// tcpsniff is a second EVENT_SOURCE_SSL producer on the SAME shared ring as
// sslsniff. #1000 made sslsniff reserve the smallest per-tier record that fits
// each payload, but tcpsniff still reserved the full 4 MiB probe_SSL_data_t per
// event (sizeof(*data)) — so tcpsniff's reservations alone re-pad the shared ring
// to the 4 MiB worst case and drop events under burst (#759). Reserve the
// smallest tier here too. This IS sslsniff's proven SSL_EMIT_ONE idiom: a single
// read at FIXED offset 0 with an asm-barrier clamp, plus the tcpsniff HTTP first-
// payload gate. A writev() request (iov[0] headers + iov[1] body) is emitted as
// TWO records (see trace_tcp_sendmsg), each a single fixed-offset read — a second
// copy at a VARIABLE destination offset is rejected by the 6.6 verifier (it cannot
// prove off+len <= tier when both vary), so we do not concatenate into one buf;
// the downstream reassembles per-connection (by ssl_ptr) as it does for any
// chunked stream. Top tier is MAX_BUF_SIZE (4 MiB), which also restores the full
// capture cap the old `& 0xFFFFF` mask silently clipped to ~1 MiB (matches #1000's
// SSL _ex fix); `truncated` is set when the payload exceeds the chosen tier.
#define TCP_EMIT_ONE(TYPE, TIER, sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_) \
    do {                                                                              \
        struct TYPE *_d = bpf_ringbuf_reserve(&rb, sizeof(struct TYPE), 0);           \
        if (!_d) {                                                                    \
            record_ring_buffer_drop();                                                \
            break;                                                                    \
        }                                                                             \
        _d->source = EVENT_SOURCE_SSL;                                                \
        _d->timestamp_ns = (ts_);                                                     \
        _d->delta_ns = (delta_);                                                      \
        _d->pid = (pid_);                                                             \
        _d->tid = (tid_);                                                             \
        _d->uid = (uid_);                                                             \
        _d->len = (u32)(len_);                                                        \
        _d->rw = (rw_);                                                               \
        _d->is_handshake = 0;                                                         \
        _d->truncated = ((u32)(len_) > (u32)(TIER)) ? 1 : 0;                          \
        _d->ssl_ptr = (u64)(sk_);                                                     \
        bpf_get_current_comm(&_d->comm, sizeof(_d->comm));                            \
        /* Re-clamp the copy length to the tier behind an asm barrier so clang       \
         * cannot drop the clamp as dead code while the verifier still gets a fresh  \
         * umax=TIER bound for the read (see #1000 SSL_EMIT_ONE / "R2 min value is   \
         * negative"). Destination is FIXED offset 0 — the only variable-size access \
         * shape the 6.6 verifier accepts here. */                                   \
        u32 _n = (u32)(len_);                                                         \
        asm volatile("" : "+r"(_n));                                                  \
        if (_n > (u32)(TIER))                                                         \
            _n = (u32)(TIER);                                                         \
        int _rc = bpf_probe_read_user(&_d->buf, _n, (const char *)(src_));            \
        if (_rc) {                                                                    \
            _d->buf_filled = 0;                                                       \
            _d->buf_size = 0;                                                         \
            bpf_ringbuf_submit(_d, 0);                                                \
            break;                                                                    \
        }                                                                            \
        /* HTTP gate: emit only connections whose first payload looks like HTTP;     \
         * once confirmed, the LRU map lets later (non-keyword) chunks through. */    \
        u64 _sk_key = (u64)(sk_);                                                     \
        if (!bpf_map_lookup_elem(&tcp_http_conns, &_sk_key)) {                        \
            if (!is_http_payload((const char *)_d->buf, _n)) {                        \
                bpf_ringbuf_discard(_d, 0);                                           \
                break;                                                                \
            }                                                                        \
            u8 _v = 1;                                                                \
            bpf_map_update_elem(&tcp_http_conns, &_sk_key, &_v, BPF_ANY);             \
        }                                                                            \
        _d->buf_filled = 1;                                                           \
        _d->buf_size = _n;                                                            \
        bpf_ringbuf_submit(_d, 0);                                                    \
    } while (0)

// Pick the smallest tier (and its record type) that holds `len_`, then emit one.
#define TCP_EMIT_TIERED(sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_)         \
    do {                                                                             \
        u32 _l = (u32)(len_);                                                        \
        if (_l <= SSL_TIER_SMALL)                                                    \
            TCP_EMIT_ONE(probe_SSL_data_small, SSL_TIER_SMALL, sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_);   \
        else if (_l <= SSL_TIER_MEDIUM)                                              \
            TCP_EMIT_ONE(probe_SSL_data_medium, SSL_TIER_MEDIUM, sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_); \
        else if (_l <= SSL_TIER_LARGE)                                               \
            TCP_EMIT_ONE(probe_SSL_data_large, SSL_TIER_LARGE, sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_);   \
        else                                                                        \
            TCP_EMIT_ONE(probe_SSL_data_t, MAX_BUF_SIZE, sk_, len_, rw_, ts_, delta_, pid_, tid_, uid_, src_);         \
    } while (0)

// Emit a tiered event for a single contiguous user buffer (recv / ubuf write, and
// each writev segment emitted separately by trace_tcp_sendmsg).
static __always_inline int emit_tcp_event_buf(
    struct sock *sk,
    void *user_buf,
    u32 data_len,
    int rw,           // 1=send(request), 0=recv(response)
    u64 start_ns)
{
    if (data_len == 0 || !user_buf)
        return 0;

    u64 now = bpf_ktime_get_ns();
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = current_ns_pid();
    u32 tid = (u32)pid_tgid;
    u32 uid = (u32)bpf_get_current_uid_gid();
    u64 delta = (start_ns > 0) ? (now - start_ns) : 0;

    TCP_EMIT_TIERED(sk, data_len, rw, now, delta, pid, tid, uid, user_buf);
    return 0;
}

// Get the iov pointer from iov_iter (CO-RE safe)
static __always_inline const struct iovec *get_iov_ptr(struct iov_iter *iter)
{
    struct iov_iter___new *new_iter = (void *)iter;
    if (bpf_core_field_exists(new_iter->__iov))
        return BPF_CORE_READ(new_iter, __iov);
    return BPF_CORE_READ(iter, iov);
}

// --- tcp_sendmsg: capture outgoing data (HTTP requests) ---
// For writev() (ITER_IOVEC), data is scattered across multiple iovecs.
// Node.js typically uses writev with iov[0]=HTTP headers, iov[1]=JSON body.
// We read both segments into the event buffer for correct parsing.
// For write() (ITER_UBUF), data is contiguous — single copy.
SEC("fentry/tcp_sendmsg")
int BPF_PROG(trace_tcp_sendmsg, struct sock *sk, struct msghdr *msg, size_t size)
{
    if (!is_target_conn(sk))
        return 0;

    struct iov_iter *iter = &msg->msg_iter;

    // Check if ITER_UBUF (contiguous buffer from write() syscall)
    struct iov_iter___ubuf *ubuf_iter = (void *)iter;
    if (bpf_core_field_exists(ubuf_iter->ubuf)) {
        u8 type = BPF_CORE_READ(ubuf_iter, iter_type);
        if (type == ITER_UBUF_TYPE) {
            void *ubuf = BPF_CORE_READ(ubuf_iter, ubuf);
            u32 count = (u32)BPF_CORE_READ(iter, count);
            if (count > (u32)size)
                count = (u32)size;
            return emit_tcp_event_buf(sk, ubuf, count, 1, 0);
        }
    }

    // ITER_IOVEC: scatter-gather from writev()/sendmsg(). Emit iov[0] (HTTP
    // headers) and iov[1] (body) as TWO separate records — each a single
    // fixed-offset read (a variable-offset second copy into one buf is rejected
    // by the 6.6 verifier). iov[0] goes FIRST so it marks the connection HTTP
    // before iov[1] (a bare body) is gated; the downstream reassembles the
    // request per-connection (by ssl_ptr), as it already does for chunked recv.
    const struct iovec *iov = get_iov_ptr(iter);
    if (!iov)
        return 0;

    void *iov0_base = BPF_CORE_READ(iov, iov_base);
    u64 iov0_len = BPF_CORE_READ(iov, iov_len);
    if (!iov0_base || iov0_len == 0)
        return 0;
    emit_tcp_event_buf(sk, iov0_base, (u32)iov0_len, 1, 0);

    u32 nr_segs = (u32)BPF_CORE_READ(iter, nr_segs);
    if (nr_segs >= 2) {
        const struct iovec *iov1 = &iov[1];
        void *iov1_base = BPF_CORE_READ(iov1, iov_base);
        u64 iov1_len = BPF_CORE_READ(iov1, iov_len);
        if (iov1_base && iov1_len > 0)
            emit_tcp_event_buf(sk, iov1_base, (u32)iov1_len, 1, 0);
    }
    return 0;
}

// --- tcp_recvmsg entry: stash user_buf pointer for fexit ---
SEC("fentry/tcp_recvmsg")
int BPF_PROG(trace_tcp_recvmsg_entry, struct sock *sk, struct msghdr *msg,
             size_t size, int flags)
{
    if (!is_target_conn(sk))
        return 0;

    // Peek flag means data won't be consumed — skip
    if (flags & MSG_PEEK)
        return 0;

    // Capture user buffer pointer NOW, before kernel advances iov_iter
    struct msg_buf_info bi = get_msg_buf_info(msg);
    if (!bi.buf)
        return 0;

    u32 tid = (u32)bpf_get_current_pid_tgid();
    struct tcp_recv_args args = {
        .sk = (u64)sk,
        .user_buf = (u64)bi.buf,
        .buf_len = bi.len,
        .start_ns = bpf_ktime_get_ns(),
    };
    bpf_map_update_elem(&tcp_recv_stash, &tid, &args, BPF_ANY);
    return 0;
}

// --- tcp_recvmsg exit: read received data using stashed buffer pointer ---
SEC("fexit/tcp_recvmsg")
int BPF_PROG(trace_tcp_recvmsg_exit, struct sock *sk, struct msghdr *msg,
             size_t size, int flags, int *addr_len, int ret)
{
    u32 tid = (u32)bpf_get_current_pid_tgid();

    struct tcp_recv_args *args = bpf_map_lookup_elem(&tcp_recv_stash, &tid);
    if (!args)
        return 0;

    u64 stashed_sk = args->sk;
    u64 stashed_buf = args->user_buf;
    u64 stashed_len = args->buf_len;
    u64 start_ns = args->start_ns;
    bpf_map_delete_elem(&tcp_recv_stash, &tid);

    if (ret <= 0)
        return 0;

    // Clamp ret to buffer capacity
    u32 copy_len = (u32)ret;
    if ((u64)ret > stashed_len)
        copy_len = (u32)stashed_len;

    return emit_tcp_event_buf(
        (struct sock *)stashed_sk,
        (void *)stashed_buf,
        copy_len, 0, start_ns);
}

// --- Kernel 5.8–5.17 variants ---
// tcp_recvmsg had an extra `int nonblock` parameter before 5.18 (commit ec095263a965).
// Signature: int tcp_recvmsg(struct sock *sk, struct msghdr *msg, size_t len,
//                            int nonblock, int flags, int *addr_len)
// Userspace tries the new (5.18+) programs first; falls back to these on older kernels.

SEC("fentry/tcp_recvmsg")
int BPF_PROG(trace_tcp_recvmsg_entry_old, struct sock *sk, struct msghdr *msg,
             size_t size, int nonblock, int flags)
{
    if (!is_target_conn(sk))
        return 0;

    if (flags & MSG_PEEK)
        return 0;

    struct msg_buf_info bi = get_msg_buf_info(msg);
    if (!bi.buf)
        return 0;

    u32 tid = (u32)bpf_get_current_pid_tgid();
    struct tcp_recv_args args = {
        .sk = (u64)sk,
        .user_buf = (u64)bi.buf,
        .buf_len = bi.len,
        .start_ns = bpf_ktime_get_ns(),
    };
    bpf_map_update_elem(&tcp_recv_stash, &tid, &args, BPF_ANY);
    return 0;
}

SEC("fexit/tcp_recvmsg")
int BPF_PROG(trace_tcp_recvmsg_exit_old, struct sock *sk, struct msghdr *msg,
             size_t size, int nonblock, int flags, int *addr_len, int ret)
{
    u32 tid = (u32)bpf_get_current_pid_tgid();

    struct tcp_recv_args *args = bpf_map_lookup_elem(&tcp_recv_stash, &tid);
    if (!args)
        return 0;

    u64 stashed_sk = args->sk;
    u64 stashed_buf = args->user_buf;
    u64 stashed_len = args->buf_len;
    u64 start_ns = args->start_ns;
    bpf_map_delete_elem(&tcp_recv_stash, &tid);

    if (ret <= 0)
        return 0;

    u32 copy_len = (u32)ret;
    if ((u64)ret > stashed_len)
        copy_len = (u32)stashed_len;

    return emit_tcp_event_buf(
        (struct sock *)stashed_sk,
        (void *)stashed_buf,
        copy_len, 0, start_ns);
}

char LICENSE[] SEC("license") = "GPL";
