/* Network PAL. Emitted only when the program references ar_net_* so a filesystem
   program does not take a Winsock or libsocket link dependency. Include this
   before the main PAL so Winsock precedes windows.h. */

#include <stdint.h>
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

static intptr_t ar_pal_udp_bind(const char *host, int port) {
    if (ar_pal_net_init() != 0 || !host || port < 0 || port > 65535) return -1;
    intptr_t fd = (intptr_t)socket(AF_INET, SOCK_DGRAM, 0);
#if defined(_WIN32)
    if ((SOCKET)fd == INVALID_SOCKET) return -1;
#else
    if (fd < 0) return -1;
#endif
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &addr.sin_addr) <= 0) {
        ar_pal_udp_close(fd);
        return -1;
    }
    if (bind((
#if defined(_WIN32)
                 SOCKET
#else
                 int
#endif
                 )fd,
             (struct sockaddr *)&addr,
             sizeof(addr)) < 0) {
        ar_pal_udp_close(fd);
        return -1;
    }
    return fd;
}

static intptr_t ar_pal_udp_send(
    intptr_t fd, const void *buf, size_t len, const char *host, int port
) {
    if (ar_pal_net_init() != 0 || !buf || !host || port < 0 || port > 65535) return -1;
    struct sockaddr_in target;
    memset(&target, 0, sizeof(target));
    target.sin_family = AF_INET;
    target.sin_port = htons((uint16_t)port);
    if (inet_pton(AF_INET, host, &target.sin_addr) <= 0) return -1;
#if defined(_WIN32)
    int sent = sendto(
        (SOCKET)fd, (const char *)buf, (int)len, 0, (struct sockaddr *)&target, sizeof(target));
    return sent < 0 ? (intptr_t)-1 : (intptr_t)sent;
#else
    ssize_t sent = sendto(fd, buf, len, 0, (struct sockaddr *)&target, sizeof(target));
    return sent < 0 ? (intptr_t)-1 : (intptr_t)sent;
#endif
}

static intptr_t ar_pal_udp_recv(intptr_t fd, void *buf, size_t len, int *port_out) {
    if (ar_pal_net_init() != 0 || !buf || len == 0) return -1;
    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
#if defined(_WIN32)
    int peer_len = (int)sizeof(peer);
    int recvd = recvfrom((SOCKET)fd, (char *)buf, (int)len, 0, (struct sockaddr *)&peer, &peer_len);
    if (recvd < 0) return -1;
#else
    socklen_t peer_len = sizeof(peer);
    ssize_t recvd = recvfrom(fd, buf, len, 0, (struct sockaddr *)&peer, &peer_len);
    if (recvd < 0) return -1;
#endif
    if (port_out) *port_out = (int)ntohs(peer.sin_port);
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
