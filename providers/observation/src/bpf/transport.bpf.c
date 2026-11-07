/* Bounded, read-only x86-64 OpenSSL entry snapshots for authorized fixtures. */
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct configuration { __u32 pid; __u32 function; };
struct counters {
    __u64 hits, submitted, ring_full, over_limit, read_failed, invalid_length;
};
struct observation {
    __u64 timestamp_ns, sequence, length;
    __u32 pid, tid, function, captured, status, reserved;
    unsigned char payload[4096];
};
/* Linux x86-64 user register layout at a uprobe, independent of kernel BTF. */
struct registers {
    __u64 r15, r14, r13, r12, rbp, rbx, r11, r10, r9, r8;
    __u64 rax, rcx, rdx, rsi, rdi, orig_rax, rip, cs, flags, rsp, ss;
};
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct configuration);
} config SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __uint(map_flags, BPF_F_RDONLY);
    __type(key, __u32);
    __type(value, struct counters);
} stats SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 1048576);
} events SEC(".maps");

SEC("uprobe")
int transport_entry(struct registers *ctx) {
    __u32 zero = 0;
    struct configuration *configuration = bpf_map_lookup_elem(&config, &zero);
    __u64 process_thread = bpf_get_current_pid_tgid();
    if (!configuration || process_thread >> 32 != configuration->pid) return 0;
    struct counters *count = bpf_map_lookup_elem(&stats, &zero);
    if (!count) return 0;
    __u64 sequence = __sync_fetch_and_add(&count->hits, 1) + 1;
    struct observation *event = bpf_ringbuf_reserve(&events, sizeof(*event), 0);
    if (!event) {
        __sync_fetch_and_add(&count->ring_full, 1);
        return 0;
    }
    event->timestamp_ns = bpf_ktime_get_ns();
    event->sequence = sequence;
    event->pid = process_thread >> 32;
    event->tid = (__u32)process_thread;
    event->function = configuration->function;
    event->length = configuration->function ? ctx->rdx : (__u32)ctx->rdx;
    event->captured = 0;
    event->reserved = 0;
    event->status = 0;
    if (!configuration->function && (__s32)ctx->rdx < 0) {
        event->status = 3;
        __sync_fetch_and_add(&count->invalid_length, 1);
    } else if (event->length > 4096) {
        event->status = 1;
        __sync_fetch_and_add(&count->over_limit, 1);
    } else if (event->length) {
        __u32 size = event->length;
        if (size > 4096) size = 4096;
        if (bpf_probe_read_user(event->payload, size, (void *)ctx->rsi)) {
            event->status = 2;
            __sync_fetch_and_add(&count->read_failed, 1);
        } else {
            event->captured = size;
        }
    }
    /* Userspace reads only captured bytes; unused ring storage is never logged. */
    __sync_fetch_and_add(&count->submitted, 1);
    bpf_ringbuf_submit(event, 0);
    return 0;
}
char LICENSE[] SEC("license") = "GPL";
