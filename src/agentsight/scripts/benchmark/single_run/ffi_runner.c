/* Minimal FFI host: include conversion and delivery costs without application work. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

#include "agentsight.h"

static volatile sig_atomic_t running = 1;
static uint64_t llm_events, https_events, json_events, read_events;
static FILE *receipts;
static int receipt_error;

static void stop(int signum) {
    (void)signum;
    running = 0;
}

/* Keep typed validation receipts small even when the request contains MiB of padding. */
static void on_llm(const AgentsightLLMData *data, void *user_data) {
    (void)user_data;
    ++llm_events;
    if (receipts) {
        const char *marker = "[agentsight-benchmark:";
        const char *id = data->request_messages ? strstr(data->request_messages, marker) : NULL;
        if (!id) {
            receipt_error = 1;
            return;
        }
        id += strlen(marker);
        size_t length = strcspn(id, "]");
        if (length == 0 || length > 256 || id[length] != ']') {
            receipt_error = 1;
            return;
        }
        if (fprintf(receipts, "T\t%.*s\t%u\t%u\t%d\n", (int)length, id,
                    data->status_code, data->total_tokens, data->llm_usage) < 0) {
            receipt_error = 1;
        }
    }
}

static void on_https(const AgentsightHttpsData *data, void *user_data) {
    (void)data;
    (void)user_data;
    ++https_events;
}

#ifdef AGENTSIGHT_HAS_READ_V2
static void on_event(const AgentsightEvent *data, void *user_data) {
    (void)user_data;
    ++json_events;
    if (data->event_type == AGENTSIGHT_EVENT_TYPE_LLM) {
        ++llm_events;
        /* Forward the existing JSON; parsing and reconciliation run outside this process. */
        if (receipts &&
            (fputs("J\t", receipts) < 0 ||
             fwrite(data->payload_json, 1, data->payload_json_len, receipts) != data->payload_json_len ||
             fputc('\n', receipts) == EOF)) {
            receipt_error = 1;
        }
    } else if (data->event_type == AGENTSIGHT_EVENT_TYPE_HTTPS) {
        ++https_events;
    }
}
#endif

static int consume(AgentsightHandle *handle, int v2) {
    int count;
    if (v2) {
#ifdef AGENTSIGHT_HAS_READ_V2
        /* Generic consumers pay JSON serialization costs, without typed conversion. */
        count = agentsight_read_v2(handle, NULL, NULL, NULL, NULL, on_event, NULL, 0);
#else
        fprintf(stderr, "This AgentSight header does not support agentsight_read_v2\n");
        return -1;
#endif
    } else {
        count = agentsight_read(handle, on_https, NULL, on_llm, NULL, 0);
    }
    if (count < 0) {
        const char *error = agentsight_last_error();
        fprintf(stderr, "FFI read failed: %s\n", error ? error : "no error detail");
    } else {
        read_events += (uint64_t)count;
    }
    if (receipts && fflush(receipts) != 0) {
        receipt_error = 1;
    }
    if (receipt_error) {
        fprintf(stderr, "FFI callback receipt failed: missing benchmark request ID or write error\n");
        return -1;
    }
    return count;
}

static char *read_config(const char *path) {
    FILE *file = fopen(path, "rb");
    if (!file) {
        perror(path);
        return NULL;
    }
    char *json = NULL;
    if (fseek(file, 0, SEEK_END) != 0) {
        goto done;
    }
    long size = ftell(file);
    if (size < 0 || size > 16 * 1024 * 1024 || fseek(file, 0, SEEK_SET) != 0) {
        goto done;
    }
    json = calloc((size_t)size + 1, 1);
    if (json && fread(json, 1, (size_t)size, file) != (size_t)size) {
        free(json);
        json = NULL;
    }
done:
    fclose(file);
    if (!json) {
        fprintf(stderr, "Cannot read FFI configuration: %s\n", path);
    }
    return json;
}

static int write_stats(const char *api, int status) {
    const char *path = getenv("AGENTSIGHT_BENCH_FFI_STATS");
    if (!path) {
        return 0;
    }
    FILE *file = fopen(path, "w");
    if (!file) {
        perror(path);
        return -1;
    }
    int written = fprintf(file,
        "{\"api\":\"%s\",\"pid\":%ld,\"completed\":%s,"
        "\"llm_events\":%" PRIu64 ",\"https_events\":%" PRIu64 ","
        "\"json_events\":%" PRIu64 ",\"read_events\":%" PRIu64 "}\n",
        api, (long)getpid(), status == 0 ? "true" : "false",
        llm_events, https_events, json_events, read_events);
    int closed = fclose(file);
    if (written < 0 || closed != 0) {
        fprintf(stderr, "Cannot write FFI consumer statistics: %s\n", path);
        return -1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 4 || strcmp(argv[1], "trace") || strcmp(argv[2], "--config")) {
        fprintf(stderr, "usage: %s trace --config PATH\n", argv[0]);
        return 2;
    }
    const char *api = getenv("AGENTSIGHT_BENCH_FFI_API");
    if (!api) {
        api = "typed";
    }
    if (strcmp(api, "typed") && strcmp(api, "v2")) {
        fprintf(stderr, "AGENTSIGHT_BENCH_FFI_API must be typed or v2\n");
        return 2;
    }
    int v2 = strcmp(api, "v2") == 0;
    struct sigaction action = {0};
    action.sa_handler = stop;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGINT, &action, NULL) || sigaction(SIGTERM, &action, NULL)) {
        perror("sigaction");
        return 1;
    }
    int status = 1, epfd = -1, started = 0;
    AgentsightConfigHandle *config = NULL;
    AgentsightHandle *handle = NULL;
    const char *receipt_path = getenv("AGENTSIGHT_BENCH_FFI_EVENTS");
    if (receipt_path) {
        receipts = fopen(receipt_path, "wb");
        if (!receipts) {
            perror(receipt_path);
            return 1;
        }
    }
    char *json = read_config(argv[3]);
    if (!json) {
        goto done;
    }
    config = agentsight_config_new();
    if (!config || agentsight_config_load_config(config, json) < 0) {
        goto ffi_error;
    }
    handle = agentsight_new(config);
    if (!handle) {
        goto ffi_error;
    }
    agentsight_config_free(config);
    config = NULL;
    free(json);
    json = NULL;

    epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        perror("epoll_create1");
        goto done;
    }
    struct epoll_event event = {.events = EPOLLIN, .data.fd = agentsight_get_eventfd(handle)};
    if (event.data.fd < 0 || epoll_ctl(epfd, EPOLL_CTL_ADD, event.data.fd, &event) < 0) {
        perror("register AgentSight eventfd");
        goto done;
    }
    if (agentsight_start(handle) < 0) {
        goto ffi_error;
    }
    started = 1;
    fprintf(stderr, "AgentSight FFI version: %s; callbacks: %s\n", agentsight_version(), api);
    status = 0;
    while (running) {
        int ready = epoll_wait(epfd, &event, 1, 1000);
        if (ready < 0 && errno == EINTR) {
            continue;
        }
        if (ready < 0 || (ready > 0 && (event.events & (EPOLLERR | EPOLLHUP)))) {
            fprintf(stderr, "AgentSight eventfd wait failed\n");
            status = 1;
            break;
        }
        if (ready > 0 && consume(handle, v2) < 0) {
            status = 1;
            break;
        }
    }
    goto done;
ffi_error: {
        const char *error = agentsight_last_error();
        fprintf(stderr, "FFI initialization failed: %s\n", error ? error : "no error detail");
    }
done:
    if (started) {
        if (agentsight_stop(handle) < 0 || consume(handle, v2) < 0) {
            status = 1;
        }
    }
    if (handle) {
        agentsight_free(handle);
    }
    if (config) {
        agentsight_config_free(config);
    }
    free(json);
    if (epfd >= 0) {
        close(epfd);
    }
    if (receipts && fclose(receipts) != 0) {
        status = 1;
    }
    if (write_stats(api, status) < 0) {
        status = 1;
    }
    return status;
}
