/* Fixture-only, PID-filtered uprobe link and one bounded kernel counter. */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/bpf.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define INSN(op, dst, src, displacement, value) \
    ((struct bpf_insn){.code = (op), .dst_reg = (dst), .src_reg = (src), \
                       .off = (displacement), .imm = (value)})

static int call_bpf(enum bpf_cmd command, union bpf_attr *attr) {
    return (int)syscall(SYS_bpf, command, attr, sizeof(*attr));
}

/* The caller validates process identity and file offset before entering here.
 * Link attachment uses a kernel PID filter and never installs a system-wide probe.
 * The program additionally checks TGID before touching its single map entry. */
int observation_open(int pid, const char *path, uint64_t offset, int output[2]) {
    union bpf_attr attr = {0};
    attr.map_type = BPF_MAP_TYPE_ARRAY;
    attr.key_size = 4;
    attr.value_size = 8;
    attr.max_entries = 1;
    attr.map_flags = BPF_F_RDONLY;
    int map = call_bpf(BPF_MAP_CREATE, &attr);
    if (map < 0) return -1;
    struct bpf_insn instructions[] = {
        INSN(BPF_JMP | BPF_CALL, 0, 0, 0, BPF_FUNC_get_current_pid_tgid),
        INSN(BPF_ALU64 | BPF_RSH | BPF_K, 0, 0, 0, 32),
        INSN(BPF_JMP | BPF_JNE | BPF_K, 0, 0, 9, pid),
        INSN(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        INSN(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, map),
        INSN(0, 0, 0, 0, 0),
        INSN(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        INSN(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        INSN(BPF_JMP | BPF_CALL, 0, 0, 0, BPF_FUNC_map_lookup_elem),
        INSN(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 2, 0),
        INSN(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 1),
        INSN(BPF_STX | BPF_ATOMIC | BPF_DW, 0, 1, 0, BPF_ADD),
        INSN(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        INSN(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    const char license[] = "GPL";
    memset(&attr, 0, sizeof(attr));
    attr.prog_type = BPF_PROG_TYPE_KPROBE;
    attr.expected_attach_type = BPF_TRACE_UPROBE_MULTI;
    attr.insn_cnt = sizeof(instructions) / sizeof(instructions[0]);
    attr.insns = (uintptr_t)instructions;
    attr.license = (uintptr_t)license;
    int program = call_bpf(BPF_PROG_LOAD, &attr);
    int link = -1;
    if (program >= 0) {
        memset(&attr, 0, sizeof(attr));
        attr.link_create.prog_fd = program;
        attr.link_create.attach_type = BPF_TRACE_UPROBE_MULTI;
        attr.link_create.uprobe_multi.path = (uintptr_t)path;
        attr.link_create.uprobe_multi.offsets = (uintptr_t)&offset;
        attr.link_create.uprobe_multi.cnt = 1;
        attr.link_create.uprobe_multi.pid = pid;
        link = call_bpf(BPF_LINK_CREATE, &attr);
    }
    int error = errno;
    if (program >= 0) close(program);
    if (link < 0) { close(map); errno = error; return -1; }
    output[0] = map;
    output[1] = link;
    return 0;
}

int observation_count(int map, uint64_t *count) {
    uint32_t key = 0;
    union bpf_attr attr = {0};
    attr.map_fd = map;
    attr.key = (uintptr_t)&key;
    attr.value = (uintptr_t)count;
    return call_bpf(BPF_MAP_LOOKUP_ELEM, &attr);
}
