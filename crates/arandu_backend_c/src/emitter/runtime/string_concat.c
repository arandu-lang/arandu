/* String concatenation helpers for ArStr. */

static ArStr ar_str_concat_n(int n, ...) {
    if (n <= 0) return (ArStr){ .ptr = (const uint8_t*)"", .len = 0 };
    va_list ap;
    va_start(ap, n);
    ArStr *parts = (ArStr*)malloc((size_t)n * sizeof(ArStr));
    if (!parts) { va_end(ap); abort(); }
    int64_t total = 0;
    for (int i = 0; i < n; i++) {
        parts[i] = va_arg(ap, ArStr);
        if (parts[i].len > 0) total += parts[i].len;
    }
    va_end(ap);
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) { free(parts); abort(); }
    int64_t off = 0;
    for (int i = 0; i < n; i++) {
        if (parts[i].len > 0 && parts[i].ptr) {
            memcpy(buf + off, parts[i].ptr, (size_t)parts[i].len);
            off += parts[i].len;
        }
    }
    buf[total] = 0;
    free(parts);
    return (ArStr){ .ptr = buf, .len = total };
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
    int64_t al = a.len < 0 ? 0 : a.len;
    int64_t bl = b.len < 0 ? 0 : b.len;
    int64_t total = al + bl;
    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);
    if (!buf) abort();
    if (al > 0 && a.ptr) memcpy(buf, a.ptr, (size_t)al);
    if (bl > 0 && b.ptr) memcpy(buf + al, b.ptr, (size_t)bl);
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
    int64_t last = -1;
    for (int64_t i = 0; i + sep.len <= s.len; i++) {
        if (memcmp(s.ptr + i, sep.ptr, (size_t)sep.len) == 0) last = i;
    }
    int64_t start = last < 0 ? 0 : last + sep.len;
    int64_t n = s.len - start;
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    if (n > 0) memcpy(buf, s.ptr + start, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}
