/**
 * agentsight_example.c — AgentSight FFI 调用样例
 *
 * 编译:
 *   gcc -o agentsight_example agentsight_example.c -I./include -L./target/release -lagentsight -lpthread -ldl -lm
 *
 * 运行 (需要 root 权限以加载 eBPF):
 *   sudo LD_LIBRARY_PATH=./target/release ./agentsight_example
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>
#include <sys/epoll.h>

#include "agentsight.h"

static volatile int g_running = 1;

static void sigint_handler(int sig) {
    (void)sig;
    g_running = 0;
}

/* ---- 回调函数 ---- */

/**
 * HTTP 事件回调 — 非 LLM 的 HTTPS 流量触发此函数
 */
static void on_https_event(const AgentsightHttpsData *data, void *user_data) {
    (void)user_data;
    printf("[HTTPS] pid=%d process=%s method=%s path=%s status=%d\n",
           data->pid,
           data->process_name,
           data->method ? data->method : "(null)",
           data->path ? data->path : "(null)",
           data->status_code);

    if (data->request_body && data->request_body_len > 0) {
        printf("  request_body (%u bytes): %.128s...\n",
               data->request_body_len, data->request_body);
    }
    if (data->response_body && data->response_body_len > 0) {
        printf("  response_body (%u bytes): %.128s...\n",
               data->response_body_len, data->response_body);
    }
}

/**
 * LLM 事件回调 — 识别为 LLM API 调用时触发此函数
 */
static void on_llm_event(const AgentsightLLMData *data, void *user_data) {
    (void)user_data;
    printf("[LLM] pid=%d process=%s provider=%s model=%s\n",
           data->pid,
           data->process_name,
           data->provider ? data->provider : "(unknown)",
           data->model ? data->model : "(unknown)");

    printf("  url=%s status=%d duration_ms=%.1f\n",
           data->request_url ? data->request_url : "",
           data->status_code,
           (double)data->duration_ns / 1e6);

    if (data->agent_name) {
        printf("  agent_name=%s\n", data->agent_name);
    }
    if (data->session_id) {
        printf("  session_id=%s\n", data->session_id);
    }
    if (data->llm_usage) {
        printf("  tokens: input=%u output=%u total=%u\n",
               data->input_tokens, data->output_tokens, data->total_tokens);
        if (data->cache_read_input_tokens > 0) {
            printf("  cache: creation=%u read=%u\n",
                   data->cache_creation_input_tokens,
                   data->cache_read_input_tokens);
        }
    }
    if (data->finish_reason) {
        printf("  finish_reason=%s\n", data->finish_reason);
    }
    if (data->tools && data->tools_len > 0) {
        printf("  tools (%u bytes): %.256s\n", data->tools_len, data->tools);
    }
    if (data->input_message_delta && data->input_message_delta_len > 0) {
        printf("  input_message_delta (%u bytes): %.256s\n",
               data->input_message_delta_len, data->input_message_delta);
    }
}

int main(void) {
    printf("AgentSight version: %s\n", agentsight_version());

    signal(SIGINT, sigint_handler);
    signal(SIGTERM, sigint_handler);

    /* ---- 1. 创建并配置 ---- */
    AgentsightConfigHandle *cfg = agentsight_config_new();
    if (!cfg) {
        fprintf(stderr, "Failed to create config\n");
        return 1;
    }

    /* 开启详细日志 */
    agentsight_config_set_verbose(cfg, 1);
    /* 日志输出到文件 */
    agentsight_config_set_log_path(cfg, "/tmp/agentsight.log");

    /* HTTPS 域规则：触发 SSL probe attach（捕获到该域的 TLS 流量） */
    agentsight_config_add_https(cfg, "dashscope.aliyuncs.com");

    /* ---- 2. 创建实例 ---- */
    AgentsightHandle *handle = agentsight_new(cfg);
    if (!handle) {
        fprintf(stderr, "agentsight_new failed: %s\n", agentsight_last_error());
        agentsight_config_free(cfg);
        return 1;
    }

    /* config 在 agentsight_new 后可释放 */
    agentsight_config_free(cfg);
    cfg = NULL;

    /* ---- 3. 启动后台采集线程 ---- */
    if (agentsight_start(handle) < 0) {
        fprintf(stderr, "agentsight_start failed: %s\n", agentsight_last_error());
        agentsight_free(handle);
        return 1;
    }

    /* ---- 4. 使用 epoll + eventfd 等待事件 ---- */
    int efd = agentsight_get_eventfd(handle);
    if (efd < 0) {
        fprintf(stderr, "agentsight_get_eventfd failed\n");
        agentsight_stop(handle);
        agentsight_free(handle);
        return 1;
    }

    int epfd = epoll_create1(0);
    struct epoll_event ev = { .events = EPOLLIN, .data.fd = efd };
    epoll_ctl(epfd, EPOLL_CTL_ADD, efd, &ev);

    printf("Listening for agent events... (Ctrl+C to stop)\n");

    struct epoll_event events[1];
    while (g_running) {
        int nfds = epoll_wait(epfd, events, 1, 1000 /* 1s timeout */);
        if (nfds > 0) {
            /* 有事件就绪, 调用 agentsight_read 消费 */
            int n = agentsight_read(handle,
                                    on_https_event, NULL,
                                    on_llm_event, NULL,
                                    0 /* non-blocking */);
            if (n > 0) {
                printf("--- Processed %d event(s) ---\n", n);
            }
        }
    }

    close(epfd);

    /* ---- 5. 停止并释放 ---- */
    printf("\nStopping...\n");
    agentsight_stop(handle);
    agentsight_free(handle);

    printf("Done.\n");
    return 0;
}
