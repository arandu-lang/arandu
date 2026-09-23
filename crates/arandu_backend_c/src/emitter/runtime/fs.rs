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
}}
/* String allocation helpers. Borrowed string algorithms live in std.core.str. */
static ArStr ar_str_concat(ArStr a, ArStr b) {{
    {len_c_ty} al = a.len < 0 ? 0 : a.len;
    {len_c_ty} bl = b.len < 0 ? 0 : b.len;
    {len_c_ty} total = al + bl;
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) abort();
    if (al > 0 && a.ptr) memcpy(buf, a.ptr, (size_t)al);
    if (bl > 0 && b.ptr) memcpy(buf + al, b.ptr, (size_t)bl);
    buf[total] = 0;
    return ar_str_pack(buf, total);
}}
static {len_c_ty} ar_str_eq(ArStr a, ArStr b) {{
    if (a.len != b.len) return 0;
    if (a.len <= 0) return 1;
    if (!a.ptr || !b.ptr) return a.ptr == b.ptr ? 1 : 0;
    return memcmp(a.ptr, b.ptr, (size_t)a.len) == 0 ? 1 : 0;
}}
static ArStr ar_str_split_last(ArStr s, ArStr sep) {{
    if (s.len <= 0 || !s.ptr) return ar_str_pack((const uint8_t*)"", 0);
    if (sep.len <= 0 || !sep.ptr) {{
        uint8_t *buf = (uint8_t*)malloc((size_t)s.len + 1);
        if (!buf) abort();
        memcpy(buf, s.ptr, (size_t)s.len);
        buf[s.len] = 0;
        return ar_str_pack(buf, s.len);
    }}
    {len_c_ty} last = -1;
    for ({len_c_ty} i = 0; i + sep.len <= s.len; i++) {{
        if (memcmp(s.ptr + i, sep.ptr, (size_t)sep.len) == 0) last = i;
    }}
    {len_c_ty} start = last < 0 ? 0 : last + sep.len;
    {len_c_ty} n = s.len - start;
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    if (n > 0) memcpy(buf, s.ptr + start, (size_t)n);
    buf[n] = 0;
    return ar_str_pack(buf, n);
}}"#
        );
    }

    pub(super) fn emit_fs_runtime(&mut self, uint_c_ty: &str, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.fs host helpers (mirrors JIT ar_fs_* runtime). */
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
    (void)fd; (void)buf; (void)count;
    return -1;
}}

static {int_c_ty} ar_fs_write({int_c_ty} fd, const uint8_t *buf, {uint_c_ty} count) {{
    (void)fd; (void)buf; (void)count;
    return -1;
}}

static {int_c_ty} ar_fs_close({int_c_ty} fd) {{
    (void)fd;
    return 0;
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
#if !defined(_WIN32) || defined(__MINGW32__)
    DIR *d = opendir(c_path);
    if (!d) {{
        int e = errno;
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = ar_map_errno(e);
        return;
    }}
    size_t entries_cap = 16;
    size_t count = 0;
    size_t descriptor_size = sizeof(void *) + sizeof({uint_c_ty});
    ArHostDirEntry *entries = (ArHostDirEntry *)malloc(entries_cap * sizeof(ArHostDirEntry));
    if (!entries) {{
        closedir(d);
        if (c_path != stack_buf) free(c_path);
        *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
        return;
    }}
    size_t names_total_len = 0;
    struct dirent *ent;
    int readdir_error = 0;
    for (;;) {{
        errno = 0;
        ent = readdir(d);
        if (!ent) {{
            readdir_error = errno;
            break;
        }}
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        size_t nlen = strlen(ent->d_name);
        if (nlen == SIZE_MAX || count == SIZE_MAX || names_total_len > SIZE_MAX - nlen) {{
            for (size_t j = 0; j < count; j++) free(entries[j].name);
            free(entries);
            closedir(d);
            if (c_path != stack_buf) free(c_path);
            *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
            return;
        }}
        uint8_t is_dir = 0;
#ifdef DT_DIR
        if (ent->d_type == DT_DIR) is_dir = 1;
        else if (ent->d_type == DT_UNKNOWN) {{
            char sub_path[4096];
            snprintf(sub_path, sizeof(sub_path), "%s/%s", c_path, ent->d_name);
            struct stat st;
            if (stat(sub_path, &st) == 0 && S_ISDIR(st.st_mode)) is_dir = 1;
        }}
#else
        char sub_path[4096];
        snprintf(sub_path, sizeof(sub_path), "%s/%s", c_path, ent->d_name);
        struct stat st;
        if (stat(sub_path, &st) == 0 && S_ISDIR(st.st_mode)) is_dir = 1;
#endif
        if (count == entries_cap) {{
            if (entries_cap > SIZE_MAX / 2 ||
                entries_cap * 2 > SIZE_MAX / sizeof(ArHostDirEntry)) {{
                for (size_t j = 0; j < count; j++) free(entries[j].name);
                free(entries);
                closedir(d);
                if (c_path != stack_buf) free(c_path);
                *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
                return;
            }}
            size_t new_cap = entries_cap * 2;
            ArHostDirEntry *next = (ArHostDirEntry *)realloc(entries, new_cap * sizeof(ArHostDirEntry));
            if (!next) {{
                for (size_t j = 0; j < count; j++) free(entries[j].name);
                free(entries);
                closedir(d);
                if (c_path != stack_buf) free(c_path);
                *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
                return;
            }}
            entries = next;
            entries_cap = new_cap;
        }}
        char *copy = (char *)malloc(nlen + 1);
        if (!copy) {{
            for (size_t j = 0; j < count; j++) free(entries[j].name);
            free(entries);
            closedir(d);
            if (c_path != stack_buf) free(c_path);
            *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
            return;
        }}
        memcpy(copy, ent->d_name, nlen);
        copy[nlen] = '\0';
        entries[count].name = copy;
        entries[count].len = nlen;
        entries[count].is_dir = is_dir;
        count++;
        names_total_len += nlen;
    }}
    closedir(d);
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
#else
    if (c_path != stack_buf) free(c_path);
    *out_buf = NULL; *out_count = 0; *out_cap = 0; *err = 8;
#endif
}}"#
        );
    }
}
