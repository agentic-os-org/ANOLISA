#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <netdb.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>

int connect(int fd, const struct sockaddr *address, socklen_t length) {
    int (*original)(int, const struct sockaddr *, socklen_t) = dlsym(RTLD_NEXT, "connect");
    if (address && address->sa_family == AF_INET && length >= sizeof(struct sockaddr_in)) {
        struct sockaddr_in redirected = *(const struct sockaddr_in *)address;
        if (redirected.sin_addr.s_addr == inet_addr("100.100.100.200") &&
            redirected.sin_port == htons(80)) {
            redirected.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
            return original(fd, (const struct sockaddr *)&redirected, sizeof(redirected));
        }
    }
    return original(fd, address, length);
}

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints,
                struct addrinfo **result) {
    int (*original)(const char *, const char *, const struct addrinfo *, struct addrinfo **) =
        dlsym(RTLD_NEXT, "getaddrinfo");
    if (node && (strcmp(node, "sysom.vpc-proxy.aliyuncs.com") == 0 ||
                 strcmp(node, "sysom.cn-hangzhou.aliyuncs.com") == 0)) {
        node = "127.0.0.1";
    }
    return original(node, service, hints, result);
}
