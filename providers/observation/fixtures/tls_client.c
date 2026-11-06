/* Synthetic loopback-only OpenSSL client. No application credentials or traffic. */
#define _POSIX_C_SOURCE 200809L
#include <arpa/inet.h>
#include <dlfcn.h>
#include <openssl/ssl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t gate = PTHREAD_COND_INITIALIZER;
static int go, port, calls, length, delay_us, use_ex;
static SSL_CTX *ctx;
static uint64_t samples[65536];
static size_t sample_count;
static int failures;
static volatile sig_atomic_t signals;

static uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000 + (uint64_t)t.tv_nsec;
}

static void signal_seen(int sig) { (void)sig; signals++; }
static void deny_debug(int sig) { (void)sig; (void)prctl(PR_SET_DUMPABLE, 0); }

static void *worker(void *unused) {
    (void)unused;
    pthread_mutex_lock(&lock);
    while (!go) pthread_cond_wait(&gate, &lock);
    pthread_mutex_unlock(&lock);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_port = htons((uint16_t)port)};
    inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
    SSL *ssl = SSL_new(ctx);
    if (fd < 0 || !ssl || connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0) exit(20);
    SSL_set_fd(ssl, fd);
    if (SSL_connect(ssl) != 1) exit(21);
    unsigned char *payload = malloc((size_t)length + 1);
    if (!payload) exit(22);
    memset(payload, 'A', (size_t)length);
    for (int i = 0; i < calls; i++) {
        uint64_t before = now_ns();
        size_t written = 0;
        int result;
        if (use_ex) result = SSL_write_ex(ssl, payload, (size_t)length, &written);
        else { result = SSL_write(ssl, payload, length); written = result > 0 ? (size_t)result : 0; }
        uint64_t latency = now_ns() - before;
        pthread_mutex_lock(&lock);
        if (sample_count < 65536) samples[sample_count++] = latency;
        if (length > 0 && (result <= 0 || written != (size_t)length)) failures++;
        pthread_mutex_unlock(&lock);
        if (delay_us) { struct timespec pause = {0, (long)delay_us * 1000}; nanosleep(&pause, NULL); }
    }
    if (SSL_shutdown(ssl) == 0) SSL_shutdown(ssl);
    SSL_free(ssl);
    close(fd);
    free(payload);
    return NULL;
}

static int compare(const void *a, const void *b) {
    uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}

int main(int argc, char **argv) {
    if (argc != 9) return 2;
    port = atoi(argv[1]);
    int threads = atoi(argv[2]);
    calls = atoi(argv[3]); length = atoi(argv[4]);
    int late = atoi(argv[5]); delay_us = atoi(argv[6]); use_ex = atoi(argv[7]);
    int nondumpable = atoi(argv[8]);
    if (threads < 1 || threads > 16 || calls < 1 || calls > 4096 || length < 0 || length > 8192 || delay_us < 0 || delay_us > 500000) return 2;
    struct sigaction action = {0};
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_RESTART;
    action.sa_handler = signal_seen;
    if (sigaction(SIGUSR1, &action, NULL) != 0) return 3;
    action.sa_handler = deny_debug;
    if (sigaction(SIGUSR2, &action, NULL) != 0) return 3;
    signal(SIGPIPE, SIG_IGN);
    if (nondumpable && prctl(PR_SET_DUMPABLE, 0) != 0) return 3;
    ctx = SSL_CTX_new(TLS_client_method());
    if (!ctx) return 4;
    /* Self-signed server and loopback-only synthetic data are scoped to this fixture. */
    SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, NULL);
    pthread_t ids[16];
    if (!late) for (int i = 0; i < threads; i++) if (pthread_create(&ids[i], NULL, worker, NULL)) return 5;
    printf("{\"kind\":\"fixture_ready\",\"pid\":%d}\n", getpid()); fflush(stdout);
    if (getchar() == EOF) return 6;
    if (use_ex == 2) { execl("/bin/sleep", "sleep", "30", (char *)NULL); return 9; }
    if (use_ex == 3) {
        pid_t child = fork();
        if (child < 0) return 10;
        if (child == 0) _exit(0);
        if (waitpid(child, NULL, 0) != child) return 11;
        printf("{\"kind\":\"fixture_forked\"}\n"); fflush(stdout);
        if (getchar() == EOF) return 6;
        return 0;
    }
    uint64_t before = now_ns();
    if (late == 1) for (int i = 0; i < threads; i++) if (pthread_create(&ids[i], NULL, worker, NULL)) return 7;
    pthread_mutex_lock(&lock); go = 1; pthread_cond_broadcast(&gate); pthread_mutex_unlock(&lock);
    int waves = late == 2 ? 4 : 1;
    for (int wave = 0; wave < waves; wave++) {
        if (late == 2) for (int i = 0; i < threads; i++)
            if (pthread_create(&ids[i], NULL, worker, NULL)) return 7;
        for (int i = 0; i < threads; i++) pthread_join(ids[i], NULL);
    }
    uint64_t elapsed = now_ns() - before;
    qsort(samples, sample_count, sizeof(uint64_t), compare);
    printf("{\"kind\":\"fixture_result\",\"attempts\":%d,\"failures\":%d,\"signals\":%d,\"elapsed_ns\":%llu,\"write_latency_ns\":{\"p50\":%llu,\"p95\":%llu,\"p99\":%llu,\"max\":%llu}}\n",
        waves * threads * calls, failures, signals, (unsigned long long)elapsed,
        (unsigned long long)samples[(sample_count - 1) * 50 / 100],
        (unsigned long long)samples[(sample_count - 1) * 95 / 100],
        (unsigned long long)samples[(sample_count - 1) * 99 / 100],
        (unsigned long long)samples[sample_count - 1]);
    fflush(stdout);
    /* Keep the identity alive until the importer has correlated and the tracer detached. */
    (void)getchar();
    SSL_CTX_free(ctx);
    return failures ? 8 : 0;
}
