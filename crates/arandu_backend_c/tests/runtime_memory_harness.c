#include <assert.h>

/* Drop glue records order and count so leaks, double drops and reentrancy
   show up as wrong counters; ASan reports any use-after-free or double free. */
static int drops = 0;
static uint64_t drop_order[64];
static size_t drop_order_len = 0;
static uint64_t reentrant_token = 0;

static void count_drop(void *raw) {
    uint64_t value;
    memcpy(&value, raw, sizeof(value));
    if (drop_order_len < sizeof(drop_order) / sizeof(drop_order[0]))
        drop_order[drop_order_len++] = value;
    drops++;
}

static void reentrant_drop(void *raw) {
    (void)raw;
    uint64_t out = 0;
    /* Removing the token whose old payload is being dropped must be safe; the
       entry already holds the replacement, which moves out undropped. */
    assert(ar_gen_remove_raw(reentrant_token, &out, sizeof(out), _Alignof(uint64_t)));
    assert(out == 5);
    drops++;
}

static void gen_arena_contract(void) {
    enum { N = 20000 };
    static uint64_t tokens[N];
    for (uint64_t i = 0; i < N; i++) {
        uint64_t value = i;
        tokens[i] = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), NULL);
        assert(tokens[i] == i + 1);
    }
    /* Remove head, tail and every other middle entry; the rest stay reachable. */
    uint64_t out = 0;
    assert(ar_gen_remove_raw(tokens[0], &out, sizeof(out), _Alignof(uint64_t)) && out == 0);
    assert(ar_gen_remove_raw(tokens[N - 1], &out, sizeof(out), _Alignof(uint64_t)) && out == N - 1);
    for (uint64_t i = 2; i < N - 1; i += 2)
        assert(ar_gen_remove_raw(tokens[i], &out, sizeof(out), _Alignof(uint64_t)) && out == i);
    for (uint64_t i = 1; i < N - 1; i++) {
        int live = ar_gen_get_raw(tokens[i], &out, sizeof(out), _Alignof(uint64_t));
        assert(live == (int)(i % 2));
        if (live) assert(out == i);
    }
    /* Stale, zero and layout-mismatched handles are rejected without effect. */
    assert(!ar_gen_get_raw(tokens[0], &out, sizeof(out), _Alignof(uint64_t)));
    assert(!ar_gen_get_raw(0, &out, sizeof(out), _Alignof(uint64_t)));
    uint32_t narrow = 0;
    assert(!ar_gen_get_raw(tokens[1], &narrow, sizeof(narrow), _Alignof(uint32_t)));
    assert(!ar_gen_remove_raw(tokens[1], &narrow, sizeof(narrow), _Alignof(uint32_t)));
    for (uint64_t i = 1; i < N - 1; i += 2)
        assert(ar_gen_remove_raw(tokens[i], &out, sizeof(out), _Alignof(uint64_t)) && out == i);
    assert(ar_gen_len == 0 && !ar_gen_head && !ar_gen_tail);

    /* Tokens stay monotonic after the registry empties. */
    uint64_t value = 1;
    uint64_t first = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop);
    assert(first == N + 1);
    value = 2;
    uint64_t second = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop);
    value = 3;
    uint64_t third = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop);
    /* set drops the replaced payload exactly once. */
    value = 10;
    assert(ar_gen_set_raw(first, &value, sizeof(value), _Alignof(uint64_t), count_drop));
    assert(drops == 1 && drop_order[0] == 1);
    assert(ar_gen_upsert_raw(second, &value, sizeof(value), _Alignof(uint64_t), count_drop) == second);
    assert(drops == 2 && drop_order[1] == 2);
    /* remove moves out: no drop. */
    assert(ar_gen_remove_raw(second, &out, sizeof(out), _Alignof(uint64_t)) && out == 10);
    assert(drops == 2);

    /* Zero-sized and over-aligned payloads. */
    uint64_t zst = ar_gen_insert_raw(&value, 0, 1, NULL);
    assert(zst != 0 && ar_gen_get_raw(zst, &out, 0, 1));
    _Alignas(64) unsigned char wide[64] = {7};
    uint64_t aligned = ar_gen_insert_raw(wide, sizeof(wide), 64, NULL);
    ar_gen_entry *entry = ar_gen_find(aligned);
    assert(entry && ((uintptr_t)entry->data & 63) == 0);
    assert(!ar_gen_insert_raw(wide, sizeof(wide), 3, NULL));

    /* Drop glue may remove the token that is being replaced. */
    value = 2;
    reentrant_token = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), reentrant_drop);
    value = 5;
    assert(ar_gen_set_raw(reentrant_token, &value, sizeof(value), _Alignof(uint64_t), count_drop));
    assert(drops == 3 && !ar_gen_find(reentrant_token));

    /* Shutdown drops remaining payloads in insertion order, once. */
    drop_order_len = 0;
    ar_gen_shutdown_raw();
    assert(drops == 5 && drop_order_len == 2);
    assert(drop_order[0] == 10 && drop_order[1] == 3);
    assert(ar_gen_len == 0 && !ar_gen_find(third));
    ar_gen_shutdown_raw();
    assert(drops == 5);
    value = 4;
    assert(ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop) != 0);
    ar_gen_shutdown_raw();
    assert(drops == 6);
}

static void *gen_arena_worker(void *raw) {
    (void)raw;
    /* A fresh thread starts with its own empty registry and token counter,
       like the JIT thread_local! registry. */
    assert(ar_gen_len == 0 && !ar_gen_head);
    uint64_t value = 77, out = 0;
    uint64_t token = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop);
    assert(token == 1);
    assert(ar_gen_get_raw(token, &out, sizeof(out), _Alignof(uint64_t)) && out == 77);
    ar_gen_shutdown_raw();
    return NULL;
}

static void gen_arena_thread_isolation(void) {
    uint64_t value = 5;
    uint64_t token = ar_gen_insert_raw(&value, sizeof(value), _Alignof(uint64_t), count_drop);
    int before = drops;
    pthread_t thread;
    assert(pthread_create(&thread, NULL, gen_arena_worker, NULL) == 0);
    assert(pthread_join(thread, NULL) == 0);
    assert(drops == before + 1);
    uint64_t out = 0;
    assert(ar_gen_get_raw(token, &out, sizeof(out), _Alignof(uint64_t)) && out == 5);
    ar_gen_shutdown_raw();
    assert(drops == before + 2);
}

static ArStr lit(const char *text) {
    return ar_str_pack((const uint8_t *)text, (int64_t)strlen(text));
}

static void expect_owned(ArStr value, const char *expected) {
    size_t len = strlen(expected);
    assert(value.len == (int64_t)len);
    assert(memcmp(value.ptr, expected, len) == 0 && value.ptr[len] == 0);
    free((void *)value.ptr);
}

static void string_contract(void) {
    expect_owned(ar_str_concat_n(3, lit("ab"), lit(""), lit("cde")), "abcde");
    /* A part with a positive length and no bytes contributes nothing; the
       result never exposes uninitialized bytes. */
    ArStr missing = { NULL, 4 };
    expect_owned(ar_str_concat_n(3, lit("x"), missing, lit("y")), "xy");
    expect_owned(ar_str_concat(lit("x"), missing), "x");
    expect_owned(ar_str_concat(lit("left"), lit("right")), "leftright");
    expect_owned(ar_str_split_last(lit("a/b/c"), lit("/")), "c");
    expect_owned(ar_str_split_last(lit("abab"), lit("ab")), "");
    expect_owned(ar_str_split_last(lit("aaa"), lit("aa")), "");
    expect_owned(ar_str_split_last(lit("plain"), lit("/")), "plain");
    expect_owned(ar_str_split_last(lit("ab"), lit("abc")), "ab");
    expect_owned(ar_str_split_last(lit("ab"), lit("")), "ab");
}

static int64_t ready_task(int64_t value) {
    return (int64_t)(intptr_t)ar_co_make_ready_heap(16, &value, sizeof(value));
}

static void task_table_contract(void) {
    int64_t a = ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(1));
    int64_t b = ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(2));
    int64_t c = ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(3));
    assert(a == 0 && b == 1 && c == 2);
    ar_rt_cancel_i64(b); /* frees the pending blob */
    ar_rt_cancel_i64(b); /* idempotent on an empty row */
    assert(ar_rt_join_i64(a) == 1 && ar_rt_join_i64(a) == 1);
    ar_rt_cancel_i64(a); /* retires the cached row */
    /* The lowest free index is reused first, as in the JIT table. */
    assert(ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(4)) == 0);
    assert(ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(5)) == 1);
    assert(ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(6)) == 3);
    assert(ar_rt_join_i64(0) == 4 && ar_rt_join_i64(1) == 5);
    assert(ar_rt_join_i64(2) == 3 && ar_rt_join_i64(3) == 6);
    for (int64_t i = 0; i < 4; i++) ar_rt_cancel_i64(i);
    for (int64_t i = 0; i < 100; i++)
        assert(ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(i)) == i);
    for (int64_t i = 99; i >= 0; i -= 3) ar_rt_cancel_i64(i);
    for (int64_t i = 0; i < 100; i++) {
        if ((99 - i) % 3 == 0) {
            assert(ar_rt_spawn_i64((uint8_t *)(intptr_t)ready_task(i)) == i);
        }
    }
    for (int64_t i = 0; i < 100; i++) assert(ar_rt_join_i64(i) == i);
    for (int64_t i = 0; i < 100; i++) ar_rt_cancel_i64(i);
    free(ar_rt_slots);
}

static void format_contract(void) {
    /* Out-of-range values must not reach the double -> integer conversion. */
    expect_owned(ar_f64_to_str(1e300), "1e+300");
    expect_owned(ar_f64_to_str(-1e300), "-1e+300");
    expect_owned(ar_f64_to_str(9.3e18), "9.3e+18");
    expect_owned(ar_f64_to_str(42.0), "42");
    expect_owned(ar_f64_to_str(0.5), "0.5");
}

int main(void) {
    gen_arena_contract();
    gen_arena_thread_isolation();
    string_contract();
    task_table_contract();
    format_contract();
    return 0;
}
