// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2023 Yusheng Zheng
//
// Based on sslsniff from BCC by Adrian Lopez & Mark Drayton.
// 15-Aug-2023   Yusheng Zheng   Created this.
#include "vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include "sslsniff.h"
#include "common.h"  

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, u32);
    __type(value, size_t*);
} readbytes_ptrs SEC(".maps");

#define MAX_ENTRIES 1024

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u32);
    __type(value, u64);
} ssl_ptrs SEC(".maps");

#define min(x, y)                      \
    ({                                 \
        typeof(x) _min1 = (x);         \
        typeof(y) _min2 = (y);         \
        (void)(&_min1 == &_min2);      \
        _min1 < _min2 ? _min1 : _min2; \
    })

/* ssl_data per-CPU array removed - ring buffer allocates memory directly */

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u32);
    __type(value, u64);
} start_ns SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u32);
    __type(value, u64);
} bufs SEC(".maps");


static __always_inline u32 trace_allowed(u32 uid, u32 pid)
{
    return is_pid_traced(pid);
}

SEC("uprobe/do_handshake")
int BPF_UPROBE(probe_SSL_rw_enter, void *ssl, void *buf, int num) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    /* store arg info for later lookup */
    u64 ssl_ptr_val = (u64)ssl;
    bpf_map_update_elem(&ssl_ptrs, &tid, &ssl_ptr_val, BPF_ANY);
    bpf_map_update_elem(&bufs, &tid, &buf, BPF_ANY);
    bpf_map_update_elem(&start_ns, &tid, &ts, BPF_ANY);
    return 0;
}

/* Emit ONE SSL record using the per-tier record TYPE (e.g. probe_SSL_data_small)
 * whose buf[] is exactly TIER bytes. The reservation is sizeof(struct TYPE) — a
 * true compile-time constant — and the payload is clamped to TIER and copied
 * WITHIN that type's buf: the only shape the verifier accepts. (Reserving fewer
 * bytes than sizeof(struct) and then writing into a LARGER typed buf[] is
 * REJECTED with EACCES at load — confirmed on 6.6.) A small SSL call reserves a
 * small type, so it no longer pads the shared ring to the 4 MiB worst case
 * (#759). `truncated` is set only when the payload exceeds the chosen tier
 * (possible only at the top 4 MiB tier). */
#define SSL_EMIT_ONE(TYPE, TIER, src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_) \
    do {                                                                        \
        struct TYPE *_d = bpf_ringbuf_reserve(&rb, sizeof(struct TYPE), 0);     \
        if (!_d) {                                                              \
            record_ring_buffer_drop();                                          \
            break;                                                              \
        }                                                                       \
        _d->source = EVENT_SOURCE_SSL;                                          \
        _d->timestamp_ns = (ts_);                                              \
        _d->delta_ns = (delta_);                                               \
        _d->pid = (pid_);                                                       \
        _d->tid = (tid_);                                                       \
        _d->uid = (uid_);                                                       \
        _d->len = (u32)(len_);                                                  \
        _d->rw = (rw_);                                                         \
        _d->is_handshake = (ishs_);                                             \
        _d->ssl_ptr = (ssl_);                                                   \
        _d->truncated = ((u32)(len_) > (u32)(TIER)) ? 1 : 0;                    \
        bpf_get_current_comm(&_d->comm, sizeof(_d->comm));                      \
        /* Re-clamp the copy length to the tier right before the read. Three    \
         * verifier traps must be dodged: (1) a length computed earlier is      \
         * spilled across bpf_get_current_comm() and reloaded as an UNBOUNDED   \
         * scalar; (2) the outer tier-selection already proved len_ <= TIER, so \
         * clang folds a plain `if (_n > TIER)` away as dead code -- yet the     \
         * verifier does NOT carry that bound across the spill, so the clamp     \
         * still has to execute; (3) on kernel 5.15, clang may substitute the   \
         * original signed `len` register for _n at the call site, even after   \
         * the clamp, because it proves they hold the same value — the second   \
         * barrier forces clang to use _n's own (clamped, unsigned) register.   \
         * Without the barriers the load is rejected on 5.15:                   \
         * "R2 min value is negative, either use unsigned or 'var &= const'". */ \
        u32 _n = (u32)(len_);                                                   \
        asm volatile("" : "+r"(_n));                                            \
        if (_n > (u32)(TIER))                                                   \
            _n = (u32)(TIER);                                                   \
        asm volatile("" : "+r"(_n));                                            \
        int _rc = (src_) ? bpf_probe_read_user(&_d->buf, _n, (const char *)(src_)) : -1; \
        if (_rc) { _d->buf_filled = 0; _d->buf_size = 0; }                      \
        else     { _d->buf_filled = 1; _d->buf_size = _n; }                     \
        bpf_ringbuf_submit(_d, 0);                                              \
    } while (0)

/* Pick the smallest tier (and its record type) that holds `len_` and emit one. */
#define SSL_EMIT_TIERED(src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_)       \
    do {                                                                                   \
        u32 _l = (u32)(len_);                                                              \
        if (_l <= SSL_TIER_SMALL)                                                          \
            SSL_EMIT_ONE(probe_SSL_data_small, SSL_TIER_SMALL, src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_);   \
        else if (_l <= SSL_TIER_MEDIUM)                                                    \
            SSL_EMIT_ONE(probe_SSL_data_medium, SSL_TIER_MEDIUM, src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_); \
        else if (_l <= SSL_TIER_LARGE)                                                     \
            SSL_EMIT_ONE(probe_SSL_data_large, SSL_TIER_LARGE, src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_);   \
        else                                                                              \
            SSL_EMIT_ONE(probe_SSL_data_t, MAX_BUF_SIZE, src_, len_, rw_, ts_, delta_, pid_, tid_, uid_, ssl_, ishs_);         \
    } while (0)

static int SSL_exit(struct pt_regs *ctx, int rw) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    /* fetch the buffer pointer + timing stored at enter */
    u64 *bufp = bpf_map_lookup_elem(&bufs, &tid);
    if (bufp == 0)
        return 0;

    u64 *tsp = bpf_map_lookup_elem(&start_ns, &tid);
    if (!tsp)
        return 0;
    u64 delta_ns = ts - *tsp;

    u64 *ssl_ptrp = bpf_map_lookup_elem(&ssl_ptrs, &tid);
    u64 ssl_ptr = ssl_ptrp ? *ssl_ptrp : 0;

    int len = PT_REGS_RC(ctx);
    const char *src = (const char *)*bufp;

    bpf_map_delete_elem(&bufs, &tid);
    bpf_map_delete_elem(&start_ns, &tid);
    bpf_map_delete_elem(&ssl_ptrs, &tid);

    if (len <= 0)  // no data
        return 0;

    /* Tiered emit: reserve the smallest tier that fits `len` (#759), capture up
     * to 4 MiB whole (#763, no regression vs the prior single 4 MiB reserve). */
    SSL_EMIT_TIERED(src, len, rw, ts, delta_ns, ns_pid, tid, uid, ssl_ptr, 0);
    return 0;
}

SEC("uretprobe/SSL_read")
int BPF_URETPROBE(probe_SSL_read_exit) {
    return (SSL_exit(ctx, 0));
}

SEC("uretprobe/SSL_write")
int BPF_URETPROBE(probe_SSL_write_exit) {
    return (SSL_exit(ctx, 1));
}

SEC("uprobe/SSL_write_ex")
int BPF_UPROBE(probe_SSL_write_ex_enter, void *ssl, void *buf, size_t num, size_t *readbytes) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    u64 ssl_ptr_val = (u64)ssl;
    bpf_map_update_elem(&ssl_ptrs, &tid, &ssl_ptr_val, BPF_ANY);
    bpf_map_update_elem(&bufs, &tid, &buf, BPF_ANY);
    bpf_map_update_elem(&start_ns, &tid, &ts, BPF_ANY); 
    
    bpf_map_update_elem(&readbytes_ptrs, &tid, &readbytes, BPF_ANY);

    return 0;
}

SEC("uprobe/SSL_read_ex")
int BPF_UPROBE(probe_SSL_read_ex_enter, void *ssl, void *buf, size_t num, size_t *readbytes) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    u64 ssl_ptr_val = (u64)ssl;
    bpf_map_update_elem(&ssl_ptrs, &tid, &ssl_ptr_val, BPF_ANY);
    bpf_map_update_elem(&bufs, &tid, &buf, BPF_ANY);
    bpf_map_update_elem(&start_ns, &tid, &ts, BPF_ANY); 

    bpf_map_update_elem(&readbytes_ptrs, &tid, &readbytes, BPF_ANY);

    return 0;
}

static int ex_SSL_exit(struct pt_regs *ctx, int rw, int len) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    /* store arg info for later lookup */
    u64 *bufp = bpf_map_lookup_elem(&bufs, &tid);
    if (bufp == 0)
        return 0;

    u64 *tsp = bpf_map_lookup_elem(&start_ns, &tid);
    if (!tsp)
        return 0;
    u64 delta_ns = ts - *tsp;

    /* lookup ssl pointer for connection tracking */
    u64 *ssl_ptrp = bpf_map_lookup_elem(&ssl_ptrs, &tid);
    u64 ssl_ptr = ssl_ptrp ? *ssl_ptrp : 0;

    const char *src = (const char *)*bufp;

    bpf_map_delete_elem(&bufs, &tid);
    bpf_map_delete_elem(&start_ns, &tid);
    bpf_map_delete_elem(&ssl_ptrs, &tid);

    if (len <= 0)  // no data
        return 0;

    /* Tiered emit (same as SSL_exit). The old `& 0xFFFFF` mask silently capped
     * the _ex path at 1 MiB; the tier clamp restores the full 4 MiB cap,
     * consistent with the non-_ex path. The per-tier branch also narrows the
     * (user-read-derived) len for the verifier. */
    SSL_EMIT_TIERED(src, len, rw, ts, delta_ns, ns_pid, tid, uid, ssl_ptr, 0);
    return 0;
}

SEC("uretprobe/SSL_write_ex")
int BPF_URETPROBE(probe_SSL_write_ex_exit)
{
    u32 tid = (u32)bpf_get_current_pid_tgid();
    size_t **readbytes_ptr = bpf_map_lookup_elem(&readbytes_ptrs, &tid);
    if (!readbytes_ptr)
        return 0;

    size_t written = 0;
    bpf_probe_read_user(&written, sizeof(written), *readbytes_ptr);
    bpf_map_delete_elem(&readbytes_ptrs, &tid);

    int ret = PT_REGS_RC(ctx);
    int len = (ret == 1) ? written : 0;

    return ex_SSL_exit(ctx, 1, len);
}

SEC("uretprobe/SSL_read_ex")
int BPF_URETPROBE(probe_SSL_read_ex_exit)
{
    u32 tid = (u32)bpf_get_current_pid_tgid();
    size_t **readbytes_ptr = bpf_map_lookup_elem(&readbytes_ptrs, &tid);
    if (!readbytes_ptr)
        return 0;

    size_t written = 0;
    bpf_probe_read_user(&written, sizeof(written), *readbytes_ptr);
    bpf_map_delete_elem(&readbytes_ptrs, &tid);

    int ret = PT_REGS_RC(ctx);
    int len = (ret == 1) ? written : 0;

    return ex_SSL_exit(ctx, 0, len);
}

SEC("uprobe/do_handshake")
int BPF_UPROBE(probe_SSL_do_handshake_enter, void *ssl) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u64 ts = bpf_ktime_get_ns();
    u32 uid = bpf_get_current_uid_gid();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid) {
        return 0;
    }

    /* store arg info for later lookup */
    u64 ssl_ptr_val = (u64)ssl;
    bpf_map_update_elem(&ssl_ptrs, &tid, &ssl_ptr_val, BPF_ANY);
    bpf_map_update_elem(&start_ns, &tid, &ts, BPF_ANY);
    return 0;
}

SEC("uretprobe/do_handshake")
int BPF_URETPROBE(probe_SSL_do_handshake_exit) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();
    int ret = 0;

    /* use kernel terminology here for tgid/pid: */
    u32 tgid = pid_tgid >> 32;

    u32 ns_pid = trace_allowed(tgid, pid);
    if (!ns_pid) {
        return 0;
    }

    u64 *tsp = bpf_map_lookup_elem(&start_ns, &tid);
    if (tsp == 0)
        return 0;

    ret = PT_REGS_RC(ctx);
    if (ret <= 0)  // handshake failed
        return 0;

    /* Handshake records carry no payload: reserve ONLY the header (no buf), so a
     * handshake costs ~one header in the ring instead of the prior 4 MiB reserve. */
    struct probe_SSL_data_t *data =
        bpf_ringbuf_reserve(&rb, __builtin_offsetof(struct probe_SSL_data_t, buf), 0);
    if (!data) {
        record_ring_buffer_drop();
        return 0;
    }

    data->source = EVENT_SOURCE_SSL;
    data->timestamp_ns = ts;
    data->delta_ns = ts - *tsp;
    data->pid = ns_pid;
    data->tid = tid;
    data->uid = uid;
    data->len = ret;
    data->buf_filled = 0;
    data->buf_size = 0;
    data->rw = 2;
    data->is_handshake = 1;
    data->truncated = 0;
    data->ssl_ptr = 0;
    bpf_get_current_comm(&data->comm, sizeof(data->comm));
    bpf_map_delete_elem(&start_ns, &tid);

    /* submit to ring buffer */
    bpf_ringbuf_submit(data, 0);
    return 0;
}

/* ─── cosh-ng plaintext tap (#3042, #3115) ──────────────────────────────
 *
 * cosh-ng links rustls, which exports no SSL_read/SSL_write, so none of the
 * probes above can attach to it. Rather than locate rustls' internal plaintext
 * functions, this hooks an attach point the process exports on purpose:
 *
 *   void cosh_llm_plaintext_tap(u32 dir, const u8 *buf, usize len)
 *     rdi = dir (1 = request, 0 = response), rsi = buf, rdx = len
 *
 * Byte-pattern matching on rustls internals was tried first and withdrawn
 * (#3115): the prologue bytes encode rustc's register allocation, and the
 * release RPM is built with whatever system Rust the build host provides, so a
 * compiler bump silently reduced capture to zero. An exported symbol is immune
 * to that -- libbpf resolves the address at attach time, so the same name works
 * across toolchains even though the address moves.
 *
 * Everything the old approach needed to reconstruct is now given directly: the
 * payload is one contiguous buffer, so there is no gather-write reassembly, no
 * struct layout to decode, and no cipher-suite variants.
 */

/* Direction values are fixed by the tap's contract, not by this file. */
#define COSH_TAP_DIR_RESPONSE 0
#define COSH_TAP_DIR_REQUEST 1

SEC("uprobe/cosh_llm_plaintext_tap")
int BPF_UPROBE(probe_cosh_plaintext_tap, u32 dir, const char *buf, u64 len) {
    u64 pid_tgid = bpf_get_current_pid_tgid();
    u32 pid = pid_tgid >> 32;
    u32 tid = (u32)pid_tgid;
    u32 uid = bpf_get_current_uid_gid();
    u64 ts = bpf_ktime_get_ns();

    u32 ns_pid = trace_allowed(uid, pid);
    if (!ns_pid)
        return 0;
    if (!buf || len == 0 || len > MAX_BUF_SIZE)
        return 0;

    /* The tap reports whole request bodies and whole response chunks, so a
     * process-wide identity is enough to group them: unlike the SSL_* probes
     * there is no per-connection handle to key on, and cosh-core issues one LLM
     * call at a time. Using the pid keeps both directions on one connection,
     * which is what the HTTP aggregator needs to pair them.
     *
     * Captured at function entry, so there is no paired uretprobe and hence no
     * measurable duration. */
    int rw = (dir == COSH_TAP_DIR_REQUEST) ? 1 : 0;
    SSL_EMIT_TIERED(buf, len, rw, ts, 0, ns_pid, tid, uid, (u64)ns_pid, 0);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
