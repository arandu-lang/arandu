/* Network PAL. Emitted only when the program references ar_net_* so a filesystem
   program does not take a Winsock or libsocket link dependency. Include this
   before the main PAL so Winsock precedes windows.h. */

#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#if defined(_WIN32)
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#else
#include <arpa/inet.h>
#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

#if defined(_WIN32)
static LONG ar_pal_wsa_state = 0;

static int ar_pal_net_init(void) {
    LONG seen = InterlockedCompareExchange(&ar_pal_wsa_state, 2, 0);
    if (seen == 0) {
        WSADATA wsa;
        if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0) {
            InterlockedExchange(&ar_pal_wsa_state, -1);
            return -1;
        }
        InterlockedExchange(&ar_pal_wsa_state, 1);
        return 0;
    }
    while (InterlockedCompareExchange(&ar_pal_wsa_state, 0, 0) == 2) {
        SwitchToThread();
    }
    return ar_pal_wsa_state == 1 ? 0 : -1;
}

static intptr_t ar_pal_dup(intptr_t fd) {
    WSAPROTOCOL_INFOW info;
    memset(&info, 0, sizeof(info));
    if (WSADuplicateSocketW((SOCKET)fd, GetCurrentProcessId(), &info) != 0) return -1;
    SOCKET duped = WSASocketW(
        info.iAddressFamily, info.iSocketType, info.iProtocol, &info, 0, WSA_FLAG_OVERLAPPED);
    if (duped == INVALID_SOCKET) return -1;
    return (intptr_t)duped;
}

static void ar_pal_udp_close(intptr_t fd) { closesocket((SOCKET)fd); }
#else
static int ar_pal_net_init(void) { return 0; }

static intptr_t ar_pal_dup(intptr_t fd) {
    int duped = dup((int)fd);
    return duped < 0 ? (intptr_t)-1 : (intptr_t)duped;
}

static void ar_pal_udp_close(intptr_t fd) { close((int)fd); }
#endif

static int ar_pal_socket_invalid(intptr_t fd) {
#if defined(_WIN32)
    return (SOCKET)fd == INVALID_SOCKET;
#else
    return fd < 0;
#endif
}

static int ar_pal_copy_host(const char *host, char *out, size_t out_cap) {
    if (!host || out_cap == 0) return -1;
    size_t len = strlen(host);
    const char *start = host;
    if (len >= 2 && host[0] == '[' && host[len - 1] == ']') {
        start = host + 1;
        len -= 2;
    }
    if (len == 0 || len >= out_cap) return -1;
    memcpy(out, start, len);
    out[len] = '\0';
    return 0;
}

static struct addrinfo *ar_pal_udp_lookup(const char *host, int port) {
    char node[256];
    char service[8];
    if (ar_pal_copy_host(host, node, sizeof(node)) != 0) return NULL;
    if (snprintf(service, sizeof(service), "%d", port) < 0) return NULL;
    struct addrinfo hints;
    struct addrinfo *result = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_DGRAM;
    hints.ai_flags = AI_NUMERICSERV;
    if (getaddrinfo(node, service, &hints, &result) != 0) return NULL;
    return result;
}

static intptr_t ar_pal_udp_bind(const char *host, int port) {
    if (ar_pal_net_init() != 0 || !host || port < 0 || port > 65535) return -1;
    struct addrinfo *result = ar_pal_udp_lookup(host, port);
    if (!result) return -1;
    intptr_t fd = (intptr_t)-1;
    for (struct addrinfo *it = result; it != NULL; it = it->ai_next) {
        intptr_t candidate = (intptr_t)socket(it->ai_family, it->ai_socktype, it->ai_protocol);
        if (ar_pal_socket_invalid(candidate)) continue;
#if defined(_WIN32)
        int bound = bind((SOCKET)candidate, it->ai_addr, (int)it->ai_addrlen);
#else
        int bound = bind((int)candidate, it->ai_addr, it->ai_addrlen);
#endif
        if (bound == 0) {
            fd = candidate;
            break;
        }
        ar_pal_udp_close(candidate);
    }
    freeaddrinfo(result);
    return fd;
}

static intptr_t ar_pal_udp_send(
    intptr_t fd, const void *buf, size_t len, const char *host, int port
) {
    if (ar_pal_net_init() != 0 || !buf || !host || port < 0 || port > 65535) return -1;
#if defined(_WIN32)
    if (len > (size_t)INT_MAX) return -1;
#endif
    struct addrinfo *result = ar_pal_udp_lookup(host, port);
    if (!result) return -1;
    intptr_t sent = (intptr_t)-1;
    for (struct addrinfo *it = result; it != NULL; it = it->ai_next) {
#if defined(_WIN32)
        int wrote = sendto(
            (SOCKET)fd, (const char *)buf, (int)len, 0, it->ai_addr, (int)it->ai_addrlen);
        if (wrote >= 0) {
            sent = (intptr_t)wrote;
            break;
        }
#else
        ssize_t wrote = sendto((int)fd, buf, len, 0, it->ai_addr, it->ai_addrlen);
        if (wrote >= 0) {
            sent = (intptr_t)wrote;
            break;
        }
#endif
    }
    freeaddrinfo(result);
    return sent;
}

static intptr_t ar_pal_udp_recv(intptr_t fd, void *buf, size_t len, int *port_out) {
    if (ar_pal_net_init() != 0 || !buf || len == 0) return -1;
    struct sockaddr_storage peer;
    memset(&peer, 0, sizeof(peer));
#if defined(_WIN32)
    if (len > (size_t)INT_MAX) return -1;
    int peer_len = (int)sizeof(peer);
    int recvd =
        recvfrom((SOCKET)fd, (char *)buf, (int)len, 0, (struct sockaddr *)&peer, &peer_len);
    if (recvd < 0) return -1;
#else
    socklen_t peer_len = sizeof(peer);
    ssize_t recvd = recvfrom((int)fd, buf, len, 0, (struct sockaddr *)&peer, &peer_len);
    if (recvd < 0) return -1;
#endif
    if (port_out) {
        if (peer.ss_family == AF_INET) {
            *port_out = (int)ntohs(((struct sockaddr_in *)&peer)->sin_port);
        } else if (peer.ss_family == AF_INET6) {
            *port_out = (int)ntohs(((struct sockaddr_in6 *)&peer)->sin6_port);
        }
    }
    return (intptr_t)recvd;
}

static int ar_pal_dns_resolve(const char *name, char *ip_buf, size_t ip_cap) {
    if (ar_pal_net_init() != 0 || !name || !ip_buf || ip_cap == 0) return -1;
    struct addrinfo hints;
    struct addrinfo *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(name, NULL, &hints, &res) != 0 || !res) return -1;
    char ip_str[INET6_ADDRSTRLEN];
    const char *formatted = NULL;
    if (res->ai_family == AF_INET) {
        struct sockaddr_in *ipv4 = (struct sockaddr_in *)res->ai_addr;
        formatted = inet_ntop(AF_INET, &ipv4->sin_addr, ip_str, sizeof(ip_str));
    } else if (res->ai_family == AF_INET6) {
        struct sockaddr_in6 *ipv6 = (struct sockaddr_in6 *)res->ai_addr;
        formatted = inet_ntop(AF_INET6, &ipv6->sin6_addr, ip_str, sizeof(ip_str));
    }
    freeaddrinfo(res);
    if (!formatted) return -1;
    size_t slen = strlen(formatted);
    if (slen > ip_cap) return -1;
    memcpy(ip_buf, formatted, slen);
    return (int)slen;
}
