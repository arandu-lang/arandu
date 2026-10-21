//! C runtime helpers for paths and filesystem operations (std.path and std.fs).

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_path_runtime(&mut self, len_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.path host (Unix-oriented gold; mirrors JIT Path helpers). */
static {len_c_ty} ar_path_is_absolute(ArStr p) {{
    if (p.len <= 0 || !p.ptr) return 0;
    return p.ptr[0] == '/' ? 1 : 0;
}}
static {len_c_ty} ar_path_is_empty(ArStr p) {{
    return p.len <= 0 ? 1 : 0;
}}
static ArStr ar_path_join(ArStr a, ArStr b) {{
    /* Absolute b replaces (Unix Path::join). */
    if (b.len > 0 && b.ptr && b.ptr[0] == '/') return b;
    if (a.len <= 0) return b;
    if (b.len <= 0) return a;
    int need_sep = !(a.ptr[a.len - 1] == '/');
    {len_c_ty} total = a.len + b.len + (need_sep ? 1 : 0);
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) abort();
    memcpy(buf, a.ptr, (size_t)a.len);
    {len_c_ty} off = a.len;
    if (need_sep) buf[off++] = '/';
    memcpy(buf + off, b.ptr, (size_t)b.len);
    buf[total] = 0;
    return ar_str_pack(buf, total);
}}
static bool ar_path_join_owned(ArStr a, ArStr b, uint8_t **out_buf, {len_c_ty} *out_len, {len_c_ty} *out_cap) {{
    if (!out_buf || !out_len || !out_cap || a.len < 0 || b.len < 0 ||
        (a.len > 0 && !a.ptr) || (b.len > 0 && !b.ptr)) return false;
    *out_buf = NULL; *out_len = 0; *out_cap = 0;
    bool absolute = b.len > 0 && b.ptr[0] == '/';
    {len_c_ty} al = absolute ? 0 : a.len;
    bool need_sep = al > 0 && b.len > 0 && a.ptr[al - 1] != '/';
    if (al > INT64_MAX - b.len - (need_sep ? 1 : 0)) return false;
    {len_c_ty} total = al + b.len + (need_sep ? 1 : 0);
    if (total == 0) return true;
    uint8_t *buf = (uint8_t*)ar_vec_malloc(({len_c_ty})total);
    if (!buf) return false;
    {len_c_ty} off = 0;
    if (al > 0) {{ memcpy(buf, a.ptr, (size_t)al); off = al; }}
    if (need_sep) buf[off++] = '/';
    if (b.len > 0) memcpy(buf + off, b.ptr, (size_t)b.len);
    *out_buf = buf; *out_len = total; *out_cap = total;
    return true;
}}
static ArStr ar_path_file_name(ArStr p) {{
    if (p.len <= 0 || !p.ptr) return ar_str_pack((const uint8_t*)"", 0);
    {len_c_ty} i = p.len;
    while (i > 0 && p.ptr[i - 1] != '/') i--;
    {len_c_ty} n = p.len - i;
    if (n <= 0) return ar_str_pack((const uint8_t*)"", 0);
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, p.ptr + i, (size_t)n);
    buf[n] = 0;
    return ar_str_pack(buf, n);
}}"#
        );
    }

    pub(super) fn emit_c_string_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* NUL-terminated names shared by filesystem and environment hosts. */
static inline char *ar_str_to_c_path(ArStr s, char *stack_buf, size_t stack_sz) {{
    if (s.len < 0 || (!s.ptr && s.len > 0)) return NULL;
    size_t len = (size_t)s.len;
    if (len + 1 <= stack_sz) {{
        if (len > 0 && s.ptr) memcpy(stack_buf, s.ptr, len);
        stack_buf[len] = '\0';
        return stack_buf;
    }}
    char *heap = (char *)malloc(len + 1);
    if (!heap) return NULL;
    if (len > 0 && s.ptr) memcpy(heap, s.ptr, len);
    heap[len] = '\0';
    return heap;
}}
"#
        );
    }

    pub(super) fn emit_fs_runtime(&mut self, uint_c_ty: &str, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.fs host helpers (mirrors JIT ar_fs_* runtime). */
static inline {int_c_ty} ar_map_errno(int err) {{
    switch (err) {{
        case 0: return 0;
        case ENOENT: return 1; /* NotFound */
        case EACCES:
        case EPERM: return 2; /* PermissionDenied */
        case EEXIST: return 3; /* AlreadyExists */
#ifdef EAGAIN
        case EAGAIN: return 4; /* WouldBlock */
#endif
#if defined(EWOULDBLOCK) && (!defined(EAGAIN) || EAGAIN != EWOULDBLOCK)
        case EWOULDBLOCK: return 4; /* WouldBlock */
#endif
        case EINVAL: return 5; /* InvalidInput */
#ifdef EISDIR
        case EISDIR: return 6; /* IsADirectory */
#endif
#ifdef ENOTDIR
        case ENOTDIR: return 7; /* NotADirectory */
#endif
        default: return 8; /* Other */
    }}
}}

static bool exists(ArStr path) {{
    if (path.len <= 0 || !path.ptr) return false;
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) return false;
    struct stat st;
    int res = stat(c_path, &st);
    if (c_path != stack_buf) free(c_path);
    return res == 0;
}}

static {int_c_ty} ar_fs_open(ArStr path, {int_c_ty} flags, {int_c_ty} mode) {{
    (void)path; (void)flags; (void)mode;
    return -1;
}}

static {int_c_ty} ar_fs_read({int_c_ty} fd, uint8_t *buf, {uint_c_ty} count) {{
    if (fd < 0 || (count > 0 && !buf)) return -1;
    if (count == 0) return 0;
    int64_t n = ar_pal_read((intptr_t)fd, buf, (size_t)count);
    return n < 0 ? -1 : ({int_c_ty})n;
}}

static {int_c_ty} ar_fs_write({int_c_ty} fd, const uint8_t *buf, {uint_c_ty} count) {{
    if (fd < 0 || (count > 0 && !buf)) return -1;
    if (count == 0) return 0;
    int64_t n = ar_pal_write((intptr_t)fd, buf, (size_t)count);
    return n < 0 ? -1 : ({int_c_ty})n;
}}

static {int_c_ty} ar_fs_close({int_c_ty} fd) {{
    if (fd < 0) return 0;
    return ar_pal_close_fd((intptr_t)fd) == 0 ? 0 : -1;
}}

static void ar_fs_read_all(ArStr path, uint8_t **out_buf, {uint_c_ty} *out_len, {uint_c_ty} *out_cap, {int_c_ty} *err) {{
    if (!out_buf || !out_len || !out_cap || !err) return;
    if (path.len < 0 || (path.len > 0 && !path.ptr)) {{
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 5;
        return;
    }}
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) {{
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
        return;
    }}
    struct stat st;
    if (stat(c_path, &st) != 0) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    if (S_ISDIR(st.st_mode)) {{
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 6;
        return;
    }}
    FILE *f = fopen(c_path, "rb");
    if (!f) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    if (c_path != stack_buf) free(c_path);
    if (st.st_size < 0 || (uint64_t)st.st_size > (uint64_t)INT32_MAX) {{
        fclose(f);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t cap = (size_t)st.st_size;
    size_t len = 0;
    uint8_t *buf = cap > 0 ? (uint8_t *)ar_vec_malloc(({uint_c_ty})cap) : NULL;
    if (cap > 0 && !buf) {{
        fclose(f);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
        return;
    }}
    for (;;) {{
        if (len == cap) {{
            if (cap >= (size_t)INT32_MAX) {{
                int extra = fgetc(f);
                if (extra != EOF || ferror(f)) {{
                    if (buf) ar_vec_buf_free(buf, ({uint_c_ty})cap);
                    fclose(f);
                    *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
                    return;
                }}
                break;
            }}
            size_t new_cap = cap == 0 ? 8192 : cap * 2;
            if (new_cap > (size_t)INT32_MAX || new_cap < cap) new_cap = (size_t)INT32_MAX;
            uint8_t *next = buf
                ? (uint8_t *)ar_vec_realloc(buf, ({uint_c_ty})cap, ({uint_c_ty})new_cap)
                : (uint8_t *)ar_vec_malloc(({uint_c_ty})new_cap);
            if (!next) {{
                if (buf) ar_vec_buf_free(buf, ({uint_c_ty})cap);
                fclose(f);
                *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
                return;
            }}
            buf = next;
            cap = new_cap;
        }}
        size_t n = fread(buf + len, 1, cap - len, f);
        len += n;
        if (ferror(f)) {{
            int read_error = errno;
            if (read_error == EINTR) {{
                clearerr(f);
                continue;
            }}
            ar_vec_buf_free(buf, ({uint_c_ty})cap);
            fclose(f);
            *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = ar_map_errno(read_error);
            return;
        }}
        if (feof(f)) break;
        if (n == 0) {{
            ar_vec_buf_free(buf, ({uint_c_ty})cap);
            fclose(f);
            *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 8;
            return;
        }}
    }}
    fclose(f);
    if (len == 0) {{
        if (buf) ar_vec_buf_free(buf, ({uint_c_ty})cap);
        *out_buf = NULL; *out_len = 0; *out_cap = 0; *err = 0;
        return;
    }}
    uint8_t *shrunk = (uint8_t *)ar_vec_realloc(buf, ({uint_c_ty})cap, ({uint_c_ty})len);
    *out_buf = shrunk ? shrunk : buf;
    *out_len = ({uint_c_ty})len;
    *out_cap = shrunk ? ({uint_c_ty})len : ({uint_c_ty})cap;
    *err = 0;
}}

typedef struct {{
    char *name;
    size_t len;
    uint8_t is_dir;
}} ArHostDirEntry;

static void ar_fs_readdir(ArStr path, uint8_t **out_buf, {uint_c_ty} *out_count, {uint_c_ty} *out_cap, {int_c_ty} *err) {{
    if (!out_buf || !out_count || !out_cap || !err) return;
    if (path.len < 0 || (path.len > 0 && !path.ptr)) {{
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 5;
        return;
    }}
    char stack_buf[1024];
    char *c_path = ar_str_to_c_path(path, stack_buf, sizeof(stack_buf));
    if (!c_path) {{
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    int pal_err = 0;
    ArPalDir *d = ar_pal_readdir_open(c_path, &pal_err);
    if (!d) {{
        int e = pal_err != 0 ? pal_err : errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    size_t entries_cap = 16;
    size_t count = 0;
    size_t descriptor_size = sizeof(void *) + sizeof({uint_c_ty});
    ArHostDirEntry *entries = (ArHostDirEntry *)malloc(entries_cap * sizeof(ArHostDirEntry));
    if (!entries) {{
        ar_pal_readdir_close(d);
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t names_total_len = 0;
    int readdir_error = 0;
    for (;;) {{
        ArPalDirEnt ent;
        memset(&ent, 0, sizeof(ent));
        int next = ar_pal_readdir_next(d, &ent, &pal_err);
        if (next == 0) break;
        if (next < 0) {{
            readdir_error = errno;
            break;
        }}
        size_t nlen = ent.name_len;
        if (nlen == SIZE_MAX || count == SIZE_MAX || names_total_len > SIZE_MAX - nlen) {{
            free(ent.name);
            for (size_t j = 0; j < count; j++) free(entries[j].name);
            free(entries);
            ar_pal_readdir_close(d);
            if (c_path != stack_buf) free(c_path);
            *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
            return;
        }}
        if (count == entries_cap) {{
            if (entries_cap > SIZE_MAX / 2 ||
                entries_cap * 2 > SIZE_MAX / sizeof(ArHostDirEntry)) {{
                free(ent.name);
                for (size_t j = 0; j < count; j++) free(entries[j].name);
                free(entries);
                ar_pal_readdir_close(d);
                if (c_path != stack_buf) free(c_path);
                *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
                return;
            }}
            size_t new_cap = entries_cap * 2;
            ArHostDirEntry *grown = (ArHostDirEntry *)realloc(entries, new_cap * sizeof(ArHostDirEntry));
            if (!grown) {{
                free(ent.name);
                for (size_t j = 0; j < count; j++) free(entries[j].name);
                free(entries);
                ar_pal_readdir_close(d);
                if (c_path != stack_buf) free(c_path);
                *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
                return;
            }}
            entries = grown;
            entries_cap = new_cap;
        }}
        entries[count].name = ent.name;
        entries[count].len = nlen;
        entries[count].is_dir = ent.is_dir ? 1 : 0;
        count++;
        names_total_len += nlen;
    }}
    ar_pal_readdir_close(d);
    if (c_path != stack_buf) free(c_path);
    if (readdir_error != 0) {{
        for (size_t j = 0; j < count; j++) free(entries[j].name);
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = ar_map_errno(readdir_error);
        return;
    }}

    if (count == 0) {{
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 0;
        return;
    }}

    if (count > (SIZE_MAX - 8) / (16 + descriptor_size) ||
        names_total_len > SIZE_MAX - 8 - count * (16 + descriptor_size)) {{
        for (size_t j = 0; j < count; j++) free(entries[j].name);
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t descriptor_base = 8 + count * 16;
    size_t names_base = descriptor_base + count * descriptor_size;
    size_t blob_len = names_base + names_total_len;
    // The blob header and entries store count, total size, name offsets and
    // lengths as u32. The ABI narrows allocation size and out values further
    // to uint_c_ty, so reject values that cannot round-trip before allocation.
    if (count > UINT32_MAX || blob_len > UINT32_MAX || blob_len > INT32_MAX ||
        count > (size_t)(({uint_c_ty})-1) ||
        blob_len > (size_t)(({uint_c_ty})-1)) {{
        for (size_t j = 0; j < count; j++) free(entries[j].name);
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    uint8_t *blob = (uint8_t *)ar_vec_malloc(({uint_c_ty})blob_len);
    if (!blob) {{
        for (size_t j = 0; j < count; j++) free(entries[j].name);
        free(entries);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    *(uint32_t *)blob = (uint32_t)count;
    *(uint32_t *)(blob + 4) = (uint32_t)blob_len;
    size_t cursor = names_base;
    for (size_t i = 0; i < count; i++) {{
        size_t base = 8 + i * 16;
        size_t off = cursor - names_base;
        *(uint32_t *)(blob + base) = (uint32_t)off;
        *(uint32_t *)(blob + base + 4) = (uint32_t)entries[i].len;
        blob[base + 8] = entries[i].is_dir;
        memset(blob + base + 9, 0, 7);
        size_t descriptor = descriptor_base + i * descriptor_size;
        uint8_t *name_ptr = blob + cursor;
        {uint_c_ty} name_len = ({uint_c_ty})entries[i].len;
        memcpy(blob + descriptor, &name_ptr, sizeof(name_ptr));
        memcpy(blob + descriptor + sizeof(name_ptr), &name_len, sizeof(name_len));
        if (entries[i].len > 0) {{
            memcpy(blob + cursor, entries[i].name, entries[i].len);
            cursor += entries[i].len;
        }}
        free(entries[i].name);
    }}
    free(entries);
    *out_buf = blob;
    *out_count = ({uint_c_ty})count;
    *out_cap = ({uint_c_ty})blob_len;
    *err = 0;
}}

#if defined(__linux__)
#include <sys/syscall.h>
#endif
#if defined(__APPLE__)
#include <TargetConditionals.h>
#endif
#if (defined(__APPLE__) && TARGET_OS_OSX) || defined(__FreeBSD__) || defined(__OpenBSD__) || defined(__NetBSD__) || defined(__DragonFly__)
#define AR_CAP_COMPONENT_ONLY 1
#endif
#if defined(__linux__) || defined(AR_CAP_COMPONENT_ONLY)
#define AR_CAP_SUPPORTED 1
#endif
static bool ar_cap_path_valid(const uint8_t *path, {uint_c_ty} len, bool allow_dot) {{
    if (!path || len == 0 || len >= 4096) return false;
    if (allow_dot && len == 1 && path[0] == '.') return true;
    size_t begin = 0;
    for (size_t i = 0; i <= (size_t)len; ++i) {{
        if (i < (size_t)len && path[i] == 0) return false;
        if (i == (size_t)len || path[i] == '/') {{
            size_t count = i - begin;
            if (count == 0 || (count == 1 && path[begin] == '.') ||
                (count == 2 && path[begin] == '.' && path[begin + 1] == '.')) return false;
            begin = i + 1;
        }}
    }}
#if defined(AR_CAP_COMPONENT_ONLY)
    if (memchr(path, '/', (size_t)len)) return false;
#endif
    return true;
}}
static int ar_cap_open_beneath(int dir_fd, const char *path, int flags, unsigned mode) {{
#if defined(__linux__) && defined(SYS_openat2)
    if (dir_fd < 0) return -1;
    struct {{ uint64_t flags, mode, resolve; }} how = {{
        (uint64_t)(flags | O_CLOEXEC), (uint64_t)mode,
        UINT64_C(0x08) | UINT64_C(0x04) | UINT64_C(0x02)
    }};
    return (int)syscall(SYS_openat2, dir_fd, path, &how, sizeof(how));
#elif defined(AR_CAP_COMPONENT_ONLY)
    if (dir_fd < 0 || !path || !*path || strchr(path, '/')) return -1;
    int fd = openat(dir_fd, path, flags | O_NOFOLLOW | O_CLOEXEC, (mode_t)mode);
    if (fd < 0) return -1;
    if (flags & O_DIRECTORY) {{
        struct stat metadata;
        if (fstat(fd, &metadata) != 0 || !S_ISDIR(metadata.st_mode)) {{
            close(fd);
            return -1;
        }}
    }}
    return fd;
#else
    (void)dir_fd; (void)path; (void)flags; (void)mode;
    return -1; /* No weaker traversal fallback. */
#endif
}}
static int ar_cap_parent(int dir_fd, char *path, char **leaf) {{
    char *separator = strrchr(path, '/');
    if (separator) {{
        *separator = 0;
        *leaf = separator + 1;
    }} else {{
        *leaf = path;
    }}
#if defined(AR_CAP_SUPPORTED)
    return ar_cap_open_beneath(dir_fd, separator ? path : ".", O_RDONLY | O_DIRECTORY, 0);
#else
    (void)dir_fd;
    return -1;
#endif
}}
static {int_c_ty} ar_fs_dir_open(const uint8_t *path_ptr, {uint_c_ty} path_len) {{
#if defined(AR_CAP_SUPPORTED)
    if (!path_ptr || path_len == 0 || path_len >= 4096 || memchr(path_ptr, 0, (size_t)path_len)) return -1;
    char stack_buf[4096];
    memcpy(stack_buf, path_ptr, (size_t)path_len);
    stack_buf[path_len] = 0;
    return ({int_c_ty})open(stack_buf, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
#else
    (void)path_ptr; (void)path_len;
    return -1;
#endif
}}
static {int_c_ty} ar_fs_dir_openat({int_c_ty} dir_fd, const uint8_t *path_ptr, {uint_c_ty} path_len) {{
#if defined(AR_CAP_SUPPORTED)
    if (!ar_cap_path_valid(path_ptr, path_len, true)) return -1;
    char path[4096]; memcpy(path, path_ptr, (size_t)path_len); path[path_len] = 0;
    return ({int_c_ty})ar_cap_open_beneath((int)dir_fd, path, O_RDONLY | O_DIRECTORY, 0);
#else
    (void)dir_fd; (void)path_ptr; (void)path_len;
    return -1;
#endif
}}
static {int_c_ty} ar_fs_openat({int_c_ty} dir_fd, const uint8_t *path_ptr, {uint_c_ty} path_len, bool read, bool write, bool create, bool truncate) {{
#if defined(AR_CAP_SUPPORTED)
    if (!ar_cap_path_valid(path_ptr, path_len, false) || (!read && !write) || ((create || truncate) && !write)) return -1;
    char path[4096]; memcpy(path, path_ptr, (size_t)path_len); path[path_len] = 0;
    int flags = (read && write) ? O_RDWR : (write ? O_WRONLY : O_RDONLY);
    if (create) flags |= O_CREAT;
    if (truncate) flags |= O_TRUNC;
    return ({int_c_ty})ar_cap_open_beneath((int)dir_fd, path, flags, create ? 0666 : 0);
#else
    (void)dir_fd; (void)path_ptr; (void)path_len; (void)read; (void)write; (void)create; (void)truncate;
    return -1;
#endif
}}
static {int_c_ty} ar_fs_mkdirat({int_c_ty} dir_fd, const uint8_t *path_ptr, {uint_c_ty} path_len) {{
#if defined(AR_CAP_SUPPORTED)
    if (!ar_cap_path_valid(path_ptr, path_len, false)) return -1;
    char path[4096]; memcpy(path, path_ptr, (size_t)path_len); path[path_len] = 0;
    char *leaf = NULL;
    int parent = ar_cap_parent((int)dir_fd, path, &leaf);
    if (parent < 0) return -1;
    int result = -1;
    if (mkdirat(parent, leaf, 0777) == 0) result = ar_cap_open_beneath(parent, leaf, O_RDONLY | O_DIRECTORY, 0);
    close(parent);
    return ({int_c_ty})result;
#else
    (void)dir_fd; (void)path_ptr; (void)path_len;
    return -1;
#endif
}}
static {int_c_ty} ar_fs_unlinkat({int_c_ty} dir_fd, const uint8_t *path_ptr, {uint_c_ty} path_len, bool is_dir) {{
#if defined(AR_CAP_SUPPORTED)
    if (!ar_cap_path_valid(path_ptr, path_len, false)) return -1;
    char path[4096]; memcpy(path, path_ptr, (size_t)path_len); path[path_len] = 0;
    char *leaf = NULL;
    int parent = ar_cap_parent((int)dir_fd, path, &leaf);
    if (parent < 0) return -1;
    int result = unlinkat(parent, leaf, is_dir ? AT_REMOVEDIR : 0);
    close(parent);
    return ({int_c_ty})result;
#else
    (void)dir_fd; (void)path_ptr; (void)path_len; (void)is_dir;
    return -1;
#endif
}}

"#
        );
    }

    pub(super) fn emit_net_runtime(&mut self, uint_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.net host helpers. Socket syscalls live in the net PAL. */
typedef struct ArUdpSlot {{
    uint16_t generation;
    intptr_t fd;
}} ArUdpSlot;

static ArUdpSlot *ar_udp_slots = NULL;
static size_t ar_udp_slot_count = 0;
static size_t ar_udp_slot_cap = 0;
static atomic_flag ar_udp_table_lock = ATOMIC_FLAG_INIT;

#define AR_UDP_GEN_MASK 0x7FFFu
#define AR_UDP_MAX_SLOTS 65536u

static void ar_udp_lock_table(void) {{
    while (atomic_flag_test_and_set_explicit(&ar_udp_table_lock, memory_order_acquire)) {{}}
}}

static void ar_udp_unlock_table(void) {{
    atomic_flag_clear_explicit(&ar_udp_table_lock, memory_order_release);
}}

static int32_t ar_udp_pack(size_t index, uint16_t generation) {{
    return (int32_t)((((uint32_t)generation & (uint32_t)AR_UDP_GEN_MASK) << 16) | (uint32_t)index);
}}

static int ar_udp_unpack(int32_t id, size_t *index_out, uint16_t *generation_out) {{
    if (id < 0) return 0;
    uint32_t raw = (uint32_t)id;
    *index_out = (size_t)(raw & 0xFFFFu);
    *generation_out = (uint16_t)((raw >> 16) & (uint32_t)AR_UDP_GEN_MASK);
    return 1;
}}

static uint16_t ar_udp_bump_generation(uint16_t generation) {{
    uint16_t next = (uint16_t)((generation + 1u) & AR_UDP_GEN_MASK);
    return next == 0 ? 1u : next;
}}

static int ar_udp_reserve(void) {{
    if (ar_udp_slot_count < ar_udp_slot_cap) return 1;
    if (ar_udp_slot_cap >= AR_UDP_MAX_SLOTS) return 0;
    size_t next = ar_udp_slot_cap == 0 ? 8u : ar_udp_slot_cap * 2u;
    if (next > AR_UDP_MAX_SLOTS) next = AR_UDP_MAX_SLOTS;
    ArUdpSlot *grown = (ArUdpSlot *)realloc(ar_udp_slots, next * sizeof(ArUdpSlot));
    if (!grown) return 0;
    for (size_t i = ar_udp_slot_cap; i < next; i++) {{
        grown[i].generation = 0;
        grown[i].fd = (intptr_t)-1;
    }}
    ar_udp_slots = grown;
    ar_udp_slot_cap = next;
    return 1;
}}

static intptr_t ar_udp_duplicate_fd(int32_t id) {{
    size_t index = 0;
    uint16_t generation = 0;
    if (!ar_udp_unpack(id, &index, &generation)) return -1;
    ar_udp_lock_table();
    intptr_t fd = (intptr_t)-1;
    if (index < ar_udp_slot_count && ar_udp_slots[index].generation == generation) {{
        fd = ar_udp_slots[index].fd;
    }}
    intptr_t duplicate = fd != (intptr_t)-1 ? ar_pal_dup(fd) : (intptr_t)-1;
    ar_udp_unlock_table();
    return duplicate;
}}

static int32_t ar_net_udp_bind(const uint8_t *host_ptr, {uint_c_ty} host_len, int32_t port) {{
    if (!host_ptr || host_len == 0 || port < 0 || port > 65535) return -1;
    char host_buf[256];
    if (host_len >= sizeof(host_buf)) return -1;
    memcpy(host_buf, host_ptr, (size_t)host_len);
    host_buf[host_len] = '\0';

    intptr_t fd = ar_pal_udp_bind(host_buf, port);
    if (fd == (intptr_t)-1) return -1;

    ar_udp_lock_table();
    size_t slot = ar_udp_slot_count;
    int reused = 0;
    for (size_t i = 0; i < ar_udp_slot_count; i++) {{
        if (ar_udp_slots[i].fd == (intptr_t)-1) {{
            slot = i;
            reused = 1;
            break;
        }}
    }}
    if (!reused) {{
        if (ar_udp_slot_count >= AR_UDP_MAX_SLOTS || !ar_udp_reserve()) {{
            ar_udp_unlock_table();
            ar_pal_udp_close(fd);
            return -1;
        }}
        slot = ar_udp_slot_count++;
    }}
    uint16_t generation = ar_udp_slots[slot].generation;
    ar_udp_slots[slot].fd = fd;
    ar_udp_unlock_table();
    return ar_udp_pack(slot, generation);
}}

static int32_t ar_net_udp_send_to(int32_t id, const uint8_t *buf, {uint_c_ty} len, const uint8_t *host_ptr, {uint_c_ty} host_len, int32_t port) {{
    if (id < 0 || !buf || !host_ptr || host_len == 0 || port < 0 || port > 65535) return -1;
    intptr_t fd = ar_udp_duplicate_fd(id);
    if (fd == (intptr_t)-1) return -1;

    char host_buf[256];
    if (host_len >= sizeof(host_buf)) {{ ar_pal_udp_close(fd); return -1; }}
    memcpy(host_buf, host_ptr, (size_t)host_len);
    host_buf[host_len] = '\0';

    intptr_t sent = ar_pal_udp_send(fd, buf, (size_t)len, host_buf, port);
    ar_pal_udp_close(fd);
    return sent < 0 ? -1 : (int32_t)sent;
}}

static int32_t ar_net_udp_recv_from(int32_t id, uint8_t *buf, {uint_c_ty} len, int32_t *port_out) {{
    if (id < 0 || !buf || len == 0) return -1;
    intptr_t fd = ar_udp_duplicate_fd(id);
    if (fd == (intptr_t)-1) return -1;
    int port = 0;
    intptr_t recvd = ar_pal_udp_recv(fd, buf, (size_t)len, &port);
    ar_pal_udp_close(fd);
    if (recvd < 0) return -1;
    if (port_out) *port_out = port;
    return (int32_t)recvd;
}}

static void ar_net_udp_close(int32_t id) {{
    size_t index = 0;
    uint16_t generation = 0;
    if (!ar_udp_unpack(id, &index, &generation)) return;
    intptr_t fd = (intptr_t)-1;
    ar_udp_lock_table();
    if (index < ar_udp_slot_count
        && ar_udp_slots[index].generation == generation
        && ar_udp_slots[index].fd != (intptr_t)-1) {{
        fd = ar_udp_slots[index].fd;
        ar_udp_slots[index].fd = (intptr_t)-1;
        ar_udp_slots[index].generation = ar_udp_bump_generation(generation);
    }}
    ar_udp_unlock_table();
    if (fd != (intptr_t)-1) ar_pal_udp_close(fd);
}}

static int32_t ar_net_dns_resolve(const uint8_t *name_ptr, {uint_c_ty} name_len, uint8_t *ip_buf, {uint_c_ty} ip_cap) {{
    if (!name_ptr || name_len == 0 || !ip_buf || ip_cap == 0) return -1;
    char name_buf[256];
    if (name_len >= sizeof(name_buf)) return -1;
    memcpy(name_buf, name_ptr, (size_t)name_len);
    name_buf[name_len] = '\0';
    int n = ar_pal_dns_resolve(name_buf, (char *)ip_buf, (size_t)ip_cap);
    return n < 0 ? -1 : (int32_t)n;
}}
"#
        );
    }
}
