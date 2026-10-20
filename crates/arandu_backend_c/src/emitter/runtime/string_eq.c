/* String equality comparison for ArStr. */
static int64_t ar_str_eq(ArStr a, ArStr b) {
    if (a.len != b.len) return 0;
    if (a.len <= 0) return 1;
    if (!a.ptr || !b.ptr) return a.ptr == b.ptr ? 1 : 0;
    return memcmp(a.ptr, b.ptr, (size_t)a.len) == 0 ? 1 : 0;
}
