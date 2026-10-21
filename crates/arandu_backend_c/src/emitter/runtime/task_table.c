/* Platform-width unsigned integer matching what the C emitter produces for
   `usize` (uint64_t on LP64/LLP64, uint32_t on ILP32). Used for thread IDs
   shared between static runtime helpers and emitted extern declarations. */
#if UINTPTR_MAX == UINT64_MAX
typedef uint64_t ar_usize_t;
#else
typedef uint32_t ar_usize_t;
#endif

/* SL_R.0 cooperative task table (mirrors JIT ar_rt_*). */
typedef struct {
    int32_t state;          /* 0 EMPTY, 1 PENDING, 2 RUNNING, 3 COMPLETED */
    int32_t cancel_requested;
    uint8_t *blob;          /* owned while PENDING; RUNNING stores none */
    int64_t result;         /* COMPLETED cache */
} ar_rt_slot;
static ar_rt_slot *ar_rt_slots = NULL;
static uint64_t ar_rt_capacity = 0;
static uint64_t ar_rt_len = 0;
/* Table mutations only. The join releases this lock before driving the blob,
   so one task cannot stall spawn/cancel of another. */
static atomic_flag ar_rt_table_lock = ATOMIC_FLAG_INIT;

static void ar_rt_lock_table(void) {
    while (atomic_flag_test_and_set_explicit(&ar_rt_table_lock, memory_order_acquire)) {}
}

static void ar_rt_unlock_table(void) {
    atomic_flag_clear_explicit(&ar_rt_table_lock, memory_order_release);
}

static int64_t ar_rt_spawn_i64(uint8_t *state) {
    if (!state) abort();
    ar_rt_lock_table();
    uint64_t index = ar_rt_capacity;
    for (uint64_t i = 0; i < ar_rt_len; i++) {
        if (ar_rt_slots[i].state == 0) { index = i; break; }
    }
    if (index == ar_rt_capacity) {
        if (ar_rt_len == ar_rt_capacity) {
            uint64_t new_cap = ar_rt_capacity == 0 ? 8 : ar_rt_capacity * 2;
            if (new_cap > (uint64_t)INT64_MAX) {
                ar_rt_unlock_table();
                abort();
            }
            ar_rt_slot *next = (ar_rt_slot*)realloc(
                ar_rt_slots, (size_t)(new_cap * sizeof(ar_rt_slot)));
            if (!next) {
                ar_rt_unlock_table();
                abort();
            }
            memset(next + ar_rt_capacity, 0,
                (size_t)((new_cap - ar_rt_capacity) * sizeof(ar_rt_slot)));
            ar_rt_slots = next;
            ar_rt_capacity = new_cap;
        }
        index = ar_rt_len++;
    }
    ar_rt_slots[index].state = 1;
    ar_rt_slots[index].cancel_requested = 0;
    ar_rt_slots[index].blob = state;
    ar_rt_unlock_table();
    return (int64_t)index;
}

static int64_t ar_rt_join_i64(int64_t handle) {
    ar_rt_lock_table();
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) {
        ar_rt_unlock_table();
        abort();
    }
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 3) {
        int64_t result = slot->result;
        ar_rt_unlock_table();
        return result;
    }
    if (slot->state != 1) {
        ar_rt_unlock_table();
        abort();
    } /* RUNNING = concurrent join; EMPTY = invalid */
    uint8_t *blob = slot->blob;
    slot->state = 2;
    slot->cancel_requested = 0;
    ar_rt_unlock_table();
    int64_t result = ar_co_block_on_i64(blob);
    if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
    free(blob);
    ar_rt_lock_table();
    /* realloc during another spawn may have moved the table. */
    slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->cancel_requested) {
        slot->state = 0;
        slot->blob = NULL;
    } else {
        slot->state = 3;
        slot->result = result;
    }
    ar_rt_unlock_table();
    return result;
}

static void ar_rt_cancel_i64(int64_t handle) {
    ar_rt_lock_table();
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) {
        ar_rt_unlock_table();
        return;
    }
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 1) {
        uint8_t *blob = slot->blob;
        slot->state = 0;
        slot->blob = NULL;
        ar_rt_unlock_table();
        if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
        free(blob);
        return;
    } else if (slot->state == 2) {
        slot->cancel_requested = 1;
    } else if (slot->state == 3) {
        slot->state = 0; /* cached row is retired by cancel (JIT slot.take) */
    } /* EMPTY: no-op */
    ar_rt_unlock_table();
}

static int64_t ar_rt_block_on_i64(uint8_t *state) {
    return ar_co_block_on_i64(state);
}

