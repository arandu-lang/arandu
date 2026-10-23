/* String concatenation helpers for ArStr. */

static ArStr ar_str_concat_n(int n, ...) {
    if (n <= 0) return (ArStr){ .ptr = (const uint8_t*)"", .len = 0 };
    /* Two passes over the arguments (measure, copy) need no parts buffer. */
    va_list ap, copy;
    va_start(ap, n);
    va_copy(copy, ap);
    size_t total = 0;
    for (int i = 0; i < n; i++) {
        ArStr part = va_arg(ap, ArStr);
        if (part.len <= 0) continue;
        /* ArStr.len is signed and pointer-width: reject totals it cannot hold. */
        if ((size_t)part.len > (size_t)PTRDIFF_MAX - 1 - total) { va_end(copy); va_end(ap); abort(); }
        total += (size_t)part.len;
    }
    va_end(ap);
    uint8_t *buf = (uint8_t*)malloc(total + 1);
    if (!buf) { va_end(copy); abort(); }
    size_t off = 0;
    for (int i = 0; i < n; i++) {
        ArStr part = va_arg(copy, ArStr);
        if (part.len > 0 && part.ptr) {
            memcpy(buf + off, part.ptr, (size_t)part.len);
            off += (size_t)part.len;
        }
    }
    va_end(copy);
    /* A part with len > 0 and a null ptr contributes no bytes. */
    buf[off] = 0;
    return (ArStr){ .ptr = buf, .len = off };
}

/* Allocation-free measure/write for proven-private integer concat parts. */
static size_t ar_i64_write_digits(int64_t v, uint8_t *out) {
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "%lld", (long long)v);
    if (n < 0 || (size_t)n >= sizeof(tmp)) abort();
    if (out) memcpy(out, tmp, (size_t)n);
    return (size_t)n;
}

static size_t ar_u64_write_digits(uint64_t v, uint8_t *out) {
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "%llu", (unsigned long long)v);
    if (n < 0 || (size_t)n >= sizeof(tmp)) abort();
    if (out) memcpy(out, tmp, (size_t)n);
    return (size_t)n;
}

/* String allocation helpers. Borrowed string algorithms live in std.core.str. */
static ArStr ar_str_concat(ArStr a, ArStr b) {
    size_t al = a.len > 0 && a.ptr ? (size_t)a.len : 0;
    size_t bl = b.len > 0 && b.ptr ? (size_t)b.len : 0;
    if (bl > (size_t)PTRDIFF_MAX - 1 - al) abort();
    size_t total = al + bl;
    uint8_t *buf = (uint8_t*)malloc(total + 1);
    if (!buf) abort();
    if (al > 0) memcpy(buf, a.ptr, al);
    if (bl > 0) memcpy(buf + al, b.ptr, bl);
    buf[total] = 0;
    return (ArStr){ .ptr = buf, .len = total };
}

static ArStr ar_str_split_last(ArStr s, ArStr sep) {
    if (s.len <= 0 || !s.ptr) return (ArStr){ .ptr = (const uint8_t*)"", .len = 0 };
    if (sep.len <= 0 || !sep.ptr) {
        uint8_t *buf = (uint8_t*)malloc((size_t)s.len + 1);
        if (!buf) abort();
        memcpy(buf, s.ptr, (size_t)s.len);
        buf[s.len] = 0;
        return (ArStr){ .ptr = buf, .len = s.len };
    }
    /* Scan from the end: the last match is the first one found. */
    int64_t start = 0;
    for (int64_t i = s.len - sep.len; i >= 0; i--) {
        if (s.ptr[i] == sep.ptr[0] && memcmp(s.ptr + i, sep.ptr, (size_t)sep.len) == 0) {
            start = i + sep.len;
            break;
        }
    }
    int64_t n = s.len - start;
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    if (n > 0) memcpy(buf, s.ptr + start, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}
