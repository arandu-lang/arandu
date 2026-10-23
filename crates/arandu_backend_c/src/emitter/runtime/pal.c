/* Arandu PAL. This is the only region of generated C that branches on the host
   for filesystem, clock, process arguments, and native thread handles.
   Callers use ar_pal_* and do not repeat those branches. Network syscalls live
   in the companion net PAL, emitted only when the program uses them. */

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <malloc.h>
#include <windows.h>
#include <wchar.h>
#if !defined(__MINGW32__)
#include <io.h>
#else
#include <unistd.h>
#endif
#else
#include <stdatomic.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#if defined(__linux__)
#include <sys/syscall.h>
#endif
#endif

#if defined(_WIN32) && !defined(__MINGW32__)
#include <errno.h>
#endif

#ifndef ENOTDIR
#define ENOTDIR EIO
#endif
#ifndef EILSEQ
#define EILSEQ EIO
#endif

static int ar_env_c_argc = 0;
static char **ar_env_c_argv = NULL;

#if defined(_WIN32)
static void ar_pal_ensure_process_args(void) {
    if (!ar_env_c_argv) {
        ar_env_c_argc = __argc;
        ar_env_c_argv = __argv;
    }
}
#elif defined(__GNUC__) || defined(__clang__)
__attribute__((constructor)) static void ar_pal_capture_process_args(int argc, char **argv) {
    ar_env_c_argc = argc;
    ar_env_c_argv = argv;
}
static void ar_pal_ensure_process_args(void) {}
#else
static void ar_pal_ensure_process_args(void) {}
#endif

static int64_t ar_pal_monotonic_ns(void) {
#if defined(_WIN32)
    static LARGE_INTEGER ar_qpc_origin;
    static LARGE_INTEGER ar_qpc_freq;
    static LONG ar_qpc_init = 0;
    while (InterlockedCompareExchange(&ar_qpc_init, 1, 0) != 0) {
        if (InterlockedCompareExchange(&ar_qpc_init, 2, 2) == 2) break;
        SwitchToThread();
    }
    if (ar_qpc_freq.QuadPart == 0) {
        QueryPerformanceFrequency(&ar_qpc_freq);
        QueryPerformanceCounter(&ar_qpc_origin);
        InterlockedExchange(&ar_qpc_init, 2);
    }
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    uint64_t ticks = (uint64_t)(now.QuadPart - ar_qpc_origin.QuadPart);
    uint64_t freq = (uint64_t)ar_qpc_freq.QuadPart;
    uint64_t secs = ticks / freq;
    uint64_t rem = ticks % freq;
    if (secs > (uint64_t)(INT64_MAX / 1000000000LL)) return INT64_MAX;
    uint64_t ns = secs * 1000000000ULL + (rem * 1000000000ULL) / freq;
    return ns > (uint64_t)INT64_MAX ? INT64_MAX : (int64_t)ns;
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) abort();
    uint64_t secs = (uint64_t)ts.tv_sec;
    if (secs > (uint64_t)(INT64_MAX / 1000000000LL)) return INT64_MAX;
    uint64_t ns = secs * 1000000000ULL + (uint64_t)ts.tv_nsec;
    return ns > (uint64_t)INT64_MAX ? INT64_MAX : (int64_t)ns;
#endif
}

/* Long durations are sliced/saturated instead of truncated: usleep takes a
   32-bit microsecond count and may reject >= 1s; DWORD INFINITE never wakes. */
static void ar_pal_sleep_ms(uint64_t ms) {
#if defined(_WIN32)
    do {
        DWORD slice = ms >= (uint64_t)INFINITE ? INFINITE - 1 : (DWORD)ms;
        Sleep(slice);
        ms -= slice;
    } while (ms > 0);
#else
    uint64_t seconds = ms / 1000;
    uint64_t max_seconds = sizeof(time_t) <= 4 ? INT32_MAX : INT64_MAX;
    struct timespec request;
    request.tv_sec = (time_t)(seconds > max_seconds ? max_seconds : seconds);
    request.tv_nsec = (long)((ms % 1000) * 1000000);
    while (nanosleep(&request, &request) != 0 && errno == EINTR) {}
#endif
}

static void ar_pal_yield(void) {
#if defined(_WIN32)
    SwitchToThread();
#elif defined(__GLIBC__) || defined(__linux__)
    sched_yield();
#else
    usleep(0);
#endif
}

static void ar_pal_spin_hint(void) {
#if defined(_WIN32)
    YieldProcessor();
#elif (defined(__i386__) || defined(__x86_64__)) && (defined(__GNUC__) || defined(__clang__))
    __builtin_ia32_pause();
#elif defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
    __asm__ __volatile__("isb" ::: "memory");
#else
    atomic_signal_fence(memory_order_seq_cst);
#endif
}

static uint64_t ar_pal_thread_id(void) {
#if defined(_WIN32)
    return (uint64_t)GetCurrentThreadId();
#elif defined(__APPLE__)
    uint64_t tid = 0;
    if (pthread_threadid_np(NULL, &tid) != 0) return 0;
    return tid;
#elif defined(__linux__)
    return (uint64_t)syscall(SYS_gettid);
#else
    return (uint64_t)(uintptr_t)pthread_self();
#endif
}

typedef struct ArPalThread {
#if defined(_WIN32)
    HANDLE handle;
#else
    pthread_t handle;
#endif
} ArPalThread;

typedef void (*ar_pal_thread_entry)(void *);

typedef struct ArPalThreadStart {
    ar_pal_thread_entry entry;
    void *arg;
} ArPalThreadStart;

#if defined(_WIN32)
static DWORD WINAPI ar_pal_thread_thunk(LPVOID raw) {
    ArPalThreadStart start = *(ArPalThreadStart *)raw;
    free(raw);
    start.entry(start.arg);
    return 0;
}
#else
static void *ar_pal_thread_thunk(void *raw) {
    ArPalThreadStart start = *(ArPalThreadStart *)raw;
    free(raw);
    start.entry(start.arg);
    return NULL;
}
#endif

static int ar_pal_thread_spawn(ArPalThread *out, ar_pal_thread_entry entry, void *arg) {
    if (!out || !entry) return -1;
    ArPalThreadStart *start = (ArPalThreadStart *)malloc(sizeof(*start));
    if (!start) return -1;
    start->entry = entry;
    start->arg = arg;
#if defined(_WIN32)
    HANDLE handle = CreateThread(NULL, 0, ar_pal_thread_thunk, start, 0, NULL);
    if (!handle) {
        free(start);
        return -1;
    }
    out->handle = handle;
    return 0;
#else
    if (pthread_create(&out->handle, NULL, ar_pal_thread_thunk, start) != 0) {
        free(start);
        return -1;
    }
    return 0;
#endif
}

static void ar_pal_thread_join(ArPalThread *thread) {
#if defined(_WIN32)
    if (WaitForSingleObject(thread->handle, INFINITE) != WAIT_OBJECT_0) abort();
    if (!CloseHandle(thread->handle)) abort();
#else
    if (pthread_join(thread->handle, NULL) != 0) abort();
#endif
}

static int ar_pal_thread_is_current(const ArPalThread *thread) {
#if defined(_WIN32)
    return GetThreadId(thread->handle) == GetCurrentThreadId();
#else
    return pthread_equal(thread->handle, pthread_self()) != 0;
#endif
}

static int64_t ar_pal_read(intptr_t fd, void *buf, size_t count) {
    if (count > (size_t)INT32_MAX) count = (size_t)INT32_MAX;
#if defined(_WIN32) && !defined(__MINGW32__)
    int n = _read((int)fd, buf, (unsigned)count);
    return n < 0 ? (int64_t)-1 : (int64_t)n;
#else
    for (;;) {
        ssize_t n = read((int)fd, buf, count);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        return (int64_t)n;
    }
#endif
}

static int64_t ar_pal_write(intptr_t fd, const void *buf, size_t count) {
    if (count > (size_t)INT32_MAX) count = (size_t)INT32_MAX;
#if defined(_WIN32) && !defined(__MINGW32__)
    int n = _write((int)fd, buf, (unsigned)count);
    return n < 0 ? (int64_t)-1 : (int64_t)n;
#else
    for (;;) {
        ssize_t n = write((int)fd, buf, count);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        return (int64_t)n;
    }
#endif
}

static int ar_pal_close_fd(intptr_t fd) {
#if defined(_WIN32) && !defined(__MINGW32__)
    return _close((int)fd) == 0 ? 0 : -1;
#else
    return close((int)fd) == 0 ? 0 : -1;
#endif
}

typedef struct ArPalDirEnt {
    char *name;
    size_t name_len;
    int is_dir;
} ArPalDirEnt;

typedef struct ArPalDir {
#if defined(_WIN32)
    HANDLE handle;
    WIN32_FIND_DATAW data;
    int first;
#else
    DIR *dir;
    char *path;
#endif
} ArPalDir;

#if defined(_WIN32)
static int ar_pal_win32_errno(DWORD error) {
    switch (error) {
        case ERROR_FILE_NOT_FOUND:
        case ERROR_PATH_NOT_FOUND:
        case ERROR_INVALID_NAME:
            return ENOENT;
        case ERROR_ACCESS_DENIED:
            return EACCES;
        case ERROR_DIRECTORY:
            return ENOTDIR;
        case ERROR_ALREADY_EXISTS:
            return EEXIST;
        default:
            return EIO;
    }
}

static wchar_t *ar_pal_utf8_to_wide(const char *text) {
    int needed = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, NULL, 0);
    UINT codepage = CP_UTF8;
    DWORD flags = MB_ERR_INVALID_CHARS;
    if (needed <= 0) {
        needed = MultiByteToWideChar(CP_ACP, 0, text, -1, NULL, 0);
        if (needed <= 0) return NULL;
        codepage = CP_ACP;
        flags = 0;
    }
    wchar_t *wide = (wchar_t *)malloc((size_t)needed * sizeof(wchar_t));
    if (!wide) return NULL;
    if (MultiByteToWideChar(codepage, flags, text, -1, wide, needed) <= 0) {
        free(wide);
        return NULL;
    }
    return wide;
}
#endif

static ArPalDir *ar_pal_readdir_open(const char *path, int *err) {
    if (err) *err = 0;
    if (!path) {
        errno = EINVAL;
        if (err) *err = EINVAL;
        return NULL;
    }
#if defined(_WIN32)
    wchar_t *wide = ar_pal_utf8_to_wide(path);
    if (!wide) {
        errno = EINVAL;
        if (err) *err = EINVAL;
        return NULL;
    }
    size_t wlen = wcslen(wide);
    wchar_t *pattern = (wchar_t *)malloc((wlen + 3) * sizeof(wchar_t));
    if (!pattern) {
        free(wide);
        errno = ENOMEM;
        if (err) *err = ENOMEM;
        return NULL;
    }
    memcpy(pattern, wide, wlen * sizeof(wchar_t));
    size_t at = wlen;
    if (wlen == 0 || (pattern[wlen - 1] != L'\\' && pattern[wlen - 1] != L'/')) {
        pattern[at++] = L'\\';
    }
    pattern[at++] = L'*';
    pattern[at] = 0;
    free(wide);
    WIN32_FIND_DATAW data;
    HANDLE handle = FindFirstFileW(pattern, &data);
    free(pattern);
    if (handle == INVALID_HANDLE_VALUE) {
        int mapped = ar_pal_win32_errno(GetLastError());
        errno = mapped;
        if (err) *err = mapped;
        return NULL;
    }
    ArPalDir *dir = (ArPalDir *)calloc(1, sizeof(*dir));
    if (!dir) {
        FindClose(handle);
        errno = ENOMEM;
        if (err) *err = ENOMEM;
        return NULL;
    }
    dir->handle = handle;
    dir->data = data;
    dir->first = 1;
    return dir;
#else
    DIR *raw = opendir(path);
    if (!raw) {
        if (err) *err = errno;
        return NULL;
    }
    ArPalDir *dir = (ArPalDir *)calloc(1, sizeof(*dir));
    if (!dir) {
        int saved = errno;
        closedir(raw);
        errno = saved != 0 ? saved : ENOMEM;
        if (err) *err = errno;
        return NULL;
    }
    size_t len = strlen(path);
    dir->path = (char *)malloc(len + 1);
    if (!dir->path) {
        closedir(raw);
        free(dir);
        errno = ENOMEM;
        if (err) *err = ENOMEM;
        return NULL;
    }
    memcpy(dir->path, path, len + 1);
    dir->dir = raw;
    return dir;
#endif
}

#if !defined(_WIN32)
/* d_type fallback. Paths that do not fit the stack buffer use the heap
   instead of stat-ing a silently truncated (different) path. */
static int ar_pal_dirent_is_dir(const char *dir_path, const char *name) {
    char stack_path[4096];
    char *sub_path = stack_path;
    int needed = snprintf(stack_path, sizeof(stack_path), "%s/%s", dir_path, name);
    if (needed < 0) return 0;
    if ((size_t)needed >= sizeof(stack_path)) {
        sub_path = (char *)malloc((size_t)needed + 1);
        if (!sub_path) return 0;
        snprintf(sub_path, (size_t)needed + 1, "%s/%s", dir_path, name);
    }
    struct stat st;
    int is_dir = stat(sub_path, &st) == 0 && S_ISDIR(st.st_mode);
    if (sub_path != stack_path) free(sub_path);
    return is_dir;
}
#endif

static int ar_pal_readdir_next(ArPalDir *dir, ArPalDirEnt *out, int *err) {
    if (err) *err = 0;
    if (!dir || !out) {
        errno = EINVAL;
        if (err) *err = EINVAL;
        return -1;
    }
    memset(out, 0, sizeof(*out));
#if defined(_WIN32)
    for (;;) {
        if (!dir->first) {
            if (!FindNextFileW(dir->handle, &dir->data)) {
                DWORD last = GetLastError();
                if (last == ERROR_NO_MORE_FILES) return 0;
                int mapped = ar_pal_win32_errno(last);
                errno = mapped;
                if (err) *err = mapped;
                return -1;
            }
        }
        dir->first = 0;
        if (wcscmp(dir->data.cFileName, L".") == 0 || wcscmp(dir->data.cFileName, L"..") == 0) {
            continue;
        }
        int bytes = WideCharToMultiByte(CP_UTF8, 0, dir->data.cFileName, -1, NULL, 0, NULL, NULL);
        if (bytes <= 1) {
            errno = EILSEQ;
            if (err) *err = EILSEQ;
            return -1;
        }
        char *name = (char *)malloc((size_t)bytes);
        if (!name) {
            errno = ENOMEM;
            if (err) *err = ENOMEM;
            return -1;
        }
        if (WideCharToMultiByte(CP_UTF8, 0, dir->data.cFileName, -1, name, bytes, NULL, NULL) <= 0) {
            free(name);
            errno = EILSEQ;
            if (err) *err = EILSEQ;
            return -1;
        }
        out->name = name;
        out->name_len = (size_t)bytes - 1;
        out->is_dir = (dir->data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
        return 1;
    }
#else
    for (;;) {
        errno = 0;
        struct dirent *ent = readdir(dir->dir);
        if (!ent) {
            if (errno == 0) return 0;
            if (err) *err = errno;
            return -1;
        }
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        size_t nlen = strlen(ent->d_name);
        char *name = (char *)malloc(nlen + 1);
        if (!name) {
            errno = ENOMEM;
            if (err) *err = ENOMEM;
            return -1;
        }
        memcpy(name, ent->d_name, nlen + 1);
        int is_dir = 0;
#ifdef DT_DIR
        if (ent->d_type == DT_DIR) is_dir = 1;
        else if (ent->d_type == DT_UNKNOWN) is_dir = ar_pal_dirent_is_dir(dir->path, ent->d_name);
#else
        is_dir = ar_pal_dirent_is_dir(dir->path, ent->d_name);
#endif
        out->name = name;
        out->name_len = nlen;
        out->is_dir = is_dir;
        return 1;
    }
#endif
}

static void ar_pal_readdir_close(ArPalDir *dir) {
    if (!dir) return;
#if defined(_WIN32)
    if (dir->handle != INVALID_HANDLE_VALUE && dir->handle != NULL) FindClose(dir->handle);
#else
    if (dir->dir) closedir(dir->dir);
    free(dir->path);
#endif
    free(dir);
}
