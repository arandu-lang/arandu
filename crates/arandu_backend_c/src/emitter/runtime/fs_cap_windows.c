/* RFC 0016 Dir capabilities on Windows NT (ST.12b); mirrors the Rust host in
   arandu_runtime/src/fs_runtime/capability_windows.rs.
   Token ABI: a Dir/File token is a CRT descriptor from _open_osfhandle. The
   CRT owns the HANDLE and _close releases it exactly once; the HANDLE never
   travels through an int, so nothing is truncated. NtCreateFile and
   NtSetInformationFile are resolved from the always-loaded ntdll at run time,
   so generated C needs no extra link flags. */
#if defined(_WIN32)
#include <fcntl.h>
#include <io.h>
#include <limits.h>

typedef struct ArNtUnicodeString {
    USHORT Length;
    USHORT MaximumLength;
    PWSTR Buffer;
} ArNtUnicodeString;
typedef struct ArNtObjectAttributes {
    ULONG Length;
    HANDLE RootDirectory;
    ArNtUnicodeString *ObjectName;
    ULONG Attributes;
    PVOID SecurityDescriptor;
    PVOID SecurityQualityOfService;
} ArNtObjectAttributes;
typedef struct ArNtIoStatusBlock {
    union {
        LONG Status;
        PVOID Pointer;
    } u;
    ULONG_PTR Information;
} ArNtIoStatusBlock;
typedef LONG (NTAPI *ArNtCreateFileFn)(PHANDLE, ACCESS_MASK, ArNtObjectAttributes *,
    ArNtIoStatusBlock *, PLARGE_INTEGER, ULONG, ULONG, ULONG, ULONG, PVOID, ULONG);
typedef LONG (NTAPI *ArNtSetInformationFileFn)(HANDLE, ArNtIoStatusBlock *, PVOID, ULONG, int);

#define AR_NT_FILE_OPEN 1u
#define AR_NT_FILE_CREATE 2u
#define AR_NT_FILE_OPEN_IF 3u
#define AR_NT_FILE_DIRECTORY_FILE 0x00000001u
#define AR_NT_FILE_SYNCHRONOUS_IO_NONALERT 0x00000020u
#define AR_NT_FILE_NON_DIRECTORY_FILE 0x00000040u
#define AR_NT_FILE_OPEN_REPARSE_POINT 0x00200000u
#define AR_NT_OBJ_CASE_INSENSITIVE 0x00000040u
#define AR_NT_OBJ_DONT_REPARSE 0x00001000u
#define AR_NT_STATUS_INVALID_INFO_CLASS ((LONG)0xC0000003L)
#define AR_NT_STATUS_INVALID_PARAMETER ((LONG)0xC000000DL)
#define AR_NT_STATUS_NOT_SUPPORTED ((LONG)0xC00000BBL)
#define AR_NT_FILE_DISPOSITION_INFORMATION 13
#define AR_NT_FILE_DISPOSITION_INFORMATION_EX 64
#define AR_NT_FILE_DISPOSITION_DELETE 0x1u
#define AR_NT_FILE_DISPOSITION_POSIX_SEMANTICS 0x2u
#define AR_CAP_WIN_DIR_ACCESS (FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
#define AR_CAP_WIN_SHARE_ALL (FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)

static FARPROC ar_cap_win_ntdll(const char *name) {
    HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
    return ntdll ? GetProcAddress(ntdll, name) : NULL;
}

static int ar_cap_win_ieq(const uint8_t *text, size_t len, const char *upper) {
    size_t i = 0;
    for (; i < len && upper[i]; ++i) {
        uint8_t c = text[i];
        if (c >= 'a' && c <= 'z') c = (uint8_t)(c - 'a' + 'A');
        if (c != (uint8_t)upper[i]) return 0;
    }
    return i == len && upper[i] == 0;
}

/* Win32 maps these base names (before the first dot, trailing spaces
   ignored) to devices; COM/LPT also accept superscript 1-3 (Go CVE-2023-45284). */
static int ar_cap_win_reserved(const uint8_t *name, size_t len) {
    static const char *const fixed[] = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"};
    size_t base = 0;
    while (base < len && name[base] != '.') base++;
    while (base > 0 && name[base - 1] == ' ') base--;
    for (size_t i = 0; i < sizeof(fixed) / sizeof(fixed[0]); ++i) {
        if (ar_cap_win_ieq(name, base, fixed[i])) return 1;
    }
    if (base < 4 || (!ar_cap_win_ieq(name, 3, "COM") && !ar_cap_win_ieq(name, 3, "LPT"))) return 0;
    if (base == 4) return name[3] >= '0' && name[3] <= '9';
    return base == 5 && name[3] == 0xC2 && (name[4] == 0xB9 || name[4] == 0xB2 || name[4] == 0xB3);
}

/* One component only: no separators, drive/stream colon, wildcard, control
   character, "."/"..", trailing dot/space or device name. Absolute, UNC,
   \\?\ and \??\ forms therefore never pass. UTF-8 is checked on conversion. */
static int ar_cap_win_name_valid(const uint8_t *name, size_t len, int allow_dot) {
    if (!name) return 0;
    if (allow_dot && len == 1 && name[0] == '.') return 1;
    if (len == 0 || len >= 4096) return 0;
    if ((len == 1 && name[0] == '.') || (len == 2 && name[0] == '.' && name[1] == '.')) return 0;
    for (size_t i = 0; i < len; ++i) {
        uint8_t c = name[i];
        if (c < 0x20 || c == '\\' || c == '/' || c == ':' || c == '*' || c == '?' ||
            c == '"' || c == '<' || c == '>' || c == '|') return 0;
    }
    if (name[len - 1] == '.' || name[len - 1] == ' ') return 0;
    return !ar_cap_win_reserved(name, len);
}

/* Strict UTF-8 to UTF-16; no ANSI code-page fallback for capability names. */
static wchar_t *ar_cap_win_wide(const uint8_t *text, size_t len, int *units_out) {
    if (!text || len == 0 || len >= 4096) return NULL;
    int units = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, (const char *)text, (int)len, NULL, 0);
    if (units <= 0 || units > 32767) return NULL;
    wchar_t *wide = (wchar_t *)malloc(((size_t)units + 1) * sizeof(wchar_t));
    if (!wide) return NULL;
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, (const char *)text, (int)len, wide, units) != units) {
        free(wide);
        return NULL;
    }
    wide[units] = 0;
    *units_out = units;
    return wide;
}

/* Borrows the HANDLE owned by a CRT descriptor; the CRT keeps ownership. */
static HANDLE ar_cap_win_root(int64_t fd) {
    if (fd < 0 || fd > INT_MAX) return INVALID_HANDLE_VALUE;
    intptr_t raw = _get_osfhandle((int)fd);
    if (raw == (intptr_t)-1 || raw == (intptr_t)-2) return INVALID_HANDLE_VALUE;
    return (HANDLE)raw;
}

/* Disk object of the expected kind; reparse points only when removing the
   file link entry itself. */
static int ar_cap_win_matches(HANDLE handle, int want_dir, int allow_reparse) {
    BY_HANDLE_FILE_INFORMATION info;
    if (GetFileType(handle) != FILE_TYPE_DISK) return 0;
    if (!GetFileInformationByHandle(handle, &info)) return 0;
    if (!allow_reparse && (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) return 0;
    return ((info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) == (want_dir != 0);
}

/* Moves the HANDLE into the CRT table; on failure it is closed exactly once. */
static int ar_cap_win_adopt(HANDLE handle) {
    if (handle == INVALID_HANDLE_VALUE || !handle) return -1;
    int fd = _open_osfhandle((intptr_t)handle, _O_NOINHERIT);
    if (fd < 0) {
        CloseHandle(handle);
        return -1;
    }
    return fd;
}

static HANDLE ar_cap_win_nt_open(HANDLE root, const uint8_t *name, size_t len, int allow_dot,
                                 ACCESS_MASK access, ULONG disposition, ULONG options) {
    ArNtCreateFileFn create = (ArNtCreateFileFn)ar_cap_win_ntdll("NtCreateFile");
    if (!create || !root || root == INVALID_HANDLE_VALUE) return INVALID_HANDLE_VALUE;
    if (!ar_cap_win_name_valid(name, len, allow_dot)) return INVALID_HANDLE_VALUE;
    wchar_t *wide = NULL;
    int units = 0;
    if (!(len == 1 && name[0] == '.')) {
        wide = ar_cap_win_wide(name, len, &units);
        if (!wide) return INVALID_HANDLE_VALUE;
    }
    /* "." (when allowed) is an empty name: NtCreateFile reopens RootDirectory. */
    ArNtUnicodeString object_name = {(USHORT)(units * 2), (USHORT)(units * 2), wide};
    ArNtObjectAttributes attributes = {(ULONG)sizeof(ArNtObjectAttributes), root, &object_name,
                                       AR_NT_OBJ_CASE_INSENSITIVE | AR_NT_OBJ_DONT_REPARSE, NULL, NULL};
    ULONG flags = options | AR_NT_FILE_OPEN_REPARSE_POINT | AR_NT_FILE_SYNCHRONOUS_IO_NONALERT;
    ArNtIoStatusBlock status_block;
    HANDLE handle = NULL;
    memset(&status_block, 0, sizeof(status_block));
    LONG status = create(&handle, access | SYNCHRONIZE, &attributes, &status_block, NULL,
                         FILE_ATTRIBUTE_NORMAL, AR_CAP_WIN_SHARE_ALL, disposition, flags, NULL, 0);
    if (status == AR_NT_STATUS_INVALID_PARAMETER) {
        /* Kernels without OBJ_DONT_REPARSE: one component plus
           FILE_OPEN_REPARSE_POINT and the attribute check still hold. */
        attributes.Attributes = AR_NT_OBJ_CASE_INSENSITIVE;
        memset(&status_block, 0, sizeof(status_block));
        handle = NULL;
        status = create(&handle, access | SYNCHRONIZE, &attributes, &status_block, NULL,
                        FILE_ATTRIBUTE_NORMAL, AR_CAP_WIN_SHARE_ALL, disposition, flags, NULL, 0);
    }
    free(wide);
    if (status < 0 || !handle || handle == INVALID_HANDLE_VALUE) return INVALID_HANDLE_VALUE;
    return handle;
}

static HANDLE ar_cap_win_checked(HANDLE handle, int want_dir, int allow_reparse) {
    if (handle == INVALID_HANDLE_VALUE) return INVALID_HANDLE_VALUE;
    if (!ar_cap_win_matches(handle, want_dir, allow_reparse)) {
        CloseHandle(handle);
        return INVALID_HANDLE_VALUE;
    }
    return handle;
}

static int ar_cap_win_dir_open(const uint8_t *path, size_t len) {
    if (!path || len == 0 || len >= 4096 || memchr(path, 0, len)) return -1;
    int units = 0;
    wchar_t *wide = ar_cap_win_wide(path, len, &units);
    if (!wide) return -1;
    /* Explicit ambient authority, like POSIX open(path, O_DIRECTORY). */
    HANDLE handle = CreateFileW(wide, AR_CAP_WIN_DIR_ACCESS, AR_CAP_WIN_SHARE_ALL, NULL,
                                OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    free(wide);
    if (handle == INVALID_HANDLE_VALUE || !handle) return -1;
    return ar_cap_win_adopt(ar_cap_win_checked(handle, 1, 0));
}

static int ar_cap_win_dir_openat(int64_t fd, const uint8_t *name, size_t len) {
    HANDLE handle = ar_cap_win_nt_open(ar_cap_win_root(fd), name, len, 1, AR_CAP_WIN_DIR_ACCESS,
                                       AR_NT_FILE_OPEN, AR_NT_FILE_DIRECTORY_FILE);
    return ar_cap_win_adopt(ar_cap_win_checked(handle, 1, 0));
}

static int ar_cap_win_openat(int64_t fd, const uint8_t *name, size_t len, int read, int write,
                             int create, int truncate) {
    if ((!read && !write) || ((create || truncate) && !write)) return -1;
    ACCESS_MASK access = FILE_READ_ATTRIBUTES | (read ? FILE_GENERIC_READ : 0) | (write ? FILE_GENERIC_WRITE : 0);
    HANDLE handle = ar_cap_win_nt_open(ar_cap_win_root(fd), name, len, 0, access,
                                       create ? AR_NT_FILE_OPEN_IF : AR_NT_FILE_OPEN,
                                       AR_NT_FILE_NON_DIRECTORY_FILE);
    handle = ar_cap_win_checked(handle, 0, 0);
    if (handle == INVALID_HANDLE_VALUE) return -1;
    /* Truncate only after the checks, so a link or device is never overwritten. */
    if (truncate && !SetEndOfFile(handle)) {
        CloseHandle(handle);
        return -1;
    }
    return ar_cap_win_adopt(handle);
}

static int ar_cap_win_mkdirat(int64_t fd, const uint8_t *name, size_t len) {
    HANDLE handle = ar_cap_win_nt_open(ar_cap_win_root(fd), name, len, 0, AR_CAP_WIN_DIR_ACCESS,
                                       AR_NT_FILE_CREATE, AR_NT_FILE_DIRECTORY_FILE);
    return ar_cap_win_adopt(ar_cap_win_checked(handle, 1, 0));
}

static int ar_cap_win_unlinkat(int64_t fd, const uint8_t *name, size_t len, int is_dir) {
    ArNtSetInformationFileFn set = (ArNtSetInformationFileFn)ar_cap_win_ntdll("NtSetInformationFile");
    if (!set) return -1;
    HANDLE handle = ar_cap_win_nt_open(ar_cap_win_root(fd), name, len, 0, DELETE | FILE_READ_ATTRIBUTES,
                                       AR_NT_FILE_OPEN,
                                       is_dir ? AR_NT_FILE_DIRECTORY_FILE : AR_NT_FILE_NON_DIRECTORY_FILE);
    /* A file link is removed as the entry itself (POSIX unlink); a directory
       junction/symlink fails like rmdir on a symlink. */
    handle = ar_cap_win_checked(handle, is_dir, !is_dir);
    if (handle == INVALID_HANDLE_VALUE) return -1;
    ArNtIoStatusBlock status_block;
    ULONG posix = AR_NT_FILE_DISPOSITION_DELETE | AR_NT_FILE_DISPOSITION_POSIX_SEMANTICS;
    memset(&status_block, 0, sizeof(status_block));
    LONG status = set(handle, &status_block, &posix, (ULONG)sizeof(posix), AR_NT_FILE_DISPOSITION_INFORMATION_EX);
    if (status == AR_NT_STATUS_INVALID_INFO_CLASS || status == AR_NT_STATUS_INVALID_PARAMETER ||
        status == AR_NT_STATUS_NOT_SUPPORTED) {
        /* Pre-1607 kernels or non-NTFS volumes: delete-on-close keeps the
           confinement; only the visibility timing differs. */
        BOOLEAN legacy = TRUE;
        memset(&status_block, 0, sizeof(status_block));
        status = set(handle, &status_block, &legacy, (ULONG)sizeof(legacy), AR_NT_FILE_DISPOSITION_INFORMATION);
    }
    CloseHandle(handle);
    return status >= 0 ? 0 : -1;
}
#endif
