#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int close(int fd) {
    int (*actual_close)(int) = dlsym(RTLD_NEXT, "close");
    static _Atomic int paused;
    const char *ready = getenv("COSH_TEST_BUDGET_READY");
    const char *release = getenv("COSH_TEST_BUDGET_RELEASE");
    const char *target = getenv("COSH_TEST_BUDGET_LOCK");
    if (!atomic_load(&paused) && ready && release && target) {
        char descriptor[64], path[4096];
        snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
        ssize_t length = readlink(descriptor, path, sizeof(path) - 1);
        if (length > 0) {
            path[length] = '\0';
            if (!strcmp(path, target)) {
                if (atomic_exchange(&paused, 1)) return actual_close(fd);
                int marker = open(ready, O_CREAT | O_WRONLY | O_EXCL, 0600);
                if (marker < 0) _exit(91);
                actual_close(marker);
                for (int i = 0; i < 1000 && access(release, F_OK); i++) usleep(10000);
                if (access(release, F_OK)) _exit(92);
            }
        }
    }
    return actual_close(fd);
}

int fsync(int fd) {
    int (*actual_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    static _Atomic int failed;
    const char *target = getenv("COSH_TEST_COMMIT_FSYNC_TARGET");
    const char *phase = getenv("COSH_TEST_COMMIT_FSYNC_PHASE");
    if (target && phase && !atomic_load(&failed)) {
        char descriptor[64], path[4096];
        snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
        ssize_t length = readlink(descriptor, path, sizeof(path) - 1);
        if (length > 0) {
            path[length] = '\0';
            size_t prefix = strlen(target), size = strlen(path);
            int before = !strcmp(phase, "before") && size > prefix + 4
                && !strncmp(path, target, prefix) && path[prefix] == '/'
                && !strchr(path + prefix + 1, '/') && !strcmp(path + size - 4, ".tmp");
            int after = !strcmp(phase, "after") && !strcmp(path, target);
            if ((before || after) && !atomic_exchange(&failed, 1)) {
                errno = EIO;
                return -1;
            }
        }
    }
    return actual_fsync(fd);
}
