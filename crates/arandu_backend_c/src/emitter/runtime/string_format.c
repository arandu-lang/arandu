/* ToStr helpers return caller-owned malloc buffers for ArStr. */

static ArStr ar_i64_to_str(int64_t v) {
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "%lld", (long long)v);
    if (n < 0) abort();
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, tmp, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}

static ArStr ar_u64_to_str(uint64_t v) {
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "%llu", (unsigned long long)v);
    if (n < 0) abort();
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, tmp, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}

/* Keep in sync with arandu_runtime::to_str_runtime::format_f64_v01
   (specials + integer-looking values + %.15g for the rest). */
static ArStr ar_f64_to_str(double v) {
    char tmp[64];
    int n;
    if (isnan(v)) {
        n = snprintf(tmp, sizeof(tmp), "nan");
    } else if (isinf(v)) {
        n = snprintf(tmp, sizeof(tmp), "%s", (v < 0) ? "-inf" : "inf");
    } else if (v == (double)(long long)v && v < 1e15 && v > -1e15) {
        n = snprintf(tmp, sizeof(tmp), "%lld", (long long)v);
    } else {
        n = snprintf(tmp, sizeof(tmp), "%.15g", v);
    }
    if (n < 0) abort();
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, tmp, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}

static ArStr ar_bool_to_str(bool v) {
    const char *s = v ? "true" : "false";
    int64_t n = v ? 4 : 5;
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, s, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}

static ArStr ar_char_to_str(uint32_t cp) {
    uint8_t tmp[4];
    int n = 0;
    if (cp <= 0x7F) {
        tmp[0] = (uint8_t)cp;
        n = 1;
    } else if (cp <= 0x7FF) {
        tmp[0] = (uint8_t)(0xC0 | (cp >> 6));
        tmp[1] = (uint8_t)(0x80 | (cp & 0x3F));
        n = 2;
    } else if (cp <= 0xFFFF) {
        tmp[0] = (uint8_t)(0xE0 | (cp >> 12));
        tmp[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
        tmp[2] = (uint8_t)(0x80 | (cp & 0x3F));
        n = 3;
    } else {
        tmp[0] = (uint8_t)(0xF0 | (cp >> 18));
        tmp[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F));
        tmp[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
        tmp[3] = (uint8_t)(0x80 | (cp & 0x3F));
        n = 4;
    }
    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);
    if (!buf) abort();
    memcpy(buf, tmp, (size_t)n);
    buf[n] = 0;
    return (ArStr){ .ptr = buf, .len = n };
}
