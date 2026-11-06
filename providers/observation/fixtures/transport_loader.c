/* Fixture loader: transfer only stats, ring and link fds; keep no pinned object. */
#define _GNU_SOURCE
#include <bpf/bpf.h>
#include <bpf/libbpf.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

int transport_open(int pid, const char *path, uint64_t offset, const char *object,
                   int function, int output[3]) {
    struct bpf_object *loaded = bpf_object__open_file(object, NULL);
    long error = libbpf_get_error(loaded);
    if (!loaded || error) { errno = error ? (int)-error : EINVAL; return -1; }
    int result = -1;
    int copies[3] = {-1, -1, -1};
    int link = -1;
    struct bpf_program *program = bpf_object__find_program_by_name(loaded, "transport_entry");
    if (!program) { errno = EINVAL; goto cleanup; }
    bpf_program__set_type(program, BPF_PROG_TYPE_KPROBE);
    bpf_program__set_expected_attach_type(program, BPF_TRACE_UPROBE_MULTI);
    if (bpf_object__load(loaded)) goto cleanup;
    uint32_t key = 0, configuration[2] = {(uint32_t)pid, (uint32_t)function};
    int config = bpf_object__find_map_fd_by_name(loaded, "config");
    if (config < 0 || bpf_map_update_elem(config, &key, configuration, BPF_ANY)) goto cleanup;
    if (bpf_map_freeze(config)) goto cleanup;
    /* Use the kernel UAPI directly; libbpf 1.2 lacks its multi-uprobe wrapper. */
    union bpf_attr attr = {0};
    attr.link_create.prog_fd = bpf_program__fd(program);
    attr.link_create.attach_type = BPF_TRACE_UPROBE_MULTI;
    attr.link_create.uprobe_multi.path = (uintptr_t)path;
    attr.link_create.uprobe_multi.offsets = (uintptr_t)&offset;
    attr.link_create.uprobe_multi.cnt = 1;
    attr.link_create.uprobe_multi.pid = pid;
    link = (int)syscall(SYS_bpf, BPF_LINK_CREATE, &attr, sizeof(attr));
    if (link < 0) goto cleanup;
    int stats = bpf_object__find_map_fd_by_name(loaded, "stats");
    int events = bpf_object__find_map_fd_by_name(loaded, "events");
    if (stats < 0 || events < 0) { errno = EINVAL; goto cleanup; }
    copies[0] = fcntl(stats, F_DUPFD_CLOEXEC, 3);
    copies[1] = fcntl(events, F_DUPFD_CLOEXEC, 3);
    copies[2] = fcntl(link, F_DUPFD_CLOEXEC, 3);
    if (copies[0] < 0 || copies[1] < 0 || copies[2] < 0) goto cleanup;
    for (int i = 0; i < 3; i++) output[i] = copies[i];
    result = 0;
cleanup:
    error = errno;
    if (link >= 0) close(link);
    bpf_object__close(loaded);
    if (result) for (int i = 0; i < 3; i++) if (copies[i] >= 0) close(copies[i]);
    errno = (int)error;
    return result;
}
