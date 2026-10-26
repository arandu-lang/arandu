/* Mirrors arandu_runtime::rt_runtime::phase_lock. The existing ParkingLot
   bucket serializes exact-address state and thread identities validate unlocks.
   The caller keeps the key stationary. */
typedef struct ar_phase_reader_owner {
    ar_usize_t thread_id;
    size_t count;
    struct ar_phase_reader_owner *next;
} ar_phase_reader_owner;
typedef struct ar_phase_state {
    const void *key;
    size_t readers;
    int writer;
    ar_usize_t writer_owner;
    ar_phase_reader_owner *reader_owners;
    ar_cv_waiter *read_head, *read_tail, *write_head, *write_tail;
    struct ar_phase_state *next;
} ar_phase_state;
static ar_phase_state *ar_phase_states[AR_PARKING_LOT_BUCKETS];

static ar_phase_state **ar_phase_link(ar_cv_bucket *bucket, const void *key) {
    ar_phase_state **link = &ar_phase_states[(size_t)(bucket - ar_cv_buckets)];
    while (*link && (*link)->key != key) link = &(*link)->next;
    return link;
}

static void ar_phase_add_reader(ar_phase_state *state, ar_usize_t owner_id) {
    if (state->readers == SIZE_MAX) abort();
    ar_phase_reader_owner *owner = state->reader_owners;
    while (owner && owner->thread_id != owner_id) owner = owner->next;
    if (owner) {
        if (owner->count == SIZE_MAX) abort();
        owner->count++;
    } else {
        owner = (ar_phase_reader_owner*)malloc(sizeof(*owner));
        if (!owner) abort();
        owner->thread_id = owner_id;
        owner->count = 1;
        owner->next = state->reader_owners;
        state->reader_owners = owner;
    }
    state->readers++;
}

static int ar_phase_remove_reader(ar_phase_state *state, ar_usize_t owner_id) {
    ar_phase_reader_owner **link = &state->reader_owners;
    while (*link && (*link)->thread_id != owner_id) link = &(*link)->next;
    ar_phase_reader_owner *owner = *link;
    if (!owner || !owner->count || !state->readers) return 0;
    owner->count--;
    state->readers--;
    if (!owner->count) {
        *link = owner->next;
        free(owner);
    }
    return 1;
}

static void ar_phase_signal(ar_cv_waiter *waiter) {
    waiter->notified = 1;
#if defined(_WIN32)
    WakeConditionVariable(&waiter->cv);
#else
    if (pthread_cond_signal(&waiter->cv) != 0) abort();
#endif
}

static void ar_phase_writer(ar_phase_state *state) {
    ar_cv_waiter *waiter = state->write_head;
    if (!waiter) return;
    state->write_head = waiter->next;
    if (!state->write_head) state->write_tail = NULL;
    state->writer = 1;
    state->writer_owner = waiter->owner_id;
    ar_phase_signal(waiter);
}

static void ar_phase_readers(ar_phase_state *state) {
    ar_cv_waiter *waiter = state->read_head;
    state->read_head = state->read_tail = NULL;
    while (waiter) {
        ar_cv_waiter *next = waiter->next;
        ar_phase_add_reader(state, waiter->owner_id);
        ar_phase_signal(waiter);
        waiter = next;
    }
}

static void ar_phase_release_state(ar_phase_state *state, int write, ar_usize_t owner_id) {
    if (write) {
        if (!state->writer || state->writer_owner != owner_id || state->readers) abort();
        state->writer = 0;
        state->writer_owner = 0;
        ar_phase_readers(state); /* PF3: all already queued readers enter. */
        if (!state->readers) ar_phase_writer(state);
    } else {
        if (state->writer || !ar_phase_remove_reader(state, owner_id)) abort();
        if (!state->readers) ar_phase_writer(state);
    }
}

static void ar_phase_retire(ar_cv_bucket *bucket, ar_phase_state *state) {
    if (!state->writer && !state->readers && !state->read_head && !state->write_head) {
        if (state->writer_owner || state->reader_owners) abort();
        ar_phase_state **link = ar_phase_link(bucket, state->key);
        if (*link != state) abort();
        *link = state->next;
        free(state);
    }
}

#if !defined(_WIN32)
typedef struct {
    ar_cv_bucket *bucket;
    ar_phase_state *state;
    ar_cv_waiter *waiter;
    int write;
} ar_phase_cancel_context;

static void ar_phase_cancel_wait(void *raw) {
    ar_phase_cancel_context *ctx = (ar_phase_cancel_context*)raw;
    ar_phase_state *state = ctx->state;
    if (ctx->waiter->notified) {
        /* POSIX cancellation can race admission; give its reserved slot back. */
        ar_phase_release_state(state, ctx->write, ctx->waiter->owner_id);
    } else {
        ar_cv_waiter **head = ctx->write ? &state->write_head : &state->read_head;
        ar_cv_waiter **tail = ctx->write ? &state->write_tail : &state->read_tail;
        ar_cv_waiter **link = head, *previous = NULL;
        while (*link && *link != ctx->waiter) { previous = *link; link = &(*link)->next; }
        if (*link) {
            *link = ctx->waiter->next;
            if (*tail == ctx->waiter) *tail = previous;
        }
        if (!state->writer && !state->write_head) ar_phase_readers(state);
    }
    ar_phase_retire(ctx->bucket, state);
    ar_cv_unlock(ctx->bucket);
    if (pthread_cond_destroy(&ctx->waiter->cv) != 0) abort();
}
#endif

static int32_t ar_phase_acquire(uint8_t *key, int write, int blocking) {
    if (!key) abort();
    ar_usize_t owner_id = ar_rt_thread_current_id();
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    ar_phase_state **link = ar_phase_link(bucket, key);
    if (!*link) {
        *link = (ar_phase_state*)calloc(1, sizeof(ar_phase_state));
        if (!*link) abort();
        (*link)->key = key;
    }
    ar_phase_state *state = *link;
    int reentrant_reader = 0;
    for (ar_phase_reader_owner *owner = state->reader_owners; owner; owner = owner->next)
        if (owner->thread_id == owner_id) { reentrant_reader = 1; break; }
    if (blocking && (state->writer_owner == owner_id
        || (write && reentrant_reader)
        || (!write && reentrant_reader && state->write_head))) {
        fprintf(stderr,
            "arandu runtime: reentrant %s acquisition on RwLock key %p would deadlock\n",
            write ? "write" : "read", (void*)key);
        abort();
    }
    if (!state->writer && !state->write_head
        && (!write || !state->readers)) {
        if (write) { state->writer = 1; state->writer_owner = owner_id; }
        else ar_phase_add_reader(state, owner_id);
        ar_cv_unlock(bucket);
        return 1;
    }
    if (!blocking) { ar_cv_unlock(bucket); return 0; }
    ar_cv_waiter waiter = {0};
    waiter.owner_id = owner_id;
#if defined(_WIN32)
    InitializeConditionVariable(&waiter.cv);
#else
    if (pthread_cond_init(&waiter.cv, NULL) != 0) abort();
#endif
    ar_cv_waiter **head = write ? &state->write_head : &state->read_head;
    ar_cv_waiter **tail = write ? &state->write_tail : &state->read_tail;
    if (*tail) (*tail)->next = &waiter; else *head = &waiter;
    *tail = &waiter;
#if defined(_WIN32)
    while (!waiter.notified) {
        if (!SleepConditionVariableSRW(&waiter.cv, &bucket->mutex, INFINITE, 0)) abort();
    }
#else
    /* Deferred cancellation removes either its waiter or reserved admission.
       Asynchronous pthread cancellation is outside the runtime contract. */
    ar_phase_cancel_context cancel_ctx = {bucket, state, &waiter, write};
    pthread_cleanup_push(ar_phase_cancel_wait, &cancel_ctx);
    while (!waiter.notified) {
        if (pthread_cond_wait(&waiter.cv, &bucket->mutex) != 0) abort();
    }
    pthread_cleanup_pop(0);
    if (pthread_cond_destroy(&waiter.cv) != 0) abort();
#endif
    ar_cv_unlock(bucket);
    return 1;
}

static void ar_phase_release(uint8_t *key, int write) {
    if (!key) abort();
    ar_usize_t owner_id = ar_rt_thread_current_id();
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    ar_phase_state *state = *ar_phase_link(bucket, key);
    if (!state) abort();
    ar_phase_release_state(state, write, owner_id);
    ar_phase_retire(bucket, state);
    ar_cv_unlock(bucket);
}

static void ar_rt_rwlock_read(uint8_t *key) { (void)ar_phase_acquire(key, 0, 1); }
static void ar_rt_rwlock_write(uint8_t *key) { (void)ar_phase_acquire(key, 1, 1); }
static int32_t ar_rt_rwlock_try_read(uint8_t *key) { return ar_phase_acquire(key, 0, 0); }
static int32_t ar_rt_rwlock_try_write(uint8_t *key) { return ar_phase_acquire(key, 1, 0); }
static void ar_rt_rwlock_read_unlock(uint8_t *key) { ar_phase_release(key, 0); }
static void ar_rt_rwlock_write_unlock(uint8_t *key) { ar_phase_release(key, 1); }
