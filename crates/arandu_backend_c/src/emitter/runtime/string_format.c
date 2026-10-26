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

/* The shortest decimal that reads back as the finite `v`, in fixed
   notation: the digits Rust's `Display` prints. The precision search keeps
   the first `%.Ne` that `strtod` round-trips. */
static int ar_f64_shortest(double v, char *out, size_t cap) {
    char sci[40];
    for (int precision = 1; precision <= 17; precision++) {
        snprintf(sci, sizeof(sci), "%.*e", precision - 1, v);
        if (strtod(sci, NULL) == v) break;
    }
    char digits[24];
    int count = 0;
    const char *p = sci;
    int negative = *p == '-';
    if (negative) p++;
    for (; *p && *p != 'e'; p++) {
        if (*p >= '0' && *p <= '9' && count < (int)sizeof(digits)) digits[count++] = *p;
    }
    int exponent = *p == 'e' ? atoi(p + 1) : 0;
    while (count > 1 && digits[count - 1] == '0') count--;
    size_t n = 0;
#define AR_PUT(c) do { if (n + 1 < cap) out[n] = (c); n++; } while (0)
    if (negative) AR_PUT('-');
    if (exponent >= count - 1) {
        for (int i = 0; i < count; i++) AR_PUT(digits[i]);
        for (int i = 0; i < exponent - (count - 1); i++) AR_PUT('0');
    } else if (exponent >= 0) {
        for (int i = 0; i <= exponent; i++) AR_PUT(digits[i]);
        AR_PUT('.');
        for (int i = exponent + 1; i < count; i++) AR_PUT(digits[i]);
    } else {
        AR_PUT('0');
        AR_PUT('.');
        for (int i = 0; i < -exponent - 1; i++) AR_PUT('0');
        for (int i = 0; i < count; i++) AR_PUT(digits[i]);
    }
#undef AR_PUT
    out[n < cap ? n : cap - 1] = 0;
    return (int)(n < cap ? n : cap - 1);
}

/* Keep in sync with arandu_runtime::to_str_runtime::format_f64_v01
   (specials + integer-looking values + shortest round-trip digits). */
static ArStr ar_f64_to_str(double v) {
    char tmp[400];
    int n;
    if (isnan(v)) {
        n = snprintf(tmp, sizeof(tmp), "nan");
    } else if (isinf(v)) {
        n = snprintf(tmp, sizeof(tmp), "%s", (v < 0) ? "-inf" : "inf");
    } else if (v < 1e15 && v > -1e15 && v == (double)(long long)v) {
        /* Range first: converting an out-of-range double to an integer is UB. */
        n = snprintf(tmp, sizeof(tmp), "%lld", (long long)v);
    } else {
        n = ar_f64_shortest(v, tmp, sizeof(tmp));
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
